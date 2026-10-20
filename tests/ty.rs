//! `ty_check` tool tests: Astral's type checker compiled in, read-only.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::{server::Server, temp_dir};
use serde_json::{Value, json};

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
