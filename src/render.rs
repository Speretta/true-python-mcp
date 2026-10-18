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

use crate::notebook::{AnyNotebook, Cell, Notebook, SourceText};

/// Output format for tool results.
#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// Human/LLM-friendly markdown (default).
    #[default]
    Markdown,
    /// Structured JSON in `structuredContent`.
    Json,
}

/// How much cell content `notebook_read` renders.
#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Detail {
    /// Truncate long sources; compact headers (default).
    #[default]
    Brief,
    /// Numbered source lines (for line-range edits) and complete outputs.
    Full,
}

/// How cell outputs are rendered.
#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WithOutputs {
    /// Omit outputs entirely.
    None,
    /// One compact line per cell listing output types (default).
    #[default]
    Summary,
    /// Render output payloads (text inline, binaries described).
    Full,
}

/// Maximum source lines shown per cell in brief mode.
const BRIEF_SOURCE_LINES: usize = 40;
/// Maximum characters shown per output payload in full mode.
const FULL_OUTPUT_CHARS: usize = 4000;
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
        write_cell_markdown(&mut out, index, cell, detail, with_outputs);
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
    if let Some(kind) = cell
        .extra
        .get("marimo")
        .and_then(|marimo| marimo.get("kind"))
        .and_then(Value::as_str)
        && kind != "cell"
    {
        let _ = write!(out, " ({kind})");
    }
    if let Some(count) = cell.extra.get("execution_count").and_then(Value::as_u64) {
        let _ = write!(out, " · exec {count}");
    }
    let source = cell.source_text();
    let lines = crate::fs::logical_lines(&source);
    let _ = write!(out, " · {} lines", lines.len());
    if let Some(id) = cell.extra.get("id").and_then(Value::as_str) {
        let _ = write!(out, " · id {id}");
    }
    out.push('\n');
    write_source_block(out, &cell.cell_type, &lines, detail);
    write_outputs(out, cell, detail, with_outputs);
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
        Detail::Brief => lines.len().min(BRIEF_SOURCE_LINES),
    };
    for (number, line) in lines.iter().take(shown).enumerate() {
        match detail {
            Detail::Full => {
                let _ = writeln!(out, "{:>4} | {line}", number + 1);
            }
            Detail::Brief => {
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

/// Append the cell's output section to `out`, if it has one to render.
fn write_outputs(out: &mut String, cell: &Cell, detail: Detail, with_outputs: WithOutputs) {
    if matches!(with_outputs, WithOutputs::None) || !cell.is_code() {
        return;
    }
    let Some(outputs) = cell.extra.get("outputs").and_then(Value::as_array) else {
        return;
    };
    if outputs.is_empty() {
        return;
    }
    match with_outputs {
        WithOutputs::None => {}
        WithOutputs::Summary => {
            let _ = writeln!(out, "outputs({}): {}", outputs.len(), summarize(outputs));
        }
        WithOutputs::Full => {
            let _ = writeln!(out, "outputs({}):", outputs.len());
            for (position, output) in outputs.iter().enumerate() {
                let _ = writeln!(out, "  [{position}] {}", render_output_full(output, detail));
            }
        }
    }
}

fn summarize(outputs: &[Value]) -> String {
    outputs
        .iter()
        .map(output_summary)
        .collect::<Vec<_>>()
        .join(", ")
}

/// One-line summary of an output: `stream(stderr)`,
/// `execute_result[text/plain,image/png]`, `error`.
fn output_summary(output: &Value) -> String {
    let kind = output
        .get("output_type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match output.get("data") {
        Some(Value::Object(data)) => {
            let mimes: Vec<&str> = data.keys().map(String::as_str).collect();
            format!("{kind}[{}]", mimes.join(","))
        }
        _ => match output.get("name").and_then(Value::as_str) {
            Some(name) => format!("{kind}({name})"),
            None => kind.to_string(),
        },
    }
}

fn render_output_full(output: &Value, detail: Detail) -> String {
    let kind = output
        .get("output_type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match kind {
        "stream" => {
            let name = output
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("stdout");
            let text = multiline_to_string(output.get("text"));
            format!("stream({name}):\n{}", indent(&truncate(&text, detail)))
        }
        "error" => {
            let ename = output.get("ename").and_then(Value::as_str).unwrap_or("");
            let evalue = output.get("evalue").and_then(Value::as_str).unwrap_or("");
            let traceback = output
                .get("traceback")
                .and_then(Value::as_array)
                .map(|lines| {
                    lines
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            format!(
                "error: {ename}: {evalue}\n{}",
                indent(&truncate(&strip_ansi(&traceback), detail))
            )
        }
        _ => {
            // execute_result / display_data: prefer text/plain, describe binaries.
            let mut parts = Vec::new();
            if let Some(Value::Object(data)) = output.get("data") {
                for (mime, payload) in data {
                    if mime == "text/plain" {
                        parts.push(format!(
                            "{mime}:\n{}",
                            indent(&truncate(&multiline_to_string(Some(payload)), detail))
                        ));
                    } else {
                        parts.push(format!(
                            "{mime}: <{} bytes>",
                            payload.as_str().map(str::len).unwrap_or(0)
                        ));
                    }
                }
            }
            format!("{kind} {}\n", parts.join(" | "))
        }
    }
}

/// `"text"` may be a JSON string or an array of lines; normalize to one string.
fn multiline_to_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(lines)) => lines
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .concat(),
        _ => String::new(),
    }
}

fn truncate(text: &str, detail: Detail) -> String {
    match detail {
        Detail::Full if text.len() <= FULL_OUTPUT_CHARS => text.to_string(),
        Detail::Full => format!(
            "{}\n... truncated ({} chars total)",
            &text[..text.floor_char_boundary(FULL_OUTPUT_CHARS)],
            text.len()
        ),
        Detail::Brief => {
            let lines: Vec<&str> = text.lines().collect();
            let shown = lines.len().min(10);
            let mut out = lines[..shown].join("\n");
            if shown < lines.len() {
                let _ = write!(out, "\n... {} more lines", lines.len() - shown);
            }
            out
        }
    }
}

/// Prefix every line of `text` with four spaces; lines are joined exactly as
/// `str::lines` yields them (a trailing terminator does not add an empty
/// last line).
fn indent(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 4 * text.lines().count());
    for (position, line) in text.lines().enumerate() {
        if position > 0 {
            out.push('\n');
        }
        out.push_str("    ");
        out.push_str(line);
    }
    out
}

/// Remove ANSI escape sequences from tracebacks so they don't burn tokens.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            // CSI: ESC [ ... final byte in @..~
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(next) = chars.next()
                    && !('@'..='~').contains(&next)
                {}
            }
        } else {
            out.push(character);
        }
    }
    out
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
    with_outputs: WithOutputs,
    window: Window,
) -> Value {
    let cells: Vec<Value> = window
        .cells(notebook.cells())
        .map(|(index, cell)| cell_json(index, cell, with_outputs))
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

fn cell_json(index: usize, cell: &Cell, with_outputs: WithOutputs) -> Value {
    let mut value = json!({
        "index": index,
        "cell_type": cell.cell_type,
        "id": cell.extra.get("id").and_then(Value::as_str),
        "execution_count": cell.extra.get("execution_count"),
        "line_count": crate::fs::logical_lines(&cell.source_text()).len(),
        "source": cell.source_text(),
    });
    // Outputs render for code cells only; `None` never reaches the JSON.
    let cell_outputs = || {
        cell.extra
            .get("outputs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
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
