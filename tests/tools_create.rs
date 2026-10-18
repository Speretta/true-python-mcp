//! End-to-end tests that drive the real `tpmcp` binary over the MCP stdio
//! protocol: the notebook_create conversion path (`from_path`). Sibling of
//! tests/tools.rs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::{ground_truth_python, run_python, server::Server, temp_dir};
use serde_json::{Value, json};

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
