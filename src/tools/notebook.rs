//! Notebook-level tools: reading and creating notebooks (`.ipynb` and
//! marimo `.py`).
//!
//! Cell-level editing lives in [`crate::tools::cell`]. Reads never mutate;
//! `notebook_read` returns the file revision that every mutating tool
//! requires.

use std::path::{Path, PathBuf};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::json;

use crate::error::Error;
use crate::fs;
use crate::notebook::{self, AnyNotebook, Format as NotebookFormat, Notebook};
use crate::render::{self, Detail, Format, Window, WithOutputs};
use crate::server::Tpmcp;
use crate::tools::{CellType, blocking, mutation_result, read_notebook, respond};

mod read;

use read::{ReadView, run_validation};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NotebookReadArgs {
    /// Path to the notebook file (.ipynb or marimo .py).
    pub path: PathBuf,
    /// "brief" (default) truncates long cells; "full" numbers source lines
    /// (needed for cell_set_source line ranges) and renders outputs fully;
    /// "outline" lists each cell's index, type, first line and line count
    /// without bodies.
    #[serde(default)]
    pub detail: Detail,
    /// "none", "summary" (default) or "full" output rendering.
    #[serde(default)]
    pub with_outputs: WithOutputs,
    /// First cell index to render (0-based, default 0).
    #[serde(default)]
    pub start: Option<usize>,
    /// Maximum number of cells to render (default: all).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Cap on each rendered output payload in full mode, in bytes (default
    /// 4000; pass a large number to see more). The markdown view truncates
    /// with a note; the JSON view returns outputs raw, so use with_outputs
    /// there to control size. To find things in outputs, use notebook_search.
    #[serde(default)]
    pub output_limit: Option<usize>,
    /// Validation to attach: "none" (default), "check" (notebook_check
    /// structural findings), or "check+lint" (also ruff lint diagnostics).
    /// Findings carry 0-based cell indexes. If ruff cannot lint the file,
    /// the read still succeeds and reports the reason instead of lint
    /// issues. Use notebook_check alone to validate without reading content.
    #[serde(default)]
    pub diagnostics: DiagnosticsLevel,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

/// Where [`Tpmcp::notebook_search`] looks: cell sources, stored outputs, or
/// both. Variants carry no doc comments on purpose: the `search_scope` param
/// description is the schema text callers see.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SearchScope {
    /// Cell sources.
    #[default]
    Source,
    /// Stored outputs (Jupyter only; marimo files store none).
    Output,
    /// Both sources and outputs.
    Both,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NotebookSearchArgs {
    /// Path to the notebook file.
    pub path: PathBuf,
    /// Case-insensitive substring to look for. With only_failed it searches
    /// inside the failures; alone it searches the scope; required unless
    /// only_failed is set.
    #[serde(default)]
    pub query: Option<String>,
    /// Where to look: "source" (default), "output" (stored outputs — Jupyter
    /// only, marimo files store none) or "both".
    #[serde(default)]
    pub search_scope: SearchScope,
    /// Only cells that failed when the notebook ran (stored error outputs):
    /// the cheap answer to "which cells dropped and why", with the error
    /// name, value and a capped traceback.
    #[serde(default)]
    pub only_failed: bool,
    /// Only cells of this type.
    #[serde(default)]
    pub cell_type: Option<CellType>,
    /// First matching cell to return (0-based over the match list).
    #[serde(default)]
    pub start: Option<usize>,
    /// Maximum number of matching cells to return (default: all).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

/// Validation level attached to a `notebook_read` response. Variants carry
/// no doc comments on purpose: the `diagnostics` param description is the
/// single source of meaning and the schema stays a compact enum.
#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
pub enum DiagnosticsLevel {
    #[default]
    #[serde(rename = "none")]
    None,
    #[serde(rename = "check")]
    Check,
    #[serde(rename = "check+lint")]
    CheckLint,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NewCell {
    /// Type of the cell to create.
    pub cell_type: CellType,
    /// Initial cell source (may be empty).
    #[serde(default)]
    pub source: String,
    /// Read the initial source from this file instead (the stream path for
    /// huge payloads); mutually exclusive with source.
    #[serde(default)]
    pub source_file: Option<PathBuf>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NotebookCreateArgs {
    /// Path of the notebook file to create; must not exist yet. The
    /// extension picks the format: .ipynb Jupyter, .py marimo, .md a
    /// markdown export of from_path.
    pub path: PathBuf,
    /// Optional initial cells for a fresh notebook; an empty notebook is
    /// created when omitted. Mutually exclusive with from_path.
    #[serde(default)]
    pub cells: Option<Vec<NewCell>>,
    /// Path of an existing notebook to convert into the new file (.ipynb →
    /// marimo .py or .md, marimo .py → .ipynb or .md). Mutually exclusive
    /// with cells. Nothing is dropped silently: magics, raw cells and
    /// binary outputs are kept or reported as per-cell warnings.
    #[serde(default)]
    pub from_path: Option<PathBuf>,
    /// Revision of the `from_path` source (from a read). Providing it turns
    /// the create into a move: the target is written and verified first, then
    /// the source is deleted. The create is refused with a conflict error
    /// before anything is written if the source no longer hashes to this
    /// revision, so nothing is ever deleted unverified. Omit to keep the
    /// source (a plain conversion). Requires `from_path`.
    #[serde(default)]
    pub source_revision: Option<String>,
    /// Override the format inferred from the extension: "jupyter" or
    /// "marimo" (not valid for a .md export target).
    #[serde(default)]
    pub notebook_format: Option<NotebookFormat>,
    /// PEP 723 dependencies for a fresh marimo notebook: each entry is a
    /// dependency spec (e.g. "numpy" or "pandas>=2"). Emitted in a
    /// `# /// script` block above `import marimo`. marimo targets only;
    /// Jupyter targets and from_path conversions reject it.
    #[serde(default)]
    pub dependencies: Option<Vec<String>>,
    /// PEP 723 `requires-python` spec for a fresh marimo notebook (e.g.
    /// ">=3.12"), emitted in the same `# /// script` block.
    #[serde(default)]
    pub requires_python: Option<String>,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[tool_router(router = notebook_router, vis = "pub")]
impl Tpmcp {
    #[tool(
        title = "Read Notebook",
        description = "Read a notebook (Jupyter .ipynb or marimo .py) as indexed, token-efficient \
                       markdown or JSON, with optional outline, content search and diagnostics \
                       (see the detail, query and diagnostics parameters). Returns the file \
                       revision required by all mutating tools. Never read notebooks as raw JSON \
                       or raw Python; use this tool. A file that is not a notebook (a .md export, \
                       a corrupt .ipynb, any readable text) comes back as raw numbered lines \
                       under an explicit fallback marker — there start/limit page through lines \
                       and query filters them. On large notebooks prefer detail=outline or \
                       query over reading every cell.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn notebook_read(
        &self,
        Parameters(args): Parameters<NotebookReadArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            // A file that is not a notebook is not a read failure: `.md`
            // exports and corrupt `.ipynb` files come back as raw text under
            // an explicit fallback marker (see `read::fallback`), because a
            // caller without file tools otherwise cannot see them at all.
            let snapshot = fs::read(&args.path)?;
            let notebook = match notebook::open(&snapshot.content) {
                Ok(notebook) => notebook,
                Err(reason) => {
                    return read::fallback(
                        &args.path,
                        &snapshot,
                        &reason,
                        None,
                        args.start,
                        args.limit,
                        args.format,
                    );
                }
            };
            let fs::Snapshot { content, revision } = snapshot;
            let path = args.path.display().to_string();
            let validation = run_validation(&args, &notebook, content, &revision);
            let view = ReadView {
                path: &path,
                revision: &revision,
                notebook: &notebook,
                detail: args.detail,
                with_outputs: args.with_outputs,
                window: Window::resolve(notebook.cells().len(), args.start, args.limit),
                output_limit: args.output_limit,
                validation: validation.as_ref(),
            };
            Ok(respond(args.format, || view.markdown(), || view.json()))
        })
        .await
    }

    #[tool(
        title = "Search Notebook",
        description = "Find cells without reading the notebook: a case-insensitive substring in \
                       cell sources and/or stored outputs (`search_scope`), or just the cells \
                       that failed when the notebook ran (`only_failed`) with the error name, \
                       value and a capped traceback. Returns only the matching cells, each with \
                       the location of its matches (`source:N`, `output N:line`) — the cheap \
                       answer to \"where does this appear?\" and \"what dropped?\". A file that is \
                       not a notebook (a .md export, a corrupt .ipynb) is searched as raw text \
                       lines under an explicit fallback marker. marimo files store no outputs, \
                       so output scope and only_failed there report that instead of pretending. \
                       start/limit page through the matches.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn notebook_search(
        &self,
        Parameters(args): Parameters<NotebookSearchArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            if args.query.is_none() && !args.only_failed {
                return Err(Error::invalid_input("provide a query and/or only_failed"));
            }
            if args
                .query
                .as_ref()
                .is_some_and(|query| query.trim().is_empty())
            {
                return Err(Error::invalid_input("query must not be empty"));
            }
            let (sources, outputs) = match args.search_scope {
                SearchScope::Source => (true, false),
                SearchScope::Output => (false, true),
                SearchScope::Both => (true, true),
            };
            let snapshot = fs::read(&args.path)?;
            let notebook = match notebook::open(&snapshot.content) {
                Ok(notebook) => notebook,
                // A non-notebook is still searchable: its raw lines.
                Err(reason) => {
                    return read::fallback(
                        &args.path,
                        &snapshot,
                        &reason,
                        args.query.as_deref(),
                        args.start,
                        args.limit,
                        args.format,
                    );
                }
            };
            let spec = render::search::SearchSpec {
                query: args.query,
                sources,
                outputs,
                only_failed: args.only_failed,
                cell_type: args.cell_type.map(|kind| kind.as_str().to_string()),
            };
            let mut search =
                render::search::search(notebook.cells(), &spec, args.start, args.limit);
            // marimo stores no outputs: say so instead of reporting a clean
            // "nothing failed" or "no output matches".
            if (outputs || spec.only_failed) && matches!(notebook, notebook::AnyNotebook::Marimo(_))
            {
                search.note = Some(
                    "marimo files store no outputs: output-scope matching and only_failed \
                     have nothing to look at here"
                        .to_string(),
                );
            }
            let path = args.path.display().to_string();
            Ok(respond(
                args.format,
                || render::search::search_markdown(&path, &snapshot.revision, &notebook, &search),
                || render::search::search_json(&path, &snapshot.revision, &notebook, &search),
            ))
        })
        .await
    }

    #[tool(
        title = "Create or Convert Notebook",
        description = "Create a new notebook file, or convert an existing one via from_path \
                       (.ipynb to marimo .py, or either to markdown). The extension picks the \
                       format (.ipynb Jupyter nbformat 4.5, .py marimo, .md markdown export); \
                       notebook_format overrides Jupyter/marimo. Fails if the file already \
                       exists; use the cell tools to modify existing notebooks. Conversions \
                       never drop content silently: cells that cannot convert exactly are kept \
                       and reported as per-cell warnings. Pass `source_revision` (the source's \
                       current revision) with `from_path` to make it a move: the target is \
                       written and verified first, then the source is deleted; a stale \
                       revision refuses the create before anything is written. A fresh marimo \
                       .py with no cells is a \
                       0-cell file, which marimo flags as having errors until a cell is added; \
                       prefer providing at least one initial cell. Seed initial code cells with \
                       explicit, idempotent setup (imports, paths, seeds, dtypes) so any cell \
                       can re-run later without repair.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn notebook_create(
        &self,
        Parameters(args): Parameters<NotebookCreateArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            if args.from_path.is_some() && args.cells.is_some() {
                return Err(Error::invalid_input(
                    "from_path and cells are mutually exclusive",
                ));
            }
            let has_pep723 = args.dependencies.is_some() || args.requires_python.is_some();
            if has_pep723 && args.from_path.is_some() {
                return Err(Error::invalid_input(
                    "dependencies and requires_python apply only to a fresh marimo notebook; \
                     they cannot be combined with from_path",
                ));
            }
            if args.from_path.is_none() && has_extension(&args.path, "md") {
                return Err(Error::invalid_input("a .md target requires from_path"));
            }
            if args.notebook_format.is_some() && has_extension(&args.path, "md") {
                return Err(Error::invalid_input(
                    "notebook_format does not apply to a .md export target",
                ));
            }
            if args.source_revision.is_some() && args.from_path.is_none() {
                return Err(Error::invalid_input("source_revision requires from_path"));
            }
            // A move must never delete a source the caller has not seen: the
            // revision is checked before anything is written, so a stale one
            // refuses the whole create under the usual conflict contract.
            if let (Some(from_path), Some(expected)) = (&args.from_path, &args.source_revision) {
                let current = fs::read(from_path)?.revision;
                if current != *expected {
                    return Err(Error::Conflict {
                        path: from_path.clone(),
                        expected: expected.clone(),
                        current,
                    });
                }
            }
            let (content, cell_count, warnings) = match &args.from_path {
                Some(from_path) => convert_notebook(from_path, &args.path, args.notebook_format)?,
                None => {
                    let format = match args.notebook_format {
                        Some(format) => format,
                        None => notebook::format_from_extension(&args.path).ok_or_else(|| {
                            Error::invalid_input(format!(
                                "cannot infer notebook format from {}; use a .ipynb or .py path, \
                                 or pass notebook_format",
                                args.path.display()
                            ))
                        })?,
                    };
                    if has_pep723 && format != NotebookFormat::Marimo {
                        return Err(Error::invalid_input(
                            "dependencies and requires_python apply only to marimo notebooks",
                        ));
                    }
                    let mut notebook = notebook::create_with_dependencies(
                        format,
                        args.dependencies.as_deref(),
                        args.requires_python.as_deref(),
                    );
                    for new_cell in args.cells.unwrap_or_default() {
                        let source = if let Some(path) = new_cell.source_file {
                            if !new_cell.source.is_empty() {
                                return Err(Error::invalid_input(
                                    "source and source_file are mutually exclusive",
                                ));
                            }
                            fs::read(&path)?.content
                        } else {
                            new_cell.source
                        };
                        let index = notebook.cells().len();
                        notebook.insert(index, new_cell.cell_type.as_str(), &source)?;
                    }
                    // cell_count reports the requested cells; a scaffold cell
                    // tpmcp adds (the `mo` import marimo markdown cells need)
                    // is disclosed in the warnings instead.
                    let cell_count = notebook.cells().len();
                    let warnings = canonicalize_new_notebook(&mut notebook)?;
                    (notebook.serialize(), cell_count, warnings)
                }
            };
            let new_revision = fs::with_file_lock(&args.path, || {
                // Existence check INSIDE the lock, so concurrent creators are
                // serialized and exactly one wins; stat-based, so entries
                // fs::read cannot see (non-UTF-8, oversized, permission-denied,
                // directories, dangling symlinks) are refused instead of
                // silently clobbered.
                if fs::exists(&args.path) {
                    return Err(Error::AlreadyExists {
                        path: args.path.clone(),
                    });
                }
                fs::create(&args.path, &content)
            })?;
            let summary = match &args.from_path {
                Some(from_path) => format!(
                    "created notebook with {cell_count} cells from {}",
                    from_path.display()
                ),
                None => format!("created notebook with {cell_count} cells"),
            };
            // The move half of `source_revision`: only after the target exists
            // and is verified is the source removed, guarded by the same
            // revision. If the source changed in flight or cannot be removed,
            // the target stands and the answer says so — never an error that
            // hides the created file, never silence about the leftover.
            let mut source_removed = None;
            let mut source_note = None;
            if let (Some(from_path), Some(expected)) = (&args.from_path, &args.source_revision) {
                match fs::delete(from_path, Some(expected)) {
                    Ok(_) => source_removed = Some(true),
                    Err(Error::Conflict { .. }) => {
                        source_removed = Some(false);
                        source_note = Some(format!(
                            "the source changed during the move and was NOT removed: {}",
                            from_path.display()
                        ));
                    }
                    Err(error) => {
                        source_removed = Some(false);
                        source_note = Some(format!("the source could not be removed: {error}"));
                    }
                }
            }
            Ok(mutation_result(
                args.format,
                summary,
                &args.path,
                &new_revision,
                || {
                    let mut parts = Vec::new();
                    if !warnings.is_empty() {
                        let lines: Vec<String> = warnings
                            .iter()
                            .map(|warning| format!("- {warning}"))
                            .collect();
                        parts.push(format!("warnings:\n{}", lines.join("\n")));
                    }
                    if let Some(note) = &source_note {
                        parts.push(format!("note: {note}"));
                    }
                    (!parts.is_empty()).then(|| parts.join("\n\n"))
                },
                || {
                    json!({
                        "cell_count": cell_count,
                        "warnings": &warnings,
                        "source_removed": source_removed,
                        "note": source_note,
                    })
                },
            ))
        })
        .await
    }
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension().and_then(|found| found.to_str()) == Some(extension)
}

