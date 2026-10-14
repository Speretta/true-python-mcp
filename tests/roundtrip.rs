//! Byte-exactness guarantees for the notebook serializer.
//!
//! The corpus in `tests/data/` was produced by real writers:
//! - `nbformat_rich.ipynb`, `nbformat_empty.ipynb` — Python nbformat 5.11
//! - `vscode_sample.ipynb` — VS Code
//! - `nbformat_minor4.ipynb`, `edge_source_forms.ipynb` — canonical layout
//!   with old-format and exotic `source` shapes
//! - `noncanonical.ipynb` — 2-space indent, unsorted keys, no trailing
//!   newline, ascii-escaped unicode

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
use tpmcp::notebook::{JupyterNotebook, Notebook, SourceText};

const CANONICAL_CORPUS: &[&str] = &[
    "tests/data/nbformat_rich.ipynb",
    "tests/data/nbformat_empty.ipynb",
    "tests/data/nbformat_minor4.ipynb",
    "tests/data/edge_source_forms.ipynb",
    "tests/data/vscode_sample.ipynb",
];

fn parse_file(path: &str) -> JupyterNotebook {
    let content =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("cannot read {path}: {error}"));
    JupyterNotebook::parse(&content).unwrap_or_else(|error| panic!("cannot parse {path}: {error}"))
}

/// The core invariant: canonical files round-trip byte-for-byte.
#[test]
fn write_read_canonical_files_is_byte_exact() {
    for path in CANONICAL_CORPUS {
        let original = std::fs::read_to_string(path).unwrap();
        let serialized = parse_file(path).to_string_canonical();
        assert_eq!(
            serialized, original,
            "byte-exact roundtrip failed for {path}"
        );
    }
}

/// Non-canonical input is normalized once, then stable forever.
#[test]
fn noncanonical_input_normalizes_then_is_stable() {
    let original = std::fs::read_to_string("tests/data/noncanonical.ipynb").unwrap();
    let notebook = JupyterNotebook::parse(&original).unwrap();

    let normalized = notebook.to_string_canonical();
    assert_ne!(normalized, original);
    assert!(normalized.ends_with("}\n") && !normalized.ends_with("\n\n"));
    assert!(normalized.contains("\n \"cells\""), "1-space indent");
    assert!(normalized.contains("ünicode"), "raw UTF-8, no \\u escapes");

    let again = JupyterNotebook::parse(&normalized)
        .unwrap()
        .to_string_canonical();
    assert_eq!(again, normalized, "normalization must be idempotent");
}

/// Editing one cell leaves every other cell byte-identical, including
/// exotic source shapes, and converts the edited cell to canonical lines.
#[test]
fn editing_one_cell_preserves_the_others() {
    let path = "tests/data/edge_source_forms.ipynb";
    let original = std::fs::read_to_string(path).unwrap();
    let mut notebook = parse_file(path);

    // Cell 0 has string-form source; editing converts it to canonical lines.
    assert!(matches!(notebook.cells[0].source, SourceText::Str(_)));
    notebook.cells[0].set_source_text("print('edited')\n");

    let serialized = notebook.to_string_canonical();
    assert!(serialized.contains("\"print('edited')\\n\""));
    // Cell 1 keeps its trailing empty line, cell 2 keeps CRLF inside lines.
    assert!(serialized.contains("\"a\\n\",\n    \"\""));
    assert!(serialized.contains("\"crlf\\r\\n\""));
    // Unknown top-level field survives.
    assert!(serialized.contains("\"custom_top_level\""));
    assert_ne!(serialized, original);
}

/// Outputs, execution_count and metadata are untouched by source edits.
#[test]
fn source_edit_preserves_outputs_and_metadata() {
    let mut notebook = parse_file("tests/data/nbformat_rich.ipynb");
    let before = notebook.cells[1].extra.clone();
    notebook.cells[1].set_source_text("x = 1\n");
    assert_eq!(notebook.cells[1].extra, before);
}

/// clear_outputs resets execution state per the nbformat spec only.
#[test]
fn clear_outputs_resets_execution_state() {
    let mut notebook = parse_file("tests/data/nbformat_rich.ipynb");
    assert!(notebook.cells[1].is_code());
    notebook.clear_outputs(Some(1)).unwrap();
    let serialized = notebook.to_string_canonical();
    assert!(serialized.contains("\"execution_count\": null"));
    assert!(serialized.contains("\"outputs\": []"));
    // Cell identity and metadata survive.
    assert!(notebook.cells[1].extra.contains_key("id"));
    assert!(notebook.cells[1].extra.contains_key("metadata"));
}

/// A fresh notebook matches what `nbformat.v4.new_notebook()` writes.
#[test]
fn new_notebook_matches_nbformat_empty() {
    let expected = std::fs::read_to_string("tests/data/nbformat_empty.ipynb").unwrap();
    assert_eq!(JupyterNotebook::new().to_string_canonical(), expected);
}

/// New cells look like `nbformat.v4.new_*_cell` output, with ids only for
/// nbformat_minor >= 5.
#[test]
fn new_cell_shape_follows_nbformat() {
    let code = JupyterNotebook::new_cell("code", "x = 1\n", true);
    assert_eq!(code.extra["execution_count"], serde_json::Value::Null);
    assert_eq!(code.extra["outputs"], serde_json::json!([]));
    assert_eq!(code.extra["id"].as_str().unwrap().len(), 8);
    assert_eq!(code.source, SourceText::Lines(vec!["x = 1\n".to_string()]));

    let markdown = JupyterNotebook::new_cell("markdown", "# hi", false);
    assert!(!markdown.extra.contains_key("outputs"));
    assert!(!markdown.extra.contains_key("execution_count"));
    assert!(!markdown.extra.contains_key("id"));
}

/// Distinct generated ids.
#[test]
fn cell_ids_are_unique() {
    let a = JupyterNotebook::new_cell("code", "", true);
    let b = JupyterNotebook::new_cell("code", "", true);
    assert_ne!(a.extra["id"], b.extra["id"]);
}

#[test]
fn parse_rejects_malformed_notebooks() {
    for (content, expected) in [
        ("not json", "invalid JSON"),
        ("[]", "must be a JSON object"),
        (
            r#"{"nbformat": 3, "nbformat_minor": 0, "cells": []}"#,
            "unsupported nbformat version 3",
        ),
        (
            r#"{"nbformat_minor": 5, "cells": []}"#,
            "missing integer field \"nbformat\"",
        ),
        (
            r#"{"nbformat": 4, "cells": []}"#,
            "missing integer field \"nbformat_minor\"",
        ),
        (
            r#"{"nbformat": 4, "nbformat_minor": 5}"#,
            "missing \"cells\" array",
        ),
        (
            r#"{"nbformat": 4, "nbformat_minor": 5, "cells": [{"cell_type": "code"}]}"#,
            "cell 0: missing \"source\"",
        ),
        (
            r#"{"nbformat": 4, "nbformat_minor": 5, "cells": [{"source": []}]}"#,
            "cell 0: missing string field \"cell_type\"",
        ),
        (
            r#"{"nbformat": 4, "nbformat_minor": 5, "cells": [{"cell_type": "code", "source": [1]}]}"#,
            "cell 0: \"source\" array must contain only strings",
        ),
    ] {
        let error = JupyterNotebook::parse(content).unwrap_err();
        assert!(
            error.contains(expected),
            "expected {expected:?} in {error:?} for input {content}"
        );
    }
}
