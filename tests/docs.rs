//! `py_docs` symbol resolver: prove the ty-backed resolution pipeline works
//! on the pinned crates against the vendored typeshed (no interpreter needed)
//! and that the `py_docs` tool exposes it over the wire.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::server::Server;
use common::temp_dir;
use serde_json::json;
use tpmcp::docs::{resolve_module_members, resolve_symbol};

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

/// `datetime.datetime.strptime` is a three-hop walk (module → class → method)
/// whose trace names each hop's kind, and whose terminal is a method.
#[test]
fn resolves_nested_datetime_strptime() {
    let dir = temp_dir("docs-strptime");
    let symbol = resolve_symbol("datetime.datetime.strptime", Some(&dir)).unwrap();
    assert_eq!(symbol.resolved_name, "strptime");
    assert_eq!(symbol.kind, "method");
    assert!(
        !symbol.signatures.is_empty(),
        "strptime should have a signature"
    );
    assert!(
        symbol.signatures[0].contains("date_string"),
        "signature should list date_string: {}",
        symbol.signatures[0]
    );
    assert_eq!(
        symbol.resolution,
        [
            "datetime -> module datetime",
            "datetime -> class datetime.datetime",
            "strptime -> method datetime.datetime.strptime",
        ]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `pathlib.Path.read_text` is another module → class → method chain.
#[test]
fn resolves_nested_pathlib_read_text() {
    let dir = temp_dir("docs-read-text");
    let symbol = resolve_symbol("pathlib.Path.read_text", Some(&dir)).unwrap();
    assert_eq!(symbol.kind, "method");
    assert_eq!(
        symbol.resolution,
        [
            "pathlib -> module pathlib",
            "Path -> class pathlib.Path",
            "read_text -> method pathlib.Path.read_text",
        ]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `collections.abc.Iterable` walks module → submodule → class, and the
/// submodule hop is reported as a module (not a bare import).
#[test]
fn resolves_nested_collections_abc_iterable() {
    let dir = temp_dir("docs-iterable");
    let symbol = resolve_symbol("collections.abc.Iterable", Some(&dir)).unwrap();
    assert_eq!(symbol.kind, "class");
    assert_eq!(
        symbol.resolution,
        [
            "collections -> module collections",
            "abc -> module collections.abc",
            "Iterable -> class collections.abc.Iterable",
        ]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A multi-segment dead-end reports the failing segment, not the terminal.
#[test]
fn nested_dead_end_names_failing_segment() {
    let dir = temp_dir("docs-deadend");
    let error = resolve_symbol("json.nonexistent.thing", Some(&dir)).unwrap_err();
    assert_eq!(error.kind(), "analysis_error");
    let message = error.to_string();
    assert!(message.contains("did not resolve"), "{message}");
    assert!(message.contains("nonexistent"), "{message}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `print` has two `@overload`s: one symbol, two signatures, `disambiguation:
/// overloads` — not `multiple`.
#[test]
fn resolves_builtin_print_overloads() {
    let dir = temp_dir("docs-print-overloads");
    let symbol = resolve_symbol("print", Some(&dir)).unwrap();
    assert_eq!(symbol.kind, "function");
    assert_eq!(symbol.disambiguation, "overloads");
    assert_eq!(
        symbol.signatures.len(),
        2,
        "print has two overloads: {:?}",
        symbol.signatures
    );
    assert!(
        symbol.signatures[0].contains("values"),
        "first overload should list *values: {}",
        symbol.signatures[0]
    );
    assert!(
        symbol.signatures[1].contains("flush"),
        "second overload should list flush: {}",
        symbol.signatures[1]
    );
    assert!(
        symbol.candidates.len() == 1,
        "the non-top overload should be listed as a candidate: {:?}",
        symbol.candidates
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A class terminal renders its constructor, so the LLM learns how to
/// instantiate it, while `kind` stays `class`.
#[test]
fn resolves_class_constructor() {
    let dir = temp_dir("docs-deque");
    let symbol = resolve_symbol("collections.deque", Some(&dir)).unwrap();
    assert_eq!(symbol.kind, "class");
    assert_eq!(symbol.disambiguation, "unique");
    assert_eq!(symbol.signatures.len(), 1);
    assert!(
        symbol.signatures[0].starts_with("class deque("),
        "constructor should be rendered as `class deque(...)`: {}",
        symbol.signatures[0]
    );
    assert!(
        symbol.signatures[0].contains("maxlen"),
        "constructor should list maxlen: {}",
        symbol.signatures[0]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `print` over the wire: JSON carries the two overload signatures, the
/// `overloads` marker, the candidates and the `is_reexported` flag.
#[test]
fn py_docs_print_overloads_over_the_wire() {
    let mut server = Server::start(&[]);
    let (err, text, sc) = server.call("py_docs", json!({"symbol": "print", "format": "json"}));
    assert!(!err, "{text}");
    assert_eq!(sc["kind"], "function");
    assert_eq!(sc["disambiguation"], "overloads");
    assert_eq!(sc["is_reexported"], false);
    let signatures = sc["signatures"].as_array().unwrap();
    assert_eq!(signatures.len(), 2, "{sc}");
    assert!(signatures[0].as_str().unwrap().contains("values"));
    let candidates = sc["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 1, "{sc}");
    assert_eq!(candidates[0]["kind"], "function");
}

/// `json`'s member listing names the public functions and classes, each with a
/// one-line signature, and a docstring for the documented ones.
#[test]
fn resolves_json_members() {
    let dir = temp_dir("docs-members-json");
    let module = resolve_module_members("json", Some(&dir), None).unwrap();
    assert_eq!(module.resolved_name, "json");
    assert!(!module.truncated, "json has far fewer than the cap");
    assert_eq!(module.members.len(), module.member_count);

    let names: Vec<&str> = module.members.iter().map(|m| m.name.as_str()).collect();
    for expected in ["dumps", "loads", "JSONEncoder"] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
    let dumps = module.members.iter().find(|m| m.name == "dumps").unwrap();
    assert_eq!(dumps.kind, "function");
    assert!(
        dumps
            .signature
            .as_deref()
            .is_some_and(|s| s.contains("dumps")),
        "dumps should carry a signature: {:?}",
        dumps.signature
    );
    assert!(
        dumps
            .docstring_summary
            .as_deref()
            .is_some_and(|d| d.contains("Serialize")),
        "dumps should carry a docstring summary: {:?}",
        dumps.docstring_summary
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// An unknown module is an honest error, not a panic or an empty listing.
#[test]
fn unknown_module_members_is_an_error() {
    let dir = temp_dir("docs-members-unknown");
    let error =
        resolve_module_members("definitely_not_a_real_module", Some(&dir), None).unwrap_err();
    assert_eq!(error.kind(), "analysis_error");
    assert!(error.to_string().contains("did not resolve"), "{error}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `py_module_members` over the wire: JSON carries the resolved module, the
/// member count and the members with kind + signature.
#[test]
fn py_module_members_json_over_the_wire() {
    let mut server = Server::start(&[]);

    let (err, text, sc) = server.call(
        "py_module_members",
        json!({"module": "json", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["module"], "json");
    assert_eq!(sc["resolved_name"], "json");
    assert_eq!(sc["truncated"], false);
    let member_count = sc["member_count"].as_u64().unwrap();
    assert!(member_count >= 3, "{sc}");

    let members = sc["members"].as_array().unwrap();
    let names: Vec<&str> = members
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    for expected in ["dumps", "loads", "JSONEncoder"] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
    let dumps = members.iter().find(|m| m["name"] == "dumps").unwrap();
    assert_eq!(dumps["kind"], "function");
    assert!(
        dumps["signature"].as_str().unwrap().contains("dumps"),
        "{}",
        dumps["signature"]
    );

    // An unknown module is an honest tool error (isError), not a panic.
    let (err, text, _) = server.call("py_module_members", json!({"module": "no_such_module_xyz"}));
    assert!(err, "unknown module must be an error, got: {text}");
    assert!(text.contains("did not resolve"), "{text}");
}
