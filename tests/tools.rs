//! End-to-end tests that drive the real `tpmcp` binary over the MCP stdio
//! protocol, exactly as a host (opencode, Claude, VS Code) would.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::{server::Server, temp_dir};
use serde_json::{Value, json};

/// Pull the revision out of a markdown tool response.
fn revision_of(text: &str) -> String {
    for line in text.lines() {
        if let Some(rev) = line.strip_prefix("revision: ") {
            return rev.trim().to_string();
        }
    }
    panic!("no revision in output:\n{text}");
}

const DEFAULT_TOOLS: &[&str] = &[
    "cell_clear_outputs",
    "cell_delete",
    "cell_edit",
    "cell_insert",
    "cell_move",
    "cell_set_source",
    "cell_set_type",
    "notebook_check",
    "notebook_create",
    "notebook_read",
    "ruff_check",
    "ruff_format",
];

#[test]
fn default_tool_surface() {
    let mut server = Server::start(&[]);
    assert_eq!(server.list_tools(), DEFAULT_TOOLS);
}

#[test]
fn file_tools_flag_adds_py_tools() {
    let mut server = Server::start(&["--file-tools"]);
    let tools = server.list_tools();
    assert_eq!(tools.len(), DEFAULT_TOOLS.len() + 3);
    for name in ["py_read", "py_write", "py_edit"] {
        assert!(tools.contains(&name.to_string()), "missing {name}");
    }
}

