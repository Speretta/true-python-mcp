//! ruff integration layer.
//!
//! This module is the single place that touches the ruff crates: config
//! resolution, linting (`.py` and `.ipynb`), safe-fix application and
//! formatting. The rest of tpmcp only sees the plain types defined here, so
//! upgrading ruff means bumping the pinned versions in `Cargo.toml` and
//! adjusting this file alone.
//!
//! Diagnostics are produced by ruff's own JSON renderer (which already maps
//! notebook offsets back to `cell`/`row`), then parsed into our [`Diagnostic`]
//! so the tool output is stable across ruff internals. Notebook writes always
//! go back through [`crate::notebook`]'s byte-exact serializer: ruff's output
//! is re-parsed and re-emitted canonically, so fixing or formatting a
//! notebook never introduces diff noise in untouched cells.

use std::path::Path;

use ruff_db::diagnostic::{Diagnostic as RuffDiagnostic, DisplayDiagnosticConfig};
use ruff_diagnostics::SourceMap;
use ruff_linter::linter::{FixerResult, ParseSource, lint_fix, lint_only};
use ruff_linter::message::{EmitterContext, render_diagnostics};
use ruff_linter::settings::flags::Noqa;
use ruff_linter::settings::types::{OutputFormat, UnsafeFixes};
use ruff_linter::source_kind::SourceKind;
use ruff_notebook::{Notebook as RuffNotebook, NotebookIndex};
use ruff_python_ast::{PySourceType, SourceType};
use ruff_python_formatter::format_module_source;
use ruff_text_size::{TextLen, TextRange, TextSize};
use ruff_workspace::Settings;
use ruff_workspace::configuration::Configuration;
use ruff_workspace::resolver::{
    ConfigurationOrigin, ConfigurationTransformer, resolve_root_settings,
};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::fs;
use crate::notebook::JupyterNotebook;

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

/// Result of [`check`].
pub struct CheckOutcome {
    pub diagnostics: Vec<Diagnostic>,
    /// True when `fix` rewrote the file on disk.
    pub changed: bool,
    /// New revision after a fix write, for chaining with the edit tools.
    pub new_revision: Option<String>,
}

impl CheckOutcome {
    /// Diagnostics that carry an automatic fix.
    pub fn fixable_count(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.fix.is_some()).count()
    }
}

/// Result of [`format_file`].
#[derive(Debug)]
pub enum FormatOutcome {
    /// Already formatted; nothing to do.
    Unchanged,
    /// Would change the file; `diff` is a unified diff for text files.
    WouldReformat { diff: Option<String> },
    /// The file was rewritten; carries the new revision.
    Written { new_revision: String },
}

/// Lint `path`, optionally applying safe fixes in place.
pub fn check(path: &Path, fix: bool) -> Result<CheckOutcome, Error> {
    let settings = resolve_settings(path)?;
    let Some(source_type) = python_source_type(&settings, path) else {
        return Ok(empty_check());
    };
    let source_kind = load_source_kind(path, source_type)?;
    let noqa = Noqa::Enabled;

    let (diagnostics, owned_kind, changed, new_revision) = if fix {
        let FixerResult {
            result,
            transformed,
            fixed,
        } = lint_fix(
            path,
            None,
            noqa,
            UnsafeFixes::Disabled,
            &settings.linter,
            &source_kind,
            source_type,
        )
        .map_err(Error::analysis)?;
        let changed = !fixed.is_empty();
        let owned = transformed.into_owned();
        let new_revision = if changed {
            Some(write_source_kind(path, &owned)?)
        } else {
            None
        };
        (result.diagnostics, owned, changed, new_revision)
    } else {
        let result = lint_only(
            path,
            None,
            &settings.linter,
            noqa,
            &source_kind,
            source_type,
            ParseSource::None,
        );
        (result.diagnostics, source_kind, false, None)
    };

    let mut notebook_indexes = FxHashMap::default();
    if let SourceKind::IpyNotebook(notebook) = owned_kind {
        notebook_indexes.insert(path.to_string_lossy().to_string(), notebook.into_index());
    }
    let diagnostics = map_diagnostics(&diagnostics, &notebook_indexes)?;
    Ok(CheckOutcome {
        diagnostics,
        changed,
        new_revision,
    })
}

/// Format `path`. When `write` is false, report what would change instead of
/// touching the file.
pub fn format_file(path: &Path, write: bool) -> Result<FormatOutcome, Error> {
    let settings = resolve_settings(path)?;
    let Some(source_type) = python_source_type(&settings, path) else {
        return Err(Error::invalid_input(format!(
            "not a Python file or notebook: {}",
            path.display()
        )));
    };
    let source_kind = load_source_kind(path, source_type)?;
    let formatted = match &source_kind {
        SourceKind::Python { code, is_stub } => {
            let options = settings
                .formatter
                .to_format_options(source_type, code, Some(path));
            let formatted = format_module_source(code, options)
                .map(|printed| printed.into_code())
                .map_err(Error::analysis)?;
            if formatted == *code {
                None
            } else {
                Some(SourceKind::Python {
                    code: formatted,
                    is_stub: *is_stub,
                })
            }
        }
        SourceKind::IpyNotebook(notebook) => format_notebook(notebook, &settings, path)?,
        SourceKind::Markdown(_) => {
            return Err(Error::invalid_input(format!(
                "cannot format markdown: {}",
                path.display()
            )));
        }
    };

    let Some(formatted) = formatted else {
        return Ok(FormatOutcome::Unchanged);
    };
    if write {
        let new_revision = write_source_kind(path, &formatted)?;
        return Ok(FormatOutcome::Written { new_revision });
    }
    let diff = match (&source_kind, &formatted) {
        (SourceKind::Python { code: old, .. }, SourceKind::Python { code: new, .. }) => {
            Some(crate::render::unified_diff(old, new))
        }
        _ => None,
    };
    Ok(FormatOutcome::WouldReformat { diff })
}

