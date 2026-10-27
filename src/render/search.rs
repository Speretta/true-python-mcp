//! The `notebook_search` view: finding cells by content or by failure,
//! rendered as a match view (only the matching cells with their evidence)
//! instead of full bodies.
//!
//! The evidence is where the match is: `source:N` line numbers (as
//! `cell_set_source` ranges use) or `output N` positions. A failed cell
//! carries the identity of its stored error output (ename/evalue) plus a
//! capped traceback — the answer to "which cells dropped and why".

use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::notebook::{AnyNotebook, Cell, Notebook};
use crate::render::output::{error_fields, output_text};

/// Maximum matching cells rendered in one response; the rest are reported
/// as a count with a steering line (page or narrow the query).
const MATCH_CELL_CAP: usize = 100;

/// Maximum bytes of one matching line rendered; truncation backs off to a
/// char boundary, so multibyte lines can end slightly earlier.
const MATCH_LINE_BYTES: usize = 120;

/// Maximum traceback lines rendered per failed cell.
const TRACEBACK_LINES: usize = 20;

/// One evidence line: where it matched and the (trimmed, capped) text.
pub struct Hit {
    /// `source:12` or `output 1:7` — the location of the match.
    pub at: String,
    pub text: String,
}

/// One matching cell: its position, its stable reference (Jupyter id or
/// marimo name) when it has one, the evidence lines, and for failed cells
/// the error identity.
pub struct CellHits {
    pub index: usize,
    pub id: Option<String>,
    pub cell_type: String,
    /// marimo block label (`setup`, `broken`, ...) when not a plain cell.
    pub kind: Option<String>,
    pub hits: Vec<Hit>,
    /// `(ename, evalue, capped traceback)` for cells with error outputs.
    pub error: Option<(String, String, Vec<String>)>,
}

/// The result of one search: the rendered page of hits plus the totals the
/// response reports so the caller can page or narrow.
pub struct Search {
    pub query: Option<String>,
    /// What was searched: "source", "output" or "both".
    pub scope: &'static str,
    pub only_failed: bool,
    pub hits: Vec<CellHits>,
    /// Total matching cells in the notebook (before paging).
    pub matched_cells: usize,
    /// Total evidence lines in the notebook (before paging).
    pub matched_lines: usize,
    /// Matching cells left after this page (window plus cap).
    pub more: usize,
    /// An honest note about what the search could not look at (marimo files
    /// store no outputs, for example). Never silent about a missing half.
    pub note: Option<String>,
}

/// The parameters of one search, resolved from the tool arguments.
pub struct SearchSpec {
    pub query: Option<String>,
    pub sources: bool,
    pub outputs: bool,
    pub only_failed: bool,
    pub cell_type: Option<String>,
}

/// Find every cell matching `spec`, then page through the MATCH LIST (not
/// the notebook) with `start`/`limit`.
pub fn search(
    cells: &[Cell],
    spec: &SearchSpec,
    start: Option<usize>,
    limit: Option<usize>,
) -> Search {
    let needle = spec.query.as_ref().map(|query| query.to_lowercase());
    let mut matched = Vec::new();
    let mut matched_lines = 0;
    for (index, cell) in cells.iter().enumerate() {
        if let Some(wanted) = &spec.cell_type
            && cell.cell_type != *wanted
        {
            continue;
        }
        let error = spec.only_failed.then(|| failed_cell(cell)).flatten();
        if spec.only_failed && error.is_none() {
            continue;
        }
        let mut hits = Vec::new();
        if let Some(needle) = &needle {
            if spec.sources {
                let source = cell.source_text();
                let lower = source.to_lowercase();
                let lower_lines = crate::fs::logical_lines(&lower);
                for (number, line) in crate::fs::logical_lines(&source).iter().enumerate() {
                    if lower_lines[number].contains(needle) {
                        hits.push(Hit {
                            at: format!("source:{}", number + 1),
                            text: cap_line(line),
                        });
                    }
                }
            }
            if spec.outputs {
                for (position, output) in cell.outputs().iter().enumerate() {
                    for (number, line) in crate::fs::logical_lines(&output_text(output))
                        .iter()
                        .enumerate()
                    {
                        if line.to_lowercase().contains(needle) {
                            hits.push(Hit {
                                at: format!("output {position}:{}", number + 1),
                                text: cap_line(line),
                            });
                        }
                    }
                }
            }
        }
        let is_hit = if needle.is_some() {
            !hits.is_empty()
        } else {
            error.is_some()
        };
        if !is_hit {
            continue;
        }
        matched_lines += hits.len();
        matched.push(CellHits {
            index,
            id: cell
                .id()
                .map(str::to_string)
                .or_else(|| cell.marimo_name().map(str::to_string)),
            cell_type: cell.cell_type.clone(),
            kind: super::kind_label(cell).map(str::to_string),
            hits,
            error,
        });
    }
    let matched_cells = matched.len();
    let start = start.unwrap_or(0).min(matched_cells);
    let end = match limit {
        Some(limit) => start.saturating_add(limit).min(matched_cells),
        None => matched_cells,
    };
    let end = end.min(start.saturating_add(MATCH_CELL_CAP));
    let more = matched_cells - end;
    Search {
        query: spec.query.clone(),
        scope: match (spec.sources, spec.outputs) {
            (true, true) => "both",
            (false, true) => "output",
            _ => "source",
        },
        only_failed: spec.only_failed,
        hits: matched.into_iter().skip(start).take(end - start).collect(),
        matched_cells,
        matched_lines,
        more,
        note: None,
    }
}

