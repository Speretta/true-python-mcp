//! The fixed indent of marimo cell bodies: stripping it at parse, recording
//! which lines carried it, and putting it back when an edited cell is
//! regenerated.
//!
//! Lines that lack the fixed indent in the file are byte content, not
//! indentation: the interior of column-0 multiline strings (`sql = """`
//! followed by unindented query text) and stray low-indented comments.
//! Re-indenting them on write-back would silently change string VALUES while
//! the tool's diff (computed on the dedented logical source) shows only the
//! intended edit. marimo's own loader agrees: its token-aware `fixed_dedent`
//! leaves under-indented string interiors completely untouched ("Dedenting
//! such a block would eat into its content"). So every body line's origin is
//! remembered per line and honored on regeneration.

use ruff_python_ast::PySourceType;
use ruff_python_ast::token::TokenKind;
use ruff_python_parser::{ParseOptions, parse_unchecked};
use ruff_text_size::Ranged;

/// Re-indent `text` by `indent`, one trailing newline per line, blank lines
/// left empty (matching marimo's generator). `memory[i] == false` marks a
/// line that lacked the fixed indent in the file (a preserved column-0
/// string-interior or comment line); such lines are written verbatim.
/// `None` memory (the common case) indents every non-empty line. Lines
/// without a memory entry (inserted by an edit) always end in a bare `'\n'`:
/// inside a CRLF file a newly inserted line keeps an LF ending while its
/// neighbors keep CRLF. marimo renormalizes line endings on its own save,
/// so the mix is cosmetic and self-healing.
pub(super) fn indent_lines(text: &str, indent: &str, memory: Option<&[bool]>) -> String {
    let text = text.strip_suffix('\n').unwrap_or(text);
    if text.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for (index, line) in text.split('\n').enumerate() {
        let carried_indent = memory
            .and_then(|flags| flags.get(index))
            .copied()
            .unwrap_or(true);
        if line.is_empty() {
            out.push('\n');
        } else {
            if carried_indent {
                out.push_str(indent);
            }
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Strip a fixed indent prefix from each line of a cell body (best effort;
/// lines indented less than `indent` are preserved verbatim) and record per
/// line whether it carried the indent. `None` memory means every line did.
/// Not to be confused with markdown::dedent_common_margin (textwrap-style).
pub(super) fn strip_fixed_indent(text: &str, indent: &str) -> (String, Option<Vec<bool>>) {
    if indent.is_empty() {
        return (text.trim_end_matches('\n').to_string(), None);
    }
    let mut stripped = String::with_capacity(text.len());
    let mut memory = Vec::new();
    let mut any_preserved = false;
    for line in text.split('\n') {
        match line.strip_prefix(indent) {
            Some(rest) => {
                stripped.push_str(rest);
                memory.push(true);
            }
            None => {
                stripped.push_str(line);
                // Blank lines carry no content to preserve; recording them as
                // indented keeps ordinary bodies at `None` memory.
                let carried = line.is_empty();
                memory.push(carried);
                any_preserved |= !carried;
            }
        }
        stripped.push('\n');
    }
    stripped.truncate(stripped.trim_end_matches('\n').len());
    memory.truncate(count_lines(&stripped));
    (stripped, any_preserved.then_some(memory))
}

/// Recompute the per-line indent memory after an edit.
///
/// Lines that survived the edit (matched by a line-level diff) keep their
/// recorded origin, except a former string-interior line that left the
/// string: as a code line it must carry the indent or the file breaks. New
/// or changed lines carry the indent by default, except inside a multiline
/// string, where they inherit the origin of the string block's first
/// surviving interior line (marimo's loader protects under-indented string
/// interiors per block; a brand-new string defaults to indented, matching
/// what marimo's own save writes).
pub(super) fn remap_indent_memory(
    old_source: &str,
    old_memory: Option<&[bool]>,
    new_source: &str,
) -> Option<Vec<bool>> {
    let Some(old_memory) = old_memory else {
        // Every original line carried the fixed indent and fresh lines
        // default to carrying it: there is nothing to remember.
        return None;
    };
    let new_interiors = string_interiors(new_source);
    let old_interiors = string_interiors(old_source);
    let aligned = line_alignment(old_source, new_source);
    let carried_origin = |old_index: usize| old_memory.get(old_index).copied().unwrap_or(true);

    // The origin each string block lends to interior lines the edit rewrote:
    // its first surviving interior line's flag.
    let mut block_origin: Vec<Option<bool>> = vec![None; new_interiors.block_count];
    for (new_index, old_index) in aligned.iter().enumerate() {
        if let (Some(block), Some(old_index)) = (new_interiors.block_of_line[new_index], *old_index)
        {
            block_origin[block].get_or_insert(carried_origin(old_index));
        }
    }

    let mut memory = Vec::with_capacity(aligned.len());
    let mut any_preserved = false;
    for (new_index, old_index) in aligned.iter().enumerate() {
        let interior = new_interiors.block_of_line[new_index];
        let carried = match (*old_index, interior) {
            (Some(old_index), _) => {
                let left_a_string =
                    old_interiors.block_of_line[old_index].is_some() && interior.is_none();
                // A string line that became code must be re-indented.
                !left_a_string && carried_origin(old_index)
            }
            (None, Some(block)) => block_origin[block].unwrap_or(true),
            (None, None) => true,
        };
        any_preserved |= !carried;
        memory.push(carried);
    }
    any_preserved.then_some(memory)
}

/// Multiline string literal interiors, mirroring marimo's
/// `_get_protected_lines`: every line strictly after a string's opening line
/// through its closing line is interior (its bytes are string content).
struct StringInteriors {
    /// Per source line: the string block the line is interior to, if any.
    block_of_line: Vec<Option<usize>>,
    /// Number of multiline string blocks (ids are dense, 0-based).
    block_count: usize,
}

fn string_interiors(source: &str) -> StringInteriors {
    let mut block_of_line = vec![None; count_lines(source)];
    let mut block_count = 0;
    let mut line_starts: Vec<usize> = vec![0];
    line_starts.extend(
        source
            .bytes()
            .enumerate()
            .filter(|(_, byte)| *byte == b'\n')
            .map(|(index, _)| index + 1),
    );
    let line_of = |offset: usize| line_starts.partition_point(|start| *start <= offset) - 1;

    let parsed = parse_unchecked(source, ParseOptions::from(PySourceType::Python));
    let mut open_start: Option<usize> = None;
    for token in parsed.tokens().iter() {
        let (start, end) = (
            token.range().start().to_usize(),
            token.range().end().to_usize(),
        );
        let span = match token.kind() {
            TokenKind::String => Some((start, end)),
            TokenKind::FStringStart | TokenKind::TStringStart => {
                // Nested f/t-strings stay part of the outermost span.
                if open_start.is_none() {
                    open_start = Some(start);
                }
                None
            }
            TokenKind::FStringEnd | TokenKind::TStringEnd => {
                open_start.take().map(|open| (open, end))
            }
            _ => None,
        };
        if let Some((start, end)) = span
            && source[start..end].contains('\n')
        {
            for line in (line_of(start) + 1)..=line_of(end) {
                if let Some(slot) = block_of_line.get_mut(line) {
                    *slot = Some(block_count);
                }
            }
            block_count += 1;
        }
    }
    super::parse::guarded_drop(parsed, source.len());
    StringInteriors {
        block_of_line,
        block_count,
    }
}

/// For each line of `new_source`, the line of `old_source` it survives from
/// (line-level diff, the same engine the tools' unified diffs use).
fn line_alignment(old_source: &str, new_source: &str) -> Vec<Option<usize>> {
    let mut aligned = vec![None; count_lines(new_source)];
    let diff = similar::TextDiff::from_lines(old_source, new_source);
    for op in diff.ops() {
        if let similar::DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = *op
        {
            for offset in 0..len {
                if let Some(slot) = aligned.get_mut(new_index + offset) {
                    *slot = Some(old_index + offset);
                }
            }
        }
    }
    aligned
}

/// Line count under the canonical form: split after every `\n`, no trailing
/// empty line (matches `SourceText::split_canonical`).
fn count_lines(source: &str) -> usize {
    if source.is_empty() {
        0
    } else {
        source.split('\n').count() - usize::from(source.ends_with('\n'))
    }
}
