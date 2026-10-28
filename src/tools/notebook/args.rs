//! Argument structs for the notebook-level tools, re-exported unchanged by
//! [`super`] so the tool surface is identical. Split out of `notebook.rs` to
//! keep each file under the 500-line house limit.

use std::path::PathBuf;

use serde::Deserialize;

use crate::notebook::Format as NotebookFormat;
use crate::render::{Detail, Format, WithOutputs};
use crate::tools::CellType;

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
