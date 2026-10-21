//! Analysis tools: `ruff_check` (lint, optionally fixing) and `ruff_format`.
//!
//! Read-only invocations need no revision. Mutating invocations (`fix=true`,
//! or formatting in place) run under the path lock, verify the caller's
//! revision, and return the new revision — same conflict-safety contract as
//! the cell tools.

use std::path::{Path, PathBuf};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::json;

use crate::error::Error;
use crate::fs;
use crate::notebook::{Finding, Notebook, Severity};
use crate::render::{self, Format};
use crate::ruff::{self, CheckOutcome, FormatWrite, SafeFixes};
use crate::server::Tpmcp;
use crate::tools::{blocking, read_notebook, respond};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RuffCheckArgs {
    /// Path to a .py/.pyi file or a .ipynb notebook.
    pub path: PathBuf,
    /// Apply safe automatic fixes in place. Requires `revision`.
    #[serde(default)]
    pub fix: bool,
    /// Current file revision; required when fix=true.
    #[serde(default)]
    pub revision: Option<String>,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RuffFormatArgs {
    /// Path to a .py/.pyi file or a .ipynb notebook.
    pub path: PathBuf,
    /// Dry run: report what would change without writing. When false
    /// (default) the file is formatted in place and `revision` is required.
    #[serde(default)]
    pub check: bool,
    /// Current file revision; required unless check=true.
    #[serde(default)]
    pub revision: Option<String>,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NotebookCheckArgs {
    /// Path to the notebook (.ipynb or marimo .py).
    pub path: PathBuf,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[tool_router(router = analysis_router, vis = "pub")]
impl Tpmcp {
    #[tool(
        title = "Ruff Check",
        description = "Lint a Python file or Jupyter notebook with ruff (uses the nearest \
                       project ruff/pyproject config, falling back to the global user config). \
                       Notebook diagnostics carry a 0-based `cell` index and a cell-relative \
                       line, so they map directly onto the cell tools. Responses report the \
                       current revision; set fix=true with that revision to apply safe fixes in \
                       place.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn ruff_check(
        &self,
        Parameters(args): Parameters<RuffCheckArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let outcome = if args.fix {
                let revision = args.revision.ok_or_else(|| {
                    Error::invalid_input("ruff_check with fix=true requires `revision`")
                })?;
                fs::with_file_lock(&args.path, || {
                    // Single read (CPU-F2): verify the caller's revision and lint
                    // the exact same bytes — strictly tighter than verify-then-
                    // reread, since the file cannot change between the two reads.
                    let snapshot = fs::read_verified(&args.path, &revision)?;
                    ruff::check(&args.path, SafeFixes::Apply, snapshot)
                })?
            } else {
                // Single read: lint these bytes and report their revision, so a
                // follow-up fix=true call chains onto the snapshot we linted.
                let snapshot = fs::read(&args.path)?;
                let current = snapshot.revision.clone();
                let mut outcome = ruff::check(&args.path, SafeFixes::ReportOnly, snapshot)?;
                outcome.new_revision = Some(current);
                outcome
            };
            Ok(respond(
                args.format,
                || check_markdown(&args.path, &outcome),
                || check_json(&args.path, &outcome),
            ))
        })
        .await
    }

    #[tool(
        title = "Ruff Format",
        description = "Format a Python file or Jupyter notebook with ruff format. Formats in \
                       place by default (requires `revision`, returns the new revision); pass \
                       check=true for a dry run that returns a diff without writing. Every \
                       response reports the revision to use next, so a dry run can chain into \
                       the in-place call. Notebook formatting is byte-exact: only reformatted \
                       cells change.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn ruff_format(
        &self,
        Parameters(args): Parameters<RuffFormatArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            // Single read (CPU-F2): read the file once, format those bytes, and
            // report the revision of that same snapshot for read-only outcomes.
            let (outcome, current_revision) = if args.check {
                let snapshot = fs::read(&args.path)?;
                let current = snapshot.revision.clone();
                let outcome = ruff::format_file(&args.path, FormatWrite::DryRun, snapshot)?;
                (outcome, Some(current))
            } else {
                let revision = args.revision.ok_or_else(|| {
                    Error::invalid_input(
                        "ruff_format (in-place) requires `revision`; use check=true for a dry run",
                    )
                })?;
                fs::with_file_lock(&args.path, || {
                    let snapshot = fs::read_verified(&args.path, &revision)?;
                    let current = snapshot.revision.clone();
                    let outcome = ruff::format_file(&args.path, FormatWrite::InPlace, snapshot)?;
                    // An in-place write carries its own new revision; otherwise
                    // report the revision of the bytes we formatted.
                    let current = match &outcome {
                        ruff::FormatOutcome::Written { .. } => None,
                        _ => Some(current),
                    };
                    Ok((outcome, current))
                })?
            };
            Ok(respond(
                args.format,
                || format_markdown(&args.path, &outcome, current_revision.as_deref()),
                || format_json(&args.path, &outcome, current_revision.as_deref()),
            ))
        })
        .await
    }

    #[tool(
        title = "Check Notebook",
        description = "Validate a notebook without modifying it. Jupyter: nbformat schema \
                       subset (cell types, ids, metadata, outputs, execution_count) plus git \
                       conflict markers. marimo: reactive-graph semantics (multiple \
                       definitions, dependency cycles, setup-cell root, syntax errors) and \
                       stale return/signature drift that marimo would regenerate. Returns \
                       findings with severity error/warning/info; no findings means the \
                       notebook is clean. Use after edits or merges before trusting a file. To \
                       get findings alongside the notebook content in one call, use \
                       notebook_read with its diagnostics parameter.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn notebook_check(
        &self,
        Parameters(args): Parameters<NotebookCheckArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let (snapshot, notebook) = read_notebook(&args.path)?;
            let findings = notebook.check();
            let label = notebook.format_label();
            Ok(respond(
                args.format,
                || notebook_check_markdown(&args.path, &label, &snapshot.revision, &findings),
                || {
                    json!({
                        "path": args.path.display().to_string(),
                        "format": label,
                        "revision": snapshot.revision,
                        "error_count": count(&findings, Severity::Error),
                        "warning_count": count(&findings, Severity::Warning),
                        "info_count": count(&findings, Severity::Info),
                        "findings": findings,
                    })
                },
            ))
        })
        .await
    }
}