/// The stored error identity of `cell`, if it failed when it ran: the first
/// error output's ename/evalue and a capped traceback.
fn failed_cell(cell: &Cell) -> Option<(String, String, Vec<String>)> {
    for output in cell.outputs() {
        if let Some((name, value)) = error_fields(output) {
            let traceback: Vec<String> = crate::fs::logical_lines(&output_text(output))
                .iter()
                .skip(1) // the "ename: evalue" line the text starts with
                .take(TRACEBACK_LINES)
                .map(|line| line.to_string())
                .collect();
            return Some((name, value, traceback));
        }
    }
    None
}

fn cap_line(line: &str) -> String {
    let text = line.trim();
    text[..text.floor_char_boundary(MATCH_LINE_BYTES)].to_string()
}

/// The search view as markdown: the notebook header, the query totals, then
/// only the matching cells with their evidence.
pub fn search_markdown(
    path: &str,
    revision: &str,
    notebook: &AnyNotebook,
    search: &Search,
) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "notebook: {path}\nrevision: {revision}\ncells: {} ({}) · {}\n",
        notebook.cells().len(),
        super::cell_type_census(notebook.cells()),
        notebook.format_label()
    );
    match (&search.query, search.only_failed) {
        (Some(query), true) => {
            let _ = writeln!(
                out,
                "search: {query:?} in {} of failed cells — {} cell(s), {} line(s)",
                search.scope, search.matched_cells, search.matched_lines
            );
        }
        (Some(query), false) => {
            let _ = writeln!(
                out,
                "search: {query:?} in {} — {} cell(s), {} line(s)",
                search.scope, search.matched_cells, search.matched_lines
            );
        }
        (None, _) => {
            let _ = writeln!(
                out,
                "failed cells: {} (stored error outputs)",
                search.matched_cells
            );
        }
    }
    if let Some(note) = &search.note {
        let _ = writeln!(out, "note: {note}");
    }
    if search.hits.is_empty() {
        // An empty PAGE is not "no matches": say which one it is.
        if search.matched_cells == 0 {
            out.push_str("\n(no cells match)\n");
        } else {
            let _ = writeln!(
                out,
                "\n(this page is empty: {} matching cell(s) in total; page with start/limit)",
                search.matched_cells
            );
        }
        return out;
    }
    for hit in &search.hits {
        out.push('\n');
        let _ = write!(out, "### [{}] {}", hit.index, hit.cell_type);
        if let Some(kind) = &hit.kind {
            let _ = write!(out, " ({kind})");
        }
        if let Some(id) = &hit.id {
            let _ = write!(out, " · id {id}");
        }
        if let Some((name, value, traceback)) = &hit.error {
            let _ = write!(out, "\nerror: {name}: {value}");
            for line in traceback {
                let _ = write!(out, "\n  {line}");
            }
        }
        for evidence in &hit.hits {
            let _ = write!(out, "\n{}: {}", evidence.at, evidence.text);
        }
        out.push('\n');
    }
    if search.more > 0 {
        let _ = write!(
            out,
            "\n({} more matching cell(s); page with start/limit or narrow the query)\n",
            search.more
        );
    }
    out
}

/// The search view as JSON.
pub fn search_json(path: &str, revision: &str, notebook: &AnyNotebook, search: &Search) -> Value {
    let hits: Vec<Value> = search
        .hits
        .iter()
        .map(|hit| {
            let mut value = json!({
                "index": hit.index,
                "cell_type": hit.cell_type,
                "kind": hit.kind,
                "id": hit.id,
                "matches": hit.hits.iter()
                    .map(|evidence| json!({"at": evidence.at, "text": evidence.text}))
                    .collect::<Vec<_>>(),
            });
            if let Some((name, value_text, traceback)) = &hit.error {
                value["error"] = json!({
                    "ename": name,
                    "evalue": value_text,
                    "traceback": traceback,
                });
            }
            value
        })
        .collect();
    json!({
        "path": path,
        "revision": revision,
        "format": notebook.format_label(),
        "cell_count": notebook.cells().len(),
        "query": search.query,
        "search_scope": search.scope,
        "only_failed": search.only_failed,
        "matched_cells": search.matched_cells,
        "matched_lines": search.matched_lines,
        "more": search.more,
        "note": search.note,
        "hits": hits,
    })
}
