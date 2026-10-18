//! The diagnostics renderers shared by `notebook_check`, `ruff_check` and
//! the `notebook_read` diagnostics view: one finding bullet, one lint line,
//! and the section `notebook_read` appends to its content views.

use std::fmt::Write as _;

use serde_json::Value;

use crate::notebook::{Finding, Severity};
use crate::ruff::Diagnostic;

/// The lint half of a diagnostics view.
pub enum Lint {
    /// `diagnostics=check`: no lint pass was run.
    NotRequested,
    Issues(Vec<Diagnostic>),
    /// ruff refused or failed to lint this file (e.g. its notebook parser is
    /// stricter than ours). The read still succeeds; the reason renders in
    /// place of the issues.
    Unavailable(String),
}

/// The diagnostics section `notebook_read` appends when validation was
/// requested: `findings` are structural (notebook_check's list), `lint`
/// carries the ruff pass or why there is none.
pub fn diagnostics_markdown(findings: &[Finding], lint: &Lint) -> String {
    let mut out = format!("\ndiagnostics: {} finding(s)", findings.len());
    match lint {
        Lint::NotRequested => {}
        Lint::Issues(diagnostics) => {
            let _ = write!(out, ", {} lint issue(s)", diagnostics.len());
        }
        Lint::Unavailable(_) => out.push_str(", lint unavailable"),
    }
    out.push('\n');
    match lint {
        Lint::Issues(diagnostics) if findings.is_empty() && diagnostics.is_empty() => {
            out.push_str("no findings: the notebook is valid\n");
            return out;
        }
        Lint::NotRequested if findings.is_empty() => {
            out.push_str("no findings: the notebook is valid\n");
            return out;
        }
        Lint::Unavailable(reason) => {
            let _ = writeln!(out, "\n- lint unavailable: {reason}");
        }
        _ => {}
    }
    for finding in findings {
        let _ = writeln!(out, "\n{}", finding_line(finding));
    }
    if let Lint::Issues(diagnostics) = lint {
        for diagnostic in diagnostics {
            let _ = writeln!(out, "\n- lint {}", diagnostic_line(diagnostic));
        }
    }
    out
}

/// The JSON shape of the lint half: nothing for `check` alone, the
/// diagnostics array, or the `lint_error` reason.
pub fn lint_json(lint: &Lint, value: &mut Value) {
    match lint {
        Lint::NotRequested => {}
        Lint::Issues(diagnostics) => value["lint"] = serde_json::json!(diagnostics),
        Lint::Unavailable(reason) => value["lint_error"] = serde_json::json!(reason),
    }
}

/// One lint diagnostic as a markdown line (shared by `ruff_check` and the
/// `notebook_read` diagnostics view).
pub fn diagnostic_line(diagnostic: &Diagnostic) -> String {
    let mut line = String::new();
    if let Some(cell) = diagnostic.cell {
        line.push_str(&format!("cell {cell} "));
    }
    if let Some(location) = &diagnostic.location {
        line.push_str(&format!("{}:{} ", location.row, location.column));
    }
    line.push_str(&format!(
        "{} {}",
        diagnostic.code.as_deref().unwrap_or("-"),
        diagnostic.message
    ));
    if let Some(fix) = &diagnostic.fix {
        line.push_str(&format!(" [fixable: {}]", fix.applicability));
    }
    line
}

/// One validation finding as a markdown bullet (shared by `notebook_check`
/// and the `notebook_read` diagnostics view).
pub fn finding_line(finding: &Finding) -> String {
    let severity = match finding.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    };
    let cell = finding
        .cell
        .map_or(String::new(), |cell| format!(" cell {cell}"));
    format!("- {severity}{cell} `{}`: {}", finding.code, finding.message)
}
