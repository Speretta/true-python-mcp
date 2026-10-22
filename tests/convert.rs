//! Conversion sweeps and ground-truth oracles for `notebook_create
//! from_path`: every fixture through both directions, real-marimo checks on
//! the artifacts, and pins for the conversion-honesty guarantees (nothing
//! dropped silently, markdown cells that actually run).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use std::path::PathBuf;

use common::{ground_truth_python, run_python, server::Server, temp_dir};
use serde_json::{Value, json};

/// Ground truth: real marimo loads the converted file (what `marimo edit`
/// does on open) and compiles every cell it could parse. Unparsable regions
/// are expected for sources with Jupyter magics — the loader degrades them,
/// exactly like our own reader. A nonzero `failed` count means marimo could
/// parse a cell but not compile it: a broken artifact.
const CONVERT_CHECK: &str = r#"
import sys
from marimo._ast.parse import parse_notebook
from marimo._ast.compiler import compile_cell
path = sys.argv[1]
nb = parse_notebook(open(path).read(), path)
compiled = failed = unparsable = 0
for i, cell in enumerate(nb.cells):
    if type(cell).__name__ == "UnparsableCell":
        unparsable += 1
        continue
    try:
        compile_cell(cell.code, str(i))
        compiled += 1
    except Exception as error:
        failed += 1
        print(f"cell {i}: {type(error).__name__}: {error}", file=sys.stderr)
print(f"cells:{len(nb.cells)} compiled:{compiled} failed:{failed} unparsable:{unparsable}")
"#;

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

/// Ground truth: nbformat itself validates the file.
const NB_VALIDATE: &str = r#"
import sys, nbformat
nb = nbformat.read(sys.argv[1], as_version=4)
nbformat.validate(nb)
print("valid:", sys.argv[1])
"#;

/// A clean Jupyter notebook with a markdown cell and no external
/// dependencies, so the converted marimo file can be EXECUTED.
const CLEAN_WITH_MARKDOWN: &str = r##"{
 "cells": [
  {"cell_type": "code", "execution_count": null, "id": "c1", "metadata": {}, "outputs": [],
   "source": ["import math\n", "value = math.pi\n"]},
  {"cell_type": "markdown", "id": "c2", "metadata": {}, "source": ["# Title\n", "\n", "math is fun\n"]},
  {"cell_type": "code", "execution_count": null, "id": "c3", "metadata": {}, "outputs": [],
   "source": ["print(value)\n"]}
 ],
 "metadata": {},
 "nbformat": 4,
 "nbformat_minor": 5
}"##;

/// A Jupyter notebook carrying every passthrough layer: outputs, execution
/// counts, cell metadata, attachments and notebook metadata.
const RICH: &str = r#"{
 "cells": [
  {"cell_type": "code", "execution_count": 7, "id": "r1",
   "metadata": {"tags": ["important"], "jupyter": {"source_hidden": true}},
   "outputs": [
    {"output_type": "stream", "name": "stdout", "text": ["hello\n"]},
    {"output_type": "execute_result", "execution_count": 7,
     "data": {"text/plain": "42", "image/png": "aGVsbG8="}, "metadata": {}}
   ],
   "source": ["print('hello')\n", "42"]},
  {"cell_type": "markdown", "id": "r2", "metadata": {},
   "attachments": {"image.png": {"image/png": "aGVsbG8="}},
   "source": ["![pic](attachment:image.png)\n"]}
 ],
 "metadata": {"kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"},
              "language_info": {"name": "python", "version": "3.13.0"}},
 "nbformat": 4,
 "nbformat_minor": 5
}"#;

/// A marimo file with app configuration and a top-level comment: both live
/// outside cells and a conversion must report leaving them behind.
const MARIMO_WITH_LITERALS: &str = r#"import marimo

__generated_with = "0.25.1"
app = marimo.App(width="full")


# a comment outside any cell


@app.cell
def _():
    x = 1
    return (x,)


if __name__ == "__main__":
    app.run()
"#;

/// A code cell whose source contains a markdown fence: the export must
/// out-fence it instead of letting it close the block early.
const FENCE_NB: &str = r#"{
 "cells": [
  {"cell_type": "code", "execution_count": null, "id": "f1", "metadata": {}, "outputs": [],
   "source": ["help_md = \"\"\"\n", "```python\n", "y = 2\n", "```\n", "\"\"\"\n"]}
 ],
 "metadata": {}, "nbformat": 4, "nbformat_minor": 5
}"#;

/// Every fixture file with `extension` under `directory` (one level of
/// subdirectories included), sorted for deterministic sweeps.
fn corpus(directory: &str, extension: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut collect = |entry: &std::path::Path| {
        if entry.extension().is_some_and(|found| found == extension) {
            paths.push(entry.to_path_buf());
        }
    };
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            for nested in std::fs::read_dir(&path).unwrap() {
                collect(&nested.unwrap().path());
            }
        } else {
            collect(&path);
        }
    }
    paths.sort();
    assert!(
        !paths.is_empty(),
        "corpus {directory}/*.{extension} is empty"
    );
    paths
}

