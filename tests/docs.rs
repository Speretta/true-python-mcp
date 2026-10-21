//! `py_docs` symbol resolver: prove the ty-backed resolution pipeline works
//! on the pinned crates against the vendored typeshed (no interpreter needed)
//! and that the `py_docs` tool exposes it over the wire.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use std::path::Path;

use common::ground_truth_python;
use common::server::Server;
use common::temp_dir;
use serde_json::json;
use tpmcp::docs::{resolve_module_members, resolve_symbol, resolve_symbol_with_python};

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

/// A class terminal reports its direct superclasses in `bases`.
#[test]
fn resolves_class_bases() {
    let dir = temp_dir("docs-bases");
    let symbol = resolve_symbol("pathlib.Path", Some(&dir)).unwrap();
    assert_eq!(symbol.kind, "class");
    assert!(
        symbol.bases.iter().any(|base| base == "PurePath"),
        "Path should list PurePath among its bases: {:?}",
        symbol.bases
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `bases` reaches the JSON wire for a class terminal.
#[test]
fn py_docs_bases_over_the_wire() {
    let mut server = Server::start(&[]);
    let (err, text, sc) = server.call(
        "py_docs",
        json!({"symbol": "pathlib.Path", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["kind"], "class");
    let bases = sc["bases"]
        .as_array()
        .unwrap_or_else(|| panic!("bases missing: {sc}"));
    assert!(
        bases.iter().any(|base| base.as_str() == Some("PurePath")),
        "bases should list PurePath: {bases:?}"
    );
}

/// A definition with no typeshed docstring falls back to the interpreter's real
/// CPython `Lib/*.py` source and reports `docstring_source: "source"`.
#[test]
fn enriches_docstring_from_real_source() {
    let Some(python) = ground_truth_python() else {
        return;
    };
    let dir = temp_dir("docs-enrich");
    let symbol =
        resolve_symbol_with_python("antigravity.geohash", Some(&dir), Some(Path::new(&python)))
            .unwrap();
    assert_eq!(symbol.kind, "function");
    assert_eq!(symbol.docstring_source, "source");
    assert!(
        symbol
            .docstring
            .as_deref()
            .is_some_and(|docstring| docstring.contains("geohash")),
        "expected the real-source docstring: {:?}",
        symbol.docstring
    );
    assert!(symbol.note.is_none(), "enriched symbols carry no note");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A symbol with no docstring anywhere returns an honest note, never a bare
/// `None`.
#[test]
fn missing_docstring_returns_a_note() {
    let dir = temp_dir("docs-note");
    std::fs::write(dir.join("mymod.py"), "def no_docs():\n    pass\n").unwrap();
    let symbol = resolve_symbol("mymod.no_docs", Some(&dir)).unwrap();
    assert_eq!(symbol.kind, "function");
    assert!(symbol.docstring.is_none(), "should have no docstring");
    assert_eq!(symbol.docstring_source, "none");
    let note = symbol
        .note
        .as_deref()
        .expect("a missing docstring must carry a note, not a silent None");
    assert!(note.contains("no docstring"), "{note}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The note reaches the JSON wire when a docstring is missing.
#[test]
fn missing_docstring_note_over_the_wire() {
    let mut server = Server::start(&[]);
    let dir = temp_dir("docs-note-wire");
    std::fs::write(dir.join("mymod.py"), "def no_docs():\n    pass\n").unwrap();
    let (err, text, sc) = server.call(
        "py_docs",
        json!({
            "symbol": "mymod.no_docs",
            "path": dir.to_str().unwrap(),
            "format": "json"
        }),
    );
    assert!(!err, "{text}");
    assert!(sc["docstring"].is_null(), "no docstring: {sc}");
    assert_eq!(sc["docstring_source"], "none");
    let note = sc["note"]
        .as_str()
        .unwrap_or_else(|| panic!("a missing docstring must carry a note: {sc}"));
    assert!(note.contains("no docstring"), "{note}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A first-party function whose docstring embeds doctest examples surfaces them
/// in `doctest_examples`, split per `>>>` prompt with continuations and
/// expected output grouped and leading indentation normalized away.
#[test]
fn resolves_function_doctest() {
    let dir = temp_dir("docs-doctest");
    std::fs::write(
        dir.join("mymod.py"),
        "def add(a, b):\n    \"\"\"Add two numbers.\n\n    >>> add(1, 2)\n    3\n    \"\"\"\n    return a + b\n",
    )
    .unwrap();
    let symbol = resolve_symbol("mymod.add", Some(&dir)).unwrap();
    assert_eq!(symbol.kind, "function");
    assert_eq!(symbol.doctest_examples, vec![">>> add(1, 2)\n3"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A docstring with no doctest produces an empty `doctest_examples` — an honest
/// empty list, not an error and not a note.
#[test]
fn docstring_without_doctest_is_empty_list() {
    let dir = temp_dir("docs-no-doctest");
    std::fs::write(
        dir.join("mymod.py"),
        "def no_examples():\n    \"\"\"Just prose.\"\"\"\n    return 1\n",
    )
    .unwrap();
    let symbol = resolve_symbol("mymod.no_examples", Some(&dir)).unwrap();
    assert!(symbol.doctest_examples.is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The doctest examples reach the wire in both the markdown (`doctests:` section)
/// and JSON (`doctest_examples`) renderings.
#[test]
fn py_docs_doctest_over_the_wire() {
    let mut server = Server::start(&[]);
    let dir = temp_dir("docs-doctest-wire");
    std::fs::write(
        dir.join("mymod.py"),
        "def add(a, b):\n    \"\"\"Add two numbers.\n\n    >>> add(1, 2)\n    3\n    \"\"\"\n    return a + b\n",
    )
    .unwrap();

    let (err, text, sc) = server.call(
        "py_docs",
        json!({"symbol": "mymod.add", "path": dir.to_str().unwrap(), "format": "json"}),
    );
    assert!(!err, "{text}");
    let doctests = sc["doctest_examples"]
        .as_array()
        .unwrap_or_else(|| panic!("doctest_examples missing: {sc}"));
    assert_eq!(doctests.len(), 1, "{sc}");
    assert_eq!(doctests[0], ">>> add(1, 2)\n3");

    let (err, text, _) = server.call(
        "py_docs",
        json!({"symbol": "mymod.add", "path": dir.to_str().unwrap()}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("doctests:"), "{text}");
    assert!(text.contains(">>> add(1, 2)"), "{text}");
    std::fs::remove_dir_all(&dir).unwrap();
}
