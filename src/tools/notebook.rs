//! Notebook-level tools: reading and creating `.ipynb` files.
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
use crate::server::Tpmcp;
use crate::tools::{CellType, blocking, mutation_result, read_notebook, respond};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NotebookReadArgs {
    /// Path to the .ipynb file.
    pub path: PathBuf,
    /// "brief" (default) truncates long cells; "full" numbers source lines
    /// (needed for cell_set_source line ranges) and renders outputs fully.
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
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
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
        description = "Read a Jupyter notebook (.ipynb) as indexed, token-efficient markdown or JSON. \
                       Returns the file revision required by all mutating tools. Never read .ipynb \
                       files as raw JSON text."
    )]
    async fn notebook_read(
        &self,
        Parameters(args): Parameters<NotebookReadArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let (snapshot, notebook) = read_notebook(&args.path)?;
            let path = args.path.display().to_string();
            let revision = snapshot.revision;
            let window = Window::resolve(notebook.cells().len(), args.start, args.limit);
            Ok(respond(
                args.format,
                || {
                    render::notebook_markdown(
                        &path,
                        &revision,
                        &notebook,
                        args.detail,
                        args.with_outputs,
                        window,
                    )
                },
                || render::notebook_json(&path, &revision, &notebook, args.with_outputs, window),
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
                notebook
                    .insert(index, new_cell.cell_type.as_str(), &new_cell.source)
                    .map_err(Error::invalid_input)?;
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
                None,
                json!({"cell_count": cell_count}),
            ))
        })
        .await
    }
}
