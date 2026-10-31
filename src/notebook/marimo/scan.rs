//! Recovery parser for marimo files with a syntax error.
//!
//! When the whole file fails to parse, split it into regions at the cell
//! boundaries marimo itself uses (`@app.cell`, `with app.setup`,
//! `app._unparsable_cell`, `if __name__ ==`) and parse each region on its own.
//! Regions that parse are walked statement-by-statement by the SAME walker
//! the structured parser uses, so both parsers make identical decisions for
//! identical bytes; regions that do not become read-only "broken" cells, so
//! one bad cell never locks the whole notebook.
//!
//! Boundaries are read off the token stream, mirroring marimo's
//! `_ast/scanner.py`: whitespace between tokens is irrelevant (so `@ app.cell`
//! is the same boundary as `@app.cell`), and a `@app.cell` inside a string or
//! comment belongs to that token, never to a new region. The lexer keeps going
//! through characters it cannot tokenize (`TokenKind::Unknown`), which is
//! exactly the degraded input this scanner exists for. A lexer that stops
//! before the end (an unterminated string, say) falls back to a line scan that
//! accepts the same inter-token whitespace, so neither path merges two cells
//! the other keeps.
//!
//! Two coherence rules of our own:
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

use ruff_python_ast::token::{TokenFlags, TokenKind};
use ruff_python_parser::lexer::lex;
use ruff_python_parser::{Mode, parse_module};

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

/// Split the source into contiguous regions at cell/guard boundaries.
fn split_regions(src: &str) -> Vec<(usize, usize, Region)> {
    let marks = token_marks(src).unwrap_or_else(|| line_marks(src));
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

/// Boundary state over the token stream, mirroring marimo's `_BoundaryDetector`
/// (`marimo/_ast/scanner.py`). Each state expects the next token of one
/// spelling (`@ app . cell`, `with app . setup`, `app . _unparsable_cell`,
/// `if __name__ ==`); any other token resets to `Idle`, so a coincidence of
/// tokens cannot fake a boundary.
#[derive(Clone, Copy)]
enum TokenState {
    Idle,
    At,
    AtApp,
    AtAppDot,
    With,
    WithApp,
    WithAppDot,
    AppDirect,
    AppDirectDot,
    If,
    IfName,
}

/// Cell/guard boundary offsets read off the token stream, or `None` when the
/// stream is not trustworthy (an unclosed string, an unrecognized character,
/// or a lexer that stopped before the end); the caller then line-scans.
fn token_marks(src: &str) -> Option<Vec<(usize, Region)>> {
    let mut lexer = lex(src, Mode::Module);
    let mut marks: Vec<(usize, Region)> = Vec::new();
    let mut state = TokenState::Idle;
    let mut start = 0usize;
    let mut consumed = 0usize;
    loop {
        let kind = lexer.next_token();
        if kind == TokenKind::EndOfFile {
            break;
        }
        // An unterminated string is reported as a string token carrying
        // UNCLOSED_STRING, and it swallows the rest of the file, so the token
        // stream cannot be trusted past it. The line scan is marimo's own
        // fallback for the same input.
        if lexer.current_flags().contains(TokenFlags::UNCLOSED_STRING) {
            return None;
        }
        // An unrecognized character is reported as Unknown; the rest of the
        // stream may still be readable, but the line scan is the conservative
        // choice and marimo's fallback for a tokenizer that does not accept it.
        if kind == TokenKind::Unknown {
            return None;
        }
        let range = lexer.current_range();
        let tok_start = range.start().to_usize();
        let tok_end = range.end().to_usize();
        consumed = consumed.max(tok_end);
        let text = src.get(tok_start..tok_end).unwrap_or("");
        let before = tok_start
            .checked_sub(1)
            .and_then(|index| src.as_bytes().get(index))
            .copied();
        let column_zero = matches!(before, None | Some(b'\n' | b'\r'));
        state = match state {
            TokenState::Idle => {
                if column_zero && kind == TokenKind::At {
                    start = tok_start;
                    TokenState::At
                } else if column_zero && kind == TokenKind::With {
                    start = tok_start;
                    TokenState::With
                } else if column_zero && kind == TokenKind::If {
                    start = tok_start;
                    TokenState::If
                } else if column_zero && kind == TokenKind::Identifier && text == "app" {
                    start = tok_start;
                    TokenState::AppDirect
                } else {
                    TokenState::Idle
                }
            }
            TokenState::At if kind == TokenKind::Identifier && text == "app" => TokenState::AtApp,
            TokenState::AtApp if kind == TokenKind::Dot => TokenState::AtAppDot,
            TokenState::AtAppDot => {
                if kind == TokenKind::Identifier
                    && matches!(text, "cell" | "function" | "class_definition")
                {
                    marks.push((start, Region::Cell));
                }
                TokenState::Idle
            }
            TokenState::With if kind == TokenKind::Identifier && text == "app" => {
                TokenState::WithApp
            }
            TokenState::WithApp if kind == TokenKind::Dot => TokenState::WithAppDot,
            TokenState::WithAppDot => {
                if kind == TokenKind::Identifier && text == "setup" {
                    marks.push((start, Region::Cell));
                }
                TokenState::Idle
            }
            TokenState::AppDirect if kind == TokenKind::Dot => TokenState::AppDirectDot,
            TokenState::AppDirectDot => {
                if kind == TokenKind::Identifier && text == "_unparsable_cell" {
                    marks.push((start, Region::Cell));
                }
                TokenState::Idle
            }
            TokenState::If if kind == TokenKind::Identifier && text == "__name__" => {
                TokenState::IfName
            }
            TokenState::IfName => {
                if kind == TokenKind::EqEqual {
                    marks.push((start, Region::Literal));
                }
                TokenState::Idle
            }
            _ => TokenState::Idle,
        };
    }
    // The lexer must also have consumed every non-whitespace byte; a stop
    // somewhere other than the end (not one of the cases above) leaves the
    // marks incomplete, so the caller line-scans.
    let tail = src.get(consumed.min(src.len())..).unwrap_or("");
    if !tail.trim().is_empty() {
        return None;
    }
    Some(marks)
}

/// Boundary offsets from the line scan: the fallback for a lexer that stopped
/// early. Uses the same cell/guard spellings as the token scan.
fn line_marks(src: &str) -> Vec<(usize, Region)> {
    py_lines(src)
        .filter_map(|(offset, line)| boundary(line).map(|region| (offset, region)))
        .collect()
}

/// Column-0 boundary detection for the line fallback (the token scan handles
/// the normal case). Accepts the whitespace Python allows between tokens, so
/// both paths agree on an input either can read.
fn boundary(line: &str) -> Option<Region> {
    if let Some(rest) = line.strip_prefix('@') {
        return match app_access(rest) {
            Some("cell" | "function" | "class_definition") => Some(Region::Cell),
            _ => None,
        };
    }
    if let Some(rest) = keyword_rest(line, "with") {
        return match app_access(rest) {
            Some("setup") => Some(Region::Cell),
            _ => None,
        };
    }
    if let Some(rest) = keyword_rest(line, "if") {
        let after_name = rest.trim_start().strip_prefix("__name__");
        return after_name
            .is_some_and(|tail| tail.trim_start().starts_with("=="))
            .then_some(Region::Literal);
    }
    match app_access(line) {
        Some("_unparsable_cell") => Some(Region::Cell),
        _ => None,
    }
}

/// The remainder after `keyword` when it opens the line as a word, so a longer
/// name such as `within` does not match `with`.
fn keyword_rest<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(keyword)?;
    (rest.starts_with(' ') || rest.starts_with('\t')).then_some(rest)
}