// ---- rendering ----

fn check_markdown(path: &Path, outcome: &CheckOutcome) -> String {
    let fixable = outcome.fixable_count();
    let mut out = String::new();
    out.push_str(&format!("ruff check: {}\n", path.display()));
    if outcome.changed {
        out.push_str(&format!(
            "applied automatic fixes · revision: {}\n",
            outcome.new_revision.as_deref().unwrap_or("?")
        ));
    } else if let Some(revision) = &outcome.new_revision {
        out.push_str(&format!("revision: {revision}\n"));
    }
    if outcome.diagnostics.is_empty() {
        out.push_str("no issues found\n");
        return out;
    }
    out.push_str(&format!(
        "{} diagnostic(s), {fixable} fixable\n",
        outcome.diagnostics.len()
    ));
    for diagnostic in &outcome.diagnostics {
        out.push_str(&format!("\n{}", render::diagnostic_line(diagnostic)));
    }
    out.push('\n');
    out
}

fn check_json(path: &Path, outcome: &CheckOutcome) -> serde_json::Value {
    let fixable = outcome.fixable_count();
    json!({
        "path": path.display().to_string(),
        "changed": outcome.changed,
        "revision": outcome.new_revision,
        "diagnostic_count": outcome.diagnostics.len(),
        "fixable_count": fixable,
        "diagnostics": outcome.diagnostics,
    })
}

fn format_markdown(
    path: &Path,
    outcome: &ruff::FormatOutcome,
    current_revision: Option<&str>,
) -> String {
    match outcome {
        ruff::FormatOutcome::Unchanged => {
            let mut out = format!("ruff format: {}\nalready formatted\n", path.display());
            if let Some(revision) = current_revision {
                out.push_str(&format!("revision: {revision}\n"));
            }
            out
        }
        ruff::FormatOutcome::Written { new_revision } => format!(
            "ruff format: {}\nformatted · revision: {new_revision}\n",
            path.display()
        ),
        ruff::FormatOutcome::WouldReformat { diff } => {
            let mut out = format!(
                "ruff format (dry run): {}\nwould reformat\n",
                path.display()
            );
            if let Some(revision) = current_revision {
                out.push_str(&format!("revision: {revision}\n"));
            }
            if let Some(diff) = diff {
                out.push_str(&format!("\n```diff\n{diff}```\n"));
            }
            out
        }
    }
}

fn format_json(
    path: &Path,
    outcome: &ruff::FormatOutcome,
    current_revision: Option<&str>,
) -> serde_json::Value {
    let (status, revision, diff) = match outcome {
        ruff::FormatOutcome::Unchanged => ("unchanged", current_revision, None),
        ruff::FormatOutcome::Written { new_revision } => {
            ("formatted", Some(new_revision.as_str()), None)
        }
        ruff::FormatOutcome::WouldReformat { diff } => {
            ("would_reformat", current_revision, diff.as_deref())
        }
    };
    json!({
        "path": path.display().to_string(),
        "status": status,
        "revision": revision,
        "diff": diff,
    })
}

fn count(findings: &[Finding], severity: Severity) -> usize {
    findings
        .iter()
        .filter(|finding| finding.severity == severity)
        .count()
}

fn notebook_check_markdown(
    path: &Path,
    label: &str,
    revision: &str,
    findings: &[Finding],
) -> String {
    let mut out = format!(
        "notebook_check: {}\nformat: {label} · revision: {revision}\n",
        path.display()
    );
    if findings.is_empty() {
        out.push_str("no findings: the notebook passes this check\n");
        return out;
    }
    out.push_str(&format!(
        "{} finding(s): {} error(s), {} warning(s), {} info\n",
        findings.len(),
        count(findings, Severity::Error),
        count(findings, Severity::Warning),
        count(findings, Severity::Info),
    ));
    for finding in findings {
        let severity = match finding.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        };
        let cell = finding
            .cell
            .map_or(String::new(), |cell| format!(" cell {cell}"));
        out.push_str(&format!(
            "\n- {severity}{cell} `{}`: {}",
            finding.code, finding.message
        ));
    }
    out.push('\n');
    out
}
