//! Argument structs for the cell-level editing tools, re-exported unchanged by
//! [`super`] so the tool surface is identical. Split out of `cell/mod.rs` to
//! keep each file under the 500-line house limit.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::render::Format;
use crate::tools::CellType;

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