/// Convert the notebook at `from_path` into the format the target `path`
/// (or `format_override`) selects. Returns the file content, the cell
/// count, and per-cell warnings; nothing is ever dropped silently.
fn convert_notebook(
    from_path: &Path,
    path: &Path,
    format_override: Option<NotebookFormat>,
) -> Result<(String, usize, Vec<String>), Error> {
    let (_, source) = read_notebook(from_path)?;
    if has_extension(path, "md") {
        let (content, mut warnings) = render::markdown_export(&source);
        if let AnyNotebook::Marimo(marimo) = &source {
            warnings.extend(marimo.conversion_warnings());
        }
        return Ok((content, source.cells().len(), warnings));
    }
    let target = match format_override {
        Some(format) => format,
        None => notebook::format_from_extension(path).ok_or_else(|| {
            Error::invalid_input(format!(
                "cannot infer a target format from {}; use a .ipynb or .py path, \
                 or pass notebook_format",
                path.display()
            ))
        })?,
    };
    let mut notebook = notebook::create(target);
    let mut warnings = match (&source, target) {
        (AnyNotebook::Marimo(marimo), _) => marimo.conversion_warnings(),
        (AnyNotebook::Jupyter(_), NotebookFormat::Marimo) => jupyter_metadata_warnings(&source),
        (AnyNotebook::Jupyter(_), NotebookFormat::Jupyter) => Vec::new(),
    };
    let mut dropped_outputs = false;
    for (index, cell) in source.cells().iter().enumerate() {
        let text = cell.source_text();
        let cell_type = match target {
            NotebookFormat::Marimo => {
                if cell.cell_type == "raw" {
                    warnings.push(format!(
                        "cell {index}: raw cells have no marimo equivalent; kept as a code cell"
                    ));
                }
                // Magics are a code-cell concept: markdown text starting
                // with `!` or `%` (e.g. an image `![alt](...)`) is content.
                if !cell.is_markdown()
                    && let Some(magic) = first_magic(&text)
                {
                    warnings.push(format!(
                        "cell {index}: Jupyter magic `{magic}` has no marimo equivalent; kept \
                         verbatim, marimo will report a syntax error"
                    ));
                }
                if !cell.outputs().is_empty() {
                    dropped_outputs = true;
                }
                if cell.is_markdown() {
                    "markdown"
                } else {
                    "code"
                }
            }
            NotebookFormat::Jupyter => {
                if let Some(kind) = cell.marimo_kind()
                    && kind != "cell"
                {
                    warnings.push(format!(
                        "cell {index}: marimo {kind} block exported as a plain code cell"
                    ));
                }
                cell.cell_type.as_str()
            }
        };
        notebook.insert(notebook.cells().len(), cell_type, &text)?;
    }
    if dropped_outputs {
        warnings.push("outputs and execution counts are not stored in marimo files".to_string());
    }
    preserve_jupyter_extras(&source, &mut notebook);
    // cell_count reports the converted source cells; a scaffold cell tpmcp
    // adds (the `mo` import marimo markdown cells need) is disclosed in the
    // warnings instead.
    let cell_count = notebook.cells().len();
    warnings.extend(canonicalize_new_notebook(&mut notebook)?);
    Ok((notebook.serialize(), cell_count, warnings))
}

