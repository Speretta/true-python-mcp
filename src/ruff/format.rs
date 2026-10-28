//! The format half of the ruff integration: [`format_file`] and the
//! per-cell notebook loop it runs, mirroring ruff's own. The shared
//! single-read intake (`check`'s helpers) stays in the parent module.

use std::path::Path;

use ruff_diagnostics::SourceMap;
use ruff_linter::source_kind::SourceKind;
use ruff_notebook::Notebook as RuffNotebook;
use ruff_python_ast::PySourceType;
use ruff_python_formatter::format_module_source;
use ruff_text_size::{TextLen, TextRange, TextSize};
use ruff_workspace::Settings;

use super::settings::resolve_settings;
use super::{not_python, python_source_type, source_kind_from, write_source_kind};
use crate::error::Error;
use crate::fs;

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

/// Format the already-read `snapshot` of `path`. See [`super::check`] for
/// the single-read contract. [`FormatWrite::DryRun`] reports what would
/// change (a unified diff for text files) without touching the file.
pub fn format_file(
    path: &Path,
    write: FormatWrite,
    snapshot: fs::Snapshot,
) -> Result<FormatOutcome, Error> {
    let settings = resolve_settings(path)?;
    let Some(source_type) = python_source_type(&settings, path) else {
        return Err(not_python(path));
    };
    // why: formatting a marimo `.py` whole-file fights the serializer's
    // canonical shapes (the one-line `return`, the scaffold layout) and
    // rewrites bytes no cell edit touched. Like `check`'s fix path, refuse
    // until a per-cell format pass regenerates blocks through the byte-exact
    // serializer.
    if crate::notebook::is_marimo(&snapshot.content) {
        return Err(Error::invalid_input(
            "formatting marimo notebooks is not supported yet (phase 1 is lint-only)",
        ));
    }
    let source_kind = source_kind_from(snapshot.content, source_type, path)?;

    // Consume `source_kind` so the notebook branch can hand ownership to
    // `format_notebook`, which mutates it in place (no deep clone; Memory-4).
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
