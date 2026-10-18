//! marimo reactive-graph validation tests: findings on the crafted graph
//! fixtures, agreement with marimo's own validator on the corpus, and the
//! deep-nesting stacker guard. Core round-trip/mutation tests live in
//! tests/marimo.rs, markdown cells in tests/marimo_markdown.rs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::{ground_truth_python, marimo_fixture, run_python, synthetic_marimo};
use tpmcp::notebook::{Notebook, open};

/// Ground truth: marimo's own graph validator over the same fixtures.
const GRAPH_CHECK: &str = r#"
import sys
from pathlib import Path
from marimo._ast.parse import parse_notebook
from marimo._ast.compiler import compile_cell
from marimo._ast.names import SETUP_CELL_NAME
from marimo._runtime.dataflow import DirectedGraph
from marimo._lint.validate_graph import check_for_errors
from marimo._types.ids import CellId_t

path = Path(sys.argv[1])
nb = parse_notebook(path.read_text(), str(path))
graph = DirectedGraph()
for i, cell in enumerate(nb.cells):
    kind = type(cell).__name__
    if kind == "UnparsableCell":
        continue
    cid = SETUP_CELL_NAME if kind == "SetupCell" else str(i)
    try:
        impl = compile_cell(cell.code, cid)
    except SyntaxError:
        continue
    graph.register_cell(CellId_t(cid), impl)
for cid, errors in sorted(check_for_errors(graph).items()):
    for error in errors:
        print(cid, type(error).__name__)
"#;

fn codes(findings: &[tpmcp::notebook::Finding]) -> Vec<(&str, Option<usize>)> {
    findings
        .iter()
        .map(|finding| (finding.code.as_str(), finding.cell))
        .collect()
}

#[test]
fn check_flags_cycles() {
    let notebook = open(&marimo_fixture("graph_cycle.py")).unwrap();
    let findings = notebook.check();
    assert_eq!(codes(&findings), vec![("cycle", Some(0))], "{findings:?}");
    assert!(findings[0].message.contains("0, 1"), "{findings:?}");
    assert!(findings[0].message.contains('x'), "{findings:?}");
}

#[test]
fn check_flags_multiple_definitions() {
    let notebook = open(&marimo_fixture("graph_dupedef.py")).unwrap();
    let findings = notebook.check();
    assert_eq!(
        codes(&findings),
        vec![("multiple-definitions", Some(0))],
        "{findings:?}"
    );
    assert!(findings[0].message.contains("`config`"), "{findings:?}");
    assert!(findings[0].message.contains("0, 1"), "{findings:?}");
}

#[test]
fn check_flags_setup_root() {
    let notebook = open(&marimo_fixture("graph_setup_root.py")).unwrap();
    let findings = notebook.check();
    assert_eq!(
        codes(&findings),
        vec![("setup-root", Some(0))],
        "{findings:?}"
    );
    assert!(findings[0].message.contains("payload"), "{findings:?}");
}

#[test]
fn check_reports_stale_drift_as_info() {
    let notebook = open(&marimo_fixture("graph_stale.py")).unwrap();
    let findings = notebook.check();
    assert_eq!(
        codes(&findings),
        vec![
            ("stale-return", Some(0)),
            ("stale-return", Some(1)),
            ("stale-return", Some(2)),
            ("stale-return", Some(3)),
            ("stale-signature", Some(3)),
        ],
        "{findings:?}"
    );
    use tpmcp::notebook::Severity;
    assert!(findings.iter().all(|f| f.severity == Severity::Info));
}

#[test]
fn check_is_clean_on_valid_fixtures() {
    for name in [
        "emoji.py",
        "toplevel.py",
        "generate.py",
        "empty.py",
        "no_cells.py",
    ] {
        let notebook = open(&marimo_fixture(name)).unwrap();
        assert_eq!(notebook.check(), Vec::new(), "{name} should be clean");
    }
    // broken.py: the unparsable region is one syntax finding; the intact
    // cell after it is still analyzed (its stored return is stale drift).
    let notebook = open(&marimo_fixture("broken.py")).unwrap();
    let findings = notebook.check();
    assert_eq!(
        codes(&findings),
        vec![("syntax-error", Some(1)), ("stale-return", Some(2))],
        "{findings:?}"
    );
}

