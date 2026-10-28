//! Cell-level editing tools. Every tool runs under the per-path lock,
//! requires the current revision, and returns the new revision so edits can
//! be chained without re-reading.
//!
//! Cells are addressed by index (negative counts from the end) or by a stable
//! reference — the Jupyter id or the marimo cell name — through
//! [`resolve`]. Insert and delete take several cells at once: the references
//! of one call all resolve against the notebook as it was before the call.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use serde_json::{Value, json};

use crate::error::Error;
use crate::fs;
use crate::notebook::{Cell, Notebook};
use crate::render::{self, Detail, WithOutputs};
use crate::server::Tpmcp;
use crate::tools::{apply_edit, blocking, mutation_result, resolve_text_or_file, with_notebook};

mod args;
mod resolve;

pub use args::{
    CellDeleteArgs, CellEditArgs, CellInsertArgs, CellMoveArgs, CellSetSourceArgs, CellUpdateArgs,
};
pub use resolve::CellRef;
use resolve::resolve_index;

#[tool_router(router = cell_router, vis = "pub")]
impl Tpmcp {
    #[tool(
        title = "Edit Cell",
        description = "Surgically edit one notebook cell: replace an exact old_string with \
                       new_string inside the cell source (like an editor's edit tool; use \
                       cell_set_source to replace a whole cell). old_string must be unique \
                       unless replace_all is set. The cell can be addressed by index (negative \
                       counts from the end), Jupyter id or marimo name. Returns a unified diff \
                       and the new revision. Outputs and metadata are preserved. Keep the cell \
                       re-runnable: make the edited code explicit (names, imports, conversions) \
                       and check what calls return before using the result.",
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
            let ((index, diff), new_revision) =
                with_notebook(&args.path, &args.revision, |notebook| {
                    let index = resolve_index(notebook.cells(), &args.cell)?;
                    let source = notebook.cells()[index].source_text();
                    let updated = apply_edit(
                        &source,
                        &args.old_string,
                        &args.new_string,
                        args.replace_all,
                        &format!("cell {index}"),
                    )?;
                    // Render the diff before the mutation: `source` borrows
                    // the notebook, and the write needs it mutably.
                    let diff = render::unified_diff(&source, &updated);
                    notebook.set_source(index, &updated)?;
                    Ok((index, diff))
                })?;
            Ok(mutation_result(
                args.format,
                format!("edited cell {index}"),
                &args.path,
                &new_revision,
                || Some(format!("```diff\n{diff}```")),
                || json!({"cell": index, "diff": diff.as_str()}),
            ))
        })
        .await
    }

    #[tool(
        title = "Set Cell Source",
        description = "Set a notebook cell's source: the whole cell, or just the 1-based \
                       inclusive line range start_line..=end_line (use notebook_read with \
                       detail=full to see line numbers). The cell can be addressed by index \
                       (negative counts from the end), Jupyter id or marimo name. Provide the \
                       source inline, or via source_file to stream a large payload from disk \
                       without round-tripping it through your context (e.g. a long generated \
                       cell kept in a .py file). Returns the new cell source and revision. \
                       Outputs and metadata are preserved. When the new source is code, keep it \
                       explicit and re-runnable in any order: no wildcard imports, no dependence \
                       on ambient state or prior execution.",
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
            let incoming = resolve_text_or_file(
                args.source,
                args.source_file.as_deref(),
                "source",
                "source_file",
            )?;
            let ((index, cell_type, source), new_revision) =
                with_notebook(&args.path, &args.revision, |notebook| {
                    let index = resolve_index(notebook.cells(), &args.cell)?;
                    let updated = match (args.start_line, args.end_line) {
                        // incoming is owned and no later step needs it here:
                        // move it into the whole-cell replacement.
                        (None, None) => incoming,
                        (Some(start), Some(end)) => {
                            let current = notebook.cells()[index].source_text();
                            fs::replace_line_range(
                                &current,
                                start,
                                end,
                                &fs::logical_lines(&incoming),
                            )
                            .map_err(|message| {
                                Error::invalid_input(format!("cell {index}: {message}"))
                            })?
                        }
                        _ => {
                            return Err(Error::invalid_input(
                                "start_line and end_line must be provided together",
                            ));
                        }
                    };
                    notebook.set_source(index, &updated)?;
                    let cell_type = notebook.cells()[index].cell_type.clone();
                    Ok((index, cell_type, updated))
                })?;
            let line_count = fs::logical_lines(&source).len();
            let preview = Cell::from_text(cell_type, &source);
            Ok(mutation_result(
                args.format,
                format!("updated source of cell {index}"),
                &args.path,
                &new_revision,
                || {
                    Some(render::cell_markdown(
                        index,
                        &preview,
                        Detail::Full,
                        WithOutputs::None,
                        None,
                    ))
                },
                || json!({"cell": index, "line_count": line_count, "source": source.as_str()}),
            ))
        })
        .await
    }

    #[tool(
        title = "Insert Cell",
        description = "Insert one or more cells (code, markdown, raw) at a 0-based index; omit \
                       index to append at the end. Pass `cells` for a batch (a markdown header \
                       followed by code in one call) or the single-cell form (cell_type, \
                       source). source may be empty; pass source_file instead to stream a \
                       large payload from disk without round-tripping it through your context \
                       (e.g. a long generated cell kept in a .py file). Returns the new \
                       revision. Write code cells that stand alone: every name the cell uses is \
                       imported or defined in the notebook, and optional call results are \
                       guarded before use.",
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
            // A batch (`cells`) and the single-cell form are mutually
            // exclusive; an empty source is a valid empty cell here (unlike
            // cell_set_source), so neither param set is not an error.
            let mut batch: Vec<(String, String)> = Vec::new();
            match (args.cells, args.cell_type) {
                (Some(cells), None) => {
                    for cell in cells {
                        // source may legitimately be empty; source_file is
                        // the stream path. An unreadable file is an error,
                        // never an empty cell.
                        let source = if let Some(path) = cell.source_file {
                            if !cell.source.is_empty() {
                                return Err(Error::invalid_input(
                                    "source and source_file are mutually exclusive",
                                ));
                            }
                            fs::read(&path)?.content
                        } else {
                            cell.source
                        };
                        batch.push((cell.cell_type.as_str().to_string(), source));
                    }
                    if batch.is_empty() {
                        return Err(Error::invalid_input("cells must not be empty"));
                    }
                }
                (None, Some(cell_type)) => {
                    let source = match (args.source, args.source_file) {
                        (Some(_), Some(_)) => {
                            return Err(Error::invalid_input(
                                "source and source_file are mutually exclusive",
                            ));
                        }
                        (Some(text), None) => text,
                        (None, Some(path)) => fs::read(&path)?.content,
                        (None, None) => String::new(),
                    };
                    batch.push((cell_type.as_str().to_string(), source));
                }
                (Some(_), Some(_)) => {
                    return Err(Error::invalid_input(
                        "cells and cell_type are mutually exclusive",
                    ));
                }
                (None, None) => {
                    return Err(Error::invalid_input("provide cells or cell_type"));
                }
            }
            let ((index, inserted, warning), new_revision) =
                with_notebook(&args.path, &args.revision, |notebook| {
                    let mut index = args.index.unwrap_or(notebook.cells().len());
                    let mut inserted = Vec::new();
                    let mut warning = None;
                    for (cell_type, source) in &batch {
                        notebook.insert(index, cell_type, source)?;
                        let notices = notebook.finalize_mutation(index)?;
                        if !notices.is_empty() {
                            warning = Some(notices.join("\n\n"));
                        }
                        inserted.push(json!({"index": index, "cell_type": cell_type}));
                        index += 1;
                    }
                    Ok((args.index.unwrap_or(index - batch.len()), inserted, warning))
                })?;
            Ok(mutation_result(
                args.format,
                format!("inserted {} cell(s) at index {index}", batch.len()),
                &args.path,
                &new_revision,
                || warning.clone(),
                || {
                    let mut value = json!({"first_index": index, "inserted": inserted});
                    if let Some(warning) = &warning {
                        value["warning"] = json!(warning);
                    }
                    value
                },
            ))
        })
        .await
    }

    #[tool(
        title = "Delete Cells",
        description = "Delete one or more notebook cells. Every reference (index, Jupyter id or \
                       marimo name) resolves against the notebook as it is before the call, \
                       then the cells are removed together and later cells shift down. Listing \
                       the same cell twice is an error. Returns the new revision. When unsure, \
                       re-read the notebook first to confirm which cells the references name.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn cell_delete(
        &self,
        Parameters(args): Parameters<CellDeleteArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let (deleted, new_revision) = with_notebook(&args.path, &args.revision, |notebook| {
                if args.cells.is_empty() {
                    return Err(Error::invalid_input("cells must not be empty"));
                }
                let mut indices = Vec::new();
                for reference in &args.cells {
                    indices.push(resolve_index(notebook.cells(), reference)?);
                }
                indices.sort_unstable();
                if let Some(pair) = indices.windows(2).find(|pair| pair[0] == pair[1]) {
                    return Err(Error::invalid_input(format!(
                        "cell {} is listed twice",
                        pair[0]
                    )));
                }
                let mut deleted = Vec::new();
                for index in indices.into_iter().rev() {
                    let cell = notebook.delete(index)?;
                    deleted.push(json!({"index": index, "cell_type": cell.cell_type}));
                }
                deleted.reverse();
                Ok(deleted)
            })?;
            Ok(mutation_result(
                args.format,
                format!("deleted {} cell(s)", deleted.len()),
                &args.path,
                &new_revision,
                || None,
                || json!({"deleted": deleted}),
            ))
        })
        .await
    }

    #[tool(
        title = "Move Cell",
        description = "Move a notebook cell to another position (`to` is interpreted after \
                       removing the cell; negative counts from the end). The cell can be \
                       addressed by index, Jupyter id or marimo name. Returns the new revision.",
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
            let ((from, to), new_revision) =
                with_notebook(&args.path, &args.revision, |notebook| {
                    let from = resolve_index(notebook.cells(), &args.from)?;
                    // `to` is a destination after the removal: with one cell
                    // gone the valid positions are 0..=len-1 and -1 is the
                    // last one, matching the cell-reference convention.
                    let max_to = notebook.cells().len() - 1;
                    let to = if args.to < 0 {
                        max_to as isize + args.to + 1
                    } else {
                        args.to
                    };
                    if to < 0 || to as usize > max_to {
                        return Err(Error::invalid_input(format!(
                            "destination index {} out of bounds: the notebook has {} cells \
                             after the move",
                            args.to, max_to
                        )));
                    }
                    let to = to as usize;
                    notebook.move_cell(from, to)?;
                    Ok((from, to))
                })?;
            Ok(mutation_result(
                args.format,
                format!("moved cell {from} to index {to}"),
                &args.path,
                &new_revision,
                || None,
                || json!({"from": from, "to": to}),
            ))
        })
        .await
    }

    #[tool(
        title = "Update Cell Properties",
        description = "Change cell properties: type (code, markdown, raw), metadata (Jupyter \
                       only: papermill tags, slideshow settings — full replace, the previous \
                       value comes back in the answer), and/or clear the outputs (Jupyter \
                       only: outputs become [] and execution_count null). Give `cells` to \
                       target specific cells (index, Jupyter id or marimo name), or omit it to \
                       target every code cell — what clearing all outputs usually wants. At \
                       least one property is required. Jupyter cells stay schema-valid when \
                       converting type; marimo cells convert between code and markdown \
                       (markdown wraps the source in mo.md(...)). Returns the new revision.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn cell_update(
        &self,
        Parameters(args): Parameters<CellUpdateArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            if args.cell_type.is_none() && args.metadata.is_none() && !args.clear_outputs {
                return Err(Error::invalid_input(
                    "provide at least one of cell_type, metadata, clear_outputs",
                ));
            }
            // Only clearing outputs may target "every code cell" implicitly;
            // changing type or replacing metadata must name its cells.
            if (args.cell_type.is_some() || args.metadata.is_some()) && args.cells.is_none() {
                return Err(Error::invalid_input(
                    "cells is required with cell_type or metadata (omit it only to clear \
                     outputs everywhere)",
                ));
            }
            let new_type = args
                .cell_type
                .as_ref()
                .map(|kind| kind.as_str().to_string());
            let ((updated, warning), new_revision) =
                with_notebook(&args.path, &args.revision, |notebook| {
                    let targets: Vec<usize> = match &args.cells {
                        Some(refs) => {
                            let mut indices = Vec::new();
                            for reference in refs {
                                indices.push(resolve_index(notebook.cells(), reference)?);
                            }
                            indices.sort_unstable();
                            if let Some(pair) = indices.windows(2).find(|pair| pair[0] == pair[1]) {
                                return Err(Error::invalid_input(format!(
                                    "cell {} is listed twice",
                                    pair[0]
                                )));
                            }
                            indices
                        }
                        None => (0..notebook.cells().len())
                            .filter(|index| notebook.cells()[*index].cell_type == "code")
                            .collect(),
                    };
                    let mut updated = Vec::new();
                    let mut warning = None;
                    for index in targets {
                        let mut entry =
                            Value::Object([("index".into(), json!(index))].into_iter().collect());
                        // Outputs first: a code->markdown conversion would
                        // leave nothing to clear, and one call should not
                        // fail on its own ordering.
                        if args.clear_outputs {
                            notebook.clear_outputs(Some(index))?;
                            entry["outputs_cleared"] = json!(true);
                        }
                        if let Some(metadata) = &args.metadata {
                            let previous = notebook.set_metadata(index, metadata.clone())?;
                            entry["previous_metadata"] = json!(previous);
                        }
                        if let Some(new_type) = &new_type {
                            let previous = notebook.cells()[index].cell_type.clone();
                            notebook.set_type(index, new_type)?;
                            let notices = notebook.finalize_mutation(index)?;
                            if !notices.is_empty() {
                                warning = Some(notices.join("\n\n"));
                            }
                            entry["cell_type"] = json!(new_type);
                            entry["previous_type"] = json!(previous);
                        }
                        updated.push(entry);
                    }
                    Ok((updated, warning))
                })?;
            // The previous metadata is echoed so it can be restored from
            // the answer alone (the disclosure cell_set_metadata made).
            let echoes: Vec<String> = updated
                .iter()
                .filter_map(|entry| {
                    let index = entry.get("index")?;
                    let previous = entry.get("previous_metadata")?;
                    Some(format!(
                        "cell {index} previous metadata:\n```json\n{}\n```",
                        serde_json::to_string_pretty(previous)
                            .unwrap_or_else(|_| previous.to_string())
                    ))
                })
                .collect();
            Ok(mutation_result(
                args.format,
                format!("updated {} cell(s)", updated.len()),
                &args.path,
                &new_revision,
                || {
                    let mut parts = echoes;
                    if let Some(warning) = &warning {
                        parts.push(warning.clone());
                    }
                    (!parts.is_empty()).then(|| parts.join("\n\n"))
                },
                || {
                    let mut value = json!({"updated": updated});
                    if let Some(warning) = &warning {
                        value["warning"] = json!(warning);
                    }
                    value
                },
            ))
        })
        .await
    }
}
