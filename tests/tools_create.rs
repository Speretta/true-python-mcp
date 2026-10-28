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

/// `notebook_format` overrides the format the extension would select: a `.py`
/// path created with `notebook_format: "jupyter"` is written as nbformat JSON,
/// and a `.ipynb` path with `notebook_format: "marimo"` is written as a
/// marimo file.
#[test]
fn notebook_format_overrides_extension() {
    let dir = temp_dir("format-override");
    let mut server = Server::start(&[]);

    // .py extension, jupyter override -> nbformat JSON on disk.
    let as_py = dir.join("jupyter.py");
    let (err, text, sc) = server.call(
        "notebook_create",
        json!({"path": as_py, "notebook_format": "jupyter",
               "cells": [{"cell_type": "code", "source": "x = 1\n"}], "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["cell_count"], 1);
    let raw = std::fs::read_to_string(&as_py).unwrap();
    let parsed: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["nbformat"], 4);
    assert_eq!(parsed["cells"][0]["cell_type"], "code");

    // .ipynb extension, marimo override -> a marimo file on disk.
    let as_ipynb = dir.join("marimo.ipynb");
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": as_ipynb, "notebook_format": "marimo",
               "cells": [{"cell_type": "code", "source": "y = 2\n"}]}),
    );
    assert!(!err, "{text}");
    let raw = std::fs::read_to_string(&as_ipynb).unwrap();
    assert!(!raw.trim_start().starts_with('{'), "not JSON: {raw}");
    assert!(raw.contains("import marimo"), "{raw}");
    assert!(raw.contains("@app.cell"), "{raw}");
    assert!(raw.contains("app.run()"), "{raw}");
}

/// `dependencies` + `requires_python` emit a PEP 723 `# /// script` block
/// above `import marimo` (marimo's single-line serializer shape), the file
/// still opens as a healthy marimo notebook, and the block is absent from the
/// default scaffold.
#[test]
fn create_with_pep723_header_emits_block() {
    let dir = temp_dir("pep723-create");
    let mut server = Server::start(&[]);
    let target = dir.join("app.py");
    let (err, text, sc) = server.call(
        "notebook_create",
        json!({"path": target, "format": "json",
               "requires_python": ">=3.12",
               "dependencies": ["marimo", "wigglystuff==0.5.0"],
               "cells": [{"cell_type": "code", "source": "x = 1\n"}]}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["cell_count"], 1);
    let raw = std::fs::read_to_string(&target).unwrap();
    assert!(
        raw.starts_with(
            "# /// script\n# requires-python = \">=3.12\"\n\
             # dependencies = [\"marimo\", \"wigglystuff==0.5.0\"]\n# ///\n\nimport marimo\n"
        ),
        "PEP 723 header precedes import marimo: {raw}"
    );

    // The header is part of the recognized file, not a parse error.
    let (err, text, read) = server.call("notebook_read", json!({"path": target, "format": "json"}));
    assert!(!err, "{text}");
    assert_eq!(read["cell_count"], 1);

    // The default scaffold stays untouched: no block when neither is given.
    let plain = dir.join("plain.py");
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": plain, "format": "json",
               "cells": [{"cell_type": "code", "source": "x = 1\n"}]}),
    );
    assert!(!err, "{text}");
    let plain_raw = std::fs::read_to_string(&plain).unwrap();
    assert!(plain_raw.starts_with("import marimo\n"), "{plain_raw}");
    assert!(!plain_raw.contains("# ///"), "{plain_raw}");
}

/// The PEP 723 args are marimo-create-only: Jupyter targets and `from_path`
/// conversions refuse them instead of silently dropping the metadata.
#[test]
fn pep723_args_reject_non_marimo_targets() {
    let dir = temp_dir("pep723-reject");
    let mut server = Server::start(&[]);

    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": dir.join("nb.ipynb"), "dependencies": ["marimo"]}),
    );
    assert!(err && text.contains("only to marimo"), "{text}");

    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": dir.join("from.py"), "from_path": "tests/data/vscode_sample.ipynb",
               "requires_python": ">=3.12"}),
    );
    assert!(
        err && text.contains("cannot be combined with from_path"),
        "{text}"
    );
}

