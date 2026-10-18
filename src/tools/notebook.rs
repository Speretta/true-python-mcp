//! Notebook-level tools: reading and creating notebooks (`.ipynb` and
//! marimo `.py`).
//!
//! Cell-level editing lives in [`crate::tools::cell`]. Reads never mutate;
//! `notebook_read` returns the file revision that every mutating tool
//! requires.

use std::path::PathBuf;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::json;

use crate::error::Error;
use crate::fs;
use crate::notebook::{self, Format as NotebookFormat, Notebook};
use crate::render::{self, Detail, Format, Window, WithOutputs};
use crate::ruff::{self, SafeFixes};
use crate::server::Tpmcp;
use crate::tools::{CellType, blocking, mutation_result, read_notebook, respond};

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
    /// Case-insensitive substring search over cell sources. Returns only
    /// the matching cells with their index, type and matching lines;
    /// start/limit page through the matches.
    #[serde(default)]
    pub query: Option<String>,
    /// First cell index to render (0-based, default 0); with query, the
    /// first matching cell to render.
    #[serde(default)]
    pub start: Option<usize>,
    /// Maximum number of cells to render (default: all); with query, the
    /// maximum number of matching cells.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Validation to attach: "none" (default), "check" (notebook_check
    /// structural findings), or "check+lint" (also ruff lint diagnostics).
    /// Findings carry 0-based cell indexes. Use notebook_check alone to
    /// validate without reading content.
    #[serde(default)]
    pub diagnostics: DiagnosticsLevel,
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
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NotebookCreateArgs {
    /// Path of the notebook file to create; must not exist yet. The extension
    /// picks the format (.ipynb → Jupyter, .py → marimo) unless notebook_format
    /// is given.
    pub path: PathBuf,
    /// Optional initial cells; an empty notebook is created when omitted.
    #[serde(default)]
    pub cells: Option<Vec<NewCell>>,
    /// Override the format inferred from the extension: "jupyter" or "marimo".
    #[serde(default)]
    pub notebook_format: Option<NotebookFormat>,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[tool_router(router = notebook_router, vis = "pub")]
impl Tpmcp {
    #[tool(
        description = "Read a notebook (Jupyter .ipynb or marimo .py) as indexed, token-efficient \
                       markdown or JSON, with optional outline, content search and diagnostics \
                       (see the detail, query and diagnostics parameters). Returns the file \
                       revision required by all mutating tools. Never read notebooks as raw JSON \
                       or raw Python; use this tool. On large notebooks prefer detail=outline or \
                       query over reading every cell."
    )]
    async fn notebook_read(
        &self,
        Parameters(args): Parameters<NotebookReadArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let (snapshot, notebook) = read_notebook(&args.path)?;
            let fs::Snapshot { content, revision } = snapshot;
            let path = args.path.display().to_string();

            // Optional validation runs on the same snapshot, outside any
            // lock (reads never lock): the revision reported is the one
            // these findings were computed from. `content` moves into ruff
            // only for check+lint — no second read, no clone.
            let validation = match args.diagnostics {
                DiagnosticsLevel::None => None,
                DiagnosticsLevel::Check => Some((notebook.check(), None)),
                DiagnosticsLevel::CheckLint => {
                    let outcome = ruff::check(
                        &args.path,
                        SafeFixes::ReportOnly,
                        fs::Snapshot {
                            content,
                            revision: revision.clone(),
                        },
                    )?;
                    Some((notebook.check(), Some(outcome.diagnostics)))
                }
            };

            let found = match &args.query {
                Some(query) if query.trim().is_empty() => {
                    return Err(Error::invalid_input("query must not be empty"));
                }
                Some(query) => Some(render::search::search(
                    notebook.cells(),
                    query,
                    args.start,
                    args.limit,
                )),
                None => None,
            };
            let window = Window::resolve(notebook.cells().len(), args.start, args.limit);

            Ok(respond(
                args.format,
                || {
                    let mut out = match &found {
                        Some(found) => {
                            render::search::query_markdown(&path, &revision, &notebook, found)
                        }
                        None => render::notebook_markdown(
                            &path,
                            &revision,
                            &notebook,
                            args.detail,
                            args.with_outputs,
                            window,
                        ),
                    };
                    if let Some((findings, lint)) = &validation {
                        out.push_str(&render::diagnostics_markdown(findings, lint.as_deref()));
                    }
                    out
                },
                || {
                    let mut value = match &found {
                        Some(found) => {
                            render::search::query_json(&path, &revision, &notebook, found)
                        }
                        None => render::notebook_json(
                            &path,
                            &revision,
                            &notebook,
                            args.detail,
                            args.with_outputs,
                            window,
                        ),
                    };
                    if let Some((findings, lint)) = &validation {
                        value["findings"] = json!(findings);
                        if let Some(lint) = lint {
                            value["lint"] = json!(lint);
                        }
                    }
                    value
                },
            ))
        })
        .await
    }

    #[tool(
        description = "Create a new empty notebook. The extension picks the format: .ipynb for \
                       Jupyter (nbformat 4.5) or .py for marimo; pass notebook_format to \
                       override. Fails if the file already exists; use the cell tools to modify \
                       existing notebooks."
    )]
    async fn notebook_create(
        &self,
        Parameters(args): Parameters<NotebookCreateArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
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
            let mut notebook = notebook::create(format);
            for new_cell in args.cells.unwrap_or_default() {
                let index = notebook.cells().len();
                notebook.insert(index, new_cell.cell_type.as_str(), &new_cell.source)?;
            }
            let cell_count = notebook.cells().len();
            let content = notebook.serialize();
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
            Ok(mutation_result(
                args.format,
                format!("created notebook with {cell_count} cells"),
                &args.path,
                &new_revision,
                || None,
                || json!({"cell_count": cell_count}),
            ))
        })
        .await
    }
}
