//! Argument structs for the cell-level editing tools, re-exported unchanged by
//! [`super`] so the tool surface is identical. Split out of `cell/mod.rs` to
//! keep each file under the 500-line house limit.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Map, Value};

use super::resolve::CellRef;
use crate::render::Format;
use crate::tools::CellType;
use crate::tools::notebook::NewCell;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CellEditArgs {
    /// Path to the notebook file.
    pub path: PathBuf,
    /// Which cell to edit: Cell to target: a 0-based index (negative counts from the end, -1 is the last cell), the cell's id (Jupyter 4.5+), or its name (marimo `def name():`).
    pub cell: CellRef,
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
    /// Which cell to replace in: Cell to target: a 0-based index (negative counts from the end, -1 is the last cell), the cell's id (Jupyter 4.5+), or its name (marimo `def name():`).
    pub cell: CellRef,
    /// New source: the whole cell, or the replacement for start_line..=end_line.
    /// Mutually exclusive with source_file.
    #[serde(default)]
    pub source: Option<String>,
    /// Read the new source from this file instead (the stream path for huge
    /// payloads); mutually exclusive with source.
    #[serde(default)]
    pub source_file: Option<PathBuf>,
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
    /// 0-based position for the new cells; defaults to appending at the end.
    #[serde(default)]
    pub index: Option<usize>,
    /// Cells to insert, in order. Mutually exclusive with the single-cell
    /// form (cell_type/source/source_file).
    #[serde(default)]
    pub cells: Option<Vec<NewCell>>,
    /// Type of the new cell (the single-cell form). Mutually exclusive with
    /// cells.
    #[serde(default)]
    pub cell_type: Option<CellType>,
    /// Initial source for the new cell (may be empty). Mutually exclusive
    /// with source_file and with cells.
    #[serde(default)]
    pub source: Option<String>,
    /// Read the initial source from this file instead (the stream path for
    /// huge payloads); mutually exclusive with source and with cells.
    #[serde(default)]
    pub source_file: Option<PathBuf>,
    /// Current file revision (from notebook_read or a previous mutation).
    pub revision: String,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CellDeleteArgs {
    /// Path to the notebook file.
    pub path: PathBuf,
    /// Cells to delete. Every reference resolves against the notebook as it
    /// is before the call, then the cells are removed together; listing the
    /// same cell twice is an error.
    pub cells: Vec<CellRef>,
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
    /// Which cell to move: Cell to target: a 0-based index (negative counts from the end, -1 is the last cell), the cell's id (Jupyter 4.5+), or its name (marimo `def name():`).
    pub from: CellRef,
    /// 0-based destination index (after removal of the moved cell); negative
    /// counts from the end.
    pub to: isize,
    /// Current file revision (from notebook_read or a previous mutation).
    pub revision: String,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

// Unknown fields are rejected on purpose: a typo like "cell" for "cells"
// must not silently mean "every code cell".
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellUpdateArgs {
    /// Path to the notebook file.
    pub path: PathBuf,
    /// Which cells to update. Omit to target every code cell — what
    /// `clear_outputs: true` alone usually wants.
    #[serde(default)]
    pub cells: Option<Vec<CellRef>>,
    /// New cell type for the targeted cells.
    #[serde(default)]
    pub cell_type: Option<CellType>,
    /// The complete new metadata object for the cell (replaces the previous
    /// one; the answer returns what it was). Jupyter only.
    #[serde(default)]
    pub metadata: Option<Map<String, Value>>,
    /// Clear the outputs of the targeted cells. Jupyter only.
    #[serde(default)]
    pub clear_outputs: bool,
    /// Current file revision (from notebook_read or a previous mutation).
    pub revision: String,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}
