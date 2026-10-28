//! Line-based fallback parser for marimo files with a syntax error.
//!
//! When the whole file fails to parse, split it into regions at the column-0
//! boundaries marimo itself uses (`@app.`, `with app.setup`,
//! `app._unparsable_cell`, `if __name__ ==`) and parse each region on its own.
//! Regions that parse are walked statement-by-statement by the SAME walker
//! the structured parser uses, so both parsers make identical decisions for
//! identical bytes; regions that do not become read-only "broken" cells, so
//! one bad cell never locks the whole notebook. The boundary split mirrors
//! marimo's `scanner.py` recovery that keeps `marimo edit --watch` alive
//! through syntax errors, with two coherence rules of our own:
//!
//! - a boundary preceded by dangling column-0 decorator lines moves back to
//!   the start of that run, because the decorators grammatically belong to
//!   the definition starting the next region and `classify` reads the whole
//!   stack, and
//! - a parsed region keeps every non-cell statement as a literal segment,
//!   exactly like the whole-file parse does.
//!
//! A file only degrades into this scanner when some region is unparseable,
//! so any divergence from the structured parser would silently change the
//! cells no edit touched (property 3 in tests/property.rs pins this).

use ruff_python_parser::parse_module;

use super::parse::{Builder, guarded_drop, marimo_extra, walk_statements};
use super::{CellMeta, Kind, MarimoNotebook};
use crate::notebook::Cell;

#[derive(Clone, Copy)]
enum Region {
    Cell,
    Literal,
}

pub(super) fn scan(src: &str) -> Result<MarimoNotebook, String> {
    let mut builder = Builder::default();
    builder.mark_recovery();

    for (start, end, region) in split_regions(src) {
        let text = &src[start..end];
        match region {
            Region::Literal => push_literal_region(&mut builder, text),
            Region::Cell => match parse_module(text) {
                Ok(parsed) => {
                    walk_statements(&mut builder, text, &parsed);
                    guarded_drop(parsed, text.len());
                }
                Err(_) => {
                    let (cell, meta, trailing) = broken_cell(text);
                    builder.push_cell(cell, meta);
                    if !trailing.is_empty() {
                        builder.push_literal(trailing);
                    }
                }
            },
        }
    }
    builder.finish()
}

/// Push a region as a literal: its statements are inspected for header and
/// version tracking when it parses, and its bytes are kept verbatim either
/// way.
fn push_literal_region(builder: &mut Builder, text: &str) {
    if let Ok(parsed) = parse_module(text) {
        for stmt in parsed.syntax().body.iter() {
            builder.inspect_literal(stmt);
        }
        guarded_drop(parsed, text.len());
    }
    builder.push_literal(text.to_string());
}

/// A region that failed to parse: kept verbatim (byte-exact) and shown
/// read-only so the LLM can see the broken code. The trailing whitespace up
/// to the next region boundary becomes its own literal segment, so broken
/// cells honor the file-wide invariant that cell blocks never end in a
/// newline and separators live between segments; deleting or inserting
/// cells around a broken one must not fuse its neighbors.
fn broken_cell(text: &str) -> (Cell, CellMeta, String) {
    let content_end = text.trim_end_matches(['\n', '\r', ' ', '\t']).len();
    let (block, trailing) = text.split_at(content_end);
    (
        Cell {
            extra: marimo_extra("broken", None),
            ..Cell::from_text("code", block)
        },
        CellMeta {
            kind: Kind::Broken,
            block: block.to_string(),
            header: String::new(),
            indent: String::new(),
            indent_memory: None,
            return_suffix: None,
            md: None,
            edited: false,
        },
        trailing.to_string(),
    )
}

/// Split the source into contiguous regions at column-0 cell/guard boundaries.
fn split_regions(src: &str) -> Vec<(usize, usize, Region)> {
    let mut marks: Vec<(usize, Region)> = Vec::new();
    for (offset, line) in py_lines(src) {
        if let Some(region) = boundary(line) {
            marks.push((offset, region));
        }
    }
    let marks = rejoin_split_decorator_stacks(&marks, src);
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

/// Decorator stacks split across a region boundary: a trailing run of
/// column-0 decorator lines (comment and blank lines transparent) at the end
/// of one region grammatically belongs to the def or class that starts the
/// next region. The structured parser reads the whole stack as ONE statement
/// and `classify` looks at every decorator in it, so the scanner must see the
/// same bytes in one region. Move the boundary back to the start of the run.
///
/// The shift is unconditional (no parse gate): whether the FOLLOWING region
/// parses must not change the segmentation of the previous one, or an edit
/// that breaks one cell would flip its neighbors between parsed and broken
/// (tests/property.rs, property 3).
fn rejoin_split_decorator_stacks(marks: &[(usize, Region)], src: &str) -> Vec<(usize, Region)> {
    let mut adjusted: Vec<(usize, Region)> = Vec::with_capacity(marks.len());
    for (position, &(start, region)) in marks.iter().enumerate() {
        // The region before mark 0 is the leading literal (offset 0); its
        // dangling decorators belong to the first cell block just the same,
        // and without the shift the literal would end on a bare decorator —
        // unparseable, so its header markers would go unrecorded.
        let region_start = position
            .checked_sub(1)
            .map_or(0, |previous| marks[previous].0);
        let shifted =
            dangling_decorator_start(&src[region_start..start]).map(|offset| region_start + offset);
        adjusted.push((shifted.unwrap_or(start), region));
    }
    adjusted
}

/// The offset of the first line of the dangling decorator run at the end of
/// `text`: trailing column-0 `@` lines, skipping blank and comment-only
/// lines between them, stopping at the first content line. `None` when the
/// text does not end in a decorator.
fn dangling_decorator_start(text: &str) -> Option<usize> {
    let content_end = text.trim_end_matches(['\n', '\r', ' ', '\t']).len();
    let mut found = None;
    for (line_start, line) in lines_with_offsets(&text[..content_end]).into_iter().rev() {
        if line.starts_with('@') {
            found = Some(line_start);
        } else if !line.is_empty() && !line.starts_with('#') {
            break;
        }
    }
    found
}

/// The lines of `text` (terminators stripped) paired with their byte offsets.
fn lines_with_offsets(text: &str) -> Vec<(usize, &str)> {
    py_lines(text)
        .map(|(offset, line)| (offset, line.trim_end_matches(['\n', '\r'])))
        .collect()
}

/// The lines of `text` on Python's full set of line terminators (`\n`, `\r\n`,
/// bare `\r`), each still carrying its terminator, paired with byte offsets.
/// `str::split_inclusive('\n')` alone hides a column-0 boundary behind a bare
/// `\r`, which Python's lexer still ends a line at — and a hidden `@app.cell`
/// boundary merges two cells into one region.
fn py_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut rest = text;
    let mut offset = 0usize;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = line_len(rest);
        let (line, tail) = rest.split_at(end);
        rest = tail;
        let start = offset;
        offset += end;
        Some((start, line))
    })
}

/// The byte length of `text`'s first line, terminator included.
fn line_len(text: &str) -> usize {
    for (index, byte) in text.bytes().enumerate() {
        match byte {
            b'\n' => return index + 1,
            b'\r' => {
                return if text.as_bytes().get(index + 1) == Some(&b'\n') {
                    index + 2
                } else {
                    index + 1
                };
            }
            _ => {}
        }
    }
    text.len()
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
