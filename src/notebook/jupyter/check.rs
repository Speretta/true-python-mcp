//! nbformat v4 validation: a practical schema subset plus a git
//! conflict-marker scan.
//!
//! nbformat's own validator checks the full JSON schema; we check the parts
//! that matter for notebooks an LLM edits by hand or through merges: cell
//! types, per-cell metadata and ids (4.5+), code-cell outputs and
//! execution_count shapes, output objects per `output_type`, fields that only
//! code cells may carry, and unresolved git conflict markers in sources.
//! Schema violations are Errors (the notebook fails `nbformat.validate`);
//! conflict markers are Warnings (valid schema, broken content).

use serde_json::Value;

use super::JupyterNotebook;
use crate::notebook::{Finding, Severity};

fn finding(severity: Severity, cell: Option<usize>, code: &str, message: String) -> Finding {
    Finding {
        severity,
        cell,
        code: code.to_string(),
        message,
    }
}

fn error(cell: Option<usize>, code: &str, message: String) -> Finding {
    finding(Severity::Error, cell, code, message)
}

/// `source`-like fields: a string or an array of strings.
fn is_multiline_string(value: &Value) -> bool {
    match value {
        Value::String(_) => true,
        Value::Array(items) => items.iter().all(|item| item.is_string()),
        _ => false,
    }
}

/// nbformat allows arbitrary JSON for `application/json` and
/// `application/*+json` mime keys (schema `patternProperties`).
fn is_json_mime(key: &str) -> bool {
    key == "application/json" || (key.starts_with("application/") && key.ends_with("+json"))
}

fn is_mime_bundle(value: &Value) -> bool {
    value.as_object().is_some_and(|map| {
        map.iter()
            .all(|(key, value)| is_json_mime(key) || is_multiline_string(value))
    })
}

/// Validate one entry of a code cell's `outputs` array; returns a problem
/// description when the output object violates the nbformat schema.
fn check_output(output: &Value) -> Option<String> {
    let map = output.as_object()?;
    let output_type = match map.get("output_type").and_then(Value::as_str) {
        Some(output_type) => output_type,
        None => return Some("missing \"output_type\"".to_string()),
    };
    let require = |key: &str, predicate: fn(&Value) -> bool| -> Option<String> {
        match map.get(key) {
            Some(value) if predicate(value) => None,
            Some(_) => Some(format!("\"{output_type}\" output has an invalid \"{key}\"")),
            None => Some(format!("\"{output_type}\" output is missing \"{key}\"")),
        }
    };
    match output_type {
        "stream" => {
            require("name", Value::is_string).or_else(|| require("text", is_multiline_string))
        }
        "execute_result" => require("execution_count", Value::is_u64)
            .or_else(|| require("data", is_mime_bundle))
            .or_else(|| require("metadata", Value::is_object)),
        "display_data" => {
            require("data", is_mime_bundle).or_else(|| require("metadata", Value::is_object))
        }
        "error" => require("ename", Value::is_string)
            .or_else(|| require("evalue", Value::is_string))
            .or_else(|| {
                require("traceback", |value| {
                    value
                        .as_array()
                        .is_some_and(|items| items.iter().all(|item| item.is_string()))
                })
            }),
        _ => Some(format!("invalid \"output_type\": {output_type:?}")),
    }
}

/// Git conflict markers left by a bad merge. `=======` alone is legal
/// markdown (setext heading), so it only counts alongside the other two.
fn conflict_markers(source: &str) -> Option<String> {
    let mut markers = Vec::new();
    let mut equals = false;
    for line in source.lines() {
        if line.starts_with("<<<<<<<") {
            markers.push("<<<<<<<");
        } else if line.starts_with(">>>>>>>") {
            markers.push(">>>>>>>");
        } else if line.starts_with("=======") {
            equals = true;
        }
    }
    if markers.is_empty() {
        return None;
    }
    if equals {
        markers.push("=======");
    }
    markers.sort_unstable();
    markers.dedup();
    Some(markers.join(" "))
}

