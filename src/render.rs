//! Rendering of notebook content for LLM consumption.
//!
//! Markdown (the default) is token-efficient: indexed cell headers, fenced
//! sources and output summaries. JSON carries the same information as
//! structured content. Both modes always include the file revision so the
//! model can chain mutations without re-reading.

// Rendered views are built by writing directly into their destination
// buffer. `fmt::Write for String` is infallible — it grows the string and
// never returns `Err` (allocation failure aborts) — and this crate denies
// panicking wrappers, so every `write!` result below is discarded with
// `let _ =`.
use std::fmt::Write as _;

use serde::Deserialize;
use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};

use crate::notebook::{AnyNotebook, Cell, Finding, Notebook, Severity, SourceText};
use crate::ruff::Diagnostic;

pub mod search;

mod output;

use output::{summarize, write_outputs};

/// Output format for tool results (values documented on each tool param).
#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Markdown,
    Json,
}

/// How much cell content `notebook_read` renders (values documented on the
/// param).
#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Detail {
    #[default]
    Brief,
    Full,
    Outline,
}

/// How cell outputs are rendered (values documented on the param).
#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WithOutputs {
    None,
    #[default]
    Summary,
    Full,
}

/// Maximum source lines shown per cell in brief mode.
const BRIEF_SOURCE_LINES: usize = 40;
/// Rough markdown overhead per cell (header line, source fences, output
/// summary line), used as a buffer capacity hint.
const PER_CELL_MARKDOWN_OVERHEAD: usize = 96;

/// A cell window requested by the caller.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub start: usize,
    pub end: usize,
}

impl Window {
    /// Resolve `start`/`limit` arguments against the cell count.
    pub fn resolve(cell_count: usize, start: Option<usize>, limit: Option<usize>) -> Self {
        let start = start.unwrap_or(0).min(cell_count);
        let end = match limit {
            Some(limit) => start.saturating_add(limit).min(cell_count),
            None => cell_count,
        };
        Self { start, end }
    }

    /// True when the window is empty and starts at or beyond the last cell.
    pub fn is_past_the_end(&self, cell_count: usize) -> bool {
        self.start >= self.end && self.start >= cell_count
    }

    /// The windowed cells paired with their absolute notebook indexes.
    pub fn cells<'a>(&self, cells: &'a [Cell]) -> impl Iterator<Item = (usize, &'a Cell)> {
        cells.iter().enumerate().take(self.end).skip(self.start)
    }
}

/// Render the notebook header plus the selected cell window as markdown.
pub fn notebook_markdown(
    path: &str,
    revision: &str,
    notebook: &AnyNotebook,
    detail: Detail,
    with_outputs: WithOutputs,
    window: Window,
) -> String {
    let cells = notebook.cells();
    let mut out = String::with_capacity(markdown_capacity(cells, window));
    let _ = write!(
        out,
        "notebook: {path}\nrevision: {revision}\ncells: {} ({}) · {}\n",
        cells.len(),
        cell_type_census(cells),
        notebook.format_label()
    );
    if window.is_past_the_end(cells.len()) {
        out.push_str("\n(no cells in range)");
        return out;
    }
    for (index, cell) in window.cells(cells) {
        out.push('\n');
        match detail {
            Detail::Outline => write_cell_outline(&mut out, index, cell),
            _ => write_cell_markdown(&mut out, index, cell, detail, with_outputs),
        }
    }
    out
}

/// Capacity hint for a rendered notebook: the windowed cells' source bytes
/// (the dominant term) plus a per-cell allowance for headers and fences.
/// Only a hint — full detail numbers every line and can render output
/// payloads, neither of which is cheap to predict, so the buffer may grow.
fn markdown_capacity(cells: &[Cell], window: Window) -> usize {
    window
        .cells(cells)
        .map(|(_, cell)| source_byte_len(cell) + PER_CELL_MARKDOWN_OVERHEAD)
        .sum()
}

/// Length of the cell source in bytes, without materializing the text.
fn source_byte_len(cell: &Cell) -> usize {
    match &cell.source {
        SourceText::Str(text) => text.len(),
        SourceText::Lines(lines) => lines.iter().map(String::len).sum(),
    }
}

fn cell_type_census(cells: &[Cell]) -> String {
    let code = cells.iter().filter(|cell| cell.is_code()).count();
    let markdown = cells.iter().filter(|cell| cell.is_markdown()).count();
    let other = cells.len() - code - markdown;
    let mut parts = vec![format!("{code} code"), format!("{markdown} markdown")];
    if other > 0 {
        parts.push(format!("{other} other"));
    }
    parts.join(", ")
}

/// Render one cell as markdown: header, fenced source, outputs.
pub fn cell_markdown(
    index: usize,
    cell: &Cell,
    detail: Detail,
    with_outputs: WithOutputs,
) -> String {
    let mut out = String::with_capacity(source_byte_len(cell) + PER_CELL_MARKDOWN_OVERHEAD);
    write_cell_markdown(&mut out, index, cell, detail, with_outputs);
    out
}

