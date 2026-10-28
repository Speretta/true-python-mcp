//! Unit tests for the logical-line model (`crate::fs::lines`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
use super::*;

#[test]
fn logical_lines_handles_edges() {
    assert_eq!(logical_lines(""), Vec::<&str>::new());
    assert_eq!(logical_lines("a\nb\n"), ["a", "b"]);
    assert_eq!(logical_lines("a\nb"), ["a", "b"]);
    assert_eq!(logical_lines("a\n\nb\n"), ["a", "", "b"]);
    assert_eq!(logical_lines("a\r\nb\r\n"), ["a", "b"]);
    assert_eq!(detect_eol("a\r\nb"), "\r\n");
    assert_eq!(detect_eol("a\nb"), "\n");
}

#[test]
fn replace_line_range_preserves_eol_and_trailing() {
    // Replace a middle line, trailing newline preserved.
    assert_eq!(
        replace_line_range("a\nb\nc\n", 2, 2, &["x"]).unwrap(),
        "a\nx\nc\n"
    );
    // Replace multiple lines with more lines.
    assert_eq!(
        replace_line_range("a\nb\nc\n", 1, 2, &["x", "y", "z"]).unwrap(),
        "x\ny\nz\nc\n"
    );
    // Deletion.
    assert_eq!(
        replace_line_range("a\nb\nc\n", 2, 2, &[]).unwrap(),
        "a\nc\n"
    );
    // No trailing newline in the original stays that way.
    assert_eq!(replace_line_range("a\nb", 2, 2, &["x"]).unwrap(), "a\nx");
    // CRLF is preserved.
    assert_eq!(
        replace_line_range("a\r\nb\r\n", 1, 1, &["x"]).unwrap(),
        "x\r\nb\r\n"
    );
    // Provided lines may carry their own terminators; they are stripped.
    assert_eq!(
        replace_line_range("a\nb\n", 2, 2, &["x\n", "y\r\n"]).unwrap(),
        "a\nx\ny\n"
    );
}

#[test]
fn replace_line_range_rejects_bad_bounds() {
    assert!(replace_line_range("a\nb\n", 0, 1, &["x"]).is_err());
    assert!(replace_line_range("a\nb\n", 2, 1, &["x"]).is_err());
    assert!(replace_line_range("a\nb\n", 1, 3, &["x"]).is_err());
    assert!(replace_line_range("", 1, 1, &["x"]).is_err());
}
