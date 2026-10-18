//! End-to-end tests that drive the real `tpmcp` binary over the MCP stdio
//! protocol, exactly as a host (opencode, Claude, VS Code) would.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::{ground_truth_python, run_python, server::Server, temp_dir};
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

/// Ground truth: real marimo parses the converted file and compiles every
/// cell (what marimo does on load).
const CONVERT_CHECK: &str = r#"
import sys
from marimo._ast.parse import parse_notebook
from marimo._ast.compiler import compile_cell
path = sys.argv[1]
nb = parse_notebook(open(path).read(), path)
for i, cell in enumerate(nb.cells):
    if type(cell).__name__ != "UnparsableCell":
        compile_cell(cell.code, str(i))
print("cells:", len(nb.cells))
"#;

/// notebook_create from_path: .ipynb → marimo with honest warnings (magics
/// kept verbatim, raw cells and dropped outputs reported), .md export with
/// text outputs rendered and binaries warned, marimo → .ipynb, and the
/// mutual-exclusion rules.
#[test]
fn convert_with_from_path() {
    let dir = temp_dir("convert");
    let source = dir.join("source.ipynb");
    let mut server = Server::start(&[]);
    std::fs::write(
        &source,
        r##"{
 "cells": [
  {
   "cell_type": "code",
   "execution_count": 1,
   "id": "a1",
   "metadata": {},
   "outputs": [
    {
     "output_type": "display_data",
     "metadata": {},
     "data": {"text/plain": "42", "image/png": "aGVsbG8="}
    }
   ],
   "source": ["%matplotlib inline\n", "import math\n"]
  },
  {"cell_type": "raw", "id": "a2", "metadata": {}, "source": ["notes\n"]},
  {"cell_type": "markdown", "id": "a3", "metadata": {}, "source": ["# Title\n"]}
 ],
 "metadata": {},
 "nbformat": 4,
 "nbformat_minor": 5
}"##,
    )
    .unwrap();

    // → marimo: warnings for the magic, the raw cell and the dropped
    // outputs; the magic source stays verbatim so nothing is lost.
    let target = dir.join("app.py");
    let (err, text, sc) = server.call(
        "notebook_create",
        json!({"path": target, "from_path": source, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["cell_count"], 3);
    let warnings: Vec<&str> = sc["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|warning| warning.as_str().unwrap())
        .collect();
    assert!(
        warnings.iter().any(|line| line.contains("%matplotlib")),
        "{warnings:?}"
    );
    assert!(
        warnings.iter().any(|line| line.contains("raw cell")),
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|line| line.contains("not stored in marimo")),
        "{warnings:?}"
    );
    let converted = std::fs::read_to_string(&target).unwrap();
    assert!(converted.contains("%matplotlib inline"), "{converted}");
    assert!(converted.contains("import marimo"), "{converted}");

    // The magic file is not valid marimo Python; our own reader degrades
    // gracefully and marks the region broken instead of failing.
    let (err, text, _) = server.call("notebook_read", json!({"path": target}));
    assert!(!err, "{text}");
    assert!(text.contains("broken"), "{text}");

    // A clean .ipynb converts to a file real marimo loads and compiles.
    let clean = dir.join("clean.py");
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": clean, "from_path": "tests/data/vscode_sample.ipynb"}),
    );
    assert!(!err, "{text}");
    if let Some(py) = ground_truth_python() {
        let report = run_python(&py, CONVERT_CHECK, &[clean.to_str().unwrap()]);
        assert!(report.contains("cells:"), "{report}");
    }

    // → markdown export: text outputs rendered, binaries dropped + warned.
    let export = dir.join("out.md");
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": export, "from_path": source}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("warnings:"), "{text}");
    assert!(text.contains("image/png"), "{text}");
    let exported = std::fs::read_to_string(&export).unwrap();
    assert!(exported.contains("# Title"), "{exported}");
    assert!(exported.contains("```python"), "{exported}");
    assert!(exported.contains("42"), "text output rendered: {exported}");
    assert!(!exported.contains("aGVsbG8="), "binary dropped: {exported}");

    // marimo → Jupyter
    let back = dir.join("back.ipynb");
    let (err, text, sc) = server.call(
        "notebook_create",
        json!({"path": back, "from_path": "tests/data/marimo/emoji.py", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert!(sc["cell_count"].as_u64().unwrap() > 0, "{sc}");
    let raw = std::fs::read_to_string(&back).unwrap();
    let parsed: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["nbformat"], 4);

    // Mutual exclusion and the .md rules.
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": dir.join("x.py"), "from_path": source, "cells": []}),
    );
    assert!(err && text.contains("mutually exclusive"), "{text}");
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": dir.join("y.md"), "from_path": source, "notebook_format": "jupyter"}),
    );
    assert!(err && text.contains(".md export target"), "{text}");
    let (err, text, _) = server.call("notebook_create", json!({"path": dir.join("z.md")}));
    assert!(err && text.contains("requires from_path"), "{text}");
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
