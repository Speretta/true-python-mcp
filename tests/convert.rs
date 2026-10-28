//! Format conversion semantics: corpus round-trips, the `mo` scaffold,
//! passthrough fields, and the warnings that disclose everything a
//! conversion leaves behind.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use std::path::PathBuf;

use common::server::Server;
use common::{
    CLEAN_WITH_MARKDOWN, CONVERT_CHECK, ground_truth_python, run_python, temp_dir, warnings_of,
};
use serde_json::{Value, json};

/// Ground truth: nbformat itself validates the file.
const NB_VALIDATE: &str = r#"
import sys, nbformat
nb = nbformat.read(sys.argv[1], as_version=4)
nbformat.validate(nb)
print("valid:", sys.argv[1])
"#;

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
