//! Unit tests for validation helpers (builtin set, return/signature parsing).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
use super::{is_marimo_builtin, parse_params, parse_return, split_top_level};

/// Our builtin set must match marimo's BUILTINS (dumped from the real
/// marimo package) so signature regeneration checks agree.
#[test]
fn builtins_match_marimo() {
    let dump = include_str!("../../../tests/data/marimo_builtins.json");
    let names: Vec<&str> = serde_json::from_str(dump).expect("valid json");
    assert!(names.len() > 100);
    for name in names {
        assert!(is_marimo_builtin(name), "{name} should be a builtin");
    }
    for name in ["numpy", "mo", "x", "print_"] {
        assert!(!is_marimo_builtin(name), "{name} is not a builtin");
    }
}

/// The builtin set is pinned to Python 3.13, not the latest Python: 3.13's
/// `PythonFinalizationError` is a builtin while 3.15's `sentinel` is not. This
/// pins the assumption so a bump is a deliberate change, not an accident.
#[test]
fn builtins_are_pinned_to_python_313() {
    assert!(is_marimo_builtin("PythonFinalizationError"));
    assert!(!is_marimo_builtin("sentinel"));
}

#[test]
fn return_suffix_parsing() {
    assert_eq!(parse_return("    return"), Vec::<String>::new());
    assert_eq!(parse_return("    return (mo,)"), vec!["mo"]);
    assert_eq!(parse_return("    return (a, b)"), vec!["a", "b"]);
    assert_eq!(parse_return("    return x"), vec!["x"]);
}

#[test]
fn signature_parsing() {
    assert_eq!(parse_params("@app.cell\ndef _(mo):\n"), vec!["mo"]);
    assert_eq!(parse_params("@app.cell\ndef _():\n"), Vec::<String>::new());
    assert_eq!(
        parse_params("@app.cell\ndef _(a, b: int, c: tuple[int, str]):\n"),
        vec!["a", "b", "c"]
    );
    assert_eq!(parse_params("@app.cell\nasync def _(x):\n"), vec!["x"]);
}

#[test]
fn splitting_respects_brackets() {
    assert_eq!(
        split_top_level("a, f(b, c), [d, e]"),
        vec!["a", " f(b, c)", " [d, e]"]
    );
}
