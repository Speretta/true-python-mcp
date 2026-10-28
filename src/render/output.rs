//! Rendering of cell outputs: the one-line summary (`with_outputs =
//! summary`) and the full payload views (`with_outputs = full`) with text
//! inline, binaries described by size and ANSI-stripped tracebacks.

// As in the parent module, `fmt::Write for String` is infallible, so every
// `write!` result is discarded with `let _ =`.
use std::fmt::Write as _;

use serde_json::Value;

use super::{Detail, WithOutputs};
use crate::notebook::{Cell, MimePart, OutputView};

/// Maximum bytes shown per output payload in full mode; truncation backs off
/// to a char boundary.
const FULL_OUTPUT_BYTES: usize = 4000;

/// Append the cell's output section to `out`, if it has one to render.
/// `output_limit` caps each rendered payload in full mode (bytes); `None`
/// keeps the default cap.
pub(super) fn write_outputs(
    out: &mut String,
    cell: &Cell,
    detail: Detail,
    with_outputs: WithOutputs,
    output_limit: Option<usize>,
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
                let _ = writeln!(
                    out,
                    "  [{position}] {}",
                    render_output_full(output, detail, output_limit)
                );
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
    let view = OutputView::new(output);
    let kind = view.kind();
    match view.data_mimes() {
        Some(mimes) => format!("{kind}[{}]", mimes.join(",")),
        None => match view.name() {
            Some(name) => format!("{kind}({name})"),
            None => kind.to_string(),
        },
    }
}

/// Append the cell's outputs to a markdown export: every output rendered
/// as a blockquote; binary MIME parts are reported as warnings (their
/// `<N bytes>` placeholder in the rendering is the only trace left).
pub(super) fn export_outputs(
    out: &mut String,
    index: usize,
    cell: &Cell,
    warnings: &mut Vec<String>,
) {
    let outputs = cell.outputs();
    if outputs.is_empty() {
        return;
    }
    let mut binaries = Vec::new();
    let mut rendered = String::new();
    for output in outputs {
        let view = OutputView::new(output);
        binaries.extend(view.mime_parts().into_iter().filter_map(|part| match part {
            MimePart::Binary { mime, .. } => Some(mime),
            MimePart::Plain(_) => None,
        }));
        rendered.push_str(&render_output_full(output, Detail::Full, None));
        rendered.push('\n');
    }
    if !binaries.is_empty() {
        warnings.push(format!(
            "cell {index}: binary outputs not exported ({})",
            binaries.join(", ")
        ));
    }
    out.push_str("\noutputs:\n");
    for line in rendered.trim_end().lines() {
        let _ = writeln!(out, "> {line}");
    }
    out.push('\n');
}

fn render_output_full(output: &Value, detail: Detail, output_limit: Option<usize>) -> String {
    let view = OutputView::new(output);
    let kind = view.kind();
    match kind {
        "stream" => format!(
            "stream({}):\n{}",
            view.stream_name(),
            indent(&truncate(&view.text(), detail, output_limit))
        ),
        "error" => {
            let (ename, evalue) = view.error_identity().unwrap_or_default();
            format!(
                "error: {ename}: {evalue}\n{}",
                indent(&truncate(&view.traceback_display(), detail, output_limit))
            )
        }
        _ => {
            // execute_result / display_data: prefer text/plain, describe binaries.
            let parts: Vec<String> = view
                .mime_parts()
                .into_iter()
                .map(|part| match part {
                    MimePart::Plain(text) => {
                        format!(
                            "text/plain:\n{}",
                            indent(&truncate(&text, detail, output_limit))
                        )
                    }
                    MimePart::Binary { mime, bytes } => format!("{mime}: <{bytes} bytes>"),
                })
                .collect();
            format!("{kind} {}\n", parts.join(" | "))
        }
    }
}

fn truncate(text: &str, detail: Detail, output_limit: Option<usize>) -> String {
    let cap = output_limit.unwrap_or(FULL_OUTPUT_BYTES);
    match detail {
        Detail::Full if text.len() <= cap => text.to_string(),
        Detail::Full => format!(
            "{}\n... truncated ({} bytes total; raise output_limit to see more)",
            &text[..text.floor_char_boundary(cap)],
            text.len()
        ),
        // Outline never renders output payloads; group it with brief.
        Detail::Brief | Detail::Outline => {
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
