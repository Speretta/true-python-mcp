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
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::fs;
use crate::notebook::JupyterNotebook;

mod settings;

use settings::resolve_settings;

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
#[derive(Debug)]
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
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.fix.is_some())
            .count()
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

/// Whether [`check`] applies safe fixes in place or only reports them.
///
/// Replaces the old `fix: bool` trap at the module boundary so call sites read
/// as intent (Style-11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeFixes {
    /// Apply safe fixes and rewrite the file.
    Apply,
    /// Report diagnostics only; never touch the file.
    ReportOnly,
}

/// Whether [`format_file`] writes the formatted output in place.
///
/// Replaces the old `write: bool` trap at the module boundary (Style-11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatWrite {
    /// Rewrite the file with the formatted output.
    InPlace,
    /// Report what would change without writing.
    DryRun,
}

/// One lint pass: ruff's diagnostics plus what fixing did to the file.
///
/// Named replacement for the old heterogeneous 4-tuple spine of [`check`]
/// (Style-10). `notebook_index` is the only thing the linted source is needed
/// for downstream (mapping offsets back to cells), so we carry that small
/// value instead of an owned `SourceKind` — which also lets the unchanged path
/// avoid a deep clone (Memory-3).
struct LintPass {
    diagnostics: Vec<RuffDiagnostic>,
    notebook_index: Option<NotebookIndex>,
    changed: bool,
    new_revision: Option<String>,
}

/// Lint the already-read `snapshot` of `path`, optionally applying safe fixes.
///
/// Single-read contract (CPU-F2): the caller reads `path` exactly once via
/// [`crate::fs`] and hands us those bytes. For [`SafeFixes::Apply`] the caller
/// has already revision-verified them, so the verified snapshot and the linted
/// snapshot are the *same* bytes — strictly tighter than the old
/// verify-then-reread flow, where the file could change between the two reads.
pub fn check(path: &Path, fixes: SafeFixes, snapshot: fs::Snapshot) -> Result<CheckOutcome, Error> {
    let settings = resolve_settings(path)?;
    // F14: a non-Python path is a caller error, not "no issues found". Match
    // ruff_format's message/shape so the two tools never disagree.
    let Some(source_type) = python_source_type(&settings, path) else {
        return Err(not_python(path));
    };
    // Worst-case AST recursion bound for ruff's unguarded walk/drop stages
    // (crate::stack): every node consumes at least one source byte.
    let source_len = snapshot.content.len();
    let source_kind = source_kind_from(snapshot.content, source_type, path)?;
    let noqa = Noqa::Enabled;

    let pass = match fixes {
        SafeFixes::Apply => {
            let FixerResult {
                result,
                transformed,
                fixed,
            } = crate::stack::guarded_walk(source_len, || {
                lint_fix(
                    path,
                    None,
                    noqa,
                    UnsafeFixes::Disabled,
                    &settings.linter,
                    &source_kind,
                    source_type,
                )
            })
            .map_err(Error::analysis)?;
            let changed = !fixed.is_empty();
            // Only materialize a write when a fix actually changed the source;
            // when unchanged `transformed` still borrows `source_kind`, so we
            // neither write nor deep-clone the whole SourceKind (Memory-3).
            let new_revision = if changed {
                Some(write_source_kind(path, &transformed)?)
            } else {
                None
            };
            LintPass {
                // Post-fix source when changed, borrowed original otherwise —
                // either way `transformed` is the bytes the diagnostics map to.
                notebook_index: notebook_index_of(&transformed),
                diagnostics: result.diagnostics,
                changed,
                new_revision,
            }
        }
        SafeFixes::ReportOnly => {
            let result = crate::stack::guarded_walk(source_len, || {
                lint_only(
                    path,
                    None,
                    &settings.linter,
                    noqa,
                    &source_kind,
                    source_type,
                    ParseSource::None,
                )
            });
            LintPass {
                notebook_index: notebook_index_of(&source_kind),
                diagnostics: result.diagnostics,
                changed: false,
                new_revision: None,
            }
        }
    };

    let mut notebook_indexes = FxHashMap::default();
    if let Some(index) = pass.notebook_index {
        notebook_indexes.insert(path.to_string_lossy().to_string(), index);
    }
    let diagnostics = map_diagnostics(&pass.diagnostics, &notebook_indexes)?;
    Ok(CheckOutcome {
        diagnostics,
        changed: pass.changed,
        new_revision: pass.new_revision,
    })
}

