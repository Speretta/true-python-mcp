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
    /// Override the format inferred from the extension: "jupyter" or
    /// "marimo" (not valid for a .md export target).
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
                       query over reading every cell.",
        annotations(read_only_hint = true)
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
        description = "Create a new notebook file, or convert an existing one via from_path \
                       (.ipynb to marimo .py, or either to markdown). The extension picks the \
                       format (.ipynb Jupyter nbformat 4.5, .py marimo, .md markdown export); \
                       notebook_format overrides Jupyter/marimo. Fails if the file already \
                       exists; use the cell tools to modify existing notebooks. Conversions \
                       never drop content silently: cells that cannot convert exactly are kept \
                       and reported as per-cell warnings.",
        annotations(destructive_hint = false)
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
            if args.from_path.is_none() && has_extension(&args.path, "md") {
                return Err(Error::invalid_input("a .md target requires from_path"));
            }
            if args.notebook_format.is_some() && has_extension(&args.path, "md") {
                return Err(Error::invalid_input(
                    "notebook_format does not apply to a .md export target",
                ));
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
                    let mut notebook = notebook::create(format);
                    for new_cell in args.cells.unwrap_or_default() {
                        let index = notebook.cells().len();
                        notebook.insert(index, new_cell.cell_type.as_str(), &new_cell.source)?;
                    }
                    let cell_count = notebook.cells().len();
                    (notebook.serialize(), cell_count, Vec::new())
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
            Ok(mutation_result(
                args.format,
                summary,
                &args.path,
                &new_revision,
                || {
                    (!warnings.is_empty()).then(|| {
                        let lines: Vec<String> = warnings
                            .iter()
                            .map(|warning| format!("- {warning}"))
                            .collect();
                        format!("warnings:\n{}", lines.join("\n"))
                    })
                },
                || json!({"cell_count": cell_count, "warnings": &warnings}),
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
        let (content, warnings) = render::markdown_export(&source);
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
    let mut warnings = Vec::new();
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
                if let Some(magic) = first_magic(&text) {
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
    let cell_count = notebook.cells().len();
    Ok((notebook.serialize(), cell_count, warnings))
}

/// The first Jupyter magic (`%`) or shell (`!`) line of a cell source.
fn first_magic(source: &str) -> Option<&str> {
    source.lines().find_map(|line| {
        let trimmed = line.trim_start();
        (trimmed.starts_with('%') || trimmed.starts_with('!'))
            .then_some(trimmed.split_whitespace().next().unwrap_or(trimmed))
    })
}
