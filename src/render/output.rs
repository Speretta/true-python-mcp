//! Rendering of cell outputs: the one-line summary (`with_outputs =
//! summary`) and the full payload views (`with_outputs = full`) with text
//! inline, binaries described by size and ANSI-stripped tracebacks.

// As in the parent module, `fmt::Write for String` is infallible, so every
// `write!` result is discarded with `let _ =`.
use std::fmt::Write as _;

use serde_json::Value;

use super::{Detail, WithOutputs};
use crate::notebook::Cell;

/// Maximum characters shown per output payload in full mode.
const FULL_OUTPUT_CHARS: usize = 4000;

/// Append the cell's output section to `out`, if it has one to render.
pub(super) fn write_outputs(
    out: &mut String,
    cell: &Cell,
    detail: Detail,
    with_outputs: WithOutputs,
) {
    if matches!(with_outputs, WithOutputs::None) || !cell.is_code() {
        return;
    }
    let outputs = cell.outputs();
    if outputs.is_empty() {
        return;
    }
    match with_outputs {
        WithOutputs::None => {}
        WithOutputs::Summary => {
            let _ = writeln!(out, "outputs({}): {}", outputs.len(), summarize(outputs));
        }
        WithOutputs::Full => {
            let _ = writeln!(out, "outputs({}):", outputs.len());
            for (position, output) in outputs.iter().enumerate() {
                let _ = writeln!(out, "  [{position}] {}", render_output_full(output, detail));
            }
        }
    }
}

/// Comma-joined one-line summaries of `outputs`.
pub(super) fn summarize(outputs: &[Value]) -> String {
    outputs
        .iter()
        .map(output_summary)
        .collect::<Vec<_>>()
        .join(", ")
}

/// One-line summary of an output: `stream(stderr)`,
/// `execute_result[text/plain,image/png]`, `error`.
fn output_summary(output: &Value) -> String {
    let kind = output
        .get("output_type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match output.get("data") {
        Some(Value::Object(data)) => {
            let mimes: Vec<&str> = data.keys().map(String::as_str).collect();
            format!("{kind}[{}]", mimes.join(","))
        }
        _ => match output.get("name").and_then(Value::as_str) {
            Some(name) => format!("{kind}({name})"),
            None => kind.to_string(),
        },
    }
}

fn render_output_full(output: &Value, detail: Detail) -> String {
    let kind = output
        .get("output_type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match kind {
        "stream" => {
            let name = output
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("stdout");
            let text = multiline_to_string(output.get("text"));
            format!("stream({name}):\n{}", indent(&truncate(&text, detail)))
        }
        "error" => {
            let ename = output.get("ename").and_then(Value::as_str).unwrap_or("");
            let evalue = output.get("evalue").and_then(Value::as_str).unwrap_or("");
            let traceback = output
                .get("traceback")
                .and_then(Value::as_array)
                .map(|lines| {
                    lines
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            format!(
                "error: {ename}: {evalue}\n{}",
                indent(&truncate(&strip_ansi(&traceback), detail))
            )
        }
        _ => {
            // execute_result / display_data: prefer text/plain, describe binaries.
            let mut parts = Vec::new();
            if let Some(Value::Object(data)) = output.get("data") {
                for (mime, payload) in data {
                    if mime == "text/plain" {
                        parts.push(format!(
                            "{mime}:\n{}",
                            indent(&truncate(&multiline_to_string(Some(payload)), detail))
                        ));
                    } else {
                        parts.push(format!(
                            "{mime}: <{} bytes>",
                            payload.as_str().map(str::len).unwrap_or(0)
                        ));
                    }
                }
            }
            format!("{kind} {}\n", parts.join(" | "))
        }
    }
}

/// `"text"` may be a JSON string or an array of lines; normalize to one string.
fn multiline_to_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(lines)) => lines
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .concat(),
        _ => String::new(),
    }
}

fn truncate(text: &str, detail: Detail) -> String {
    match detail {
        Detail::Full if text.len() <= FULL_OUTPUT_CHARS => text.to_string(),
        Detail::Full => format!(
            "{}\n... truncated ({} chars total)",
            &text[..text.floor_char_boundary(FULL_OUTPUT_CHARS)],
            text.len()
        ),
        Detail::Brief => {
            let lines: Vec<&str> = text.lines().collect();
            let shown = lines.len().min(10);
            let mut out = lines[..shown].join("\n");
            if shown < lines.len() {
                let _ = write!(out, "\n... {} more lines", lines.len() - shown);
            }
            out
        }
    }
}

/// Prefix every line of `text` with four spaces; lines are joined exactly as
/// `str::lines` yields them (a trailing terminator does not add an empty
/// last line).
fn indent(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 4 * text.lines().count());
    for (position, line) in text.lines().enumerate() {
        if position > 0 {
            out.push('\n');
        }
        out.push_str("    ");
        out.push_str(line);
    }
    out
}

/// Remove ANSI escape sequences from tracebacks so they don't burn tokens.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            // CSI: ESC [ ... final byte in @..~
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(next) = chars.next()
                    && !('@'..='~').contains(&next)
                {}
            }
        } else {
            out.push(character);
        }
    }
    out
}
