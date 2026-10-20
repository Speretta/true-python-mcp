//! `ty_check` tool tests: Astral's type checker compiled in, read-only.
//! The fixture-driven oracle + unit tests below pin the embedded checker
//! against the real `ty 0.0.18` binary (see tests/data/ty/, captured with
//! `--output-format concise --python-version 3.11`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use std::path::Path;

use common::{ground_truth_ty, run_ty, server::Server, temp_dir};
use serde_json::{Value, json};
use tpmcp::ty;

#[test]
fn ty_check_reports_type_errors() {
    let dir = temp_dir("ty-check");
    let path = dir.join("bad.py");
    std::fs::write(&path, "x: int = \"hello\"\n").unwrap();
    let mut server = Server::start(&[]);
    let (err, text, _) = server.call("ty_check", json!({"path": path, "format": "json"}));
    assert!(!err, "{text}");
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["diagnostic_count"], 1);
    assert_eq!(value["diagnostics"][0]["code"], "invalid-assignment");
    assert_eq!(value["diagnostics"][0]["severity"], "error");
    assert_eq!(value["diagnostics"][0]["location"]["row"], 1);
}

#[test]
fn ty_check_maps_ipynb_diagnostics_to_cells() {
    let dir = temp_dir("ty-check-ipynb");
    let path = dir.join("bad.ipynb");
    std::fs::write(
        &path,
        serde_json::to_string(&json!({
            "cells": [
                {"cell_type": "code", "execution_count": null, "id": "a", "metadata": {}, "outputs": [], "source": ["x: int = 1\n"]},
                {"cell_type": "code", "execution_count": null, "id": "b", "metadata": {}, "outputs": [], "source": ["y: int = \"oops\"\n"]}
            ],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        }))
        .unwrap(),
    )
    .unwrap();
    let mut server = Server::start(&[]);
    let (err, text, _) = server.call("ty_check", json!({"path": path, "format": "json"}));
    assert!(!err, "{text}");
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["diagnostic_count"], 1);
    assert_eq!(value["diagnostics"][0]["cell"], 1);
    assert_eq!(value["diagnostics"][0]["code"], "invalid-assignment");
}

#[test]
fn ty_check_clean_file_has_no_diagnostics() {
    let dir = temp_dir("ty-check-clean");
    let path = dir.join("good.py");
    std::fs::write(&path, "x: int = 1\n").unwrap();
    let mut server = Server::start(&[]);
    let (err, text, _) = server.call("ty_check", json!({"path": path, "format": "json"}));
    assert!(!err, "{text}");
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["diagnostic_count"], 0);
}

// ---- fixture-driven oracle + unit tests (library-level, mirror tests/analysis.rs) ----

fn code(d: &tpmcp::ruff::Diagnostic) -> &str {
    d.code.as_deref().unwrap_or("")
}

fn row_col(d: &tpmcp::ruff::Diagnostic) -> (usize, usize) {
    let loc = d.location.as_ref().expect("location present");
    (loc.row, loc.column)
}

fn codes(diagnostics: &[tpmcp::ruff::Diagnostic]) -> Vec<String> {
    diagnostics.iter().map(|d| code(d).to_string()).collect()
}

/// Extract `[code]` from each `…: error[code] …` line of ty's `concise` output.
/// The `Found N diagnostics` trailer and `All checks passed!` banner carry no
/// brackets and are ignored.
fn concise_codes(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| {
            let start = line.find('[')?;
            let end = line.find(']')?;
            (end > start).then(|| line[start + 1..end].to_string())
        })
        .collect()
}

/// Ground truth (ty 0.0.18, --output-format concise --python-version 3.11):
///   invalid-assignment 1:10, invalid-return-type 5:12, invalid-argument-type 12:8
#[test]
fn check_python_matches_ty_ground_truth() {
    let diagnostics = ty::check_path(Path::new("tests/data/ty/bad.py")).unwrap();
    let got: Vec<(String, usize, usize)> = diagnostics
        .iter()
        .map(|d| (code(d).to_string(), row_col(d).0, row_col(d).1))
        .collect();
    // Order matches the ty CLI: sorted by position.
    assert_eq!(
        got,
        vec![
            ("invalid-assignment".to_string(), 1, 10),
            ("invalid-return-type".to_string(), 5, 12),
            ("invalid-argument-type".to_string(), 12, 8),
        ]
    );
    // Plain files have no cell.
    assert!(diagnostics.iter().all(|d| d.cell.is_none()));
    // The message survives tpmcp's map_diagnostic round-trip.
    let assignment = diagnostics
        .iter()
        .find(|d| code(d) == "invalid-assignment")
        .unwrap();
    assert!(assignment.message.contains("not assignable"));
}