fn warnings_of(structured: &Value) -> Vec<String> {
    structured["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|warning| warning.as_str().unwrap().to_string())
        .collect()
}

/// G1, marimo direction: every marimo fixture (including the shipped
/// tutorials) converts to .ipynb, reopens through notebook_read and
/// notebook_check without a crash, keeps its cell count, and reports
/// deterministic warnings.
#[test]
fn marimo_corpus_converts_to_jupyter_and_reopens() {
    let dir = temp_dir("convert-sweep-marimo");
    let mut server = Server::start(&[]);
    for source in corpus("tests/data/marimo", "py") {
        let name = source.file_stem().unwrap().to_string_lossy().into_owned();
        let target = dir.join(format!("from_{name}.ipynb"));
        let (err, text, created) = server.call(
            "notebook_create",
            json!({"path": target, "from_path": source, "format": "json"}),
        );
        assert!(!err, "{source:?}: {text}");

        let (err, text, read) =
            server.call("notebook_read", json!({"path": target, "format": "json"}));
        assert!(!err, "{target:?}: {text}");
        assert_eq!(
            read["cell_count"], created["cell_count"],
            "{name}: reopen sees every converted cell"
        );
        let (err, text, checked) =
            server.call("notebook_check", json!({"path": target, "format": "json"}));
        assert!(!err, "{target:?}: {text}");
        assert!(checked["findings"].is_array(), "{name}: {checked}");

        let again = dir.join(format!("again_{name}.ipynb"));
        let (_, _, second) = server.call(
            "notebook_create",
            json!({"path": again, "from_path": source, "format": "json"}),
        );
        assert_eq!(
            created["warnings"], second["warnings"],
            "{name}: warnings are deterministic"
        );
    }
}

/// G1, Jupyter direction: every .ipynb fixture converts to a marimo .py that
/// real marimo loads and compiles (cell counts reported per-file by the
/// ground-truth script). The reported cell_count counts the converted source
/// cells; an injected `mo` scaffold cell shows up in the warnings and in the
/// reopened file (+1), never silently.
#[test]
fn jupyter_corpus_converts_to_marimo_and_reopens() {
    let dir = temp_dir("convert-sweep-jupyter");
    let Some(python) = ground_truth_python() else {
        return;
    };
    let mut server = Server::start(&[]);
    for source in corpus("tests/data", "ipynb") {
        let name = source.file_stem().unwrap().to_string_lossy().into_owned();
        let target = dir.join(format!("from_{name}.py"));
        let (err, text, created) = server.call(
            "notebook_create",
            json!({"path": target, "from_path": source, "format": "json"}),
        );
        assert!(!err, "{source:?}: {text}");
        let scaffolded = warnings_of(&created)
            .iter()
            .any(|warning| warning.contains("import marimo as mo"));

        let (err, text, read) =
            server.call("notebook_read", json!({"path": target, "format": "json"}));
        assert!(!err, "{target:?}: {text}");
        let expected = created["cell_count"].as_u64().unwrap() + u64::from(scaffolded);
        assert_eq!(
            read["cell_count"].as_u64(),
            Some(expected),
            "{name}: converted cells (+ the disclosed mo scaffold) all reopen"
        );

        let report = run_python(&python, CONVERT_CHECK, &[target.to_str().unwrap()]);
        assert!(report.contains("failed:0"), "{name}: {report}");

        let again = dir.join(format!("again_{name}.py"));
        let (_, _, second) = server.call(
            "notebook_create",
            json!({"path": again, "from_path": source, "format": "json"}),
        );
        assert_eq!(
            created["warnings"], second["warnings"],
            "{name}: warnings are deterministic"
        );
    }
}

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

/// The scaffold is added exactly once and only when needed: a source that
/// already defines `mo` (a marimo file with markdown cells) converts without
/// one, and re-converting a scaffolded file does not stack a second.
#[test]
fn mo_scaffold_is_added_once_and_only_when_needed() {
    let dir = temp_dir("scaffold");
    let source = dir.join("clean.ipynb");
    std::fs::write(&source, CLEAN_WITH_MARKDOWN).unwrap();
    let mut server = Server::start(&[]);

    let once = dir.join("once.py");
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": once, "from_path": source, "format": "json"}),
    );
    assert!(!err, "{text}");
    let raw = std::fs::read_to_string(&once).unwrap();
    assert_eq!(raw.matches("import marimo as mo").count(), 1, "{raw}");

    // marimo -> marimo of the scaffolded file: mo is defined, no second one.
    let twice = dir.join("twice.py");
    let (err, text, created) = server.call(
        "notebook_create",
        json!({"path": twice, "from_path": once, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert!(
        !warnings_of(&created)
            .iter()
            .any(|warning| warning.contains("import marimo as mo")),
        "{created}"
    );
    let raw = std::fs::read_to_string(&twice).unwrap();
    assert_eq!(raw.matches("import marimo as mo").count(), 1, "{raw}");

    // A marimo fixture that brings its own mo definition converts as-is.
    let own = dir.join("own_mo.py");
    let (err, text, created) = server.call(
        "notebook_create",
        json!({"path": own, "from_path": "tests/data/marimo/tutorials/markdown.py",
               "format": "json"}),
    );
    assert!(!err, "{text}");
    assert!(
        !warnings_of(&created)
            .iter()
            .any(|warning| warning.contains("import marimo as mo")),
        "{created}"
    );
}

/// EC-2: same-format conversion is a rebuild, not a strip-down — outputs,
/// execution counts, cell metadata, ids, attachments and notebook metadata
/// all survive, no warnings are needed, and nbformat still validates.
#[test]
fn same_format_conversion_preserves_passthrough_fields() {
    let dir = temp_dir("convert-same");
    let source = dir.join("rich.ipynb");
    std::fs::write(&source, RICH).unwrap();
    let mut server = Server::start(&[]);
    let target = dir.join("copy.ipynb");
    let (err, text, created) = server.call(
        "notebook_create",
        json!({"path": target, "from_path": source, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert!(warnings_of(&created).is_empty(), "{created}");

    let copy: Value = serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
    let code = &copy["cells"][0];
    assert_eq!(code["execution_count"], 7);
    assert_eq!(code["id"], "r1");
    assert_eq!(code["outputs"].as_array().unwrap().len(), 2);
    assert_eq!(code["metadata"]["tags"][0], "important");
    assert!(copy["cells"][1]["attachments"]["image.png"].is_object());
    assert_eq!(copy["metadata"]["kernelspec"]["name"], "python3");

    if let Some(python) = ground_truth_python() {
        let report = run_python(&python, NB_VALIDATE, &[target.to_str().unwrap()]);
        assert!(report.contains("valid:"), "{report}");
    }
}

/// EC-2: what a target format cannot carry is reported — marimo app
/// configuration and top-level lines for marimo sources, the metadata
/// layers for Jupyter sources, attachments for .md exports.
#[test]
fn conversion_warnings_cover_the_silent_classes() {
    let dir = temp_dir("convert-warnings");
    let mut server = Server::start(&[]);

    // marimo literals: App configuration + a comment outside cells.
    let literal_source = dir.join("literals.py");
    std::fs::write(&literal_source, MARIMO_WITH_LITERALS).unwrap();
    let (err, text, created) = server.call(
        "notebook_create",
        json!({"path": dir.join("literals.ipynb"), "from_path": literal_source, "format": "json"}),
    );
    assert!(!err, "{text}");
    let warnings = warnings_of(&created);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("marimo.App configuration")),
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("top-level lines outside cells") && w.contains("# a comment")),
        "{warnings:?}"
    );

    // Jupyter metadata layers on the way to marimo.
    let rich = dir.join("rich.ipynb");
    std::fs::write(&rich, RICH).unwrap();
    let (err, text, created) = server.call(
        "notebook_create",
        json!({"path": dir.join("rich.py"), "from_path": rich, "format": "json"}),
    );
    assert!(!err, "{text}");
    let warnings = warnings_of(&created);
    assert!(
        warnings.iter().any(|w| w.contains("cell metadata")),
        "{warnings:?}"
    );
    assert!(
        warnings.iter().any(|w| w.contains("notebook metadata")),
        "{warnings:?}"
    );

    // .md export: attachments are content, so dropping them is reported.
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": dir.join("rich.md"), "from_path": rich, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert!(
        text.contains("attachments not exported (image.png)"),
        "{text}"
    );
}

/// A PEP 723 `# /// script` header is marimo scaffold, not stray user
/// content: converting a file whose only top-level literals are the header
/// and the standard scaffold produces no "top-level lines outside cells"
/// warning.
#[test]
fn pep723_header_is_not_stray_content() {
    let dir = temp_dir("convert-pep723");
    let mut server = Server::start(&[]);
    let source = dir.join("pep723.py");
    std::fs::write(
        &source,
        r#"# /// script
# requires-python = ">=3.12"
# dependencies = ["marimo"]
# ///

import marimo

__generated_with = "0.25.1"
app = marimo.App()


@app.cell
def _():
    x = 1
    return (x,)


if __name__ == "__main__":
    app.run()
"#,
    )
    .unwrap();

    let (err, text, created) = server.call(
        "notebook_create",
        json!({"path": dir.join("pep723.ipynb"), "from_path": source, "format": "json"}),
    );
    assert!(!err, "{text}");
    let warnings = warnings_of(&created);
    assert!(
        !warnings
            .iter()
            .any(|warning| warning.contains("top-level lines outside cells")),
        "the PEP 723 header is scaffold, not stray content: {warnings:?}"
    );
}

/// EC-6: a code cell containing a ``` fence exports inside a longer fence,
/// so the markdown document stays intact.
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
        json!({"path": target, "cell": broken_index, "revision": revision,
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
