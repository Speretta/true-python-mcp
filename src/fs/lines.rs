//! The logical-line model behind line-range edits: how a text's lines and
//! line endings are read, joined and replaced. Pure text: the filesystem
//! primitives never touch it, so it lives apart from them (split out of
//! `fs.rs` to keep each file under the 500-line house limit).

/// Majority line ending of `text`: CRLF when CRLF-terminated lines
/// outnumber LF-only-terminated ones, else LF (ties go to LF, the more
/// common default). The previous rule ("CRLF if it occurs anywhere") let
/// a single stray CRLF flip a 1000-line LF cell to CRLF on the next
/// line-range edit.
pub fn detect_eol(text: &str) -> &'static str {
    let lf = text.split('\n').count() - 1; // every '\n', CRLF-terminated or not
    let crlf = text.matches("\r\n").count();
    if crlf > lf.saturating_sub(crlf) {
        "\r\n"
    } else {
        "\n"
    }
}

/// A line without its terminator: one trailing `\n` and, before it, one
/// optional `\r` are stripped.
fn strip_terminator(line: &str) -> &str {
    line.strip_suffix('\n')
        .map(|stripped| stripped.strip_suffix('\r').unwrap_or(stripped))
        .unwrap_or(line)
}

/// Logical (1-based editor) lines of `text`, without line terminators.
/// Empty text has no lines. A trailing terminator ends the last line
/// rather than creating an empty one.
pub fn logical_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split_inclusive('\n').map(strip_terminator).collect()
}

/// Join logical lines with `eol`, appending a trailing terminator when
/// `trailing_eol` is set. Trailing terminators on the input lines are
/// stripped first so callers can pass lines verbatim from an LLM.
pub fn join_logical(lines: &[&str], eol: &str, trailing_eol: bool) -> String {
    let stripped: Vec<&str> = lines.iter().map(|line| strip_terminator(line)).collect();
    let mut joined = stripped.join(eol);
    if trailing_eol && !stripped.is_empty() {
        joined.push_str(eol);
    }
    joined
}

/// Replace the 1-based inclusive logical line range `start..=end` of
/// `text` with `new_lines` (empty = deletion).
///
/// Lines outside the range keep their original terminators byte-for-byte;
/// a stray CRLF among LF lines no longer flips the untouched lines.
/// Replacement lines are joined with the majority EOL of `text`
/// ([`detect_eol`]); terminators the caller's lines carry are stripped
/// first (LLMs often include them). When the range reaches the end of the
/// text, the original trailing-newline property applies to the last
/// replacement line; deleting the last line of a text without a trailing
/// newline leaves the previous line's terminator in place (byte
/// preservation wins over the trailing property).
pub fn replace_line_range(
    text: &str,
    start: usize,
    end: usize,
    new_lines: &[&str],
) -> Result<String, String> {
    let segments: Vec<&str> = if text.is_empty() {
        Vec::new()
    } else {
        text.split_inclusive('\n').collect()
    };
    if start < 1 || start > end || end > segments.len() {
        return Err(format!(
            "line range {start}..{end} out of bounds: text has {} lines",
            segments.len()
        ));
    }
    let eol = detect_eol(text);
    let trailing_eol = text.ends_with('\n');
    let suffix = &segments[end..];
    let mut out = String::with_capacity(text.len() + new_lines.len() * 16);
    out.push_str(&segments[..start - 1].concat());
    for (index, line) in new_lines.iter().enumerate() {
        out.push_str(strip_terminator(line));
        let is_last = index + 1 == new_lines.len();
        if !is_last || !suffix.is_empty() || trailing_eol {
            out.push_str(eol);
        }
    }
    out.push_str(&suffix.concat());
    Ok(out)
}

#[cfg(test)]
#[path = "lines_tests.rs"]
mod tests;