/// Full notebook lifecycle over the wire, with revision chaining, byte-exact
/// output validation, and conflict rejection.
#[test]
fn notebook_lifecycle() {
    let dir = temp_dir("lifecycle");
    let path = dir.join("nb.ipynb");
    let mut server = Server::start(&[]);

    // create
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": path, "cells": [
            {"cell_type": "markdown", "source": "# Title\n"},
            {"cell_type": "code", "source": "import os\nprint('a')\n"}
        ]}),
    );
    assert!(!err, "{text}");

    // read -> revision; verify structure
    let (err, text, sc) = server.call("notebook_read", json!({"path": path, "format": "json"}));
    assert!(!err, "{text}");
    assert_eq!(sc["cell_count"], 2);
    let mut rev = sc["revision"].as_str().unwrap().to_string();

    // surgical edit of cell 1 (markdown preview carries the diff)
    let (err, text, _) = server.call(
        "cell_edit",
        json!({"path": path, "cell": 1, "old_string": "print('a')",
               "new_string": "print('b')", "revision": rev}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("print('b')"), "{text}");
    rev = revision_of(&text);

    // line-range set on cell 1 (replace line 1)
    let (err, text, _) = server.call(
        "cell_set_source",
        json!({"path": path, "cell": 1, "source": "import sys",
               "start_line": 1, "end_line": 1, "revision": rev}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("import sys"), "{text}");
    rev = revision_of(&text);

    // insert, move, set_type, clear_outputs, delete
    let (_, text, _) = server.call(
        "cell_insert",
        json!({"path": path, "cell_type": "code", "source": "y = 2\n", "revision": rev}),
    );
    rev = revision_of(&text);
    let (_, text, _) = server.call(
        "cell_move",
        json!({"path": path, "from": 2, "to": 0, "revision": rev}),
    );
    rev = revision_of(&text);
    let (_, text, _) = server.call(
        "cell_set_type",
        json!({"path": path, "cell": 1, "cell_type": "raw", "revision": rev}),
    );
    rev = revision_of(&text);
    let (_, text, _) = server.call("cell_clear_outputs", json!({"path": path, "revision": rev}));
    rev = revision_of(&text);
    let (_, text, _) = server.call(
        "cell_delete",
        json!({"path": path, "cell": 1, "revision": rev}),
    );
    let _ = revision_of(&text);

    // conflict: stale revision is rejected with a helpful error
    let (err, text, sc) = server.call(
        "cell_edit",
        json!({"path": path, "cell": 0, "old_string": "y", "new_string": "z",
               "revision": "0000000000000000"}),
    );
    assert!(err, "stale revision must be rejected");
    assert!(text.contains("revision conflict"), "{text}");
    assert_eq!(sc["error"], "conflict");
    assert!(sc["current_revision"].as_str().is_some());

    // on-disk file is valid, canonical JSON
    let raw = std::fs::read_to_string(&path).unwrap();
    let parsed: Value = serde_json::from_str(&raw).unwrap();
    assert!(raw.ends_with('\n') && !raw.ends_with("\n\n"));
    assert_eq!(parsed["nbformat"], 4);
    let notebook = tpmcp::notebook::JupyterNotebook::parse(&raw).unwrap();
    assert_eq!(
        notebook.to_string_canonical(),
        raw,
        "file stays byte-exact canonical"
    );
}

#[test]
fn ruff_tools_over_the_wire() {
    let mut server = Server::start(&[]);
    let (err, text, sc) = server.call(
        "ruff_check",
        json!({"path": "tests/data/lint_sample.py", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["diagnostic_count"], 3);
    let codes: Vec<&str> = sc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|diagnostic| diagnostic["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, vec!["F401", "F401", "F821"]);

    // notebook lint reports 0-based cells
    let (_, _, sc) = server.call(
        "ruff_check",
        json!({"path": "tests/data/lint_sample.ipynb", "format": "json"}),
    );
    let cells: Vec<u64> = sc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|diagnostic| diagnostic["cell"].as_u64().unwrap())
        .collect();
    assert_eq!(cells, vec![1, 2], "0-based cell indexes");
}

#[test]
fn ruff_revision_chains_into_fix_and_format() {
    let dir = temp_dir("ruffchain");
    let path = dir.join("sample.py");
    std::fs::copy("tests/data/lint_sample.py", &path).unwrap();

    let mut server = Server::start(&[]);
    // A plain check reports the current revision in both formats...
    let (err, text, _) = server.call("ruff_check", json!({"path": path}));
    assert!(!err, "{text}");
    let revision = revision_of(&text);
    assert_eq!(revision.len(), 16, "{text}");
    let (_, _, sc) = server.call("ruff_check", json!({"path": path, "format": "json"}));
    assert_eq!(sc["revision"].as_str().unwrap(), revision);

    // ...so a fix can chain straight onto it without another read.
    let (err, text, _) = server.call(
        "ruff_check",
        json!({"path": path, "fix": true, "revision": revision}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("applied automatic fixes"), "{text}");
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(!fixed.contains("import os"), "{fixed}");
    assert!(
        fixed.contains("return undefined_var"),
        "F821 stays: {fixed}"
    );

    // A format dry run reports the revision for the in-place call.
    let (_, text, _) = server.call("ruff_format", json!({"path": path, "check": true}));
    assert!(text.contains("revision: "), "{text}");
    let revision = revision_of(&text);
    let (err, text, _) = server.call("ruff_format", json!({"path": path, "revision": revision}));
    assert!(!err, "{text}");
    assert!(
        text.contains("formatted") || text.contains("already formatted"),
        "{text}"
    );
}

#[test]
fn py_file_tools_over_the_wire() {
    let dir = temp_dir("pytools");
    let path = dir.join("a.py");
    let mut server = Server::start(&["--file-tools"]);

    let (err, text, _) = server.call(
        "py_write",
        json!({"path": path, "content": "import os\nx = 1\n"}),
    );
    assert!(!err, "{text}");
    let rev = revision_of(&text);

    let (err, text, _) = server.call("py_read", json!({"path": path}));
    assert!(!err, "{text}");
    assert!(text.contains("import os") && text.contains(&format!("revision: {rev}")));

    let (err, text, _) = server.call(
        "py_edit",
        json!({"path": path, "old_string": "x = 1", "new_string": "x = 2", "revision": rev}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("+x = 2"));
    let rev2 = revision_of(&text);
    assert_ne!(rev, rev2);

    // stale revision rejected
    let (err, text, _) = server.call(
        "py_edit",
        json!({"path": path, "old_string": "x = 2", "new_string": "x = 3", "revision": rev}),
    );
    assert!(err && text.contains("revision conflict"), "{text}");
}

/// marimo notebooks work through the same tools: create by extension, read,
/// and surgically edit with byte-exact preservation of the rest.
#[test]
fn marimo_tools_over_the_wire() {
    let dir = temp_dir("marimo");
    let path = dir.join("app.py");
    let mut server = Server::start(&[]);

    // create a marimo notebook (extension .py -> marimo)
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": path, "cells": [{"cell_type": "code", "source": "x = 1\n"}]}),
    );
    assert!(!err, "{text}");
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        raw.contains("import marimo") && raw.contains("@app.cell"),
        "{raw}"
    );
    assert!(raw.contains("app.run()"), "{raw}");

    // read it back
    let (err, text, sc) = server.call("notebook_read", json!({"path": path, "format": "json"}));
    assert!(!err, "{text}");
    assert!(sc["format"].as_str().unwrap().starts_with("marimo"), "{sc}");
    assert_eq!(sc["cell_count"], 1);
    let rev = sc["revision"].as_str().unwrap().to_string();

    // surgical edit preserves the marimo scaffolding
    let (err, text, _) = server.call(
        "cell_edit",
        json!({"path": path, "cell": 0, "old_string": "x = 1", "new_string": "x = 2", "revision": rev}),
    );
    assert!(!err, "{text}");
    let edited = std::fs::read_to_string(&path).unwrap();
    assert!(edited.contains("x = 2"), "{edited}");
    assert!(edited.contains("import marimo") && edited.contains("app.run()"));

    // an existing marimo fixture reads with the marimo label; markdown cells
    // show their extracted text
    let (err, text, _) = server.call(
        "notebook_read",
        json!({"path": "tests/data/marimo/emoji.py"}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("marimo"), "{text}");
    assert!(text.contains("markdown"), "{text}");
    assert!(text.contains("Use colon syntax"), "{text}");
}

/// notebook_check over the wire: JSON and markdown rendering, revision
/// chaining, marimo graph findings, and error propagation.
#[test]
fn notebook_check_over_the_wire() {
    let mut server = Server::start(&[]);

    // Jupyter schema issues (8 errors + 1 warning), with a revision that
    // matches notebook_read for chaining.
    let (err, _, read_sc) = server.call(
        "notebook_read",
        json!({"path": "tests/data/schema_issues.ipynb", "format": "json"}),
    );
    assert!(!err);
    let (err, text, sc) = server.call(
        "notebook_check",
        json!({"path": "tests/data/schema_issues.ipynb", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["revision"], read_sc["revision"]);
    assert_eq!(sc["error_count"].as_u64(), Some(8), "{sc}");
    assert_eq!(sc["warning_count"].as_u64(), Some(1), "{sc}");
    assert_eq!(sc["info_count"].as_u64(), Some(0), "{sc}");
    let findings = sc["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 9, "{sc}");
    assert_eq!(findings[0]["code"], "cell-id");
    assert_eq!(findings[0]["cell"], 1);
    assert_eq!(findings[7]["code"], "conflict-marker");
    assert_eq!(findings[7]["severity"], "warning");

    // Markdown rendering
    let (err, text, _) = server.call(
        "notebook_check",
        json!({"path": "tests/data/schema_issues.ipynb"}),
    );
    assert!(!err);
    assert!(
        text.contains("9 finding(s): 8 error(s), 1 warning(s), 0 info"),
        "{text}"
    );
    assert!(text.contains("- error cell 1 `cell-id`:"), "{text}");
    assert!(
        text.contains("- warning cell 4 `conflict-marker`:"),
        "{text}"
    );

    // Clean notebook
    let (err, _, sc) = server.call(
        "notebook_check",
        json!({"path": "tests/data/vscode_sample.ipynb", "format": "json"}),
    );
    assert!(!err);
    assert_eq!(sc["findings"].as_array().unwrap().len(), 0, "{sc}");

    // marimo graph findings flow through the same tool
    let (err, _, sc) = server.call(
        "notebook_check",
        json!({"path": "tests/data/marimo/graph_cycle.py", "format": "json"}),
    );
    assert!(!err);
    assert_eq!(sc["error_count"].as_u64(), Some(1), "{sc}");
    assert_eq!(sc["findings"][0]["code"], "cycle");

    // A missing file is a tool error
    let (err, text, _) = server.call("notebook_check", json!({"path": "nope_missing.ipynb"}));
    assert!(err, "{text}");
}