/// Ground truth (ty 0.0.18): notebook cells are 1-based (cell=3); tpmcp
/// reports them 0-based (2) to match the cell tools, with rows relative to
/// the cell.
#[test]
fn check_notebook_maps_cells_to_zero_based() {
    let diagnostics = ty::check_path(Path::new("tests/data/ty/cross_cell.ipynb")).unwrap();
    assert_eq!(diagnostics.len(), 1);
    let diagnostic = &diagnostics[0];
    assert_eq!(code(diagnostic), "invalid-argument-type");
    assert_eq!(diagnostic.cell, Some(2), "ty cell=3 -> tpmcp cell=2");
    assert_eq!(row_col(diagnostic), (1, 8));
}

/// Ground truth (ty 0.0.18): a `%%bash` cell contributes no source to the
/// concatenation and is skipped; the sibling typed cell still reports.
#[test]
fn magic_cell_is_skipped_like_ruff() {
    let diagnostics = ty::check_path(Path::new("tests/data/ty/magic_cell.ipynb")).unwrap();
    assert_eq!(diagnostics.len(), 1);
    let diagnostic = &diagnostics[0];
    assert_eq!(code(diagnostic), "invalid-assignment");
    assert_eq!(diagnostic.cell, Some(1), "ty cell=2 -> tpmcp cell=1");
    assert_eq!(row_col(diagnostic), (1, 10));
}

/// Ground truth (ty 0.0.18): a `.pyi` with an undefined name in an annotation
/// reports unresolved-reference. (A `.pyi` "wrong" annotation is green — a
/// stub's own annotations are trusted — so this is the stub error to pin.)
#[test]
fn stub_pyi_is_checked() {
    let diagnostics = ty::check_path(Path::new("tests/data/ty/stub.pyi")).unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(code(&diagnostics[0]), "unresolved-reference");
    assert_eq!(row_col(&diagnostics[0]), (1, 12));
}

/// Ground truth (ty 0.0.18): a stdlib-operator type error maps to
/// unsupported-operator at the value span.
#[test]
fn stdlib_operator_mismatch_is_mapped() {
    let diagnostics = ty::check_path(Path::new("tests/data/ty/stdlib_mismatch.py")).unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(code(&diagnostics[0]), "unsupported-operator");
    assert_eq!(row_col(&diagnostics[0]), (1, 10));
}

/// Clean, untyped and stdlib-import files produce no diagnostics (no
/// false-positive / implicit-any / unresolved-import noise).
#[test]
fn clean_and_untyped_files_are_empty() {
    for file in ["good.py", "untyped.py", "imports.py"] {
        let path = format!("tests/data/ty/{file}");
        let diagnostics = ty::check_path(Path::new(&path)).unwrap();
        assert!(diagnostics.is_empty(), "{file}: got {diagnostics:?}");
    }
}

/// Re-verify the embedded checker against the real `ty==0.0.18` binary: for
/// every fixture the codes must match exactly. `--python-version 3.11` pins
/// the CLI to `PythonVersion::default()` (PY311), which the CLI would infer
/// from the environment instead. Skipped when the binary is absent;
/// `TPMCP_REQUIRE_GROUND_TRUTH=1` fails instead.
#[test]
fn check_matches_ty_ground_truth() {
    let Some(ty_bin) = ground_truth_ty() else {
        return;
    };
    let cases: &[(&str, &[&str])] = &[
        ("good.py", &[]),
        (
            "bad.py",
            &[
                "invalid-assignment",
                "invalid-return-type",
                "invalid-argument-type",
            ],
        ),
        ("untyped.py", &[]),
        ("stub.pyi", &["unresolved-reference"]),
        ("imports.py", &[]),
        ("stdlib_mismatch.py", &["unsupported-operator"]),
        ("cross_cell.ipynb", &["invalid-argument-type"]),
        ("magic_cell.ipynb", &["invalid-assignment"]),
    ];
    for &(file, expected) in cases {
        let path = format!("tests/data/ty/{file}");
        let cli_codes = concise_codes(&run_ty(
            &ty_bin,
            &[
                "check",
                "--output-format",
                "concise",
                "--color",
                "never",
                "--no-progress",
                "--python-version",
                "3.11",
                path.as_str(),
            ],
        ));
        let our_codes = codes(&ty::check_path(Path::new(&path)).unwrap());
        assert_eq!(
            cli_codes, our_codes,
            "{file}: codes diverge from ty==0.0.18"
        );
        assert_eq!(our_codes, expected, "{file}: frozen counts drifted");
    }
}
