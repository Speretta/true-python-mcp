//! End-to-end tests that drive the real `tpmcp` binary over the MCP stdio
//! protocol, exactly as a host (opencode, Claude, VS Code) would.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::{revision_of, server::Server, temp_dir};
use serde_json::{Value, json};

const DEFAULT_TOOLS: &[&str] = &[
    "cell_delete",
    "cell_edit",
    "cell_insert",
    "cell_move",
    "cell_set_source",
    "cell_update",
    "notebook_check",
    "notebook_create",
    "notebook_read",
    "notebook_search",
    "py_docs",
    "py_module_members",
    "ruff_check",
    "ruff_format",
    "ty_check",
];

#[test]
fn default_tool_surface() {
    let mut server = Server::start(&[]);
    assert_eq!(server.list_tools(), DEFAULT_TOOLS);
}

#[test]
fn file_tools_flag_adds_file_tools() {
    let mut server = Server::start(&["--file-tools"]);
    let tools = server.list_tools();
    assert_eq!(tools.len(), DEFAULT_TOOLS.len() + 6);
    for name in [
        "py_read",
        "py_write",
        "py_edit",
        "file_copy",
        "file_move",
        "file_delete",
    ] {
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
        "cell_update",
        json!({"path": path, "cells": [1], "cell_type": "raw", "revision": rev}),
    );
    rev = revision_of(&text);
    let (_, text, _) = server.call(
        "cell_update",
        json!({"path": path, "clear_outputs": true, "revision": rev}),
    );
    rev = revision_of(&text);
    let (_, text, _) = server.call(
        "cell_delete",
        json!({"path": path, "cells": [1], "revision": rev}),
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

/// Editing a marimo cell autonomously rewrites that cell's `return` and
/// signature to what marimo's serializer would write: dropping cell 0's only
/// reference empties its parameter list, so a check afterwards finds no stale
/// drift (the cross-cell return ripple is covered in tests/marimo.rs).
#[test]
fn marimo_edit_canonicalizes_the_edited_cell() {
    let dir = temp_dir("marimo-canon");
    let path = dir.join("app.py");
    let mut server = Server::start(&[]);

    // Cell 0 references `b`, which nothing defines, so it takes `b` as a
    // parameter.
    let (err, text, sc) = server.call(
        "notebook_create",
        json!({"path": path, "cells": [
            {"cell_type": "code", "source": "a = b + 1\n"},
        ], "format": "json"}),
    );
    assert!(!err, "{text}");
    let revision = sc["revision"].as_str().unwrap().to_string();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("def _(b):"), "{content}");

    // Editing cell 0 to drop the reference empties its signature.
    let (err, text, _) = server.call(
        "cell_set_source",
        json!({"path": path, "cell": 0, "source": "a = 1\n", "revision": revision}),
    );
    assert!(!err, "{text}");
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(!content.contains("def _(b):"), "b is gone: {content}");
    assert!(content.contains("def _():"), "{content}");

    // The rewrite left no stale drift behind.
    let (err, _, sc) = server.call("notebook_check", json!({"path": path, "format": "json"}));
    assert!(!err);
    assert_eq!(sc["findings"].as_array().unwrap().len(), 0, "{sc}");
}
