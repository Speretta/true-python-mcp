//! Notebook document model shared by all supported formats.
//!
//! Each format lives in a sibling module and implements [`Notebook`];
//! [`open`] detects the format from file content and [`create`] builds new
//! documents, both returning [`AnyNotebook`], a statically dispatched enum
//! of the supported formats. Tools and rendering only ever see this API, so
//! adding a format means adding a module, one arm in each of those two
//! functions, and one `AnyNotebook` variant (whose missing match arms the
//! compiler then reports everywhere).

mod any;
mod jupyter;
mod marimo;

pub use any::AnyNotebook;
pub use jupyter::JupyterNotebook;
pub use marimo::parse::is_marimo;
pub use marimo::{MARIMO_VERSION, MarimoNotebook};

use std::borrow::Cow;
use std::cell::OnceCell;
use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::Error;

/// One notebook cell. `extra` carries the format-specific fields verbatim
/// (Jupyter: `id`/`metadata`/`outputs`/`execution_count`; marimo: its own
/// kind/name/config), and source edits never touch it.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub cell_type: String,
    pub source: SourceText,
    pub extra: BTreeMap<String, Value>,
}

impl Cell {
    /// A cell of `cell_type` whose source is the canonical line split of
    /// `text`, with no format extras.
    pub fn from_text(cell_type: impl Into<String>, text: &str) -> Self {
        Self {
            cell_type: cell_type.into(),
            source: SourceText::from_lines(SourceText::split_canonical(text)),
            extra: BTreeMap::new(),
        }
    }

    pub fn is_code(&self) -> bool {
        self.cell_type == "code"
    }

    pub fn is_markdown(&self) -> bool {
        self.cell_type == "markdown"
    }

    /// The cell's full source text (see [`SourceText::text`]).
    pub fn source_text(&self) -> Cow<'_, str> {
        self.source.text()
    }

    /// Replace the source, normalizing it to canonical lines.
    pub fn set_source_text(&mut self, text: &str) {
        self.source = SourceText::from_lines(SourceText::split_canonical(text));
    }

    /// Jupyter cell id (nbformat 4.5+), when present.
    pub fn id(&self) -> Option<&str> {
        self.extra.get("id").and_then(Value::as_str)
    }

    /// Jupyter execution count; `None` when absent or null (never run).
    pub fn execution_count(&self) -> Option<u64> {
        self.extra.get("execution_count").and_then(Value::as_u64)
    }

    /// Jupyter cell outputs; empty for non-code cells and for marimo.
    pub fn outputs(&self) -> &[Value] {
        self.extra
            .get("outputs")
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice)
    }

    /// marimo block kind (`cell`, `setup`, `function`, `class`, `unparsable`,
    /// `broken`); `None` for Jupyter cells.
    pub fn marimo_kind(&self) -> Option<&str> {
        self.extra
            .get("marimo")
            .and_then(|marimo| marimo.get("kind"))
            .and_then(Value::as_str)
    }
}

/// Cell source in the representation the file used. Jupyter files store
/// either a single string or an array of lines; other formats use `Lines`.
#[derive(Debug, Clone)]
pub enum SourceText {
    Str(String),
    Lines {
        /// The lines exactly as the file stored them (terminators intact), so
        /// serialization reproduces the original bytes.
        lines: Vec<String>,
        /// The concatenation of `lines`, cached on first read so [`text`]
        /// is a borrow rather than a fresh allocation on every call.
        joined: OnceCell<String>,
    },
}

impl SourceText {
    /// The full source text: borrowed when stored as a single string,
    /// concatenated (and cached) when stored as lines.
    pub fn text(&self) -> Cow<'_, str> {
        match self {
            Self::Str(text) => Cow::Borrowed(text),
            Self::Lines { lines, joined } => {
                Cow::Borrowed(joined.get_or_init(|| lines.concat()).as_str())
            }
        }
    }

    /// A [`SourceText::Lines`] whose joined text is computed lazily on first
    /// [`text`](Self::text). `lines` is immutable after construction — every
    /// mutation replaces the whole [`SourceText`] — so the cache never goes
    /// stale.
    pub fn from_lines(lines: Vec<String>) -> Self {
        Self::Lines {
            lines,
            joined: OnceCell::new(),
        }
    }

    /// Split after every `\n` (CRLF stays attached to its line), dropping a
    /// trailing empty chunk; nbformat's canonical line form.
    pub fn split_canonical(text: &str) -> Vec<String> {
        text.split_inclusive('\n').map(String::from).collect()
    }
}

impl PartialEq for SourceText {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Str(left), Self::Str(right)) => left == right,
            // Ignore the `joined` cache: it is derived from `lines`, so two
            // equal sources may still differ in whether it has been filled.
            (Self::Lines { lines: left, .. }, Self::Lines { lines: right, .. }) => left == right,
            _ => false,
        }
    }
}

/// Notebook formats tpmcp can create (values documented on the params).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Jupyter,
    Marimo,
}

/// Severity of a [`Finding`] reported by notebook validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Blocks correct execution/loading of the notebook.
    Error,
    /// Likely a mistake, but the notebook still works.
    Warning,
    /// Cosmetic drift that the editor regenerates on its next save.
    Info,
}

/// One validation finding for a notebook.
#[derive(Debug, Clone, PartialEq, serde::Serialize, schemars::JsonSchema)]
pub struct Finding {
    pub severity: Severity,
    /// 0-based cell index for cell-scoped findings; `None` notebook-wide.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell: Option<usize>,
    /// Stable machine-readable code, e.g. `cycle` or `multiple-definitions`.
    pub code: String,
    /// Human/LLM-readable description.
    pub message: String,
}

