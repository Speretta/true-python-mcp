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
    "cell_set_metadata",
    "cell_set_source",
    "cell_set_type",
    "notebook_check",
    "notebook_create",
    "notebook_read",
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
fn ty_check_over_the_wire() {
    let mut server = Server::start(&[]);
    let (err, text, sc) = server.call(
        "ty_check",
        json!({"path": "tests/data/ty/bad.py", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["diagnostic_count"], 3);
    let codes: Vec<&str> = sc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|diagnostic| diagnostic["code"].as_str().unwrap())
        .collect();
    assert_eq!(
        codes,
        vec![
            "invalid-assignment",
            "invalid-return-type",
            "invalid-argument-type"
        ]
    );

    // notebook diagnostics carry a 0-based cell index
    let (_, _, sc) = server.call(
        "ty_check",
        json!({"path": "tests/data/ty/cross_cell.ipynb", "format": "json"}),
    );
    let cells: Vec<u64> = sc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|diagnostic| diagnostic["cell"].as_u64().unwrap())
        .collect();
    assert_eq!(cells, vec![2], "0-based cell indexes");
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

/// ruff_check fix=true and ruff_format on a notebook at the wire layer: a
/// plain check reports the revision and both diagnostics, the fix chains onto
/// that revision (F401 removed, F821 stays, 0-based cell retained), and a
/// format dry run reports the notebook's revision without a text diff.
#[test]
fn ruff_notebook_fix_and_format_over_the_wire() {
    let dir = temp_dir("ruff-nb");
    let path = dir.join("lint_sample.ipynb");
    std::fs::copy("tests/data/lint_sample.ipynb", &path).unwrap();
    // Mirror the fixture's governing config (select=["F"]) so the temp copy
    // lints identically to the in-tree notebook.
    std::fs::copy("tests/data/ruff.toml", dir.join("ruff.toml")).unwrap();

    let mut server = Server::start(&[]);

    // Plain check: two diagnostics (F401 + F821) and a chaining revision.
    let (err, _, sc) = server.call("ruff_check", json!({"path": path, "format": "json"}));
    assert!(!err, "{sc}");
    assert_eq!(sc["diagnostic_count"], 2);
    let revision = sc["revision"].as_str().unwrap().to_string();

    // fix=true applies the safe F401 fix; the F821 stays, still cell-mapped.
    let (err, text, sc) = server.call(
        "ruff_check",
        json!({"path": path, "fix": true, "revision": revision, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["changed"], true);
    assert_ne!(sc["revision"].as_str().unwrap(), revision);
    assert_eq!(sc["diagnostic_count"], 1);
    assert_eq!(sc["diagnostics"][0]["code"], "F821");
    assert_eq!(sc["diagnostics"][0]["cell"], 2);
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(!fixed.contains("import os"), "F401 was fixed: {fixed}");

    // ruff_format dry run on the notebook: revision present, no text diff.
    let (err, _, sc) = server.call(
        "ruff_format",
        json!({"path": path, "check": true, "format": "json"}),
    );
    assert!(!err, "{sc}");
    assert!(sc["revision"].is_string(), "{sc}");
    let status = sc["status"].as_str().unwrap();
    assert!(status == "unchanged" || status == "would_reformat", "{sc}");
    assert!(
        sc["diff"].is_null(),
        "notebooks carry no plain-text diff: {sc}"
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

/// The revision/range/replace_all branches the happy-path trio test does not
/// touch: overwrite-with-revision (and its stale conflict), JSON + line-range
/// reads, and py_edit replace_all + non-uniqueness.
#[test]
fn py_file_tools_revision_ranges_and_replace_all() {
    let dir = temp_dir("pytools-branches");
    let path = dir.join("a.py");
    let mut server = Server::start(&["--file-tools"]);

    // py_write create -> revision.
    let (err, text, _) = server.call(
        "py_write",
        json!({"path": path, "content": "x = 1\ny = 2\nx = 1\n"}),
    );
    assert!(!err, "{text}");
    let rev = revision_of(&text);

    // py_read JSON: the full-file shape (revision, line_count, range, content).
    let (err, text, sc) = server.call("py_read", json!({"path": path, "format": "json"}));
    assert!(!err, "{text}");
    assert_eq!(sc["revision"].as_str().unwrap(), rev);
    assert_eq!(sc["line_count"], 3);
    assert_eq!(sc["range"], json!([1, 3]));
    assert_eq!(sc["content"], "x = 1\ny = 2\nx = 1");

    // py_read markdown line range renders only the selected line.
    let (err, text, _) = server.call(
        "py_read",
        json!({"path": path, "line_start": 2, "line_end": 2}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("    2 | y = 2"), "{text}");
    assert!(!text.contains("1 | x = 1"), "{text}");
    assert!(!text.contains("3 | x = 1"), "{text}");

    // py_read JSON line range reflects the slice.
    let (err, _, sc) = server.call(
        "py_read",
        json!({"path": path, "line_start": 1, "line_end": 2, "format": "json"}),
    );
    assert!(!err, "{sc}");
    assert_eq!(sc["range"], json!([1, 2]));
    assert_eq!(sc["content"], "x = 1\ny = 2");

    // py_write overwrite: a revision is required and returns a fresh one.
    let (err, text, _) = server.call(
        "py_write",
        json!({"path": path, "content": "a = 1\n", "revision": rev}),
    );
    assert!(!err, "{text}");
    let rev2 = revision_of(&text);
    assert_ne!(rev, rev2);

    // py_write overwrite with a stale revision is a conflict.
    let (err, text, sc) = server.call(
        "py_write",
        json!({"path": path, "content": "clobber\n", "revision": rev}),
    );
    assert!(err, "stale revision must be rejected");
    assert!(text.contains("revision conflict"), "{text}");
    assert_eq!(sc["error"], "conflict");

    // py_edit replace_all rewrites every occurrence.
    let (err, text, _) = server.call(
        "py_write",
        json!({"path": path, "content": "x = 1\nx = 1\nx = 1\n", "revision": rev2}),
    );
    assert!(!err, "{text}");
    let rev3 = revision_of(&text);
    let (err, text, _) = server.call(
        "py_edit",
        json!({"path": path, "old_string": "x = 1", "new_string": "x = 2",
               "replace_all": true, "revision": rev3}),
    );
    assert!(!err, "{text}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "x = 2\nx = 2\nx = 2\n"
    );

    // py_edit without replace_all on a non-unique match is a caller error.
    let rev4 = revision_of(&text);
    let (err, text, _) = server.call(
        "py_edit",
        json!({"path": path, "old_string": "x = 2", "new_string": "x = 3", "revision": rev4}),
    );
    assert!(err, "non-unique match must be rejected");
    assert!(text.contains("occurs 3 times in file"), "{text}");
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

/// The wave-3 read parameters over the wire: outline, query, diagnostics.
#[test]
fn read_parameters_outline_query_diagnostics() {
    let dir = temp_dir("readparams");
    let path = dir.join("nb.ipynb");
    let mut server = Server::start(&[]);
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": path, "cells": [
            {"cell_type": "code", "source": "import pandas as pd\nx = 1\n"},
            {"cell_type": "markdown", "source": "# Notes about pandas\n"},
            {"cell_type": "code", "source": "unused_import = os\n"}
        ]}),
    );
    assert!(!err, "{text}");

    // outline: one row per cell with first line and line count, no bodies
    let (err, text, sc) = server.call(
        "notebook_read",
        json!({"path": path, "detail": "outline", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["cells"].as_array().unwrap().len(), 3);
    assert!(
        sc["cells"][0]["source"].is_null(),
        "outline carries no source"
    );
    assert_eq!(sc["cells"][0]["line_count"], 2);
    assert_eq!(sc["cells"][0]["first_line"], "import pandas as pd");
    let (err, text, _) = server.call("notebook_read", json!({"path": path, "detail": "outline"}));
    assert!(!err, "{text}");
    assert!(
        text.contains("### [0] code · 2 lines · import pandas as pd"),
        "{text}"
    );
    assert!(!text.contains("x = 1"), "outline shows no bodies: {text}");

    // query: only matching cells and lines, searched case-insensitively
    let (err, text, sc) = server.call(
        "notebook_read",
        json!({"path": path, "query": "PANDAS", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["matching_cells"], 2);
    assert_eq!(sc["cells"][0]["index"], 0);
    assert_eq!(sc["cells"][0]["matches"][0]["line"], 1);
    assert_eq!(
        sc["cells"][1]["index"], 1,
        "markdown cells are searched too"
    );
    let (err, text, _) = server.call("notebook_read", json!({"path": path, "query": "pandas"}));
    assert!(!err, "{text}");
    assert!(text.contains("matches: 2 cell(s), 2 line(s)"), "{text}");
    assert!(text.contains("   1 | import pandas as pd"), "{text}");
    let (err, text, _) = server.call(
        "notebook_read",
        json!({"path": path, "query": "zzz_missing"}),
    );
    assert!(!err && text.contains("no cells match"), "{text}");
    let (err, text, _) = server.call("notebook_read", json!({"path": path, "query": "  "}));
    assert!(err && text.contains("query must not be empty"), "{text}");

    // diagnostics=check: structural findings ride along, no lint section
    let (err, text, sc) = server.call(
        "notebook_read",
        json!({"path": path, "diagnostics": "check", "detail": "outline", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["findings"].as_array().unwrap().len(), 0, "{sc}");
    assert!(sc["lint"].is_null(), "check alone carries no lint: {sc}");

    // diagnostics=check+lint: ruff findings for the same snapshot
    let (err, text, sc) = server.call(
        "notebook_read",
        json!({"path": "tests/data/lint_sample.ipynb", "diagnostics": "check+lint", "format": "json"}),
    );
    assert!(!err, "{text}");
    let lint = sc["lint"].as_array().unwrap();
    assert_eq!(lint.len(), 2, "{sc}");
    assert_eq!(lint[0]["code"], "F401");
    assert!(sc["revision"].as_str().is_some(), "read still chains: {sc}");

    // markdown rendering of the findings section
    let (err, text, _) = server.call(
        "notebook_read",
        json!({"path": "tests/data/schema_issues.ipynb", "diagnostics": "check", "detail": "outline"}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("diagnostics: 9 finding(s)"), "{text}");
    assert!(text.contains("- error cell 1 `cell-id`:"), "{text}");
}

/// `with_outputs` renders cell outputs only when requested: `full` returns
/// the raw payload array (JSON) and the payload text (markdown), `summary`
/// collapses them to a one-line string, and `none` omits them entirely. A
/// read stays byte-safe: the on-disk notebook is never rewritten.
#[test]
fn notebook_read_with_outputs() {
    let dir = temp_dir("read-outputs");
    let path = dir.join("outputs.ipynb");
    let source = r##"{
 "cells": [
  {
   "cell_type": "code",
   "execution_count": 1,
   "id": "a1",
   "metadata": {},
   "outputs": [
    {"output_type": "stream", "name": "stdout", "text": ["hello from stdout\n"]},
    {"output_type": "execute_result", "execution_count": 1, "metadata": {},
     "data": {"text/plain": "42"}}
   ],
   "source": ["print('hello')\n"]
  }
 ],
 "metadata": {},
 "nbformat": 4,
 "nbformat_minor": 5
}"##;
    std::fs::write(&path, source).unwrap();
    let mut server = Server::start(&[]);

    // full JSON: the raw outputs array is present on the code cell.
    let (err, text, sc) = server.call(
        "notebook_read",
        json!({"path": path, "with_outputs": "full", "format": "json"}),
    );
    assert!(!err, "{text}");
    let outputs = sc["cells"][0]["outputs"].as_array().unwrap();
    assert_eq!(outputs.len(), 2, "{sc}");
    assert_eq!(outputs[0]["output_type"], "stream");
    assert_eq!(outputs[0]["name"], "stdout");
    assert_eq!(outputs[1]["output_type"], "execute_result");
    assert_eq!(outputs[1]["data"]["text/plain"], "42");

    // full markdown renders the payload text.
    let (err, text, _) = server.call(
        "notebook_read",
        json!({"path": path, "with_outputs": "full"}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("outputs(2):"), "{text}");
    assert!(text.contains("stream(stdout):"), "{text}");
    assert!(text.contains("hello from stdout"), "{text}");
    assert!(text.contains("42"), "{text}");

    // summary JSON collapses to a one-line string, not the raw array.
    let (err, text, sc) = server.call(
        "notebook_read",
        json!({"path": path, "with_outputs": "summary", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(
        sc["cells"][0]["outputs"],
        "stream(stdout), execute_result[text/plain]"
    );

    // none omits outputs entirely: no JSON "outputs" key, no markdown section.
    let (err, text, sc) = server.call(
        "notebook_read",
        json!({"path": path, "with_outputs": "none", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert!(sc["cells"][0].get("outputs").is_none(), "{sc}");
    let (err, text, _) = server.call(
        "notebook_read",
        json!({"path": path, "with_outputs": "none"}),
    );
    assert!(!err, "{text}");
    assert!(!text.contains("outputs("), "{text}");
    assert!(!text.contains("hello from stdout"), "{text}");

    // Reads never mutate: the on-disk notebook is byte-identical.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
}

/// marimo notebooks read through the same tool, and the outline labels
/// their special blocks.
#[test]
fn marimo_outline_labels_blocks() {
    let mut server = Server::start(&[]);
    let (err, text, _) = server.call(
        "notebook_read",
        json!({"path": "tests/data/marimo/graph_setup_root.py", "detail": "outline"}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("(setup)"), "{text}");
    assert!(text.contains("marimo"), "{text}");
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

    // A clean notebook is described as passing *this* check (a documented
    // subset), never overclaimed as "valid".
    let (err, text, _) = server.call(
        "notebook_check",
        json!({"path": "tests/data/vscode_sample.ipynb"}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("passes this check"), "{text}");
    assert!(!text.contains("is valid"), "{text}");

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

/// An explicit line range on an empty file is a bounds error, not a silent
/// empty result; the implicit whole-file read stays an empty view.
#[test]
fn py_read_explicit_range_on_empty_file_is_a_bounds_error() {
    let dir = temp_dir("empty-range");
    let path = dir.join("empty.py");
    std::fs::write(&path, "").unwrap();
    let mut server = Server::start(&["--file-tools"]);

    let (err, text, _) = server.call("py_read", json!({"path": path}));
    assert!(!err, "{text}");
    assert!(text.contains("lines: 0"), "{text}");

    let (err, text, _) = server.call(
        "py_read",
        json!({"path": path, "line_start": 1, "line_end": 1}),
    );
    assert!(
        err,
        "an explicit range on an empty file must not silently succeed"
    );
    assert!(text.contains("out of bounds: file has 0 lines"), "{text}");
}