/// Append one cell's markdown rendering to `out`; [`cell_markdown`] is the
/// owned-string form for rendering a single cell.
fn write_cell_markdown(
    out: &mut String,
    index: usize,
    cell: &Cell,
    detail: Detail,
    with_outputs: WithOutputs,
) {
    let _ = write!(out, "### [{index}] {}", cell.cell_type);
    // marimo special blocks (setup/function/class/broken) get a kind label.
    if let Some(kind) = cell.marimo_kind()
        && kind != "cell"
    {
        let _ = write!(out, " ({kind})");
    }
    if let Some(count) = cell.execution_count() {
        let _ = write!(out, " · exec {count}");
    }
    let source = cell.source_text();
    let lines = crate::fs::logical_lines(&source);
    let _ = write!(out, " · {} lines", lines.len());
    if let Some(id) = cell.id() {
        let _ = write!(out, " · id {id}");
    }
    out.push('\n');
    write_source_block(out, &cell.cell_type, &lines, detail);
    write_outputs(out, cell, detail, with_outputs);
}

/// One outline row: index, type, line count, first source line (trimmed to
/// [`OUTLINE_FIRST_LINE_CHARS`]). The navigational view of a notebook.
fn write_cell_outline(out: &mut String, index: usize, cell: &Cell) {
    const OUTLINE_FIRST_LINE_CHARS: usize = 80;
    let source = cell.source_text();
    let line_count = logical_line_count(&source);
    let first = crate::fs::logical_lines(&source)
        .first()
        .map(|line| line.trim())
        .unwrap_or("");
    let first = &first[..first.floor_char_boundary(OUTLINE_FIRST_LINE_CHARS)];
    let _ = write!(out, "### [{index}] {}", cell.cell_type);
    if let Some(kind) = cell.marimo_kind()
        && kind != "cell"
    {
        let _ = write!(out, " ({kind})");
    }
    let _ = writeln!(out, " · {line_count} lines · {first}");
}

/// Append the fenced source block of a cell; full detail numbers each line
/// (the numbers `cell_set_source` line ranges refer to), brief shows the
/// first [`BRIEF_SOURCE_LINES`] lines raw.
fn write_source_block(out: &mut String, cell_type: &str, lines: &[&str], detail: Detail) {
    let language = match cell_type {
        "code" => "python",
        "markdown" => "markdown",
        _ => "text",
    };
    let _ = writeln!(out, "```{language}");
    let shown = match detail {
        Detail::Full => lines.len(),
        Detail::Brief | Detail::Outline => lines.len().min(BRIEF_SOURCE_LINES),
    };
    for (number, line) in lines.iter().take(shown).enumerate() {
        match detail {
            Detail::Full => {
                let _ = writeln!(out, "{:>4} | {line}", number + 1);
            }
            Detail::Brief | Detail::Outline => {
                let _ = writeln!(out, "{line}");
            }
        }
    }
    if shown < lines.len() {
        let _ = writeln!(
            out,
            "... {} more lines (use detail=full)",
            lines.len() - shown
        );
    }
    out.push_str("```\n");
}

/// Unified diff between two source texts, used by `cell_edit`.
pub fn unified_diff(old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    // Every line of each side is emitted at most once (equal lines exactly
    // once), so the two input lengths are a good capacity bound.
    let mut out = String::with_capacity(old.len() + new.len());
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Delete => '-',
            ChangeTag::Insert => '+',
            ChangeTag::Equal => ' ',
        };
        out.push(sign);
        out.push_str(change.value());
        if !change.value().ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// The JSON representation of the notebook for `format=json` results.
pub fn notebook_json(
    path: &str,
    revision: &str,
    notebook: &AnyNotebook,
    detail: Detail,
    with_outputs: WithOutputs,
    window: Window,
) -> Value {
    let cells: Vec<Value> = window
        .cells(notebook.cells())
        .map(|(index, cell)| match detail {
            Detail::Outline => outline_cell_json(index, cell),
            _ => cell_json(index, cell, with_outputs),
        })
        .collect();
    json!({
        "path": path,
        "revision": revision,
        "format": notebook.format_label(),
        "cell_count": notebook.cells().len(),
        "range": [window.start, window.end],
        "cells": cells,
    })
}

fn outline_cell_json(index: usize, cell: &Cell) -> Value {
    let source = cell.source_text();
    json!({
        "index": index,
        "cell_type": cell.cell_type,
        "line_count": logical_line_count(&source),
        "first_line": crate::fs::logical_lines(&source)
            .first()
            .map(|line| line.trim())
            .unwrap_or(""),
    })
}

/// Export a notebook as a standalone markdown document: markdown cells
/// verbatim, other cells fenced, outputs blockquoted. Binary outputs and
/// Jupyter attachments are not exported; each is reported in the returned
/// warnings, so a conversion never loses content silently.
pub fn markdown_export(notebook: &AnyNotebook) -> (String, Vec<String>) {
    let mut out = String::new();
    let mut warnings = Vec::new();
    for (index, cell) in notebook.cells().iter().enumerate() {
        let source = cell.source_text();
        warn_attachments(index, cell, &mut warnings);
        if cell.is_markdown() {
            let _ = writeln!(out, "{}\n", source.trim_end());
            continue;
        }
        let language = if cell.is_code() { "python" } else { "text" };
        let _ = writeln!(out, "```{language}\n{}\n```", source.trim_end());
        output::export_outputs(&mut out, index, cell, &mut warnings);
    }
    (out, warnings)
}

