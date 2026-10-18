//! Cell-level editing tools. Every tool runs under the per-path lock,
//! requires the current revision, and returns the new revision so edits can
//! be chained without re-reading.

use std::path::PathBuf;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::error::Error;
use crate::fs;
use crate::notebook::{Cell, Notebook};
use crate::render::{self, Detail, Format, WithOutputs};
use crate::server::Tpmcp;
use crate::tools::{
    CellType, apply_edit, blocking, check_cell_index, mutation_result, with_notebook,
};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CellEditArgs {
    /// Path to the notebook file.
    pub path: PathBuf,
    /// 0-based cell index (see notebook_read).
    pub cell: usize,
    /// Exact text to find inside the cell source; must be unique unless
    /// replace_all is set.
    pub old_string: String,
    /// Replacement text.
    pub new_string: String,
    /// Replace every occurrence instead of requiring uniqueness.
    #[serde(default)]
    pub replace_all: bool,
    /// Current file revision (from notebook_read or a previous mutation).
    pub revision: String,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CellSetSourceArgs {
    /// Path to the notebook file.
    pub path: PathBuf,
    /// 0-based cell index (see notebook_read).
    pub cell: usize,
    /// New source: the whole cell, or the replacement for start_line..=end_line.
    pub source: String,
    /// First 1-based line to replace; requires end_line. Omit both to
    /// replace the entire cell source.
    #[serde(default)]
    pub start_line: Option<usize>,
    /// Last 1-based line to replace (inclusive); requires start_line.
    #[serde(default)]
    pub end_line: Option<usize>,
    /// Current file revision (from notebook_read or a previous mutation).
    pub revision: String,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CellInsertArgs {
    /// Path to the notebook file.
    pub path: PathBuf,
    /// 0-based position for the new cell; defaults to appending at the end.
    #[serde(default)]
    pub index: Option<usize>,
    /// Type of the new cell.
    pub cell_type: CellType,
    /// Initial source for the new cell.
    #[serde(default)]
    pub source: String,
    /// Current file revision (from notebook_read or a previous mutation).
    pub revision: String,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CellRefArgs {
    /// Path to the notebook file.
    pub path: PathBuf,
    /// 0-based cell index (see notebook_read).
    pub cell: usize,
    /// Current file revision (from notebook_read or a previous mutation).
    pub revision: String,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CellMoveArgs {
    /// Path to the notebook file.
    pub path: PathBuf,
    /// 0-based index of the cell to move.
    pub from: usize,
    /// 0-based destination index (after removal of the moved cell).
    pub to: usize,
    /// Current file revision (from notebook_read or a previous mutation).
    pub revision: String,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CellSetTypeArgs {
    /// Path to the notebook file.
    pub path: PathBuf,
    /// 0-based cell index (see notebook_read).
    pub cell: usize,
    /// New cell type.
    pub cell_type: CellType,
    /// Current file revision (from notebook_read or a previous mutation).
    pub revision: String,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CellSetMetadataArgs {
    /// Path to the notebook file.
    pub path: PathBuf,
    /// 0-based cell index (see notebook_read).
    pub cell: usize,
    /// The complete new metadata object for the cell.
    pub metadata: Map<String, Value>,
    /// Current file revision (from notebook_read or a previous mutation).
    pub revision: String,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CellClearOutputsArgs {
    /// Path to the notebook file.
    pub path: PathBuf,
    /// 0-based cell index; omit to clear the outputs of every code cell.
    #[serde(default)]
    pub cell: Option<usize>,
    /// Current file revision (from notebook_read or a previous mutation).
    pub revision: String,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[tool_router(router = cell_router, vis = "pub")]
impl Tpmcp {
    #[tool(
        title = "Edit Cell",
        description = "Surgically edit one notebook cell: replace an exact old_string with \
                       new_string inside the cell source (like an editor's edit tool). \
                       old_string must be unique unless replace_all is set. Returns a unified \
                       diff and the new revision. Outputs and metadata are preserved.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn cell_edit(
        &self,
        Parameters(args): Parameters<CellEditArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let (diff, new_revision) = with_notebook(&args.path, &args.revision, |notebook| {
                check_cell_index(notebook, args.cell)?;
                let source = notebook.cells()[args.cell].source_text();
                let updated = apply_edit(
                    &source,
                    &args.old_string,
                    &args.new_string,
                    args.replace_all,
                    &format!("cell {}", args.cell),
                )?;
                // Render the diff before the mutation: `source` borrows the
                // notebook, and the write needs it mutably.
                let diff = render::unified_diff(&source, &updated);
                notebook.set_source(args.cell, &updated)?;
                Ok(diff)
            })?;
            Ok(mutation_result(
                args.format,
                format!("edited cell {}", args.cell),
                &args.path,
                &new_revision,
                || Some(format!("```diff\n{diff}```")),
                || json!({"cell": args.cell, "diff": diff.as_str()}),
            ))
        })
        .await
    }

    #[tool(
        title = "Set Cell Source",
        description = "Set a notebook cell's source: the whole cell, or just the 1-based \
                       inclusive line range start_line..=end_line (use notebook_read with \
                       detail=full to see line numbers). Returns the new cell source and \
                       revision. Outputs and metadata are preserved.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn cell_set_source(
        &self,
        Parameters(args): Parameters<CellSetSourceArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let ((cell_type, source), new_revision) =
                with_notebook(&args.path, &args.revision, |notebook| {
                    check_cell_index(notebook, args.cell)?;
                    let updated = match (args.start_line, args.end_line) {
                        // args is owned and no later step needs source: move it.
                        (None, None) => args.source,
                        (Some(start), Some(end)) => {
                            let current = notebook.cells()[args.cell].source_text();
                            fs::replace_line_range(
                                &current,
                                start,
                                end,
                                &fs::logical_lines(&args.source),
                            )
                            .map_err(|message| {
                                Error::invalid_input(format!("cell {}: {message}", args.cell))
                            })?
                        }
                        _ => {
                            return Err(Error::invalid_input(
                                "start_line and end_line must be provided together",
                            ));
                        }
                    };
                    notebook.set_source(args.cell, &updated)?;
                    let cell_type = notebook.cells()[args.cell].cell_type.clone();
                    Ok((cell_type, updated))
                })?;
            let line_count = fs::logical_lines(&source).len();
            let preview = Cell::from_text(cell_type, &source);
            Ok(mutation_result(
                args.format,
                format!("updated source of cell {}", args.cell),
                &args.path,
                &new_revision,
                || {
                    Some(render::cell_markdown(
                        args.cell,
                        &preview,
                        Detail::Full,
                        WithOutputs::None,
                    ))
                },
                || json!({"cell": args.cell, "line_count": line_count, "source": source.as_str()}),
            ))
        })
        .await
    }

    #[tool(
        title = "Insert Cell",
        description = "Insert a new cell of type cell_type (code, markdown, raw) into a \
                       notebook at a 0-based index; omit index to append at the end. source \
                       may be empty. Returns the new revision.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn cell_insert(
        &self,
        Parameters(args): Parameters<CellInsertArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let (index, new_revision) = with_notebook(&args.path, &args.revision, |notebook| {
                let index = args.index.unwrap_or(notebook.cells().len());
                notebook.insert(index, args.cell_type.as_str(), &args.source)?;
                Ok(index)
            })?;
            Ok(mutation_result(
                args.format,
                format!("inserted {} cell at index {index}", args.cell_type.as_str()),
                &args.path,
                &new_revision,
                || None,
                || json!({"index": index, "cell_type": args.cell_type.as_str()}),
            ))
        })
        .await
    }

    #[tool(
        title = "Delete Cell",
        description = "Delete a notebook cell by 0-based index. The cell's source, outputs and \
                       metadata are discarded and every later cell shifts down one index. \
                       Returns the new revision. When unsure, re-read the notebook first to \
                       confirm the index.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn cell_delete(
        &self,
        Parameters(args): Parameters<CellRefArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let (deleted, new_revision) = with_notebook(&args.path, &args.revision, |notebook| {
                notebook.delete(args.cell)
            })?;
            Ok(mutation_result(
                args.format,
                format!("deleted {} cell {}", deleted.cell_type, args.cell),
                &args.path,
                &new_revision,
                || None,
                || json!({"cell": args.cell, "deleted_type": deleted.cell_type}),
            ))
        })
        .await
    }

    #[tool(
        title = "Move Cell",
        description = "Move a notebook cell from index `from` to index `to` (0-based; `to` is \
                       interpreted after removing the cell). Returns the new revision.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn cell_move(
        &self,
        Parameters(args): Parameters<CellMoveArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let (_, new_revision) = with_notebook(&args.path, &args.revision, |notebook| {
                notebook.move_cell(args.from, args.to)
            })?;
            Ok(mutation_result(
                args.format,
                format!("moved cell {} to index {}", args.from, args.to),
                &args.path,
                &new_revision,
                || None,
                || json!({"from": args.from, "to": args.to}),
            ))
        })
        .await
    }

    #[tool(
        title = "Set Cell Type",
        description = "Change a notebook cell's type (code, markdown, raw). Jupyter cells stay \
                       schema-valid: converting to code adds empty outputs/execution_count, \
                       converting away removes them. marimo cells convert between code and \
                       markdown: converting to markdown wraps the source in mo.md(...), \
                       converting to code unwraps it. Returns the new revision.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn cell_set_type(
        &self,
        Parameters(args): Parameters<CellSetTypeArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let new_type = args.cell_type.as_str();
            let (_, new_revision) = with_notebook(&args.path, &args.revision, |notebook| {
                notebook.set_type(args.cell, new_type)
            })?;
            Ok(mutation_result(
                args.format,
                format!("cell {} is now {new_type}", args.cell),
                &args.path,
                &new_revision,
                || None,
                || json!({"cell": args.cell, "cell_type": new_type}),
            ))
        })
        .await
    }

    #[tool(
        title = "Set Cell Metadata",
        description = "Replace the metadata object of one Jupyter notebook cell (papermill tags, \
                       slideshow settings, and similar). Full replace, not a patch: the previous \
                       metadata is echoed in the response so it can be restored. Source, outputs \
                       and execution_count are untouched. marimo cell configuration lives in code \
                       and is not supported. Returns the new revision.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn cell_set_metadata(
        &self,
        Parameters(args): Parameters<CellSetMetadataArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let (previous, new_revision) = with_notebook(&args.path, &args.revision, |notebook| {
                notebook.set_metadata(args.cell, args.metadata)
            })?;
            Ok(mutation_result(
                args.format,
                format!("replaced metadata of cell {}", args.cell),
                &args.path,
                &new_revision,
                || {
                    Some(format!(
                        "previous metadata:\n```json\n{}\n```",
                        serde_json::to_string_pretty(&previous)
                            .unwrap_or_else(|_| previous.to_string())
                    ))
                },
                || json!({"cell": args.cell, "previous_metadata": previous}),
            ))
        })
        .await
    }

    #[tool(
        title = "Clear Cell Outputs",
        description = "Clear outputs of one code cell, or of every code cell when `cell` is \
                       omitted: outputs become [] and execution_count becomes null per the \
                       nbformat spec. Returns the new revision.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn cell_clear_outputs(
        &self,
        Parameters(args): Parameters<CellClearOutputsArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let (cleared, new_revision) = with_notebook(&args.path, &args.revision, |notebook| {
                notebook.clear_outputs(args.cell)
            })?;
            let summary = match args.cell {
                Some(index) => format!("cleared outputs of cell {index}"),
                None => format!("cleared outputs of {} code cells", cleared.len()),
            };
            Ok(mutation_result(
                args.format,
                summary,
                &args.path,
                &new_revision,
                || None,
                || json!({"cleared_cells": cleared}),
            ))
        })
        .await
    }
}
