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

/// A marimo `.py` is checked as plain Python (documented in the tool
/// description): the scaffolding and cell bodies form one module, so a cell's
/// type error is reported as a plain-file diagnostic with no `cell` index —
/// and nothing crashes on the marimo decorators/scaffolding.
#[test]
fn marimo_py_is_checked_as_plain_python() {
    let dir = temp_dir("ty-marimo");
    let path = dir.join("app.py");
    let content = common::synthetic_marimo(&["x: int = \"oops\""]);
    std::fs::write(&path, &content).unwrap();

    let diagnostics = ty::check_path(&path).unwrap();
    let assignment = diagnostics
        .iter()
        .find(|d| code(d) == "invalid-assignment")
        .expect("the marimo cell's type error is reported");
    // A plain file carries no cell index, even though the error sits inside a
    // marimo cell body.
    assert!(diagnostics.iter().all(|d| d.cell.is_none()));
    // The error is located on the cell body's own line.
    let row = content
        .lines()
        .position(|line| line.contains("x: int = \"oops\""))
        .map(|i| i + 1)
        .unwrap();
    assert_eq!(row_col(assignment).0, row);
    std::fs::remove_dir_all(&dir).unwrap();
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

/// Ground truth (ty 0.0.18): a first-party import resolves across files — the
/// `add` signature lives in defs.py, so the `add(1, "two")` call in
/// uses_defs.py is checked against it and reports invalid-argument-type. This
/// pins that the embedded checker follows imports, not just the single file.
#[test]
fn cross_file_import_resolves_types() {
    let diagnostics = ty::check_path(Path::new("tests/data/ty/uses_defs.py")).unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(code(&diagnostics[0]), "invalid-argument-type");
    assert_eq!(row_col(&diagnostics[0]), (3, 8));
    assert!(
        diagnostics[0].message.contains("`add`"),
        "{:?}",
        diagnostics[0].message
    );
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
        ("uses_defs.py", &["invalid-argument-type"]),
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

// ---- hardening + containment pins (mirror tests/analysis.rs) ----

/// A deeply nested but valid `.py` must not take the server down: ty's
/// semantic-index builder (ty_python_core) recurses unguarded — unlike ruff's
/// parser, which grows its own stack via stacker — so a ~9k-term expression
/// aborts the whole process (SIGSEGV, not a catchable panic) during inference.
/// The check runs under crate::stack::guarded_walk; this pins that guard with
/// the same subprocess shape as the ruff and marimo deep-nesting tests: the
/// child does the dangerous work in-process on an ordinary libtest thread (the
/// same shape as a tokio worker), the parent only asserts the child survived.
///
/// The default is 100k terms, matching ruff's test: `check_snapshot` grows the
/// stack for the whole check including the salsa db's recursive AST drop (the
/// F5-class hazard src/stack.rs documents), and `check_file` grows a larger
/// segment for the semantic-index/inference recursion — so a deeply nested yet
/// valid expression survives instead of aborting.
#[test]
fn deeply_nested_python_does_not_crash_ty() {
    const CHILD_ENV: &str = "TPMCP_TEST_DEEP_TY";
    const TERMS_ENV: &str = "TPMCP_TEST_DEEP_TERMS";
    if std::env::var(CHILD_ENV).is_ok() {
        let terms: usize = std::env::var(TERMS_ENV)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(100_000)
            .max(2);
        let dir = temp_dir("deep-ty");
        let path = dir.join("deep.py");
        let body = format!("x = {}1\n", "1+".repeat(terms - 1));
        std::fs::write(&path, &body).unwrap();
        eprintln!("stage: checking ({terms} terms)");
        let diagnostics = ty::check_path(&path).unwrap();
        eprintln!("stage: checked, {} diagnostics", diagnostics.len());
        let _ = std::fs::remove_dir_all(&dir);
        eprintln!("deep-ty child survived {terms} terms");
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let output = std::process::Command::new(exe)
        .arg("--exact")
        .arg("deeply_nested_python_does_not_crash_ty")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .output()
        .unwrap_or_else(|error| panic!("spawn deep-ty child: {error}"));
    assert!(
        output.status.success(),
        "deep nesting killed the ty child (exit {:?}); stderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// ty_check is read-only: checking never mutates the file — byte-exact no-op
/// for .py, .pyi and .ipynb — and the notebook path materializes no temp file
/// (the in-memory virtual source never touches the caller's tree).
#[test]
fn ty_check_is_byte_exact_noop() {
    let notebook = serde_json::to_string(&json!({
        "cells": [{"cell_type": "code", "execution_count": null, "id": "a",
                   "metadata": {}, "outputs": [], "source": ["y: int = \"oops\"\n"]}],
        "metadata": {}, "nbformat": 4, "nbformat_minor": 5
    }))
    .unwrap();
    let cases: [(&str, &str); 3] = [
        ("plain.py", "x: int = 1\n"),
        ("stub.pyi", "def f() -> int: ...\n"),
        ("nb.ipynb", &notebook),
    ];
    for (name, content) in cases {
        let dir = temp_dir("ty-noop");
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        let before = std::fs::read(&path).unwrap();
        let _ = ty::check_path(&path);
        let after = std::fs::read(&path).unwrap();
        assert_eq!(before, after, "{name} changed by a read-only check");
        let leaked: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with(".tpmcp_ty_check_"))
            .collect();
        assert!(leaked.is_empty(), "notebook temp file leaked: {leaked:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// Empty and invalid-syntax inputs resolve cleanly: an empty file reports no
/// diagnostics and a syntax error reports diagnostics (not an Err, not a
/// crash).
#[test]
fn empty_and_invalid_syntax_are_clean() {
    let dir = temp_dir("ty-edge");
    let empty = dir.join("empty.py");
    std::fs::write(&empty, "").unwrap();
    assert_eq!(ty::check_path(&empty).unwrap().len(), 0);
    let bad = dir.join("bad.py");
    std::fs::write(&bad, "def f(:\n").unwrap();
    // invalid-syntax diagnostics, not a crash and not an Err.
    assert!(!ty::check_path(&bad).unwrap().is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A non-Python path is a caller error, not a green "no issues found" —
/// matching ruff's message/shape so the two checkers never disagree.
#[test]
fn non_python_path_is_invalid_input() {
    let dir = temp_dir("ty-nonpy");
    let path = dir.join("notes.md");
    std::fs::write(&path, "# not python\n").unwrap();
    let error = ty::check_path(&path).unwrap_err();
    assert_eq!(error.kind(), "invalid_input");
    assert!(
        error.to_string().contains("not a Python file or notebook"),
        "{error}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Cache-invalidation tripwire: the settings cache shares the discovery result
/// but never the file bytes. Write a clean assignment, check (this populates
/// the per-root discovery cache), overwrite with a type error, then re-check —
/// the second check reuses the cached [`ProgramSettings`] yet must still read
/// the file's fresh bytes and report `invalid-assignment`.
#[test]
fn cached_settings_still_read_fresh_bytes() {
    let dir = temp_dir("ty-cache-fresh");
    let path = dir.join("fresh.py");
    std::fs::write(&path, "x: int = 1\n").unwrap();
    assert!(ty::check_path(&path).unwrap().is_empty());
    std::fs::write(&path, "x: int = \"oops\"\n").unwrap();
    let diagnostics = ty::check_path(&path).unwrap();
    assert_eq!(
        codes(&diagnostics),
        vec!["invalid-assignment".to_string()],
        "a cached-settings check must read the file's fresh bytes"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