/// Format every Python cell of a notebook, mirroring ruff's own cell loop so
/// magics and escapes are handled correctly.
fn format_notebook(
    notebook: &RuffNotebook,
    settings: &Settings,
    path: &Path,
) -> Result<Option<SourceKind>, Error> {
    if !notebook.is_python_notebook() {
        return Ok(None);
    }
    let source_code = notebook.source_code();
    let options =
        settings
            .formatter
            .to_format_options(PySourceType::Ipynb, source_code, Some(path));
    let offsets: Vec<TextSize> = notebook.cell_offsets().iter().copied().collect();

    let mut output: Option<String> = None;
    let mut last: Option<TextSize> = None;
    let mut source_map = SourceMap::default();
    for pair in offsets.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let unformatted = &source_code[TextRange::new(start, end)];
        let formatted = format_module_source(unformatted, options.clone())
            .map(|printed| printed.into_code())
            .map_err(Error::analysis)?;
        if formatted == unformatted {
            continue;
        }
        let buffer = output.get_or_insert_with(|| String::with_capacity(source_code.len()));
        buffer.push_str(&source_code[TextRange::new(last.unwrap_or_default(), start)]);
        source_map.push_marker(start, buffer.text_len());
        buffer.push_str(&formatted);
        source_map.push_marker(end, buffer.text_len());
        last = Some(end);
    }
    let (Some(mut buffer), Some(last)) = (output, last) else {
        return Ok(None);
    };
    buffer.push_str(&source_code[usize::from(last)..]);
    let mut formatted = notebook.clone();
    formatted.update(&source_map, buffer);
    Ok(Some(SourceKind::IpyNotebook(Box::new(formatted))))
}

/// Write a (possibly fixed/formatted) source back to disk. Notebooks are
/// re-parsed and re-serialized through our byte-exact writer.
fn write_source_kind(path: &Path, kind: &SourceKind) -> Result<String, Error> {
    match kind {
        SourceKind::Python { code, .. } => fs::write(path, code),
        SourceKind::IpyNotebook(_) => {
            let mut buffer = Vec::new();
            kind.write(&mut buffer).map_err(Error::analysis)?;
            let text =
                String::from_utf8(buffer).map_err(|error| Error::parse(path, error.to_string()))?;
            let notebook =
                JupyterNotebook::parse(&text).map_err(|message| Error::parse(path, message))?;
            fs::write(path, &notebook.to_string_canonical())
        }
        SourceKind::Markdown(_) => Err(Error::invalid_input("cannot write markdown source")),
    }
}

fn python_source_type(settings: &Settings, path: &Path) -> Option<PySourceType> {
    match settings.linter.extension.get_source_type(path) {
        SourceType::Python(source_type) => Some(source_type),
        _ => None,
    }
}

fn load_source_kind(path: &Path, source_type: PySourceType) -> Result<SourceKind, Error> {
    SourceKind::from_path(path, SourceType::Python(source_type))
        .map_err(|error| Error::analysis(format!("cannot read {}: {error}", path.display())))?
        .ok_or_else(|| {
            Error::invalid_input(format!("empty or unsupported file: {}", path.display()))
        })
}

/// Resolve the effective settings for `path` by walking up to the nearest
/// ruff config (`ruff.toml`, `.ruff.toml`, or a `pyproject.toml` carrying a
/// `[tool.ruff]` table), falling back to ruff defaults.
fn resolve_settings(path: &Path) -> Result<Settings, Error> {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let start = absolute.parent().unwrap_or(&absolute);
    for dir in start.ancestors() {
        for name in ["ruff.toml", ".ruff.toml"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return root_settings(&candidate);
            }
        }
        let pyproject = dir.join("pyproject.toml");
        if pyproject.is_file() && pyproject_has_ruff(&pyproject) {
            return root_settings(&pyproject);
        }
    }
    Ok(Settings::default())
}

fn root_settings(config_path: &Path) -> Result<Settings, Error> {
    resolve_root_settings(config_path, &NoOpTransformer, ConfigurationOrigin::Ancestor).map_err(
        |error| {
            Error::analysis(format!(
                "invalid ruff config {}: {error}",
                config_path.display()
            ))
        },
    )
}

/// Cheap check for a `[tool.ruff` table so unrelated pyproject.toml files do
/// not stop the upward search.
fn pyproject_has_ruff(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|text| text.contains("[tool.ruff"))
        .unwrap_or(false)
}

struct NoOpTransformer;

impl ConfigurationTransformer for NoOpTransformer {
    fn transform(&self, config: Configuration) -> Configuration {
        config
    }
}

fn empty_check() -> CheckOutcome {
    CheckOutcome {
        diagnostics: Vec::new(),
        changed: false,
        new_revision: None,
    }
}

/// Render ruff diagnostics to JSON (mapping notebook offsets to cells) and
/// parse them into our stable [`Diagnostic`] shape.
fn map_diagnostics(
    diagnostics: &[RuffDiagnostic],
    notebook_indexes: &FxHashMap<String, NotebookIndex>,
) -> Result<Vec<Diagnostic>, Error> {
    let mut buffer = Vec::new();
    let context = EmitterContext::new(notebook_indexes);
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
    diagnostics.sort_by_key(|d| {
        (
            d.cell.unwrap_or(0),
            d.location.as_ref().map_or((0, 0), |l| (l.row, l.column)),
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
