//! Line-based fallback parser for marimo files with a syntax error.
//!
//! When the whole file fails to parse, split it into regions at the column-0
//! boundaries marimo itself uses (`@app.`, `with app.setup`,
//! `app._unparsable_cell`, `if __name__ ==`) and parse each region on its own.
//! Regions that parse become normal cells; regions that do not become
//! read-only "broken" cells, so one bad cell never locks the whole notebook.
//! This mirrors marimo's `scanner.py` recovery that keeps `marimo edit --watch`
//! alive through syntax errors.

use ruff_python_parser::parse_module;
use ruff_text_size::Ranged;

use super::parse::{Builder, Top, app_cell, classify, marimo_extra, read_only_cell, setup_cell};
use super::{CellMeta, Kind, MarimoNotebook};
use crate::notebook::Cell;

#[derive(Clone, Copy)]
enum Region {
    Cell,
    Literal,
}

pub(super) fn scan(src: &str) -> Result<MarimoNotebook, String> {
    let mut builder = Builder::default();

    for (start, end, region) in split_regions(src) {
        let text = &src[start..end];
        match region {
            Region::Literal => {
                if let Ok(parsed) = parse_module(text) {
                    for stmt in parsed.syntax().body.iter() {
                        builder.inspect_literal(stmt);
                    }
                    super::parse::guarded_drop(parsed, text.len());
                }
                builder.push_literal(text.to_string());
            }
            Region::Cell => match parse_cell_region(text) {
                Some((cell, meta, leading, trailing)) => {
                    if !leading.is_empty() {
                        builder.push_literal(leading);
                    }
                    builder.push_cell(cell, meta);
                    if !trailing.is_empty() {
                        builder.push_literal(trailing);
                    }
                }
                None => {
                    let (cell, meta) = broken_cell(text);
                    builder.push_cell(cell, meta);
                }
            },
        }
    }
    builder.finish()
}

/// Parse one cell region; returns the cell plus any leading/trailing literal
/// bytes around the statement (so the region reconstructs byte-for-byte).
fn parse_cell_region(text: &str) -> Option<(Cell, CellMeta, String, String)> {
    let parsed = parse_module(text).ok()?;
    let result = region_cell(text, &parsed);
    super::parse::guarded_drop(parsed, text.len());
    result
}

fn region_cell(
    text: &str,
    parsed: &ruff_python_parser::Parsed<ruff_python_ast::ModModule>,
) -> Option<(Cell, CellMeta, String, String)> {
    let body = &parsed.syntax().body;
    if body.len() != 1 {
        return None;
    }
    let stmt = &body[0];
    let start = stmt.range().start().to_usize();
    let end = stmt.range().end().to_usize();
    let (cell, meta) = match classify(stmt) {
        Top::AppCell(func) => app_cell(text, func, start, end),
        Top::Setup => setup_cell(text, stmt, start, end),
        Top::ReadOnly => read_only_cell(text, stmt, start, end),
        Top::Literal => return None,
    };
    Some((
        cell,
        meta,
        text[..start].to_string(),
        text[end..].to_string(),
    ))
}

/// A region that failed to parse: kept verbatim (byte-exact) and shown
/// read-only so the LLM can see the broken code.
fn broken_cell(text: &str) -> (Cell, CellMeta) {
    let display = text.trim_end_matches(['\n', '\r', ' ', '\t']);
    (
        Cell {
            extra: marimo_extra("broken", None),
            ..Cell::from_text("code", display)
        },
        CellMeta {
            kind: Kind::Broken,
            block: text.to_string(),
            header: String::new(),
            indent: String::new(),
            indent_memory: None,
            return_suffix: None,
            md: None,
            edited: false,
        },
    )
}

/// Split the source into contiguous regions at column-0 cell/guard boundaries.
fn split_regions(src: &str) -> Vec<(usize, usize, Region)> {
    let mut marks: Vec<(usize, Region)> = Vec::new();
    let mut offset = 0usize;
    for line in src.split_inclusive('\n') {
        if let Some(region) = boundary(line) {
            marks.push((offset, region));
        }
        offset += line.len();
    }
    let mut regions = Vec::new();
    let mut cursor = 0usize;
    for (index, &(start, region)) in marks.iter().enumerate() {
        if start > cursor {
            regions.push((cursor, start, Region::Literal));
        }
        let end = marks.get(index + 1).map_or(src.len(), |&(next, _)| next);
        regions.push((start, end, region));
        cursor = end;
    }
    if cursor < src.len() {
        regions.push((cursor, src.len(), Region::Literal));
    }
    regions
}

/// Column-0 boundary detection, mirroring marimo's scanner recovery regex.
fn boundary(line: &str) -> Option<Region> {
    if line.starts_with("@app.cell")
        || line.starts_with("@app.function")
        || line.starts_with("@app.class_definition")
        || line.starts_with("with app.setup")
        || line.starts_with("app._unparsable_cell")
    {
        Some(Region::Cell)
    } else if line.starts_with("if __name__") {
        Some(Region::Literal)
    } else {
        None
    }
}
