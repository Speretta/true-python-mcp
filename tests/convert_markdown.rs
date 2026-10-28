//! Markdown export and marimo-oracle parity: exports that run under real
//! marimo, backtick fencing, byte parity with marimo's own serializer, and
//! broken-cell repair. Sibling of tests/convert.rs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::server::Server;
use common::{
    CLEAN_WITH_MARKDOWN, CONVERT_CHECK, ground_truth_python, run_python, temp_dir, warnings_of,
};
use serde_json::json;

/// Ground truth stronger than parse+compile: run the file the way
/// `python app.py` does (marimo script mode executes every cell through the
/// reactive graph). Catches references no cell defines — e.g. a markdown
/// cell whose `mo` was never imported — which compile_cell does NOT resolve.
const RUN_CHECK: &str = r#"
import sys
path = sys.argv[1]
exec(compile(open(path).read(), path, "exec"), {"__name__": "__main__"})
print("ran:", path)
"#;

/// EC-1 oracle: a converted notebook with markdown cells must RUN under real
/// marimo, not just parse and compile. Before the mo scaffold this died with
/// `NameError: name 'mo' is not defined` while every static check (marimo's
/// own get_notebook_status included) said "valid".
#[test]
fn converted_markdown_notebook_runs_under_real_marimo() {
    let dir = temp_dir("convert-run");
    let source = dir.join("clean.ipynb");
    std::fs::write(&source, CLEAN_WITH_MARKDOWN).unwrap();
    let mut server = Server::start(&[]);
    let target = dir.join("clean.py");
    let (err, text, created) = server.call(
        "notebook_create",
        json!({"path": target, "from_path": source, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert!(
        warnings_of(&created)
            .iter()
            .any(|warning| warning.contains("import marimo as mo")),
        "the scaffold cell must be disclosed: {created}"
    );
    let Some(python) = ground_truth_python() else {
        return;
    };
    let report = run_python(&python, CONVERT_CHECK, &[target.to_str().unwrap()]);
    assert!(report.contains("failed:0"), "{report}");
    let report = run_python(&python, RUN_CHECK, &[target.to_str().unwrap()]);
    assert!(report.contains("ran:"), "{report}");
}

/// EC-1, create path: `notebook_create cells=[markdown]` produces a marimo
/// file that runs, and a plain-code create does not gain a scaffold cell.
#[test]
fn created_markdown_notebook_runs_under_real_marimo() {
    let dir = temp_dir("create-run");
    let mut server = Server::start(&[]);
    let with_markdown = dir.join("with_markdown.py");
    let (err, text, created) = server.call(
        "notebook_create",
        json!({"path": with_markdown, "format": "json", "cells": [
            {"cell_type": "markdown", "source": "# Title\n"},
            {"cell_type": "code", "source": "value = 41 + 1\n"}
        ]}),
    );
    assert!(!err, "{text}");
    assert_eq!(created["cell_count"], 2, "requested cells: {created}");
    assert!(
        warnings_of(&created)
            .iter()
            .any(|warning| warning.contains("import marimo as mo")),
        "{created}"
    );

    let code_only = dir.join("code_only.py");
    let (err, text, created) = server.call(
        "notebook_create",
        json!({"path": code_only, "format": "json",
               "cells": [{"cell_type": "code", "source": "x = 1\n"}]}),
    );
    assert!(!err, "{text}");
    assert!(
        warnings_of(&created).is_empty(),
        "no scaffold without markdown: {created}"
    );
    let raw = std::fs::read_to_string(&code_only).unwrap();
    assert!(!raw.contains("import marimo as mo"), "{raw}");

    let Some(python) = ground_truth_python() else {
        return;
    };
    let report = run_python(&python, RUN_CHECK, &[with_markdown.to_str().unwrap()]);
    assert!(report.contains("ran:"), "{report}");
}

/// EC-6: a code cell containing a ``` fence exports inside a longer fence,
/// so the markdown document stays intact.
/// A code cell whose source contains a markdown fence: the export must
/// out-fence it instead of letting it close the block early.
const FENCE_NB: &str = r#"{
 "cells": [
  {"cell_type": "code", "execution_count": null, "id": "f1", "metadata": {}, "outputs": [],
   "source": ["help_md = \"\"\"\n", "```python\n", "y = 2\n", "```\n", "\"\"\"\n"]}
 ],
 "metadata": {}, "nbformat": 4, "nbformat_minor": 5
}"#;

#[test]
fn markdown_export_outfences_backticks() {
    let dir = temp_dir("convert-fence");
    let source = dir.join("fence.ipynb");
    std::fs::write(&source, FENCE_NB).unwrap();
    let mut server = Server::start(&[]);
    let target = dir.join("fence.md");
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": target, "from_path": source}),
    );
    assert!(!err, "{text}");
    let exported = std::fs::read_to_string(&target).unwrap();
    assert!(exported.contains("````python"), "{exported}");
    assert!(exported.contains("```python\ny = 2\n```"), "{exported}");
}

/// Ground truth: marimo's own converter, invoked exactly as the CLI does.
const MARIMO_CONVERT: &str = r#"
import subprocess, sys
completed = subprocess.run(
    [sys.executable, "-m", "marimo", "convert", sys.argv[1]],
    capture_output=True, text=True, check=True,
)
sys.stdout.write(completed.stdout)
"#;

/// Normalize a marimo file for the byte-exactness oracle: drop the trailing
/// newline (`marimo convert`'s print adds one) and replace the value of the
/// inert `__generated_with` line — but only in the header, before the first
/// cell. The line itself stays so its position is still checked, and a user
/// string inside a code cell that happens to start the same way is untouched.
fn normalize_for_oracle(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_header = true;
    for (i, line) in text.trim_end_matches('\n').lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if in_header && line.starts_with("__generated_with = \"") {
            out.push_str("__generated_with = \"<version>\"");
        } else {
            out.push_str(line);
            if line.starts_with("@app.cell") {
                in_header = false;
            }
        }
    }
    out
}