/// Cross-check the Error-severity findings against marimo's own graph
/// validator (real marimo, real DirectedGraph, real check_for_errors).
#[test]
fn check_matches_real_marimo_graph_errors() {
    let Some(py) = ground_truth_python() else {
        return;
    };
    let fixtures = [
        "graph_cycle.py",
        "graph_dupedef.py",
        "graph_setup_root.py",
        "graph_stale.py",
        "emoji.py",
        "toplevel.py",
        "generate.py",
    ];
    let dir = std::env::temp_dir().join(format!("tpmcp-graph-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for name in fixtures {
        let path = dir.join(name);
        std::fs::write(&path, marimo_fixture(name)).unwrap();
        let output = run_python(&py, GRAPH_CHECK, &[path.to_str().unwrap()]);
        let mut marimo_errors: Vec<(String, String)> = output
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                let (cid, kind) = line.split_once(' ').unwrap();
                (cid.to_string(), kind.to_string())
            })
            .collect();
        marimo_errors.sort();

        let notebook = open(&marimo_fixture(name)).unwrap();
        let mut ours: Vec<(String, String)> = notebook
            .check()
            .iter()
            .filter(|finding| finding.severity == tpmcp::notebook::Severity::Error)
            .flat_map(|finding| {
                let mapped = match finding.code.as_str() {
                    "cycle" => "CycleError",
                    "multiple-definitions" => "MultipleDefinitionError",
                    "setup-root" => "SetupRootError",
                    "syntax-error" => "SyntaxError",
                    other => other,
                };
                // Our finding lists every involved cell in the message; marimo
                // repeats one error per involved cell. Expand ours the same
                // way by matching message cell lists. The setup-root error is
                // reported on the setup cell, which marimo ids as "setup".
                if mapped == "SetupRootError" {
                    return vec![("setup".to_string(), mapped.to_string())];
                }
                let cells: Vec<String> = message_cells(&finding.message);
                if cells.is_empty() {
                    vec![(finding.cell.unwrap().to_string(), mapped.to_string())]
                } else {
                    cells.into_iter().map(|c| (c, mapped.to_string())).collect()
                }
            })
            .collect();
        ours.sort();
        assert_eq!(ours, marimo_errors, "{name}");
    }
}

/// F5 regression: deeply nested Python in a marimo cell (a 200k-term sum —
/// ruff's stacker-guarded parser handles it, then the scope analysis walks
/// the expression tree) must not overflow the stack and SIGSEGV the whole
/// server during notebook_check. The work runs in a CHILD process so a stack
/// overflow can never kill the test runner; the child does it in-process on
/// an ordinary libtest thread (2 MiB stack, the same shape as the tokio
/// workers the server runs tools on), so passing demonstrates the
/// stacker::maybe_grow guard works without any thread-spawning tricks.
/// The child's term count is overridable via TPMCP_TEST_DEEP_TERMS for
/// threshold measurements.
#[test]
fn deeply_nested_marimo_cell_does_not_crash() {
    const CHILD_ENV: &str = "TPMCP_TEST_DEEP_NESTING";
    const TERMS_ENV: &str = "TPMCP_TEST_DEEP_TERMS";
    if std::env::var(CHILD_ENV).is_ok() {
        let terms: usize = std::env::var(TERMS_ENV)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(200_000)
            .max(2);
        let body = format!("x = {}1", "1+".repeat(terms - 1));
        let src = synthetic_marimo(&[&body]);
        eprintln!("stage: opening ({terms} terms)");
        let notebook = open(&src).unwrap_or_else(|error| panic!("deep cell must parse: {error}"));
        eprintln!("stage: opened, checking");
        let findings = notebook.check();
        eprintln!("stage: checked");
        eprintln!(
            "deep-nesting child survived {terms} terms, {} findings",
            findings.len()
        );
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let output = std::process::Command::new(exe)
        .arg("--exact")
        .arg("deeply_nested_marimo_cell_does_not_crash")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .output()
        .unwrap_or_else(|error| panic!("spawn deep-nesting child: {error}"));
    assert!(
        output.status.success(),
        "deep nesting killed the analysis child (exit {:?}); stderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Cell indexes mentioned in a finding message ("cells [0, 1]", "(0, 1)").
fn message_cells(message: &str) -> Vec<String> {
    let mut cells = Vec::new();
    for open in ['[', '('] {
        if let Some(start) = message.find(open)
            && let Some(end) = message[start..].find(if open == '[' { ']' } else { ')' })
        {
            let inner = &message[start + 1..start + end];
            if !inner.is_empty()
                && inner
                    .split(", ")
                    .all(|part| part.chars().all(|c| c.is_ascii_digit()))
            {
                cells.extend(inner.split(", ").map(str::to_string));
            }
        }
    }
    cells
}