/// Jupyter attachments (images embedded in cell metadata) have no place in a
/// standalone markdown document; report them so the export never loses
/// content silently. Direct `extra` read: no typed accessor exists for
/// attachments.
fn warn_attachments(index: usize, cell: &Cell, warnings: &mut Vec<String>) {
    let Some(attachments) = cell
        .extra
        .get("attachments")
        .and_then(|value| value.as_object())
    else {
        return;
    };
    if attachments.is_empty() {
        return;
    }
    let names: Vec<&str> = attachments.keys().map(String::as_str).collect();
    warnings.push(format!(
        "cell {index}: attachments not exported ({})",
        names.join(", ")
    ));
}

fn cell_json(index: usize, cell: &Cell, with_outputs: WithOutputs) -> Value {
    // Bind the concatenated source once: both `line_count` and `"source"`
    // derive from it (concatenating per cell is the expensive part).
    let source = cell.source_text();
    let line_count = logical_line_count(&source);
    let mut value = json!({
        "index": index,
        "cell_type": cell.cell_type,
        "id": cell.id(),
        "execution_count": cell.extra.get("execution_count"),
        "line_count": line_count,
        "source": source,
    });
    // Outputs render for code cells only; `None` never reaches the JSON.
    let cell_outputs = || cell.outputs().to_vec();
    match with_outputs {
        WithOutputs::Summary if cell.is_code() => {
            value["outputs"] = Value::String(summarize(&cell_outputs()));
        }
        WithOutputs::Full if cell.is_code() => {
            value["outputs"] = Value::Array(cell_outputs());
        }
        _ => {}
    }
    value
}

/// Number of lines [`crate::fs::logical_lines`] would yield, in one byte
/// scan and without allocating the line slice: every `'\n'` terminates one
/// line, and a non-empty text not ending in `'\n'` has one unterminated
/// final line. Empty text has no lines.
fn logical_line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let terminators = text.bytes().filter(|byte| *byte == b'\n').count();
    terminators + usize::from(!text.ends_with('\n'))
}

/// One lint diagnostic as a markdown line (shared by `ruff_check` and the
/// `notebook_read` diagnostics view).
pub fn diagnostic_line(diagnostic: &Diagnostic) -> String {
    let mut line = String::new();
    if let Some(cell) = diagnostic.cell {
        line.push_str(&format!("cell {cell} "));
    }
    if let Some(location) = &diagnostic.location {
        line.push_str(&format!("{}:{} ", location.row, location.column));
    }
    line.push_str(&format!(
        "{} {}",
        diagnostic.code.as_deref().unwrap_or("-"),
        diagnostic.message
    ));
    if let Some(fix) = &diagnostic.fix {
        line.push_str(&format!(" [fixable: {}]", fix.applicability));
    }
    line
}

/// One validation finding as a markdown bullet (shared by `notebook_check`
/// and the `notebook_read` diagnostics view).
pub fn finding_line(finding: &Finding) -> String {
    let severity = match finding.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    };
    let cell = finding
        .cell
        .map_or(String::new(), |cell| format!(" cell {cell}"));
    format!("- {severity}{cell} `{}`: {}", finding.code, finding.message)
}

/// The diagnostics section `notebook_read` appends when validation was
/// requested: `findings` are structural (notebook_check's list); `lint` is
/// `Some` only for `check+lint`.
pub fn diagnostics_markdown(findings: &[Finding], lint: Option<&[Diagnostic]>) -> String {
    let mut out = format!("\ndiagnostics: {} finding(s)", findings.len());
    if let Some(lint) = lint {
        let _ = write!(out, ", {} lint issue(s)", lint.len());
    }
    out.push('\n');
    if findings.is_empty() && lint.unwrap_or(&[]).is_empty() {
        out.push_str("no findings: the notebook is valid\n");
        return out;
    }
    for finding in findings {
        let _ = writeln!(out, "\n{}", finding_line(finding));
    }
    for diagnostic in lint.unwrap_or(&[]) {
        let _ = writeln!(out, "\n- lint {}", diagnostic_line(diagnostic));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The allocation-free counter must agree with the slice-building
    /// original on every edge shape: empty text, unterminated final line,
    /// bare and doubled terminators, CRLF.
    #[test]
    fn logical_line_count_matches_logical_lines() {
        for text in [
            "", "a", "a\n", "a\nb", "a\r\nb\n", "\n", "\n\n", "\r", "\r\n", "a\n\nb\n", "a\r\nb",
        ] {
            assert_eq!(
                logical_line_count(text),
                crate::fs::logical_lines(text).len(),
                "line count mismatch for {text:?}"
            );
        }
    }
}
