//! End-to-end tests that drive the real `tpmcp` binary over the MCP
//! stdio protocol: the verdict tools (`ruff_check`, `ruff_format`,
//! `ty_check`, `notebook_check`). Sibling of tests/tools.rs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::revision_of;
use common::{server::Server, temp_dir};
use serde_json::json;

#[test]
fn ruff_tools_over_the_wire() {
    let mut server = Server::start(&[]);
    let (err, text, sc) = server.call(
        "ruff_check",
        json!({"path": "tests/data/lint_sample.py", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["diagnostic_count"], 3);
    let codes: Vec<&str> = sc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|diagnostic| diagnostic["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, vec!["F401", "F401", "F821"]);

    // notebook lint reports 0-based cells
    let (_, _, sc) = server.call(
        "ruff_check",
        json!({"path": "tests/data/lint_sample.ipynb", "format": "json"}),
    );
    let cells: Vec<u64> = sc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|diagnostic| diagnostic["cell"].as_u64().unwrap())
        .collect();
    assert_eq!(cells, vec![1, 2], "0-based cell indexes");
}

#[test]
fn ty_check_over_the_wire() {
    let mut server = Server::start(&[]);
    let (err, text, sc) = server.call(
        "ty_check",
        json!({"path": "tests/data/ty/bad.py", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["diagnostic_count"], 3);
    let codes: Vec<&str> = sc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|diagnostic| diagnostic["code"].as_str().unwrap())
        .collect();
    assert_eq!(
        codes,
        vec![
            "invalid-assignment",
            "invalid-return-type",
            "invalid-argument-type"
        ]
    );

    // notebook diagnostics carry a 0-based cell index
    let (_, _, sc) = server.call(
        "ty_check",
        json!({"path": "tests/data/ty/cross_cell.ipynb", "format": "json"}),
    );
    let cells: Vec<u64> = sc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|diagnostic| diagnostic["cell"].as_u64().unwrap())
        .collect();
    assert_eq!(cells, vec![2], "0-based cell indexes");
}

#[test]
fn ruff_revision_chains_into_fix_and_format() {
    let dir = temp_dir("ruffchain");
    let path = dir.join("sample.py");
    std::fs::copy("tests/data/lint_sample.py", &path).unwrap();

    let mut server = Server::start(&[]);
    // A plain check reports the current revision in both formats...
    let (err, text, _) = server.call("ruff_check", json!({"path": path}));
    assert!(!err, "{text}");
    let revision = revision_of(&text);
    assert_eq!(revision.len(), 16, "{text}");
    let (_, _, sc) = server.call("ruff_check", json!({"path": path, "format": "json"}));
    assert_eq!(sc["revision"].as_str().unwrap(), revision);

    // ...so a fix can chain straight onto it without another read.
    let (err, text, _) = server.call(
        "ruff_check",
        json!({"path": path, "fix": true, "revision": revision}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("applied automatic fixes"), "{text}");
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(!fixed.contains("import os"), "{fixed}");
    assert!(
        fixed.contains("return undefined_var"),
        "F821 stays: {fixed}"
    );

    // A format dry run reports the revision for the in-place call.
    let (_, text, _) = server.call("ruff_format", json!({"path": path, "check": true}));
    assert!(text.contains("revision: "), "{text}");
    let revision = revision_of(&text);
    let (err, text, _) = server.call("ruff_format", json!({"path": path, "revision": revision}));
    assert!(!err, "{text}");
    assert!(
        text.contains("formatted") || text.contains("already formatted"),
        "{text}"
    );
}

/// ruff_check fix=true and ruff_format on a notebook at the wire layer: a
/// plain check reports the revision and both diagnostics, the fix chains onto
/// that revision (F401 removed, F821 stays, 0-based cell retained), and a
/// format dry run reports the notebook's revision without a text diff.
#[test]
fn ruff_notebook_fix_and_format_over_the_wire() {
    let dir = temp_dir("ruff-nb");
    let path = dir.join("lint_sample.ipynb");
    std::fs::copy("tests/data/lint_sample.ipynb", &path).unwrap();
    // Mirror the fixture's governing config (select=["F"]) so the temp copy
    // lints identically to the in-tree notebook.
    std::fs::copy("tests/data/ruff.toml", dir.join("ruff.toml")).unwrap();

    let mut server = Server::start(&[]);

    // Plain check: two diagnostics (F401 + F821) and a chaining revision.
    let (err, _, sc) = server.call("ruff_check", json!({"path": path, "format": "json"}));
    assert!(!err, "{sc}");
    assert_eq!(sc["diagnostic_count"], 2);
    let revision = sc["revision"].as_str().unwrap().to_string();

    // fix=true applies the safe F401 fix; the F821 stays, still cell-mapped.
    let (err, text, sc) = server.call(
        "ruff_check",
        json!({"path": path, "fix": true, "revision": revision, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["changed"], true);
    assert_ne!(sc["revision"].as_str().unwrap(), revision);
    assert_eq!(sc["diagnostic_count"], 1);
    assert_eq!(sc["diagnostics"][0]["code"], "F821");
    assert_eq!(sc["diagnostics"][0]["cell"], 2);
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(!fixed.contains("import os"), "F401 was fixed: {fixed}");

    // ruff_format dry run on the notebook: revision present, no text diff.
    let (err, _, sc) = server.call(
        "ruff_format",
        json!({"path": path, "check": true, "format": "json"}),
    );
    assert!(!err, "{sc}");
    assert!(sc["revision"].is_string(), "{sc}");
    let status = sc["status"].as_str().unwrap();
    assert!(status == "unchanged" || status == "would_reformat", "{sc}");
    assert!(
        sc["diff"].is_null(),
        "notebooks carry no plain-text diff: {sc}"
    );
}

/// notebook_check over the wire: JSON and markdown rendering, revision
/// chaining, marimo graph findings, and error propagation.
#[test]
fn notebook_check_over_the_wire() {
    let mut server = Server::start(&[]);

    // Jupyter schema issues (8 errors + 1 warning), with a revision that
    // matches notebook_read for chaining.
    let (err, _, read_sc) = server.call(
        "notebook_read",
        json!({"path": "tests/data/schema_issues.ipynb", "format": "json"}),
    );
    assert!(!err);
    let (err, text, sc) = server.call(
        "notebook_check",
        json!({"path": "tests/data/schema_issues.ipynb", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["revision"], read_sc["revision"]);
    assert_eq!(sc["error_count"].as_u64(), Some(8), "{sc}");
    assert_eq!(sc["warning_count"].as_u64(), Some(1), "{sc}");
    assert_eq!(sc["info_count"].as_u64(), Some(0), "{sc}");
    let findings = sc["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 9, "{sc}");
    assert_eq!(findings[0]["code"], "cell-id");
    assert_eq!(findings[0]["cell"], 1);
    assert_eq!(findings[7]["code"], "conflict-marker");
    assert_eq!(findings[7]["severity"], "warning");

    // Markdown rendering
    let (err, text, _) = server.call(
        "notebook_check",
        json!({"path": "tests/data/schema_issues.ipynb"}),
    );
    assert!(!err);
    assert!(
        text.contains("9 finding(s): 8 error(s), 1 warning(s), 0 info"),
        "{text}"
    );
    assert!(text.contains("- error cell 1 `cell-id`:"), "{text}");
    assert!(
        text.contains("- warning cell 4 `conflict-marker`:"),
        "{text}"
    );

    // Clean notebook
    let (err, _, sc) = server.call(
        "notebook_check",
        json!({"path": "tests/data/vscode_sample.ipynb", "format": "json"}),
    );
    assert!(!err);
    assert_eq!(sc["findings"].as_array().unwrap().len(), 0, "{sc}");

    // A clean notebook is described as passing *this* check (a documented
    // subset), never overclaimed as "valid".
    let (err, text, _) = server.call(
        "notebook_check",
        json!({"path": "tests/data/vscode_sample.ipynb"}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("passes this check"), "{text}");
    assert!(!text.contains("is valid"), "{text}");

    // marimo graph findings flow through the same tool
    let (err, _, sc) = server.call(
        "notebook_check",
        json!({"path": "tests/data/marimo/graph_cycle.py", "format": "json"}),
    );
    assert!(!err);
    assert_eq!(sc["error_count"].as_u64(), Some(1), "{sc}");
    assert_eq!(sc["findings"][0]["code"], "cycle");

    // A missing file is a tool error
    let (err, text, _) = server.call("notebook_check", json!({"path": "nope_missing.ipynb"}));
    assert!(err, "{text}");
}