/// The emitted block byte-matches real marimo's own PEP 723 serializer
/// (`tomlkit.dumps` wrapped in `# /// script` markers), parses back to the
/// same manifest, and that manifest equals the real fixture
/// `tutorials/external_dependencies.py`'s — pinning the format against both
/// the installed marimo and a shipped marimo fixture.
#[test]
fn pep723_header_matches_marimos_serializer() {
    let dir = temp_dir("pep723-oracle");
    let mut server = Server::start(&[]);
    let target = dir.join("app.py");
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": target, "format": "json",
               "requires_python": ">=3.12",
               "dependencies": ["marimo", "wigglystuff==0.5.0"]}),
    );
    assert!(!err, "{text}");

    let Some(python) = ground_truth_python() else {
        return;
    };
    const CHECK: &str = r#"
import sys, tomlkit
from pathlib import Path
from marimo._environments.script_metadata import loads, wrap_block
path = sys.argv[1]
manifest = loads(open(path).read())
fixture = loads(Path("tests/data/marimo/tutorials/external_dependencies.py").read_text())
assert manifest == fixture, (manifest, fixture)
assert manifest == {"requires-python": ">=3.12", "dependencies": ["marimo", "wigglystuff==0.5.0"]}, manifest
block = wrap_block(tomlkit.dumps(manifest))
text = open(path).read()
assert text.startswith(block + "\n\nimport marimo"), text[:200]
print("ok")
"#;
    let report = run_python(&python, CHECK, &[target.to_str().unwrap()]);
    assert!(report.contains("ok"), "{report}");
}

/// `source_revision` turns a conversion into a move: the target is written
/// and verified, then the source is deleted. A stale revision refuses the
/// whole create before anything is written — neither file changes.
#[test]
fn from_path_with_source_revision_is_a_move() {
    let dir = temp_dir("convert-move");
    let mut server = Server::start(&[]);
    let source = dir.join("src.ipynb");
    std::fs::copy("tests/data/nbformat_empty.ipynb", &source).unwrap();
    let (err, text, read) = server.call("notebook_read", json!({"path": source, "format": "json"}));
    assert!(!err, "{text}");
    let revision = read["revision"].as_str().unwrap().to_string();

    // A stale source revision refuses the create: no target, source intact.
    let stale = dir.join("stale.md");
    let (err, _, refused) = server.call(
        "notebook_create",
        json!({"path": stale, "from_path": source, "source_revision": "0000000000000000", "format": "json"}),
    );
    assert!(err, "a stale source revision must refuse: {refused}");
    assert!(!stale.exists(), "a refused move must not write the target");
    assert!(source.exists(), "a refused move must keep the source");

    // The clean move: target written, source gone, the answer says so.
    let target = dir.join("out.md");
    let (err, text, created) = server.call(
        "notebook_create",
        json!({"path": target, "from_path": source, "source_revision": revision, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(created["source_removed"], true, "{created}");
    assert!(target.exists(), "the target stands");
    assert!(!source.exists(), "the move removes the source");

    // source_revision without from_path is a caller error.
    let (err, _, _) = server.call(
        "notebook_create",
        json!({"path": dir.join("x.ipynb"), "cells": [], "source_revision": revision}),
    );
    assert!(err, "source_revision requires from_path");
}

/// A move whose removal fails keeps the target and says so, never failing
/// into silence about the leftover. Pinned on POSIX by making the source's
/// directory read-only (a read-only directory forbids unlink, not read).
#[cfg(unix)]
#[test]
fn a_move_that_cannot_remove_the_source_says_so() {
    use std::os::unix::fs::PermissionsExt;

    let dir = temp_dir("convert-move-note");
    let mut server = Server::start(&[]);
    let locked = dir.join("locked");
    std::fs::create_dir(&locked).unwrap();
    let source = locked.join("src.ipynb");
    std::fs::copy("tests/data/nbformat_empty.ipynb", &source).unwrap();
    let (err, text, read) = server.call("notebook_read", json!({"path": source, "format": "json"}));
    assert!(!err, "{text}");
    let revision = read["revision"].as_str().unwrap().to_string();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();

    let target = dir.join("out.md");
    let (err, text, created) = server.call(
        "notebook_create",
        json!({"path": target, "from_path": source, "source_revision": revision, "format": "json"}),
    );
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(!err, "the created target is not an error: {text}");
    assert!(target.exists(), "the target stands");
    assert!(source.exists(), "the source could not be removed");
    assert_eq!(created["source_removed"], false, "{created}");
    let note = created["note"].as_str().unwrap_or_default();
    assert!(note.contains("could not be removed"), "{created}");
}