pub(super) fn check(notebook: &JupyterNotebook) -> Vec<Finding> {
    let mut findings = Vec::new();

    match notebook.extra.get("metadata") {
        Some(Value::Object(_)) => {}
        Some(_) => findings.push(error(
            None,
            "notebook-metadata",
            "top-level \"metadata\" must be an object".to_string(),
        )),
        None => findings.push(error(
            None,
            "notebook-metadata",
            "missing top-level \"metadata\" object".to_string(),
        )),
    }

    // nbformat disallows unknown fields (additionalProperties: false).
    const TOP_LEVEL_FIELDS: &[&str] = &["metadata", "nbformat", "nbformat_minor"];
    for key in notebook.extra.keys() {
        if !TOP_LEVEL_FIELDS.contains(&key.as_str()) {
            findings.push(error(
                None,
                "unexpected-field",
                format!("unexpected top-level field \"{key}\" (nbformat allows only cells, metadata, nbformat, nbformat_minor)"),
            ));
        }
    }

    let requires_id = notebook.nbformat_minor() >= 5;
    let mut seen_ids: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for (index, cell) in notebook.cells.iter().enumerate() {
        let at = Some(index);
        let is_code = cell.cell_type == "code";

        if !matches!(cell.cell_type.as_str(), "code" | "markdown" | "raw") {
            findings.push(error(
                at,
                "cell-type",
                format!(
                    "invalid \"cell_type\": {:?} (expected code, markdown or raw)",
                    cell.cell_type
                ),
            ));
        }

        // Unknown cell fields (`cell_type`/`source` were split off at parse
        // time, so `extra` holds the rest). `outputs`/`execution_count` on a
        // non-code cell are flagged by the dedicated check below instead.
        let allowed: &[&str] = if is_code {
            &["id", "metadata", "outputs", "execution_count"]
        } else {
            &[
                "id",
                "metadata",
                "attachments",
                "outputs",
                "execution_count",
            ]
        };
        for key in cell.extra.keys() {
            if !allowed.contains(&key.as_str()) {
                findings.push(error(
                    at,
                    "unexpected-field",
                    format!(
                        "cell {index}: unexpected field \"{key}\" for a {} cell",
                        cell.cell_type
                    ),
                ));
            }
        }

        if let Some(attachments) = cell.extra.get("attachments")
            && !attachments.is_object()
        {
            findings.push(error(
                at,
                "attachments",
                format!("cell {index}: \"attachments\" must be an object"),
            ));
        }

        match cell.extra.get("metadata") {
            Some(Value::Object(_)) => {}
            Some(_) => findings.push(error(
                at,
                "cell-metadata",
                format!("cell {index}: \"metadata\" must be an object"),
            )),
            None => findings.push(error(
                at,
                "cell-metadata",
                format!("cell {index}: missing \"metadata\" object"),
            )),
        }

        match cell.extra.get("id") {
            Some(Value::String(id)) => {
                if let Some(first) = seen_ids.get(id) {
                    findings.push(error(
                        at,
                        "duplicate-cell-id",
                        format!("cell {index}: id \"{id}\" is already used by cell {first}"),
                    ));
                } else {
                    seen_ids.insert(id.clone(), index);
                }
            }
            Some(_) => findings.push(error(
                at,
                "cell-id",
                format!("cell {index}: \"id\" must be a string"),
            )),
            None if requires_id => findings.push(error(
                at,
                "cell-id",
                format!("cell {index}: missing \"id\" (required since nbformat 4.5)"),
            )),
            None => {}
        }

        if is_code {
            match cell.extra.get("outputs") {
                Some(Value::Array(outputs)) => {
                    for (position, output) in outputs.iter().enumerate() {
                        if !output.is_object() {
                            findings.push(error(
                                at,
                                "outputs",
                                format!("cell {index}: outputs[{position}] must be an object"),
                            ));
                        } else if let Some(problem) = check_output(output) {
                            findings.push(error(
                                at,
                                "outputs",
                                format!("cell {index}: outputs[{position}]: {problem}"),
                            ));
                        }
                    }
                }
                Some(_) => findings.push(error(
                    at,
                    "outputs",
                    format!("cell {index}: \"outputs\" must be an array"),
                )),
                None => findings.push(error(
                    at,
                    "outputs",
                    format!("cell {index}: code cell is missing \"outputs\""),
                )),
            }
            match cell.extra.get("execution_count") {
                Some(Value::Null) | Some(Value::Number(_)) => {}
                Some(_) => findings.push(error(
                    at,
                    "execution-count",
                    format!("cell {index}: \"execution_count\" must be an integer or null"),
                )),
                None => findings.push(error(
                    at,
                    "execution-count",
                    format!("cell {index}: code cell is missing \"execution_count\""),
                )),
            }
        } else {
            for key in ["outputs", "execution_count"] {
                if cell.extra.contains_key(key) {
                    findings.push(error(
                        at,
                        "non-code-fields",
                        format!(
                            "cell {index}: {} cells must not have \"{key}\"",
                            cell.cell_type
                        ),
                    ));
                }
            }
        }

        if let Some(markers) = conflict_markers(&cell.source.text()) {
            findings.push(finding(
                Severity::Warning,
                at,
                "conflict-marker",
                format!("cell {index}: source contains git conflict markers ({markers})"),
            ));
        }
    }

    findings
}
