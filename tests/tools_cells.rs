//! End-to-end tests that drive the real `tpmcp` binary over the MCP stdio
//! protocol: cell metadata replacement. Sibling of tests/tools.rs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::{server::Server, temp_dir};
use serde_json::{Value, json};
use tpmcp::notebook::JupyterNotebook;

/// Pull the revision out of a markdown tool response.
fn revision_of(text: &str) -> String {
    for line in text.lines() {
        if let Some(revision) = line.strip_prefix("revision: ") {
            return revision.trim().to_string();
        }
    }
    panic!("no revision in output:\n{text}");
}

/// cell_set_metadata over the wire: full replace with the previous object
/// echoed back (markdown and JSON shapes), revision chaining and conflict
/// refusal, restore-from-echo, out-of-range index, and the marimo refusal.
#[test]
fn cell_set_metadata_over_the_wire() {
    let dir = temp_dir("setmeta");
    let path = dir.join("nb.ipynb");
    let mut server = Server::start(&[]);
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": path, "cells": [{"cell_type": "code", "source": "x = 1\n"}]}),
    );
    assert!(!err, "{text}");
    let mut revision = revision_of(&text);

    // Full replace of the empty metadata; the JSON shape echoes it back.
    let tags = json!({"tags": ["parameters"], "jupyter": {"source_hidden": true}});
    let (err, text, sc) = server.call(
        "cell_set_metadata",
        json!({"path": path, "cell": 0, "metadata": tags,
               "revision": revision, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["ok"], true);
    assert_eq!(sc["cell"], 0);
    assert_eq!(sc["previous_metadata"], json!({}));
    revision = sc["revision"].as_str().unwrap().to_string();

    // The markdown shape echoes the previous (now non-empty) metadata as a
    // JSON block, and clears it back to {} — a restore fed by the echo.
    let (err, text, _) = server.call(
        "cell_set_metadata",
        json!({"path": path, "cell": 0, "metadata": {}, "revision": revision}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("previous metadata:"), "{text}");
    assert!(text.contains("\"tags\""), "{text}");
    assert!(text.contains("\"parameters\""), "{text}");
    revision = revision_of(&text);

    // On disk: the metadata is replaced, every other field is untouched,
    // and the file stays byte-exact canonical.
    let raw = std::fs::read_to_string(&path).unwrap();
    let parsed: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["cells"][0]["metadata"], json!({}));
    assert_eq!(parsed["cells"][0]["source"], json!(["x = 1\n"]));
    assert_eq!(parsed["cells"][0]["outputs"], json!([]));
    assert_eq!(parsed["cells"][0]["execution_count"], Value::Null);
    assert!(parsed["cells"][0]["id"].as_str().is_some(), "{raw}");
    let notebook = JupyterNotebook::parse(&raw).unwrap();
    assert_eq!(
        notebook.to_string_canonical(),
        raw,
        "file stays byte-exact canonical"
    );

    // A stale revision is refused with the structured conflict error.
    let (err, text, sc) = server.call(
        "cell_set_metadata",
        json!({"path": path, "cell": 0, "metadata": tags, "revision": "0000000000000000"}),
    );
    assert!(err, "stale revision must be rejected");
    assert!(text.contains("revision conflict"), "{text}");
    assert_eq!(sc["error"], "conflict");
    assert!(sc["current_revision"].as_str().is_some());

    // An out-of-range index is a tool error, not a write.
    let (err, text, _) = server.call(
        "cell_set_metadata",
        json!({"path": path, "cell": 7, "metadata": {}, "revision": revision}),
    );
    assert!(err && text.contains("out of bounds"), "{text}");

    // marimo is refused with an explanation naming where its cell
    // configuration actually lives; the file is not touched.
    let (err, text, _) = server.call(
        "notebook_read",
        json!({"path": "tests/data/marimo/emoji.py"}),
    );
    assert!(!err, "{text}");
    let marimo_revision = revision_of(&text);
    let (err, text, _) = server.call(
        "cell_set_metadata",
        json!({"path": "tests/data/marimo/emoji.py", "cell": 0, "metadata": {},
               "revision": marimo_revision}),
    );
    assert!(err, "{text}");
    assert!(
        text.contains("marimo cell configuration lives in code"),
        "{text}"
    );
    assert!(text.contains("Jupyter notebooks only"), "{text}");
}

/// EC-4 end to end: the cell-level `execution_count` follows nbformat's
/// integer-or-null policy, so notebook_check no longer calls a file valid
/// that nbformat rejects (float spellings, negatives) or that the next
/// tpmcp write would turn into an invalid file (a beyond-u64 integer is
/// re-spelled as a float by the serializer).
#[test]
fn execution_count_policy_over_the_wire() {
    fn notebook_with_count(count: &str) -> String {
        format!(
            r#"{{
 "cells": [
  {{
   "cell_type": "code",
   "execution_count": {count},
   "id": "c0000001",
   "metadata": {{}},
   "outputs": [],
   "source": ["x = 1\n"]
  }}
 ],
 "metadata": {{}},
 "nbformat": 4,
 "nbformat_minor": 5
}}"#
        )
    }

    let dir = temp_dir("execcount");
    let mut server = Server::start(&[]);
    for (name, count, valid) in [
        ("null", "null", true),
        ("integer", "3", true),
        ("u64_max", "18446744073709551615", true),
        ("float", "3.0", false),
        ("negative", "-1", false),
        ("beyond_u64", "18446744073709551616", false),
        ("string", "\"3\"", false),
    ] {
        let path = dir.join(format!("{name}.ipynb"));
        std::fs::write(&path, notebook_with_count(count)).unwrap();
        let (err, text, sc) =
            server.call("notebook_check", json!({"path": path, "format": "json"}));
        assert!(!err, "{text}");
        let codes: Vec<&str> = sc["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|finding| finding["code"].as_str().unwrap())
            .collect();
        if valid {
            assert!(codes.is_empty(), "{name}: {codes:?}");
        } else {
            assert!(codes.contains(&"execution-count"), "{name}: {codes:?}");
        }
    }

    // The EC-4 scenario: a beyond-u64 count survives an edit re-spelled as
    // a float, and the check reports the file as what it now is.
    let path = dir.join("edited.ipynb");
    std::fs::write(&path, notebook_with_count("18446744073709551616")).unwrap();
    let (err, text, _) = server.call("notebook_read", json!({"path": path}));
    assert!(!err, "{text}");
    let revision = revision_of(&text);
    let (err, text, _) = server.call(
        "cell_edit",
        json!({"path": path, "cell": 0, "old_string": "x = 1",
               "new_string": "x = 2", "revision": revision}),
    );
    assert!(!err, "{text}");
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        !raw.contains("18446744073709551616"),
        "the integer literal is gone after the write: {raw}"
    );
    let (err, _, sc) = server.call("notebook_check", json!({"path": path, "format": "json"}));
    assert!(!err);
    let codes: Vec<&str> = sc["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"execution-count"), "{codes:?}");
}
