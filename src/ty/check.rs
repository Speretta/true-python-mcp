use std::path::Path;

use ruff_db::diagnostic::{Diagnostic as TyDiagnostic, Severity, UnifiedFile};
use ruff_db::files::{File, system_path_to_file};
use ruff_db::source::{SourceText, source_text};
use ruff_notebook::{Notebook, NotebookIndex};
use ruff_source_file::{LineColumn, LineIndex, OneIndexed};
use ty_python_semantic::Db as SemanticDb;

use crate::error::Error;
use crate::fs;
use crate::ruff::{Diagnostic, LineCol};

use super::cache::{CacheKey, with_db};
use super::db::{NOTEBOOK_VIRTUAL_PATH, TyDb, to_system_path};

/// Sources larger than this are refused before the checker recurses: the type
/// check walks the semantic index on the small worker stack, and a pathological
/// single-cell notebook or file can abort the process no stack guard can make
/// cheap. ruff's read cap is 32 MiB; ty gets a tighter bound because a full
/// inference pass is far more expensive than lint.
const MAX_SOURCE_SIZE: usize = 2 * 1024 * 1024;

pub(super) fn check_snapshot_inner(
    path: &Path,
    snapshot: &fs::Snapshot,
    python: Option<&Path>,
) -> Result<Vec<Diagnostic>, Error> {
    if !is_python_path(path) {
        return Err(not_python(path));
    }
    if path.extension().is_some_and(|ext| ext == "ipynb") {
        return check_notebook(path, snapshot, python);
    }
    if snapshot.content.len() > MAX_SOURCE_SIZE {
        return Err(oversized(path, snapshot.content.len()));
    }
    let src_root = path.parent().unwrap_or_else(|| Path::new("."));
    let src_root_sys = to_system_path(src_root).ok_or_else(|| {
        Error::invalid_input(format!("path is not representable: {}", src_root.display()))
    })?;
    let system_path = to_system_path(path).ok_or_else(|| {
        Error::invalid_input(format!("path is not representable: {}", path.display()))
    })?;

    with_db(
        CacheKey::new(src_root, python),
        snapshot.content.len(),
        || TyDb::new(src_root_sys.as_path(), python),
        |db| {
            let file = system_path_to_file(db, &system_path)
                .map_err(|error| Error::analysis(format!("cannot read {system_path}: {error}")))?;
            // Refresh the file from disk so a cached database re-reads the
            // current bytes (invalidating any memoized check of the old ones).
            File::sync_path(db, &system_path);
            check_file(db, file)
        },
    )
}

/// Type-checks a `.ipynb` notebook from the caller's snapshot: parse it into
/// ruff's `Notebook` (concatenated cell source + offset index), serve that
/// source to ty as one virtual `.py` file, then map every concatenated-source
/// position back to a `cell` index and a cell-relative row. No temp file, no
/// write into the caller's tree, no second read.
fn check_notebook(
    path: &Path,
    snapshot: &fs::Snapshot,
    python: Option<&Path>,
) -> Result<Vec<Diagnostic>, Error> {
    let notebook = Notebook::from_source_code(&snapshot.content)
        .map_err(|error| Error::parse(path, format!("{error}")))?;
    let source = notebook.source_code().to_string();
    if source.len() > MAX_SOURCE_SIZE {
        return Err(oversized(path, source.len()));
    }
    let index = notebook.index().clone();

    let src_root = path.parent().unwrap_or_else(|| Path::new("."));
    let src_root_sys = to_system_path(src_root).ok_or_else(|| {
        Error::invalid_input(format!("path is not representable: {}", src_root.display()))
    })?;

    with_db(
        CacheKey::new(src_root, python),
        source.len(),
        || TyDb::new(src_root_sys.as_path(), python),
        |db| {
            let file = db.serve_virtual(NOTEBOOK_VIRTUAL_PATH, &source);
            check_file(db, file).map(|diagnostics| {
                diagnostics
                    .into_iter()
                    .map(|diagnostic| map_to_cell(diagnostic, &index))
                    .collect()
            })
        },
    )
}

// ---- input guards ----

/// A non-Python path is a caller error, not a green "no issues found". Matches
/// ruff's message/shape so the two checkers never disagree about what they
/// accept.
fn not_python(path: &Path) -> Error {
    Error::invalid_input(format!("not a Python file or notebook: {}", path.display()))
}