/// EC-7 oracle: for a clean code+markdown source, the converted file is
/// byte-identical to `marimo convert`'s output (modulo the trailing newline
/// the CLI's print adds): header spacing, the blank line before `return`
/// after a final import, named returns for defs other cells use, bare
/// `return` elsewhere, and the disclosed `mo` scaffold cell in the position
/// marimo puts it.
#[test]
fn converted_files_match_marimos_own_serializer() {
    let dir = temp_dir("convert-canonical");
    let source = dir.join("clean.ipynb");
    std::fs::write(&source, CLEAN_WITH_MARKDOWN).unwrap();
    let mut server = Server::start(&[]);
    let target = dir.join("mine.py");
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": target, "from_path": source}),
    );
    assert!(!err, "{text}");

    // A created (not converted) empty file keeps the same header spacing.
    let empty = dir.join("empty.py");
    let (err, text, _) = server.call("notebook_create", json!({"path": empty}));
    assert!(!err, "{text}");
    assert_eq!(
        std::fs::read_to_string(&empty).unwrap(),
        format!(
            "import marimo\n\n__generated_with = \"{}\"\napp = marimo.App()\n\n\n\
             if __name__ == \"__main__\":\n    app.run()\n",
            tpmcp::notebook::MARIMO_VERSION
        )
    );

    let Some(python) = ground_truth_python() else {
        return;
    };
    let official = run_python(&python, MARIMO_CONVERT, &[source.to_str().unwrap()]);
    let mine = std::fs::read_to_string(&target).unwrap();
    // `run_python` already normalizes CRLF to LF (host line endings are not
    // part of the layout claim). The `__generated_with` value is inert
    // metadata (marimo never validates it), so tpmcp's constant legitimately
    // differs from whatever marimo convert emits; normalize only its value.
    assert_eq!(normalize_for_oracle(&mine), normalize_for_oracle(&official));
}