/// Final shaping of a freshly built notebook: marimo files gain the `mo`
/// definition markdown cells need at runtime (returning the warning that
/// discloses the scaffold cell), and their generated cells get the return
/// statements and signatures marimo's own serializer would write. A no-op
/// for Jupyter targets.
fn canonicalize_new_notebook(notebook: &mut AnyNotebook) -> Result<Vec<String>, Error> {
    match notebook {
        AnyNotebook::Jupyter(_) => Ok(Vec::new()),
        AnyNotebook::Marimo(marimo) => {
            let warning = marimo.ensure_mo_definition()?;
            marimo.canonicalize_edited_cells();
            Ok(warning.into_iter().collect())
        }
    }
}

/// The Jupyter metadata layers a marimo target cannot carry, warned once per
/// layer when actually present. Direct `extra` reads: no typed accessors
/// exist for metadata or attachments.
fn jupyter_metadata_warnings(source: &AnyNotebook) -> Vec<String> {
    let AnyNotebook::Jupyter(jupyter) = source else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    let cells_carry_metadata = jupyter.cells.iter().any(|cell| {
        carries_content(cell.extra.get("metadata"))
            || carries_content(cell.extra.get("attachments"))
    });
    if cells_carry_metadata {
        warnings.push(
            "cell metadata (tags, attachments, ...) is not stored in marimo files".to_string(),
        );
    }
    if carries_content(jupyter.extra.get("metadata")) {
        warnings.push(
            "notebook metadata (kernelspec, language_info, ...) is not stored in marimo files"
                .to_string(),
        );
    }
    warnings
}

