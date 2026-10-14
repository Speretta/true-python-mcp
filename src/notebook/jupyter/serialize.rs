//! Canonical `.ipynb` writer, byte-identical to `nbformat.write` and VS Code:
//! 1-space indent, byte-wise sorted keys at every level (sorted explicitly,
//! so serde_json's `preserve_order` feature cannot affect it), raw UTF-8,
//! LF, inline empty containers, one trailing newline.
//!
//! Numbers in passthrough data go through serde_json: integers round-trip
//! exactly, exotic float exponent forms (`1e+30`) may normalize on first
//! write (`1e30`).

use serde_json::{Map, Value};

use super::JupyterNotebook;
use crate::notebook::{Cell, SourceText};

pub fn to_canonical_string(notebook: &JupyterNotebook) -> String {
    let mut out = String::with_capacity(notebook.cells.len() * 256 + 256);
    emit_value(&mut out, &to_value(notebook), 0);
    out.push('\n');
    out
}

fn to_value(notebook: &JupyterNotebook) -> Value {
    let mut map = Map::new();
    let cells: Vec<Value> = notebook.cells.iter().map(cell_to_value).collect();
    map.insert("cells".into(), Value::Array(cells));
    for (key, value) in &notebook.extra {
        map.insert(key.clone(), value.clone());
    }
    Value::Object(map)
}

fn cell_to_value(cell: &Cell) -> Value {
    let mut map = Map::new();
    map.insert("cell_type".into(), Value::String(cell.cell_type.clone()));
    let source = match &cell.source {
        SourceText::Str(text) => Value::String(text.clone()),
        SourceText::Lines(lines) => Value::Array(
            lines
                .iter()
                .map(|line| Value::String(line.clone()))
                .collect(),
        ),
    };
    map.insert("source".into(), source);
    for (key, value) in &cell.extra {
        map.insert(key.clone(), value.clone());
    }
    Value::Object(map)
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