/// `app` + optional whitespace + `.` + optional whitespace + a name, returning
/// the name. The structured parser matches the same spellings through the AST.
fn app_access(rest: &str) -> Option<&str> {
    let rest = rest.trim_start().strip_prefix("app")?;
    let rest = rest.trim_start().strip_prefix('.')?;
    let rest = rest.trim_start();
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    (end > 0).then_some(&rest[..end])
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod tests {
    use super::{Region, boundary, token_marks};

    #[test]
    fn boundary_accepts_the_whitespace_python_allows() {
        for line in ["@app.cell", "@ app.cell", "@app . cell", "@app.cell()"] {
            assert!(
                matches!(boundary(line), Some(Region::Cell)),
                "{line} must be a cell boundary"
            );
        }
        for line in ["with app.setup:", "with  app.setup:"] {
            assert!(
                matches!(boundary(line), Some(Region::Cell)),
                "{line} must be a cell boundary"
            );
        }
        assert!(matches!(
            boundary("if  __name__ == \"__main__\":"),
            Some(Region::Literal)
        ));
        assert!(boundary("within = 1").is_none());
        assert!(boundary("@app.cellx").is_none());
    }

    #[test]
    fn token_marks_ignore_a_boundary_inside_a_string() {
        let src = "abc = \"\"\"\n@app.cell\n\"\"\"\n@app.cell\ndef _():\n    pass\nwith app.setup:\n    pass\n";
        let marks = token_marks(src).expect("the fixture lexes cleanly");
        assert_eq!(marks.len(), 2);
    }
}
