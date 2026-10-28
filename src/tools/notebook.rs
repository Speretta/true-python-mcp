//! Notebook-level tools: reading and creating notebooks (`.ipynb` and
//! marimo `.py`).
//!
//! Cell-level editing lives in [`crate::tools::cell`]. Reads never mutate;
//! `notebook_read` returns the file revision that every mutating tool
//! requires.

use std::path::Path;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use serde_json::json;

use crate::error::Error;
use crate::fs;
use crate::notebook::{self, Format as NotebookFormat, Notebook};
use crate::render::{self, Window};
use crate::server::Tpmcp;
use crate::tools::{blocking, mutation_result, read_notebook, respond};

mod args;
mod read;

pub use args::{
    DiagnosticsLevel, NewCell, NotebookCreateArgs, NotebookReadArgs, NotebookSearchArgs,
    SearchScope,
};
use read::{ReadView, run_validation};

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
            let (content, cell_count, warnings) =
                match &args.from_path {
                    Some(from_path) => {
                        let (_, source) = read_notebook(from_path)?;
                        if has_extension(&args.path, "md") {
                            let (content, warnings) = source.export_markdown();
                            (content, source.cells().len(), warnings)
                        } else {
                            let target = match args.notebook_format {
                            Some(format) => format,
                            None => notebook::format_from_extension(&args.path).ok_or_else(|| {
                                Error::invalid_input(format!(
                                    "cannot infer a target format from {}; use a .ipynb or .py \
                                     path, or pass notebook_format",
                                    args.path.display()
                                ))
                            })?,
                        };
                            let converted = source.convert(target)?;
                            (converted.content, converted.cell_count, converted.warnings)
                        }
                    }
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
                        let warnings = notebook.finalize_creation()?;
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