/// A parsed notebook document. All mutations go through these operations so
/// each format keeps its byte-exactness guarantees; mutation failures are
/// [`Error::InvalidInput`] carrying the user-facing message.
pub trait Notebook {
    /// Short display label, e.g. `nbformat 4.5`.
    fn format_label(&self) -> String;

    fn cells(&self) -> &[Cell];

    fn set_source(&mut self, index: usize, text: &str) -> Result<(), Error>;

    fn insert(&mut self, index: usize, cell_type: &str, source: &str) -> Result<(), Error>;

    fn delete(&mut self, index: usize) -> Result<Cell, Error>;

    /// Move a cell; `to` is interpreted after removal.
    fn move_cell(&mut self, from: usize, to: usize) -> Result<(), Error>;

    fn set_type(&mut self, index: usize, cell_type: &str) -> Result<(), Error>;

    /// Clear outputs of one cell or (with `None`) every code cell;
    /// returns the indexes of the cleared cells.
    fn clear_outputs(&mut self, index: Option<usize>) -> Result<Vec<usize>, Error>;

    /// Byte-exact serialization: regions no operation touched reproduce the
    /// original bytes.
    fn serialize(&self) -> String;

    /// Validate the document: schema shape (Jupyter) or reactive-graph
    /// semantics (marimo). Findings are ordered by kind, then cell.
    fn check(&self) -> Vec<Finding> {
        Vec::new()
    }
}

/// Detect the format and parse.
pub fn open(content: &str) -> Result<AnyNotebook, String> {
    if content.trim_start().starts_with('{') {
        return Ok(AnyNotebook::Jupyter(JupyterNotebook::parse(content)?));
    }
    if marimo::parse::is_marimo(content) {
        return Ok(AnyNotebook::Marimo(marimo::MarimoNotebook::parse(content)?));
    }
    Err("not a recognized notebook: expected .ipynb JSON or a marimo .py file".to_string())
}

pub fn create(format: Format) -> AnyNotebook {
    create_with_dependencies(format, None, None)
}

/// [`create`] for a fresh marimo file with a PEP 723 `# /// script` header:
/// `dependencies` and `requires_python` name the inline script metadata block
/// emitted above `import marimo`. A `None` for either omits that key, and
/// both `None` reproduces the default scaffold. Jupyter targets have no PEP
/// 723 form, so the arguments are ignored there; callers that supplied them
/// for a Jupyter target reject before reaching this function.
pub fn create_with_dependencies(
    format: Format,
    dependencies: Option<&[String]>,
    requires_python: Option<&str>,
) -> AnyNotebook {
    match format {
        Format::Jupyter => AnyNotebook::Jupyter(JupyterNotebook::new()),
        Format::Marimo => AnyNotebook::Marimo(marimo::MarimoNotebook::create(
            dependencies,
            requires_python,
        )),
    }
}

impl AnyNotebook {
    /// Replace one cell's metadata object, returning the previous value
    /// (an empty object when the cell had none).
    ///
    /// Jupyter-only by contract: marimo cell configuration lives in code
    /// (the `@app.cell` decorator arguments), so the marimo arm refuses
    /// with an explanation. Deliberately an inherent method on the enum,
    /// not a [`Notebook`] trait method: a trait method would force every
    /// format to carry an impl that can only refuse, and a trait default
    /// would silently swallow the Jupyter case at the dispatch site.
    pub fn set_metadata(
        &mut self,
        index: usize,
        metadata: Map<String, Value>,
    ) -> Result<Value, Error> {
        match self {
            Self::Jupyter(notebook) => notebook.set_metadata(index, metadata),
            Self::Marimo(_) => Err(Error::invalid_input(
                "marimo cell configuration lives in code (the @app.cell decorator arguments), \
                 not in a metadata object; cell_set_metadata supports Jupyter notebooks only",
            )),
        }
    }

    /// After a mutation that introduced a markdown cell into a marimo
    /// notebook, make sure the file defines `mo`, the same scaffold
    /// marimo's own converter adds. Returns the scaffold warning when an
    /// import cell was inserted, `Ok(None)` when nothing was needed.
    /// Jupyter notebooks never need the scaffold.
    pub fn ensure_mo_definition(&mut self) -> Result<Option<String>, Error> {
        match self {
            Self::Jupyter(_) => Ok(None),
            Self::Marimo(notebook) => notebook.ensure_mo_definition(),
        }
    }

    /// After a marimo app-cell body edit, refresh the edited cells' `return`
    /// and signature to what marimo's serializer would write, so the file
    /// never carries a stale-return/stale-signature finding. `before` is the
    /// edited cell's source before the edit (`None` for an insert); the
    /// recompute is skipped when the edit left the cell's defs/refs unchanged.
    /// A no-op for Jupyter notebooks.
    pub fn canonicalize_after_edit(&mut self, index: usize, before: Option<&str>) {
        match self {
            Self::Jupyter(_) => {}
            Self::Marimo(notebook) => notebook.canonicalize_after_edit(index, before),
        }
    }
}

/// Infer the notebook format from a file extension: `.ipynb` is Jupyter,
/// `.py` is marimo.
pub fn format_from_extension(path: &std::path::Path) -> Option<Format> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("ipynb") => Some(Format::Jupyter),
        Some("py") => Some(Format::Marimo),
        _ => None,
    }
}

pub fn check_index(len: usize, index: usize) -> Result<(), Error> {
    if index >= len {
        return Err(Error::invalid_input(format!(
            "cell index {index} out of bounds: notebook has {len} cells (0-based)"
        )));
    }
    Ok(())
}
