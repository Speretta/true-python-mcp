//! ruff integration layer.
//!
//! This module is the single place that touches the ruff crates: config
//! resolution, linting (`.py` and `.ipynb`), safe-fix application and
//! formatting. The rest of tpmcp only sees the plain types defined here, so
//! upgrading ruff means bumping the pinned versions in `Cargo.toml` and
//! adjusting this module alone. Submodules: `settings` (config resolution
//! and its cache) and `diagnostics` (the stable diagnostic shape and its
//! mapping from ruff's JSON output).
//!
//! Diagnostics are produced by ruff's own JSON renderer (which already maps
//! notebook offsets back to `cell`/`row`), then parsed into our [`Diagnostic`]
//! so the tool output is stable across ruff internals. Notebook writes always
//! go back through [`crate::notebook`]'s byte-exact serializer: ruff's output
//! is re-parsed and re-emitted canonically, so fixing or formatting a
//! notebook never introduces diff noise in untouched cells.

use std::collections::HashSet;
use std::path::Path;

use ruff_db::diagnostic::Diagnostic as RuffDiagnostic;
use ruff_diagnostics::SourceMap;
use ruff_linter::linter::{FixerResult, ParseSource, lint_fix, lint_only};
use ruff_linter::settings::flags::Noqa;
use ruff_linter::settings::types::UnsafeFixes;
use ruff_linter::source_kind::SourceKind;
use ruff_notebook::{
    Cell as RuffCell, CellMetadata, CodeCell, MarkdownCell, Notebook as RuffNotebook,
    NotebookIndex, RawNotebook, RawNotebookMetadata, SourceValue,
};
use ruff_python_ast::{PySourceType, SourceType};
use ruff_python_formatter::format_module_source;
use ruff_text_size::{TextLen, TextRange, TextSize};
use ruff_workspace::Settings;

use crate::error::Error;
use crate::fs;
use crate::notebook::{JupyterNotebook, MarimoNotebook, Notebook};

mod diagnostics;
mod settings;

use diagnostics::map_diagnostics;
pub use diagnostics::{Diagnostic, Fix, LineCol};
use settings::resolve_settings;

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
/// value instead of an owned `SourceKind`, which also lets the unchanged path
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
/// snapshot are the *same* bytes, strictly tighter than the old
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
    // A marimo `.py` is linted per cell (dedented bodies concatenated into one
    // fabricated notebook), never as a single module: marimo's scaffolding
    // (cell headers, trailing `return`, `@app.function` decorators) would
    // otherwise surface as false positives. Phase 1 is lint-only; fix/format
    // write-back lands in a later phase. A file that merely *looks* like
    // marimo but fails to parse falls back to whole-file lint so a parse
    // hiccup can never silently drop diagnostics.
    let marimo = (source_type == PySourceType::Python
        && crate::notebook::is_marimo(&snapshot.content))
    .then(|| MarimoNotebook::parse(&snapshot.content).ok())
    .flatten();
    let (source_kind, source_type, cell_definitions) = match marimo {
        Some(notebook) => {
            if fixes == SafeFixes::Apply {
                return Err(Error::invalid_input(
                    "fixing marimo notebooks is not supported yet (phase 1 is lint-only)",
                ));
            }
            let cell_definitions = notebook.cell_definitions();
            let kind = marimo_source_kind(&notebook, path)?;
            (kind, PySourceType::Ipynb, Some(cell_definitions))
        }
        None => {
            let kind = source_kind_from(snapshot.content, source_type, path)?;
            (kind, source_type, None)
        }
    };
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
                // Post-fix source when changed, borrowed original otherwise;
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

    let mut diagnostics = map_diagnostics(&pass.diagnostics, path, pass.notebook_index)?;
    if let Some(cell_definitions) = cell_definitions {
        diagnostics = filter_marimo(diagnostics, &cell_definitions);
    }
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
    // Phase 1 leaves marimo formatting unimplemented: a marimo `.py` is still
    // formatted whole-file below. A later phase will branch here on
    // `crate::notebook::is_marimo` to format each cell body (re-indented,
    // `return` regenerated) through the byte-exact serializer, mirroring
    // `check`'s cell-body lint.
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

