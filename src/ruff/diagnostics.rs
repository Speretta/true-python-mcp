//! The stable diagnostic shape and its mapping from ruff's JSON output.
//!
//! ruff renders its own diagnostics to JSON, which already maps notebook
//! offsets back to `cell`/`row`. This module parses that JSON into the
//! [`Diagnostic`] type tpmcp exposes, so the tool output stays stable across
//! ruff internals. The raw wire shape is deliberately tolerant: every field
//! is optional.

use std::path::Path;

use ruff_db::diagnostic::{Diagnostic as RuffDiagnostic, DisplayDiagnosticConfig};
use ruff_linter::message::{EmitterContext, render_diagnostics};
use ruff_linter::settings::types::OutputFormat;
use ruff_notebook::NotebookIndex;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::error::Error;

/// A 1-based line/column position, matching ruff's JSON output.
#[derive(Debug, Clone, Serialize)]
pub struct LineCol {
    pub row: usize,
    pub column: usize,
}

/// An available automatic fix.
#[derive(Debug, Clone, Serialize)]
pub struct Fix {
    pub applicability: String,
    pub message: Option<String>,
}

/// A single lint diagnostic in tpmcp's stable shape.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub filename: Option<String>,
    /// 0-based notebook cell index (matches the cell tools); `None` for files.
    pub cell: Option<usize>,
    pub code: Option<String>,
    pub message: String,
    pub severity: String,
    pub location: Option<LineCol>,
    pub end_location: Option<LineCol>,
    pub fix: Option<Fix>,
    pub url: Option<String>,
}

/// Render ruff diagnostics to JSON (mapping notebook offsets to cells) and
/// parse them into our stable [`Diagnostic`] shape.
pub(super) fn map_diagnostics(
    diagnostics: &[RuffDiagnostic],
    path: &Path,
    notebook_index: Option<NotebookIndex>,
) -> Result<Vec<Diagnostic>, Error> {
    // ruff maps notebook offsets back to cells by looking the file's name up in
    // a notebook-index map; a single check lints at most one notebook (the file
    // itself), so the map has at most one entry, built only for notebooks.
    let notebook_indexes: FxHashMap<String, NotebookIndex> = notebook_index
        .into_iter()
        .map(|index| (path.to_string_lossy().into_owned(), index))
        .collect();
    let mut buffer = Vec::new();
    let context = EmitterContext::new(&notebook_indexes);
    let config = DisplayDiagnosticConfig::new("ruff")
        .color(false)
        .with_show_fix_status(true);
    render_diagnostics(
        &mut buffer,
        OutputFormat::Json,
        config,
        &context,
        diagnostics,
    )
    .map_err(Error::analysis)?;
    let raw: Vec<RawDiagnostic> = serde_json::from_slice(&buffer)
        .map_err(|error| Error::analysis(format!("ruff produced unexpected JSON: {error}")))?;
    let mut diagnostics: Vec<Diagnostic> = raw.into_iter().map(Diagnostic::from).collect();
    // Match ruff's CLI ordering: by cell, then position within the file/cell.
    diagnostics.sort_by_key(|diagnostic| {
        (
            diagnostic.cell.unwrap_or(0),
            diagnostic
                .location
                .as_ref()
                .map_or((0, 0), |location| (location.row, location.column)),
        )
    });
    Ok(diagnostics)
}

// ---- ruff JSON wire shape (tolerant: every field optional) ----

#[derive(Deserialize)]
struct RawDiagnostic {
    #[serde(default)]
    cell: Option<usize>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    severity: Option<String>,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    message: String,
    #[serde(default)]
    location: Option<RawLineCol>,
    #[serde(default)]
    end_location: Option<RawLineCol>,
    #[serde(default)]
    fix: Option<RawFix>,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Deserialize)]
struct RawLineCol {
    row: usize,
    column: usize,
}

#[derive(Deserialize)]
struct RawFix {
    #[serde(default)]
    applicability: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

impl From<RawDiagnostic> for Diagnostic {
    fn from(raw: RawDiagnostic) -> Self {
        Self {
            filename: raw.filename,
            // ruff reports 1-based notebook cells; our tools are 0-based.
            cell: raw.cell.map(|cell| cell.saturating_sub(1)),
            code: raw.code,
            message: raw.message,
            severity: raw.severity.unwrap_or_else(|| "error".to_string()),
            location: raw.location.map(LineCol::from),
            end_location: raw.end_location.map(LineCol::from),
            fix: raw.fix.map(Fix::from),
            url: raw.url,
        }
    }
}

impl From<RawLineCol> for LineCol {
    fn from(raw: RawLineCol) -> Self {
        Self {
            row: raw.row,
            column: raw.column,
        }
    }
}

impl From<RawFix> for Fix {
    fn from(raw: RawFix) -> Self {
        Self {
            applicability: raw.applicability.unwrap_or_else(|| "unknown".to_string()),
            message: raw.message,
        }
    }
}