fn is_python_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("py") | Some("pyi") | Some("ipynb")
    )
}

/// A source over [`MAX_SOURCE_SIZE`] is refused before any recursion, so a
/// pathological input becomes a clean caller error instead of a stack abort.
fn oversized(path: &Path, size: usize) -> Error {
    Error::invalid_input(format!(
        "{} is {} bytes, over the {} MiB type-check limit",
        path.display(),
        size,
        MAX_SOURCE_SIZE / (1024 * 1024),
    ))
}

// ---- diagnostic mapping ----

/// Rewrites a diagnostic's concatenated-source positions to the notebook cell:
/// sets the 0-based `cell` index and makes `location`/`end_location`
/// cell-relative.
fn map_to_cell(mut diagnostic: Diagnostic, index: &NotebookIndex) -> Diagnostic {
    if let Some(location) = diagnostic.location {
        diagnostic.cell = index
            .cell(OneIndexed::new(location.row).unwrap_or(OneIndexed::MIN))
            .map(|cell| cell.get() - 1);
        diagnostic.location = Some(translate_line_column(index, location));
    }
    if let Some(end_location) = diagnostic.end_location {
        diagnostic.end_location = Some(translate_line_column(index, end_location));
    }
    diagnostic
}

fn translate_line_column(index: &NotebookIndex, location: LineCol) -> LineCol {
    let translated = index.translate_line_column(&LineColumn {
        line: OneIndexed::new(location.row).unwrap_or(OneIndexed::MIN),
        column: OneIndexed::new(location.column).unwrap_or(OneIndexed::MIN),
    });
    LineCol {
        row: translated.line.get(),
        column: translated.column.get(),
    }
}

/// Type-checks one interned file against an already-built database and returns
/// the diagnostics in tpmcp's stable shape.
fn check_file(db: &TyDb, file: File) -> Result<Vec<Diagnostic>, Error> {
    // The type check recurses through the semantic index and inference on the
    // small worker stack; grow it like the ruff linter/format walks do.
    let source = source_text(db, file);
    let diagnostics =
        crate::stack::guarded_walk(source.len(), || <TyDb as SemanticDb>::check_file(db, file));
    // why: one line index per file, not per diagnostic (O(N·L) otherwise).
    let index = LineIndex::from_source_text(&source);
    Ok(diagnostics
        .iter()
        .map(|diagnostic| map_diagnostic(db, diagnostic, file, &source, &index))
        .collect())
}

fn map_diagnostic(
    db: &TyDb,
    diagnostic: &TyDiagnostic,
    checked: File,
    checked_source: &SourceText,
    checked_index: &LineIndex,
) -> Diagnostic {
    let (location, end_location) = match diagnostic.primary_span() {
        Some(span) => match span.file() {
            UnifiedFile::Ty(file) => {
                // The common case: the span points into the checked file, whose
                // source and line index we already hold. A span into another
                // file (rare) builds its own.
                let foreign_source;
                let foreign_index;
                let (source, index) = if *file == checked {
                    (checked_source, checked_index)
                } else {
                    foreign_source = source_text(db, *file);
                    foreign_index = LineIndex::from_source_text(&foreign_source);
                    (&foreign_source, &foreign_index)
                };
                match span.range() {
                    Some(range) => {
                        let start = index.line_column(range.start(), source);
                        let end = index.line_column(range.end(), source);
                        (
                            Some(LineCol {
                                row: start.line.get(),
                                column: start.column.get(),
                            }),
                            Some(LineCol {
                                row: end.line.get(),
                                column: end.column.get(),
                            }),
                        )
                    }
                    None => (None, None),
                }
            }
            // A Ruff span points into a source ty doesn't map (stdlib_comment);
            // leave it unlocated rather than inventing a position.
            UnifiedFile::Ruff(_) => (None, None),
        },
        None => (None, None),
    };
    Diagnostic {
        filename: None,
        cell: None,
        code: Some(diagnostic.secondary_code_or_id().to_string()),
        message: diagnostic.headline_message().to_string(),
        severity: match diagnostic.severity() {
            Severity::Error | Severity::Fatal => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
        .to_string(),
        location,
        end_location,
        fix: None,
        url: diagnostic.documentation_url().map(str::to_string),
    }
}