/// A notebook whose magic cell converts verbatim and reads back broken.
const MAGIC_NB: &str = r##"{
 "cells": [
  {"cell_type": "code", "execution_count": null, "id": "g1", "metadata": {}, "outputs": [],
   "source": ["import math\n"]},
  {"cell_type": "code", "execution_count": null, "id": "g2", "metadata": {}, "outputs": [],
   "source": ["!pip install seaborn\n"]},
  {"cell_type": "markdown", "id": "g3", "metadata": {}, "source": ["# Title\n"]}
 ],
 "metadata": {}, "nbformat": 4, "nbformat_minor": 5
}"##;

/// The repair flow of example 06, pinned: a broken (read-only) cell is
/// fixed by deleting it and inserting a valid replacement. Broken-cell
/// blocks must not swallow the separators around them, or the delete fuses
/// the neighboring cells and the file stays corrupt.
#[test]
fn repairing_broken_cells_keeps_the_file_intact() {
    let dir = temp_dir("convert-repair");
    let source = dir.join("magic.ipynb");
    std::fs::write(&source, MAGIC_NB).unwrap();
    let mut server = Server::start(&[]);
    let target = dir.join("magic.py");
    let (err, text, created) = server.call(
        "notebook_create",
        json!({"path": target, "from_path": source, "format": "json"}),
    );
    assert!(!err, "{text}");
    let revision = created["revision"].as_str().unwrap();

    // The magic cell reads back broken (outline JSON carries the kind).
    let (err, text, outline) = server.call(
        "notebook_read",
        json!({"path": target, "detail": "outline", "format": "json"}),
    );
    assert!(!err, "{text}");
    let cells = outline["cells"].as_array().unwrap();
    let broken_index = cells
        .iter()
        .position(|cell| cell["kind"] == "broken")
        .unwrap_or_else(|| panic!("the magic cell degrades to broken: {outline}"));

    // Editing a broken cell in place is refused; delete+insert is the repair.
    let (err, text, _) = server.call(
        "cell_set_source",
        json!({"path": target, "cell": broken_index, "source": "# fixed\n",
               "revision": revision}),
    );
    assert!(err && text.contains("read-only"), "{text}");
    let (err, text, deleted) = server.call(
        "cell_delete",
        json!({"path": target, "cells": [broken_index], "revision": revision,
               "format": "json"}),
    );
    assert!(!err, "{text}");
    let (err, text, _) = server.call(
        "cell_insert",
        json!({"path": target, "index": broken_index, "cell_type": "code",
               "source": "# seaborn ships with the environment",
               "revision": deleted["revision"], "format": "json"}),
    );
    assert!(!err, "{text}");

    // The repaired file is whole: no fused blocks, no broken cells left,
    // zero check errors, and real marimo compiles every cell.
    let raw = std::fs::read_to_string(&target).unwrap();
    assert!(
        !raw.contains(")@app.cell"),
        "cell blocks must not fuse: {raw}"
    );
    assert!(
        !raw.contains("return@app.cell"),
        "separators must survive: {raw}"
    );
    let (err, text, outline) = server.call(
        "notebook_read",
        json!({"path": target, "detail": "outline", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert!(
        outline["cells"]
            .as_array()
            .unwrap()
            .iter()
            .all(|cell| cell["kind"] != "broken"),
        "{outline}"
    );
    let (err, text, checked) =
        server.call("notebook_check", json!({"path": target, "format": "json"}));
    assert!(!err, "{text}");
    assert_eq!(checked["error_count"], 0, "{checked}");
    if let Some(python) = ground_truth_python() {
        let report = run_python(&python, CONVERT_CHECK, &[target.to_str().unwrap()]);
        assert!(report.contains("failed:0"), "{report}");
        assert!(report.contains("unparsable:0"), "{report}");
    }
}
