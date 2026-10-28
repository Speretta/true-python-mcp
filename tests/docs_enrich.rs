//! Docstring enrichment and doctests: the real CPython source fallback, the
//! missing-docstring note, and doctest extraction. Sibling of tests/docs.rs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use std::path::Path;

use common::ground_truth_python;
use common::server::Server;
use common::temp_dir;
use serde_json::json;
use tpmcp::docs::{resolve_symbol, resolve_symbol_with_python};

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
    assert_eq!(symbol.docstring_source, "source", "note: {:?}", symbol.note);
    assert!(
        symbol
            .docstring
            .as_deref()
            .is_some_and(|docstring| docstring.contains("geohash")),
        "expected the real-source docstring: {:?} (note: {:?})",
        symbol.docstring,
        symbol.note
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
    assert_eq!(
        note, "no docstring: the definition has none",
        "a plain .py definition is neither a C extension nor a bare stub"
    );
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

/// `py_docs` and `py_module_members` disclose the environment they resolved
/// against — `python_version`, `version_source`, `environment` — in both the
/// markdown and JSON renderings, so the LLM/user never gets a silent static
/// version assumption.
#[test]
fn docs_disclose_python_target() {
    let mut server = Server::start(&[]);

    let (err, text, sc) = server.call("py_docs", json!({"symbol": "json.dumps", "format": "json"}));
    assert!(!err, "{text}");
    assert!(
        sc["python_version"]
            .as_str()
            .is_some_and(|version| version.split('.').count() == 2),
        "python_version disclosed as major.minor: {sc}"
    );
    assert!(
        !sc["version_source"].as_str().unwrap_or("").is_empty(),
        "version_source disclosed: {sc}"
    );
    assert!(
        sc["environment"].is_string() || sc["environment"].is_null(),
        "environment is a path or null: {sc}"
    );

    let (err, text, _) = server.call("py_docs", json!({"symbol": "json.dumps"}));
    assert!(!err, "{text}");
    for label in ["python_version:", "version_source:", "environment:"] {
        assert!(
            text.contains(label),
            "py_docs markdown missing {label}: {text}"
        );
    }

    let (err, text, sc) = server.call(
        "py_module_members",
        json!({"module": "json", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert!(
        sc["python_version"]
            .as_str()
            .is_some_and(|version| version.split('.').count() == 2),
        "python_version disclosed as major.minor: {sc}"
    );
    assert!(
        !sc["version_source"].as_str().unwrap_or("").is_empty(),
        "version_source disclosed: {sc}"
    );
    assert!(
        sc["environment"].is_string() || sc["environment"].is_null(),
        "environment is a path or null: {sc}"
    );

    let (err, text, _) = server.call("py_module_members", json!({"module": "json"}));
    assert!(!err, "{text}");
    for label in ["python_version:", "version_source:", "environment:"] {
        assert!(
            text.contains(label),
            "py_module_members markdown missing {label}: {text}"
        );
    }
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
