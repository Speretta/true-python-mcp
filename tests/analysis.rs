//! Verification of the ruff integration layer against ground truth captured
//! from the real `ruff 0.17.0` binary (see tests/data/ruff.toml, select=["F"]).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use std::fs;
use std::path::Path;

use common::temp_dir;
use tpmcp::notebook::JupyterNotebook;
use tpmcp::ruff::{self, FormatOutcome, FormatWrite, SafeFixes};

fn code(d: &tpmcp::ruff::Diagnostic) -> &str {
    d.code.as_deref().unwrap_or("")
}

fn row_col(d: &tpmcp::ruff::Diagnostic) -> (usize, usize) {
    let loc = d.location.as_ref().expect("location present");
    (loc.row, loc.column)
}

/// Read `path` into the snapshot the single-read ruff API expects (CPU-F2):
/// ruff no longer re-reads the file, so tests hand it the bytes directly.
fn snap(path: &Path) -> tpmcp::fs::Snapshot {
    tpmcp::fs::read(path).unwrap()
}

/// A deeply nested but valid `.py` must not take the server down: ruff's
/// parser guards its own recursion, but the linter walk, the formatter, and
/// the recursive AST `Drop` glue do not — the marimo path got stacker guards
/// this wave; this test pins the ruff path. Subprocess shape as in the
/// marimo deep-nesting test: the child does the dangerous work in-process on
/// an ordinary test thread (the same stack shape as a tokio worker), the
/// parent only asserts that the child survived.
#[test]
fn deeply_nested_python_does_not_crash_ruff() {
    const CHILD_ENV: &str = "TPMCP_TEST_DEEP_RUFF";
    const TERMS_ENV: &str = "TPMCP_TEST_DEEP_TERMS";
    if std::env::var(CHILD_ENV).is_ok() {
        let terms: usize = std::env::var(TERMS_ENV)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(100_000)
            .max(2);
        let dir = temp_dir("deep-ruff");
        let path = dir.join("deep.py");
        let body = format!("x = {}1\n", "1+".repeat(terms - 1));
        fs::write(&path, &body).unwrap();
        eprintln!("stage: checking ({terms} terms)");
        let outcome = ruff::check(&path, SafeFixes::ReportOnly, snap(&path)).unwrap();
        eprintln!(
            "stage: checked, {} diagnostics; formatting (dry run)",
            outcome.diagnostics.len()
        );
        let formatted = ruff::format_file(&path, FormatWrite::DryRun, snap(&path)).unwrap();
        eprintln!("stage: formatted ({formatted:?})");
        let _ = fs::remove_dir_all(&dir);
        eprintln!("deep-ruff child survived {terms} terms");
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let output = std::process::Command::new(exe)
        .arg("--exact")
        .arg("deeply_nested_python_does_not_crash_ruff")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .output()
        .unwrap_or_else(|error| panic!("spawn deep-ruff child: {error}"));
    assert!(
        output.status.success(),
        "deep nesting killed the ruff child (exit {:?}); stderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Ground truth (ruff 0.17.0, select=["F"]):
///   F401 row 1 col 8, F401 row 2 col 8, F821 row 7 col 12
#[test]
fn check_python_matches_ruff_ground_truth() {
    let path = Path::new("tests/data/lint_sample.py");
    let outcome = ruff::check(path, SafeFixes::ReportOnly, snap(path)).unwrap();
    assert!(!outcome.changed);
    assert!(outcome.new_revision.is_none());

    let got: Vec<(String, usize, usize)> = outcome
        .diagnostics
        .iter()
        .map(|d| (code(d).to_string(), row_col(d).0, row_col(d).1))
        .collect();
    // Order matches the ruff CLI: sorted by position.
    assert_eq!(
        got,
        vec![
            ("F401".to_string(), 1, 8),
            ("F401".to_string(), 2, 8),
            ("F821".to_string(), 7, 12),
        ]
    );
    // Plain files have no cell.
    assert!(outcome.diagnostics.iter().all(|d| d.cell.is_none()));
    // Messages and fix metadata survive the round-trip through ruff's JSON.
    let f401 = outcome
        .diagnostics
        .iter()
        .find(|d| code(d) == "F401")
        .unwrap();
    assert!(f401.message.contains("imported but unused"));
    assert!(f401.fix.is_some(), "F401 has a safe fix");
}

/// Ground truth (ruff 0.17.0): notebook cells are 1-based (cell=2, cell=3);
/// tpmcp reports them 0-based (1, 2) to match the cell tools, with rows
/// relative to the cell.
#[test]
fn check_notebook_maps_cells_to_zero_based() {
    let path = Path::new("tests/data/lint_sample.ipynb");
    let outcome = ruff::check(path, SafeFixes::ReportOnly, snap(path)).unwrap();
    assert_eq!(outcome.diagnostics.len(), 2);

    let f401 = outcome
        .diagnostics
        .iter()
        .find(|d| code(d) == "F401")
        .expect("F401 in import cell");
    assert_eq!(f401.cell, Some(1), "ruff cell=2 -> tpmcp cell=1");
    assert_eq!(row_col(f401), (1, 8));

    let f821 = outcome
        .diagnostics
        .iter()
        .find(|d| code(d) == "F821")
        .expect("F821 in def cell");
    assert_eq!(f821.cell, Some(2), "ruff cell=3 -> tpmcp cell=2");
    assert_eq!(row_col(f821), (2, 12));
}

/// fix=true removes the unused imports and rewrites the file.
#[test]
fn check_fix_rewrites_python_file() {
    let dir = temp_dir("fix");
    let path = dir.join("sample.py");
    fs::write(&path, "import os\nimport sys\n\n\nprint('hi')\n").unwrap();

    let outcome = ruff::check(&path, SafeFixes::Apply, snap(&path)).unwrap();
    assert!(outcome.changed);
    let revision = outcome.new_revision.expect("fix returns a new revision");

    let content = fs::read_to_string(&path).unwrap();
    assert!(!content.contains("import os"), "unused import removed");
    assert!(!content.contains("import sys"), "unused import removed");
    assert!(content.contains("print('hi')"));
    assert_eq!(revision, tpmcp::fs::revision_of(content.as_bytes()));
    fs::remove_dir_all(&dir).unwrap();
}

/// format_file rewrites an unformatted Python file.
#[test]
fn format_rewrites_python_file() {
    let dir = temp_dir("format");
    let path = dir.join("sample.py");
    fs::write(&path, "x  =  1\ny=[1,2,3]\n").unwrap();

    let outcome = ruff::format_file(&path, FormatWrite::InPlace, snap(&path)).unwrap();
    assert!(matches!(outcome, FormatOutcome::Written { .. }));
    let content = fs::read_to_string(&path).unwrap();
    assert_eq!(content, "x = 1\ny = [1, 2, 3]\n");
    fs::remove_dir_all(&dir).unwrap();
}

/// check-only format reports a diff without touching the file.
#[test]
fn format_check_only_reports_diff() {
    let dir = temp_dir("format-check");
    let path = dir.join("sample.py");
    let original = "x  =  1\n";
    fs::write(&path, original).unwrap();

    let outcome = ruff::format_file(&path, FormatWrite::DryRun, snap(&path)).unwrap();
    match outcome {
        FormatOutcome::WouldReformat { diff } => {
            let diff = diff.expect("text files get a diff");
            assert!(
                diff.contains("-x  =  1") && diff.contains("+x = 1"),
                "{diff}"
            );
        }
        other => panic!("expected WouldReformat, got {other:?}"),
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), original, "untouched");
    fs::remove_dir_all(&dir).unwrap();
}

/// Formatting an already-formatted file is a no-op.
#[test]
fn format_unchanged_is_noop() {
    let dir = temp_dir("format-noop");
    let path = dir.join("sample.py");
    fs::write(&path, "x = 1\n").unwrap();
    assert!(matches!(
        ruff::format_file(&path, FormatWrite::InPlace, snap(&path)).unwrap(),
        FormatOutcome::Unchanged
    ));
    fs::remove_dir_all(&dir).unwrap();
}

/// Formatting a notebook keeps the file byte-exact canonical and formats the
/// offending code cell, leaving structure/outputs intact.
#[test]
fn format_notebook_stays_byte_exact() {
    let dir = temp_dir("format-notebook");
    let path = dir.join("notebook.ipynb");
    let notebook = JupyterNotebook::parse(
        r#"{
 "cells": [
  {
   "cell_type": "code",
   "execution_count": null,
   "id": "c0",
   "metadata": {},
   "outputs": [],
   "source": ["x  =  1\n"]
  }
 ],
 "metadata": {},
 "nbformat": 4,
 "nbformat_minor": 5
}
"#,
    )
    .unwrap();
    fs::write(&path, notebook.to_string_canonical()).unwrap();

    let outcome = ruff::format_file(&path, FormatWrite::InPlace, snap(&path)).unwrap();
    assert!(matches!(outcome, FormatOutcome::Written { .. }));

    let content = fs::read_to_string(&path).unwrap();
    let reparsed = JupyterNotebook::parse(&content).unwrap();
    // Byte-exact canonical: re-serializing is a no-op.
    assert_eq!(reparsed.to_string_canonical(), content);
    // The cell was formatted (ruff strips the last line's trailing newline,
    // per Jupyter convention; verified against `ruff format`).
    assert_eq!(reparsed.cells[0].source_text(), "x = 1");
    // Structure preserved.
    assert_eq!(reparsed.cells[0].cell_type, "code");
    assert!(reparsed.cells[0].extra.contains_key("outputs"));
    fs::remove_dir_all(&dir).unwrap();
}

/// F14: ruff_check on a non-Python path is a caller error, not a green "no
/// issues found" — and it matches ruff_format's invalid_input message/shape.
#[test]
fn check_rejects_non_python_path() {
    let dir = temp_dir("non-python");
    let path = dir.join("notes.md");
    fs::write(&path, "# not python\n").unwrap();

    let error = ruff::check(&path, SafeFixes::ReportOnly, snap(&path)).unwrap_err();
    assert_eq!(error.kind(), "invalid_input");
    assert!(
        error.to_string().contains("not a Python file or notebook"),
        "{error}"
    );
    fs::remove_dir_all(&dir).unwrap();
}

/// CPU-F1: the settings cache is validated by `(mtime, length)` of the
/// governing config, so editing that config is picked up on the next call
/// instead of serving a stale build.
#[test]
fn settings_cache_invalidates_on_config_change() {
    let dir = temp_dir("settings-cache");
    let config = dir.join("ruff.toml");
    let path = dir.join("sample.py");
    fs::write(&path, "import os\n\n\nprint(undefined_name)\n").unwrap();

    // Config selecting only F401: the undefined name is not reported.
    fs::write(&config, "[lint]\nselect = [\"F401\"]\n").unwrap();
    let first = ruff::check(&path, SafeFixes::ReportOnly, snap(&path)).unwrap();
    let first_codes: Vec<&str> = first.diagnostics.iter().map(code).collect();
    assert!(first_codes.contains(&"F401"), "{first_codes:?}");
    assert!(!first_codes.contains(&"F821"), "only F401: {first_codes:?}");

    // Same config path, different length -> the validity token differs, so the
    // cache must rebuild with the widened rule selection.
    fs::write(&config, "[lint]\nselect = [\"F401\", \"F821\"]\n").unwrap();
    let second = ruff::check(&path, SafeFixes::ReportOnly, snap(&path)).unwrap();
    let second_codes: Vec<&str> = second.diagnostics.iter().map(code).collect();
    assert!(second_codes.contains(&"F401"), "{second_codes:?}");
    assert!(
        second_codes.contains(&"F821"),
        "F821 added: {second_codes:?}"
    );

    fs::remove_dir_all(&dir).unwrap();
}

/// A plain `.py` (not a marimo notebook) still reports an unused import: the
/// marimo filter is gated on `is_marimo`, so an ordinary module never loses a
/// genuine F401.
#[test]
fn plain_python_still_reports_unused_import() {
    let dir = temp_dir("plain-f401");
    let path = dir.join("sample.py");
    fs::write(&path, "import os\n\nprint('hi')\n").unwrap();

    let outcome = ruff::check(&path, SafeFixes::ReportOnly, snap(&path)).unwrap();
    let codes: Vec<&str> = outcome.diagnostics.iter().map(code).collect();
    assert_eq!(codes, vec!["F401"], "{:?}", outcome.diagnostics);
    let f401 = &outcome.diagnostics[0];
    assert!(
        f401.message.contains("imported but unused"),
        "{}",
        f401.message
    );
    fs::remove_dir_all(&dir).unwrap();
}

/// A synthetic marimo `.py` linted per cell drops the scaffolding false
/// positives — F401(`marimo`), F811(`mo`), and the cell `return` (PLR1711) /
/// import-block (I001) noise — while a genuine C408 and F821 survive. The
/// whole-file lint would have flagged `import marimo` (F401) and each cell's
/// trailing `return` (PLR1711). A reference to the header's `app` is not an
/// undefined name either.
#[test]
fn marimo_lint_filters_scaffolding_noise() {
    let dir = temp_dir("marimo-lint");
    fs::write(
        dir.join("ruff.toml"),
        "[lint]\nselect = [\"F401\", \"F811\", \"F821\", \"PLR1711\", \"I001\", \"C408\"]\n",
    )
    .unwrap();
    let path = dir.join("notebook.py");
    fs::write(
        &path,
        r#"import marimo

__generated_with = "0.25.2"
app = marimo.App()


@app.cell
def _():
    import marimo
    import marimo as mo
    return


@app.cell
def _(mo):
    mo = 1
    result = dict(a=1)
    print(undefined_name)
    print(app)
    return


if __name__ == "__main__":
    app.run()
"#,
    )
    .unwrap();

    let outcome = ruff::check(&path, SafeFixes::ReportOnly, snap(&path)).unwrap();
    let codes: Vec<&str> = outcome.diagnostics.iter().map(code).collect();
    assert_eq!(codes, vec!["C408", "F821"], "{:?}", outcome.diagnostics);

    // Both real findings map to cell 1 (the second @app.cell), 0-based.
    assert!(
        outcome.diagnostics.iter().all(|d| d.cell == Some(1)),
        "{:?}",
        outcome.diagnostics
    );

    // The F821 is genuinely `undefined_name`, not a graph-provided name.
    let f821 = outcome
        .diagnostics
        .iter()
        .find(|d| code(d) == "F821")
        .expect("F821 present");
    assert!(f821.message.contains("undefined_name"), "{}", f821.message);

    fs::remove_dir_all(&dir).unwrap();
}