/// Format the already-read `snapshot` of `path`. See [`check`] for the
/// single-read contract. [`FormatWrite::DryRun`] reports what would change (a
/// unified diff for text files) without touching the file.
pub fn format_file(
    path: &Path,
    write: FormatWrite,
    snapshot: fs::Snapshot,
) -> Result<FormatOutcome, Error> {
    let settings = resolve_settings(path)?;
    let Some(source_type) = python_source_type(&settings, path) else {
        return Err(not_python(path));
    };
    let source_kind = source_kind_from(snapshot.content, source_type, path)?;

    // Consume `source_kind` so the notebook branch can hand ownership to
    // `format_notebook`, which mutates it in place (no deep clone — Memory-4).
    let formatted = match source_kind {
        SourceKind::Python { code, is_stub } => {
            let options = settings
                .formatter
                .to_format_options(source_type, &code, Some(path));
            let new_code =
                crate::stack::guarded_walk(code.len(), || format_module_source(&code, options))
                    .map(|printed| printed.into_code())
                    .map_err(Error::analysis)?;
            if new_code == code {
                return Ok(FormatOutcome::Unchanged);
            }
            if matches!(write, FormatWrite::DryRun) {
                let diff = crate::render::unified_diff(&code, &new_code);
                return Ok(FormatOutcome::WouldReformat { diff: Some(diff) });
            }
            SourceKind::Python {
                code: new_code,
                is_stub,
            }
        }
        SourceKind::IpyNotebook(notebook) => match format_notebook(*notebook, &settings, path)? {
            Some(formatted) => SourceKind::IpyNotebook(Box::new(formatted)),
            None => return Ok(FormatOutcome::Unchanged),
        },
        SourceKind::Markdown(_) => {
            return Err(Error::invalid_input(format!(
                "cannot format markdown: {}",
                path.display()
            )));
        }
    };

    match write {
        FormatWrite::InPlace => {
            let new_revision = write_source_kind(path, &formatted)?;
            Ok(FormatOutcome::Written { new_revision })
        }
        // Only a changed notebook reaches here on a dry run (a changed Python
        // file returned its diff above); notebooks have no plain-text diff.
        FormatWrite::DryRun => Ok(FormatOutcome::WouldReformat { diff: None }),
    }
}

/// Format every Python cell of a notebook, mirroring ruff's own cell loop so
/// magics and escapes are handled correctly.
///
/// Takes the notebook *by value* (Memory-4): `update()` needs `&mut`, and
/// owning the notebook lets us mutate it in place instead of deep-cloning the
/// whole `RuffNotebook` (source + raw JSON DOM + offsets) just to get a
/// mutable handle.
fn format_notebook(
    mut notebook: RuffNotebook,
    settings: &Settings,
    path: &Path,
) -> Result<Option<RuffNotebook>, Error> {
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
        let formatted = crate::stack::guarded_walk(unformatted.len(), || {
            format_module_source(unformatted, options.clone())
        })
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
    // Last use of the `source_code` borrow; after this line NLL frees it, so
    // `notebook.update` (which needs `&mut notebook`) mutates in place.
    buffer.push_str(&source_code[usize::from(last)..]);
    notebook.update(&source_map, buffer);
    Ok(Some(notebook))
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

/// Build ruff's [`SourceKind`] from bytes already read by [`crate::fs`], so
/// ruff never re-reads (or, for `.ipynb`, re-parses from disk) the file
/// (CPU-F2). `content` is moved in: for `.py` it becomes the source verbatim,
/// for `.ipynb` it is parsed once by `Notebook::from_source_code`.
fn source_kind_from(
    content: String,
    source_type: PySourceType,
    path: &Path,
) -> Result<SourceKind, Error> {
    SourceKind::from_source_code(content, SourceType::Python(source_type))
        .map_err(|error| Error::analysis(format!("cannot parse {}: {error}", path.display())))?
        .ok_or_else(|| {
            Error::invalid_input(format!("empty or unsupported file: {}", path.display()))
        })
}

/// The small [`NotebookIndex`] used to map ruff offsets back to `cell`/`row`,
/// cloned out of a borrow so we never need to own the whole `SourceKind`
/// (Memory-3). `None` for non-notebook sources.
fn notebook_index_of(kind: &SourceKind) -> Option<NotebookIndex> {
    match kind {
        SourceKind::IpyNotebook(notebook) => Some(notebook.index().clone()),
        _ => None,
    }
}

/// F14: ruff only lints Python; point the caller at the right tool instead of
/// silently reporting "no issues". Matches [`format_file`]'s message/shape.
fn not_python(path: &Path) -> Error {
    Error::invalid_input(format!("not a Python file or notebook: {}", path.display()))
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
