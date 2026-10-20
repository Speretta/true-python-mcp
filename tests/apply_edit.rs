//! Direct branch coverage of `tpmcp::tools::apply_edit`, the shared mutation
//! helper behind `cell_edit` and `py_edit`. Each branch is exercised with a
//! plain string, mirroring how tests/analysis.rs and tests/ty.rs call the
//! library directly.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom

use tpmcp::tools::apply_edit;

#[test]
fn exact_match_replaces_once() {
    assert_eq!(
        apply_edit("hello world", "world", "there", false, "file").unwrap(),
        "hello there"
    );
}

#[test]
fn identical_strings_are_rejected() {
    let error = apply_edit("x = 1", "x = 1", "x = 1", false, "file").unwrap_err();
    assert_eq!(error.kind(), "invalid_input");
    assert!(
        error
            .to_string()
            .contains("old_string and new_string are identical"),
        "{error}"
    );
}

#[test]
fn not_found_is_an_error() {
    let error = apply_edit("hello", "xyz", "abc", false, "file").unwrap_err();
    assert_eq!(error.kind(), "invalid_input");
    assert!(
        error.to_string().contains("old_string not found in file"),
        "{error}"
    );
}

#[test]
fn non_unique_without_replace_all_is_an_error() {
    let error = apply_edit("a a a", "a", "b", false, "file").unwrap_err();
    assert_eq!(error.kind(), "invalid_input");
    assert!(error.to_string().contains("occurs 3 times"), "{error}");
    assert!(error.to_string().contains("replace_all=true"), "{error}");
}

#[test]
fn replace_all_replaces_every_occurrence() {
    assert_eq!(
        apply_edit("a a a", "a", "b", true, "file").unwrap(),
        "b b b"
    );
}
