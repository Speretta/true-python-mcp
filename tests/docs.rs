//! `py_docs` symbol resolver: prove the ty-backed resolution pipeline works
//! on the pinned crates against the vendored typeshed (no interpreter needed)
//! and that the `py_docs` tool exposes it over the wire.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::server::Server;
use common::temp_dir;
use serde_json::json;
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

/// `json.dumps` over the wire: markdown and JSON both carry the kind, a
/// signature, the structured parameters and the docstring.
#[test]
fn py_docs_json_dumps_over_the_wire() {
    let mut server = Server::start(&[]);

    let (err, text, _) = server.call("py_docs", json!({"symbol": "json.dumps"}));
    assert!(!err, "{text}");
    assert!(text.contains("py_docs: json.dumps"), "{text}");
    assert!(text.contains("kind: function"), "{text}");
    assert!(text.contains("signature: def dumps"), "{text}");
    assert!(text.contains("return_type: str"), "{text}");
    assert!(text.contains("parameters:"), "{text}");
    assert!(text.contains("skipkeys: bool"), "{text}");
    assert!(text.contains("Serialize"), "{text}");

    let (err, text, sc) = server.call("py_docs", json!({"symbol": "json.dumps", "format": "json"}));
    assert!(!err, "{text}");
    assert_eq!(sc["symbol"], "json.dumps");
    assert_eq!(sc["resolved_name"], "dumps");
    assert_eq!(sc["kind"], "function");
    assert_eq!(sc["disambiguation"], "unique");
    assert_eq!(sc["return_type"], "str");
    assert_eq!(sc["docstring_source"], "stub");
    assert!(sc["is_stub"].as_bool().unwrap());
    assert!(
        sc["docstring"].as_str().unwrap().contains("Serialize"),
        "{}",
        sc["docstring"]
    );
    let signatures = sc["signatures"].as_array().unwrap();
    assert_eq!(signatures.len(), 1);
    assert!(signatures[0].as_str().unwrap().contains("dumps"));
    let params = sc["parameters"].as_array().unwrap();
    assert!(!params.is_empty(), "{sc}");
    assert_eq!(params[0]["name"], "obj");
    assert_eq!(params[0]["type"], "Any");
    assert_eq!(params[0]["kind"], "positional_or_keyword");
    let resolution = sc["resolution"].as_array().unwrap();
    assert_eq!(resolution.len(), 2);
    assert_eq!(resolution[0], "json -> module json");
    assert_eq!(resolution[1], "dumps -> function json.dumps");
}

/// An unknown symbol is an honest tool error (isError), not a panic.
#[test]
fn py_docs_unknown_symbol_is_an_error_over_the_wire() {
    let mut server = Server::start(&[]);
    let (err, text, sc) = server.call(
        "py_docs",
        json!({"symbol": "definitely_not_a_real_module.thing"}),
    );
    assert!(err, "unknown symbol must be an error, got: {text}");
    assert!(text.contains("did not resolve"), "{text}");
    assert_eq!(sc["error"], "analysis_error");
}
