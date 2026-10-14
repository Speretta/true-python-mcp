//! Verification of the ruff integration layer against ground truth captured
//! from the real `ruff 0.17.0` binary (see tests/data/ruff.toml, select=["F"]).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
use std::fs;
use std::path::{Path, PathBuf};

use tpmcp::notebook::JupyterNotebook;
use tpmcp::ruff::{self, FormatOutcome};

fn temp_dir(tag: &str) -> PathBuf {
    let unique = blake3::hash(tag.as_bytes()).to_hex()[..8].to_string();
    let dir =
        std::env::temp_dir().join(format!("tpmcp-ruff-{}-{tag}-{unique}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn code(d: &tpmcp::ruff::Diagnostic) -> &str {
    d.code.as_deref().unwrap_or("")
}

fn row_col(d: &tpmcp::ruff::Diagnostic) -> (usize, usize) {
    let loc = d.location.as_ref().expect("location present");
    (loc.row, loc.column)
}

/// Ground truth (ruff 0.17.0, select=["F"]):
///   F401 row 1 col 8, F401 row 2 col 8, F821 row 7 col 12
#[test]
fn check_python_matches_ruff_ground_truth() {
    let outcome = ruff::check(Path::new("tests/data/lint_sample.py"), false).unwrap();
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
    let outcome = ruff::check(Path::new("tests/data/lint_sample.ipynb"), false).unwrap();
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

    let outcome = ruff::check(&path, true).unwrap();
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

    let outcome = ruff::format_file(&path, true).unwrap();
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

    let outcome = ruff::format_file(&path, false).unwrap();
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
        ruff::format_file(&path, true).unwrap(),
        FormatOutcome::Unchanged
    ));
    fs::remove_dir_all(&dir).unwrap();
}

/// Formatting a notebook keeps the file byte-exact canonical and formats the
/// offending code cell, leaving structure/outputs intact.
#[test]
fn format_notebook_stays_byte_exact() {
    let dir = temp_dir("format-nb");
    let path = dir.join("nb.ipynb");
    let nb = JupyterNotebook::parse(
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
    fs::write(&path, nb.to_string_canonical()).unwrap();

    let outcome = ruff::format_file(&path, true).unwrap();
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
