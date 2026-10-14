//! Notebook validation tests: the Jupyter schema subset (with a cross-check
//! against the real nbformat validator) and notebook-level finding shapes.
//! marimo graph findings live in tests/marimo.rs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::{ground_truth_python, run_python};
use tpmcp::notebook::{Notebook, Severity, open};

fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("cannot read {path}: {error}"))
}

fn codes(content: &str) -> Vec<(Severity, Option<usize>, String)> {
    open(content)
        .unwrap()
        .check()
        .into_iter()
        .map(|finding| (finding.severity, finding.cell, finding.code))
        .collect()
}

#[test]
fn schema_issues_are_reported() {
    let findings = codes(&read("tests/data/schema_issues.ipynb"));
    use Severity::{Error, Warning};
    assert_eq!(
        findings,
        vec![
            (Error, Some(1), "cell-id".to_string()),
            (Error, Some(1), "outputs".to_string()),
            (Error, Some(2), "duplicate-cell-id".to_string()),
            (Error, Some(2), "outputs".to_string()),
            (Error, Some(2), "execution-count".to_string()),
            (Error, Some(3), "non-code-fields".to_string()),
            (Error, Some(3), "non-code-fields".to_string()),
            (Warning, Some(4), "conflict-marker".to_string()),
            (Error, Some(5), "cell-type".to_string()),
        ],
        "{findings:?}"
    );
}

#[test]
fn clean_fixtures_have_no_findings() {
    for name in [
        "lint_sample.ipynb",
        "nbformat_empty.ipynb",
        "nbformat_minor4.ipynb",
        "nbformat_rich.ipynb",
        "noncanonical.ipynb",
        "vscode_sample.ipynb",
    ] {
        let findings = codes(&read(&format!("tests/data/{name}")));
        assert!(findings.is_empty(), "{name}: {findings:?}");
    }
}

/// The edge-forms fixture carries a deliberate unknown top-level field; it is
/// schema-invalid and reported, while its source shapes stay untouched.
#[test]
fn unknown_top_level_field_is_reported() {
    let findings = codes(&read("tests/data/edge_source_forms.ipynb"));
    assert_eq!(
        findings,
        vec![(Severity::Error, None, "unexpected-field".to_string())],
        "{findings:?}"
    );
}

/// A markdown cell whose source is only `=======` (a legal setext heading
/// underline) must not be flagged; it needs a `<<<<<<<`/`>>>>>>>` companion.
#[test]
fn setext_heading_is_not_a_conflict() {
    let notebook = r#"{
 "cells": [
  {
   "cell_type": "markdown",
   "id": "aaaa0000",
   "metadata": {},
   "source": ["Title\n", "=======\n"]
  }
 ],
 "metadata": {},
 "nbformat": 4,
 "nbformat_minor": 5
}"#;
    assert!(codes(notebook).is_empty());

    let conflicted = notebook.replace("Title\\n", "<<<<<<< HEAD\\n");
    let findings = codes(&conflicted);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].2, "conflict-marker");
}

/// nbformat 4.4 and below do not require cell ids.
#[test]
fn ids_are_only_required_from_minor_5() {
    let minor4 = read("tests/data/nbformat_minor4.ipynb");
    assert!(codes(&minor4).is_empty());
}

/// Ground truth: every fixture we flag with Errors must fail the real
/// nbformat validator, and every fixture the validator accepts must have no
/// Error findings from us.
#[test]
fn errors_agree_with_nbformat_validate() {
    let Some(py) = ground_truth_python() else {
        return;
    };
    const VALIDATE: &str = r#"
import sys
import nbformat
try:
    nb = nbformat.read(sys.argv[1], as_version=4)
    nbformat.validate(nb)
    print("valid")
except nbformat.ValidationError:
    print("invalid")
"#;
    let dir = std::env::temp_dir().join(format!("tpmcp-validate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut names = vec!["schema_issues.ipynb".to_string()];
    names.extend(
        std::fs::read_dir("tests/data")
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".ipynb") && name != "schema_issues.ipynb"),
    );
    for name in names {
        let source = std::path::Path::new("tests/data").join(&name);
        let path = dir.join(&name);
        std::fs::copy(&source, &path).unwrap();
        let verdict = run_python(&py, VALIDATE, &[path.to_str().unwrap()]);
        let has_errors = codes(&read(&format!("tests/data/{name}")))
            .iter()
            .any(|(severity, _, _)| *severity == Severity::Error);
        assert_eq!(
            verdict.trim() == "invalid",
            has_errors,
            "{name}: nbformat says {}, we say errors={has_errors}",
            verdict.trim()
        );
    }
}
