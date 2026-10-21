//! `docs` symbol resolver (Phase 0 spike): prove that the ty-backed
//! resolution pipeline works on the pinned crates against the vendored
//! typeshed — no interpreter needed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::temp_dir;
use tpmcp::docs::resolve_symbol;

/// `json.dumps` resolves to a function whose rendered signature names `dumps`
/// and its parameters, and whose docstring typeshed keeps.
#[test]
fn resolves_json_dumps() {
    let dir = temp_dir("docs-json");
    let symbol = resolve_symbol("json.dumps", Some(&dir)).unwrap();
    assert_eq!(symbol.resolved_name, "dumps");
    assert_eq!(symbol.kind, "function");
    assert!(
        !symbol.signatures.is_empty(),
        "dumps should have a signature"
    );
    let signature = &symbol.signatures[0];
    assert!(
        signature.contains("dumps"),
        "signature should name dumps: {signature}"
    );
    assert!(
        signature.contains("obj"),
        "signature should list obj: {signature}"
    );
    let docstring = symbol
        .docstring
        .as_deref()
        .expect("typeshed keeps dumps' docstring");
    assert!(
        docstring.contains("Serialize"),
        "docstring should describe serialization: {docstring}"
    );
    assert!(symbol.is_stub, "dumps resolves in a typeshed stub");
    assert_eq!(symbol.docstring_source, "stub");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `typing.Any` resolves to a class; its docstring may be present in typeshed
/// but an absent one must be reported as `None`, never as an error.
#[test]
fn resolves_typing_any() {
    let dir = temp_dir("docs-any");
    let symbol = resolve_symbol("typing.Any", Some(&dir)).unwrap();
    assert_eq!(symbol.resolved_name, "Any");
    assert_eq!(symbol.kind, "class");
    assert!(matches!(symbol.docstring, Some(_) | None));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `print` resolves through the builtins fallback with a rendered signature.
#[test]
fn resolves_builtin_print() {
    let dir = temp_dir("docs-print");
    let symbol = resolve_symbol("print", Some(&dir)).unwrap();
    assert_eq!(symbol.resolved_name, "print");
    assert_eq!(symbol.kind, "function");
    assert!(
        !symbol.signatures.is_empty(),
        "print should have a signature"
    );
    assert!(
        symbol.signatures[0].contains("values"),
        "print's signature should list *values: {}",
        symbol.signatures[0]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A symbol with no definition is an honest error, not a panic.
#[test]
fn unknown_symbol_is_an_error() {
    let dir = temp_dir("docs-unknown");
    let error = resolve_symbol("definitely_not_a_real_module.thing", Some(&dir)).unwrap_err();
    assert_eq!(error.kind(), "analysis_error");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `None` src_root anchors at the current directory and still resolves stdlib.
#[test]
fn resolves_without_explicit_src_root() {
    let symbol = resolve_symbol("json.dumps", None).unwrap();
    assert_eq!(symbol.kind, "function");
    assert!(!symbol.signatures.is_empty());
}