/// Fabricate a [`SourceKind::IpyNotebook`] for a marimo notebook: one code
/// cell per lintable body, plus an empty markdown placeholder per skipped
/// index, so the notebook index stays 1:1 with marimo cell indices (a
/// broken/unparsable cell keeps its slot without contributing source). Lint
/// only — this notebook is never written back.
fn marimo_source_kind(marimo: &MarimoNotebook, path: &Path) -> Result<SourceKind, Error> {
    let mut bodies: Vec<Option<String>> = vec![None; marimo.cells().len()];
    for (index, body) in marimo.lint_cells() {
        if let Some(slot) = bodies.get_mut(index) {
            *slot = Some(body);
        }
    }
    let cells = bodies
        .into_iter()
        .map(|body| match body {
            Some(body) => {
                // Mirror Jupyter's cell-source convention (each entry ends in a
                // newline) so ruff's synthetic `\n` separator becomes a blank
                // line between cells; otherwise an import at the end of one
                // cell and code at the start of the next fuse into an
                // "un-formatted" I001.
                let mut source = body;
                if !source.ends_with('\n') {
                    source.push('\n');
                }
                RuffCell::Code(CodeCell {
                    execution_count: None,
                    id: None,
                    metadata: CellMetadata::default(),
                    outputs: Vec::new(),
                    source: SourceValue::String(source),
                })
            }
            None => RuffCell::Markdown(MarkdownCell {
                attachments: None,
                id: None,
                metadata: CellMetadata::default(),
                source: SourceValue::String(String::new()),
            }),
        })
        .collect();
    let notebook = RuffNotebook::from_raw_notebook(
        RawNotebook {
            cells,
            metadata: RawNotebookMetadata::default(),
            nbformat: 4,
            nbformat_minor: 4,
        },
        false,
    )
    .map_err(|error| Error::analysis(format!("cannot lint marimo {}: {error}", path.display())))?;
    Ok(SourceKind::ipy_notebook(notebook))
}

/// Post-mapping filter for marimo notebooks: drop the false positives of
/// linting cell bodies as one module. A name is "provided" when the marimo
/// runtime supplies it as a builtin or some cell defines it at its top level
/// (`cell_definitions`), so an F821 against either is a graph-provided name,
/// not a genuine undefined one. PLR1711/I001/C408 are untouched: PLR1711
/// disappears because `lint_cells` strips the trailing `return`, and the rest
/// are real lint findings.
fn filter_marimo(diags: Vec<Diagnostic>, cell_definitions: &HashSet<String>) -> Vec<Diagnostic> {
    diags
        .into_iter()
        .filter(|diagnostic| {
            let Some(code) = diagnostic.code.as_deref() else {
                return true;
            };
            let token = backtick_token(&diagnostic.message);
            match code {
                // The scaffold `import marimo` (or `import marimo as mo`) is
                // never a user mistake; marimo owns it.
                "F401" => !matches!(
                    token,
                    Some(token) if token == "marimo" || token.starts_with("marimo.")
                ),
                // `mo`/`marimo` are redefined by the graph across cells.
                "F811" => !matches!(token, Some(token) if token == "mo" || token == "marimo"),
                // A name marimo provides is not "undefined" to the runtime.
                "F821" => !token.is_some_and(|token| {
                    MarimoNotebook::runtime_builtin(token) || cell_definitions.contains(token)
                }),
                _ => true,
            }
        })
        .collect()
}

/// The first backtick-quoted token of a ruff message: `` `marimo` imported but
/// unused`` → `marimo`, ``Redefinition of unused `mo` from line 1`` → `mo`,
/// ``Undefined name `x` `` → `x`. `None` when the message has no backticks.
fn backtick_token(message: &str) -> Option<&str> {
    let rest = message.split_once('`')?.1;
    rest.split('`').next()
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
