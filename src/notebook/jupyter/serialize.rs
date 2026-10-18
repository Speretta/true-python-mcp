//! Canonical `.ipynb` writer, byte-identical to `nbformat.write` and VS Code:
//! 1-space indent, byte-wise sorted keys at every level (sorted explicitly,
//! so serde_json's `preserve_order` feature cannot affect it), raw UTF-8,
//! LF, inline empty containers, one trailing newline.
//!
//! The writer emits straight from the model: typed cell fields and the
//! passthrough `extra` values are merged in sorted-key order at emit time,
//! so serialization never rebuilds a second serde_json DOM.
//!
//! Numbers in passthrough data go through serde_json: integers up to u64
//! round-trip exactly; integers beyond u64 are read as f64 and re-emitted in
//! float form (`18446744073709551616` → `1.8446744073709552e+19`); floats
//! are re-emitted in serde_json's shortest round-trip form, which matches
//! Python's `repr` for the exponential range (`1e+30` stays, `1e30`
//! normalizes once). tests/canonical_forms.rs pins this boundary.

use std::collections::BTreeMap;

use serde_json::Value;

use super::JupyterNotebook;
use crate::notebook::{Cell, SourceText};

pub fn to_canonical_string(notebook: &JupyterNotebook) -> String {
    let mut out = String::with_capacity(notebook.cells.len() * 256 + 256);
    let mut fields = extra_fields(&notebook.extra);
    if !notebook.extra.contains_key("cells") {
        fields.push(("cells", Field::Cells(&notebook.cells)));
    }
    emit_object(&mut out, &mut fields, 0);
    out.push('\n');
    out
}

/// One object field: either model-owned (cells/cell_type/source) or a
/// passthrough `extra` value.
enum Field<'a> {
    Cells(&'a [Cell]),
    CellType(&'a str),
    Source(&'a SourceText),
    Json(&'a Value),
}

/// The passthrough entries of an `extra` map as fields. A model key shadowed
/// by an `extra` entry keeps the `extra` value — the same precedence the
/// previous DOM-based writer had, where `Map::insert` of the extras replaced
/// the model value under a colliding key.
fn extra_fields(extra: &BTreeMap<String, Value>) -> Vec<(&str, Field<'_>)> {
    extra
        .iter()
        .map(|(key, value)| (key.as_str(), Field::Json(value)))
        .collect()
}

fn emit_cell(out: &mut String, cell: &Cell, depth: usize) {
    let mut fields = extra_fields(&cell.extra);
    if !cell.extra.contains_key("cell_type") {
        fields.push(("cell_type", Field::CellType(&cell.cell_type)));
    }
    if !cell.extra.contains_key("source") {
        fields.push(("source", Field::Source(&cell.source)));
    }
    emit_object(out, &mut fields, depth);
}

/// A non-empty JSON object with byte-sorted keys, laid out exactly like
/// `emit_value` does for `Value::Object` (model objects always carry at
/// least one typed key, so the inline `{}` form never applies here).
fn emit_object(out: &mut String, fields: &mut [(&str, Field)], depth: usize) {
    fields.sort_unstable_by_key(|(key, _)| *key);
    out.push_str("{\n");
    for (index, (key, field)) in fields.iter().enumerate() {
        if index > 0 {
            out.push_str(",\n");
        }
        push_indent(out, depth + 1);
        emit_string(out, key);
        out.push_str(": ");
        emit_field(out, field, depth + 1);
    }
    out.push('\n');
    push_indent(out, depth);
    out.push('}');
}

fn emit_field(out: &mut String, field: &Field, depth: usize) {
    match field {
        Field::Cells(cells) => emit_cells(out, cells, depth),
        Field::CellType(cell_type) => emit_string(out, cell_type),
        Field::Source(source) => emit_source(out, source, depth),
        Field::Json(value) => emit_value(out, value, depth),
    }
}

/// The `cells` array — byte-identical to `emit_value` over an array of the
/// same cell objects.
fn emit_cells(out: &mut String, cells: &[Cell], depth: usize) {
    if cells.is_empty() {
        out.push_str("[]");
        return;
    }
    out.push_str("[\n");
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            out.push_str(",\n");
        }
        push_indent(out, depth + 1);
        emit_cell(out, cell, depth + 1);
    }
    out.push('\n');
    push_indent(out, depth);
    out.push(']');
}

/// `source` in the shape the document stored it: one string or an array of
/// line strings (inline `[]` when empty), exactly like `emit_value`.
fn emit_source(out: &mut String, source: &SourceText, depth: usize) {
    match source {
        SourceText::Str(text) => emit_string(out, text),
        SourceText::Lines(lines) => {
            if lines.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (index, line) in lines.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                push_indent(out, depth + 1);
                emit_string(out, line);
            }
            out.push('\n');
            push_indent(out, depth);
            out.push(']');
        }
    }
}

fn emit_value(out: &mut String, value: &Value, depth: usize) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => out.push_str(&number.to_string()),
        Value::String(text) => emit_string(out, text),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                push_indent(out, depth + 1);
                emit_value(out, item, depth + 1);
            }
            out.push('\n');
            push_indent(out, depth);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            // Defensive sort: canonical bytes must not depend on serde_json's
            // map implementation (see the module docs).
            let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
            keys.sort_unstable();
            out.push_str("{\n");
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                push_indent(out, depth + 1);
                emit_string(out, key);
                out.push_str(": ");
                emit_value(out, &map[*key], depth + 1);
            }
            out.push('\n');
            push_indent(out, depth);
            out.push('}');
        }
    }
}

/// Matches Python's `json.dumps(ensure_ascii=False)` escaping exactly.
fn emit_string(out: &mut String, text: &str) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            _ => out.push(character),
        }
    }
    out.push('"');
}

fn push_indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push(' ');
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod tests {
    use super::*;

    #[test]
    fn emitter_matches_python_json_layout() {
        let value = serde_json::json!({
            "b": [1, {"z": null, "a": true}],
            "e": {},
            "ea": [],
            "s": "ünicode \"\\\u{1f}\t",
        });
        let mut out = String::new();
        emit_value(&mut out, &value, 0);
        assert_eq!(
            out,
            "{\n \"b\": [\n  1,\n  {\n   \"a\": true,\n   \"z\": null\n  }\n ],\n \
             \"e\": {},\n \"ea\": [],\n \
             \"s\": \"ünicode \\\"\\\\\\u001f\\t\"\n}"
        );
    }
}
