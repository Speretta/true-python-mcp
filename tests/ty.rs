//! `ty_check` tool tests: Astral's type checker compiled in, read-only.
//! The tool surface over its diagnostics and disclosures.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::{server::Server, temp_dir};
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

/// `ty_check` discloses the environment it targeted — which Python version,
/// where it was measured, and the interpreter prefix — instead of silently
/// assuming a version. A measured version carries no note; an unmeasurable one
/// carries an honest `default fallback` note and `version_source: "unknown"`.
#[test]
fn ty_check_discloses_python_target() {
    let dir = temp_dir("ty-check-disclose");
    let path = dir.join("good.py");
    std::fs::write(&path, "x: int = 1\n").unwrap();
    let mut server = Server::start(&[]);

    let (err, text, sc) = server.call("ty_check", json!({"path": path, "format": "json"}));
    assert!(!err, "{text}");
    let version = sc["python_version"]
        .as_str()
        .unwrap_or_else(|| panic!("python_version disclosed: {sc}"));
    assert!(
        version.split('.').count() == 2,
        "version should be major.minor: {sc}"
    );
    let source = sc["version_source"]
        .as_str()
        .unwrap_or_else(|| panic!("version_source disclosed: {sc}"));
    assert!(!source.is_empty(), "version_source must not be empty: {sc}");
    assert!(
        sc["environment"].is_string() || sc["environment"].is_null(),
        "environment is a path or null: {sc}"
    );
    if source == "unknown" {
        let note = sc["note"]
            .as_str()
            .unwrap_or_else(|| panic!("unknown version must carry a note: {sc}"));
        assert!(note.contains("default fallback"), "{note}");
        assert!(note.contains("not measured"), "{note}");
    } else {
        assert!(
            sc["note"].is_null(),
            "measured version must not carry a note: {sc}"
        );
        assert!(
            sc["environment"].is_string(),
            "measured version implies a discovered environment: {sc}"
        );
    }

    // Markdown rendering carries the same disclosure.
    let (err, text, _) = server.call("ty_check", json!({"path": path}));
    assert!(!err, "{text}");
    for label in ["python_version:", "version_source:", "environment:"] {
        assert!(text.contains(label), "markdown missing {label}: {text}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `ty_check` also discloses which rule config it ran under and what in it
/// was not applied: the config file's path (or an explicit "ty defaults"),
/// one note per ignored key, and one per rule name ty would not apply.
#[test]
fn ty_check_discloses_the_rule_config() {
    let dir = temp_dir("ty-check-rules-disclose");
    let path = dir.join("probe.py");
    std::fs::write(&path, "import definitely_not_a_module_xyz\n").unwrap();
    std::fs::write(
        dir.join("ty.toml"),
        "[tool.nonsense]\nx = 1\n[rules]\nunresolved-import = \"ignore\"\nsome-unknown-rule = \"warn\"\n",
    )
    .unwrap();
    let mut server = Server::start(&[]);

    let (err, text, sc) = server.call("ty_check", json!({"path": path, "format": "json"}));
    assert!(!err, "{text}");
    let config = sc["rules_config"]
        .as_str()
        .unwrap_or_else(|| panic!("rules_config disclosed: {sc}"));
    assert!(
        config.ends_with("ty.toml"),
        "the config file is named: {sc}"
    );
    assert_eq!(
        sc["diagnostic_count"], 0,
        "the ignored rule must stay silent: {sc}"
    );
    let notes = sc["config_notes"]
        .as_array()
        .unwrap_or_else(|| panic!("config_notes disclosed: {sc}"));
    let joined = notes
        .iter()
        .filter_map(|note| note.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("`tool`"), "an ignored key is named: {sc}");
    assert!(
        joined.contains("some-unknown-rule"),
        "an unusable rule is named: {sc}"
    );

    // Markdown carries the same disclosure.
    let (err, text, _) = server.call("ty_check", json!({"path": path}));
    assert!(!err, "{text}");
    assert!(text.contains("rules_config:"), "{text}");
    assert!(text.contains("some-unknown-rule"), "{text}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `version_info` is the library surface of the same disclosure.
#[test]
fn version_info_reports_the_targeted_environment() {
    let dir = temp_dir("ty-version-info");
    let target = ty::version_info(&dir, None).unwrap();
    assert!(
        target.python_version.split('.').count() == 2,
        "version should be major.minor: {}",
        target.python_version
    );
    assert!(!target.version_source.is_empty());
    if target.version_source == "unknown" {
        let note = target
            .note
            .as_deref()
            .expect("unknown version must carry a note");
        assert!(note.contains("default fallback"), "{note}");
        assert!(note.contains("not measured"), "{note}");
    } else {
        assert!(
            target.note.is_none(),
            "measured version must not carry a note: {:?}",
            target.note
        );
        assert!(
            target.environment.is_some(),
            "measured version implies a discovered environment"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