/// Whether a passthrough field holds anything worth warning about: a
/// non-empty object, or an unexpected non-object shape (still dropped).
fn carries_content(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Object(map)) => !map.is_empty(),
        Some(_) => true,
        None => false,
    }
}

/// Same-format Jupyter conversion is a rebuild, not a strip-down: carry the
/// per-cell passthrough fields (outputs, execution counts, metadata, ids)
/// and the notebook metadata across. Values move as already-parsed JSON and
/// are never interpreted; the fresh target keeps its own nbformat 4.5
/// version fields. No-op unless both sides are Jupyter.
fn preserve_jupyter_extras(source: &AnyNotebook, target: &mut AnyNotebook) {
    let (AnyNotebook::Jupyter(source_notebook), AnyNotebook::Jupyter(target_notebook)) =
        (source, target)
    else {
        return;
    };
    for (source_cell, target_cell) in source_notebook
        .cells
        .iter()
        .zip(target_notebook.cells.iter_mut())
    {
        for (key, value) in &source_cell.extra {
            if key != "cell_type" && key != "source" {
                target_cell.extra.insert(key.clone(), value.clone());
            }
        }
    }
    if let Some(metadata) = source_notebook.extra.get("metadata") {
        target_notebook
            .extra
            .insert("metadata".to_string(), metadata.clone());
    }
}

/// The first Jupyter magic (`%`) or shell (`!`) line of a cell source.
fn first_magic(source: &str) -> Option<&str> {
    source.lines().find_map(|line| {
        let trimmed = line.trim_start();
        (trimmed.starts_with('%') || trimmed.starts_with('!'))
            .then_some(trimmed.split_whitespace().next().unwrap_or(trimmed))
    })
}
