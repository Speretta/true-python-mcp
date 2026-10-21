//! Content search for `notebook_read`'s `query` parameter: case-insensitive
//! substring matching over cell sources, rendered as a match view (only the
//! matching cells and lines) instead of full bodies.

use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::notebook::{AnyNotebook, Cell, Notebook};

/// Maximum matching cells rendered in one response; the rest are reported
/// as a count with a steering line (page or narrow the query).
const MATCH_CELL_CAP: usize = 100;

/// Maximum bytes of one matching line rendered; truncation backs off to a
/// char boundary, so multibyte lines can end slightly earlier.
const MATCH_LINE_BYTES: usize = 120;

/// One matching cell: its absolute notebook index and matching lines
/// (1-based line numbers, as `cell_set_source` ranges use).
pub struct CellHits {
    pub index: usize,
    pub cell_type: String,
    /// marimo block label (`setup`, `broken`, ...) when not a plain cell.
    pub kind: Option<String>,
    pub lines: Vec<(usize, String)>,
}

/// The result of one query: the rendered page of hits plus the totals the
/// response reports so the caller can page or narrow.
pub struct Search {
    pub query: String,
    pub hits: Vec<CellHits>,
    /// Total matching cells in the notebook (before paging).
    pub matched_cells: usize,
    /// Total matching lines in the notebook (before paging).
    pub matched_lines: usize,
    /// Matching cells left after this page (window plus cap).
    pub more: usize,
}

/// Find every cell whose source contains `query` (case-insensitive).
/// `start`/`limit` page through the MATCH LIST, not the notebook.
pub fn search(cells: &[Cell], query: &str, start: Option<usize>, limit: Option<usize>) -> Search {
    let needle = query.to_lowercase();
    let mut matched = Vec::new();
    let mut matched_lines = 0;
    for (index, cell) in cells.iter().enumerate() {
        let source = cell.source_text();
        // Lowercase the whole cell once, not once per line: the lowercased copy
        // is used only for the `contains` test, while the rendered text is
        // still sliced from the original `source` by index (lowercasing never
        // changes the line split, so the two line lists stay aligned).
        let lowercased = source.to_lowercase();
        let lower_lines = crate::fs::logical_lines(&lowercased);
        let lines: Vec<(usize, String)> = crate::fs::logical_lines(&source)
            .iter()
            .enumerate()
            .filter(|(number, _)| lower_lines[*number].contains(&needle))
            .map(|(number, line)| {
                let text = line.trim();
                (
                    number + 1,
                    text[..text.floor_char_boundary(MATCH_LINE_BYTES)].to_string(),
                )
            })
            .collect();
        if lines.is_empty() {
            continue;
        }
        matched_lines += lines.len();
        matched.push(CellHits {
            index,
            cell_type: cell.cell_type.clone(),
            kind: super::kind_label(cell).map(str::to_string),
            lines,
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
        query: query.to_string(),
        hits: matched.into_iter().skip(start).take(end - start).collect(),
        matched_cells,
        matched_lines,
        more,
    }
}

/// The match view as markdown: the standard notebook header, the query
/// totals, then only the matching cells and their matching lines.
pub fn query_markdown(
    path: &str,
    revision: &str,
    notebook: &AnyNotebook,
    search: &Search,
) -> String {
    let cells = notebook.cells();
    let mut out = String::new();
    let _ = write!(
        out,
        "notebook: {path}\nrevision: {revision}\ncells: {} ({}) · {}\n",
        cells.len(),
        super::cell_type_census(cells),
        notebook.format_label()
    );
    let _ = writeln!(
        out,
        "query: \"{}\"\nmatches: {} cell(s), {} line(s)",
        search.query, search.matched_cells, search.matched_lines
    );
    if search.matched_cells == 0 {
        out.push_str("\nno cells match the query\n");
        return out;
    }
    if search.hits.is_empty() {
        out.push_str("\nthis page is empty: start/limit selected no matching cells\n");
    }
    for hits in &search.hits {
        let _ = write!(out, "\n### [{}] {}", hits.index, hits.cell_type);
        if let Some(kind) = &hits.kind {
            let _ = write!(out, " ({kind})");
        }
        let _ = writeln!(out, " · {} match(es)", hits.lines.len());
        for (number, text) in &hits.lines {
            let _ = writeln!(out, "{number:>4} | {text}");
        }
    }
    if search.more > 0 {
        let _ = writeln!(
            out,
            "\n{} more matching cell(s): page with start/limit or narrow the query",
            search.more
        );
    }
    out
}

/// The match view as JSON.
pub fn query_json(path: &str, revision: &str, notebook: &AnyNotebook, search: &Search) -> Value {
    json!({
        "path": path,
        "revision": revision,
        "format": notebook.format_label(),
        "cell_count": notebook.cells().len(),
        "query": search.query,
        "matching_cells": search.matched_cells,
        "matching_lines": search.matched_lines,
        "more_matching_cells": search.more,
        "cells": search
            .hits
            .iter()
            .map(|hits| {
                json!({
                    "index": hits.index,
                    "cell_type": hits.cell_type,
                    "kind": hits.kind,
                    "matches": hits
                        .lines
                        .iter()
                        .map(|(line, text)| json!({"line": line, "text": text}))
                        .collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod tests {
    use super::*;
    use crate::notebook::Cell;

    fn cell(source: &str) -> Cell {
        Cell::from_text("code", source)
    }

    #[test]
    fn finds_case_insensitive_matches_with_line_numbers() {
        let cells = [
            cell("import os\nimport pandas as pd\n"),
            cell("x = 1\n"),
            cell("PANDAS = 2\nos.path\n"),
        ];
        let found = search(&cells, "pandas", None, None);
        assert_eq!(found.matched_cells, 2);
        assert_eq!(found.matched_lines, 2);
        assert_eq!(found.hits[0].index, 0);
        assert_eq!(
            found.hits[0].lines,
            vec![(2, "import pandas as pd".to_string())]
        );
        assert_eq!(found.hits[1].index, 2);
        assert_eq!(found.hits[1].lines[0].0, 1);
        assert_eq!(found.more, 0);
    }

    #[test]
    fn pages_and_caps_the_match_list() {
        let cells: Vec<Cell> = (0..105).map(|_| cell("needle here\n")).collect();
        let found = search(&cells, "NEEDLE", Some(100), None);
        assert_eq!(found.matched_cells, 105);
        assert_eq!(found.hits.len(), 5);
        assert_eq!(found.hits[0].index, 100);
        assert_eq!(found.more, 0);

        let capped = search(&cells, "needle", None, None);
        assert_eq!(capped.hits.len(), MATCH_CELL_CAP);
        assert_eq!(capped.more, 5);

        let paged = search(&cells, "needle", None, Some(3));
        assert_eq!(paged.hits.len(), 3);
        assert_eq!(paged.more, 102);
    }

    #[test]
    fn no_match_is_an_empty_hit_list() {
        let cells = [cell("x = 1\n")];
        let found = search(&cells, "missing", None, None);
        assert_eq!(found.matched_cells, 0);
        assert!(found.hits.is_empty());
    }
}
