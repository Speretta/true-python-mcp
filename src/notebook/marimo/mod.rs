//! marimo notebooks (`.py`).
//!
//! The file is modeled as a sequence of [`Segment`]s: verbatim literals and
//! cells. An unedited cell serializes from its captured [`CellMeta::block`],
//! so parse→serialize is byte-identical to the original. Editing a cell
//! regenerates just its body (re-indented) between the preserved header and
//! trailing `return`, then canonicalizes the `return` and signature the way
//! marimo's serializer would write them (see `create::canonicalize_edited_cells`),
//! so an edited cell never carries a stale return or signature.

mod check;
mod create;
mod cycles;
mod indent;
pub(super) mod markdown;
pub(super) mod parse;
mod scan;
pub(super) mod scope;
mod summary;

use std::borrow::Cow;
use std::collections::HashSet;

use super::{Cell, Finding, Notebook, check_index};
use crate::error::Error;
pub use create::MARIMO_VERSION;
use create::{new_app_cell, strip_hide_code, to_markdown_header};

pub struct MarimoNotebook {
    cells: Vec<Cell>,
    metas: Vec<CellMeta>,
    segments: Vec<Segment>,
    version: Option<String>,
}

enum Segment {
    Lit(String),
    Cell,
}

pub(super) enum Kind {
    AppCell,
    Setup,
    Function,
    Class,
    Unparsable,
    /// A cell region that failed to parse (whole-file syntax error). Shown
    /// read-only so the rest of the notebook stays editable.
    Broken,
}

impl Kind {
    fn label(&self) -> &'static str {
        match self {
            Self::AppCell => "cell",
            Self::Setup => "setup",
            Self::Function => "function",
            Self::Class => "class",
            Self::Unparsable => "unparsable",
            Self::Broken => "broken",
        }
    }

    fn editable(&self) -> bool {
        matches!(self, Self::AppCell | Self::Setup)
    }
}

pub(super) struct CellMeta {
    kind: Kind,
    /// Verbatim block bytes; used while the cell is unedited.
    block: String,
    /// Decorator + signature lines, preserved when regenerating an edit.
    header: String,
    /// Observed body indent, used to re-indent an edited body.
    indent: String,
    /// Per-line record of the body's original shape: `false` for lines that
    /// lacked the fixed indent in the file (column-0 multiline-string
    /// interiors, stray comments), written back verbatim so string values
    /// survive edits. `None` when every line carried the indent (the common
    /// case, and cells tpmcp creates). Unused for markdown cells (re-wrapped
    /// wholesale) and read-only blocks (replayed verbatim).
    indent_memory: Option<Vec<bool>>,
    /// Trailing `return` statement (marimo regenerates it), preserved verbatim.
    return_suffix: Option<String>,
    /// Set for markdown cells (`mo.md` wrapper); the source is then the
    /// extracted markdown text and edits re-wrap it.
    md: Option<MdInfo>,
    edited: bool,
}

/// How a markdown cell wraps its content in `mo.md(...)`.
pub(super) struct MdInfo {
    /// String prefix tag preserved across edits (`r`, `f`, `rf`, ...).
    tag: String,
    /// Comment lines between the signature and the `mo.md(` call.
    comments: String,
}

/// Whether a body contains no statement — blank and comment lines only. The
/// predicate behind marimo's `pass` fallback for a statement-less setup body
/// (codegen.py `has_only_comments`).
fn statementless(body: &str) -> bool {
    body.lines().all(|line| {
        let trimmed = line.trim();
        trimmed.is_empty() || trimmed.starts_with('#')
    })
}

impl MarimoNotebook {
    pub(crate) fn parse(src: &str) -> Result<Self, String> {
        parse::parse(src)
    }

    /// Segment positions of each cell marker, in order (index i ↔ cells[i]).
    fn cell_positions(&self) -> Vec<usize> {
        self.segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| matches!(segment, Segment::Cell))
            .map(|(position, _)| position)
            .collect()
    }

    /// The bytes cell `index` contributes to the serialization: an unedited
    /// cell borrows its captured block (no copy; serialize writes it into
    /// the output once); an edited cell regenerates just its body.
    fn cell_bytes(&self, index: usize) -> Cow<'_, str> {
        let meta = &self.metas[index];
        if !meta.edited {
            return Cow::Borrowed(&meta.block);
        }
        let source = self.cells[index].source.text();
        let body = match &meta.md {
            Some(md) => {
                format!(
                    "{}{}",
                    md.comments,
                    markdown::wrap_call(&source, &meta.indent, &md.tag)
                )
            }
            None => indent::indent_lines(&source, &meta.indent, meta.indent_memory.as_deref()),
        };
        let block = match meta.kind {
            Kind::AppCell => format!(
                "{}{}{}",
                meta.header,
                body,
                // why: marimo regenerates the `return` line on save and emits
                // it even for a cell with no defs (codegen.py replaces the
                // `(...)` placeholder with nothing). A captured block without
                // one would serialize a blank body as a bodyless `def _():` —
                // an IndentationError marimo cannot open.
                meta.return_suffix
                    .clone()
                    .unwrap_or_else(|| create::return_statement(&meta.indent, &[], false))
            ),
            Kind::Setup => {
                // why: a body without a statement is an IndentationError.
                // marimo appends `pass` to a statement-less body (codegen.py
                // `has_only_comments`) and drops an empty setup section
                // outright; tpmcp keeps the cell, so its body becomes marimo's
                // own `pass` shape.
                if statementless(&source) {
                    format!(
                        "{}{}{}",
                        meta.header,
                        body,
                        indent::indent_lines("pass", &meta.indent, None)
                    )
                } else {
                    format!("{}{}", meta.header, body)
                }
            }
            _ => meta.block.clone(),
        };
        Cow::Owned(block)
    }

    fn insert_cell(&mut self, index: usize, cell: Cell, meta: CellMeta) {
        let positions = self.cell_positions();
        let at = if index < positions.len() {
            positions[index]
        } else if let Some(&last) = positions.last() {
            last + 1
        } else {
            self.segments
                .iter()
                .position(
                    |segment| matches!(segment, Segment::Lit(text) if text.contains("marimo.App")),
                )
                .map_or(self.segments.len(), |position| position + 1)
        };
        let appending = index >= positions.len();
        let new_segments = if appending {
            // The gap in front of the new cell depends on what precedes the
            // insertion point: a literal ending in a newline (the header of a
            // created file) only needs "\n\n" for marimo's two blank lines;
            // a cell block does not end in a newline, so the standard gap is
            // the full "\n\n\n".
            let gap = match self.segments.get(at.checked_sub(1).unwrap_or(at)) {
                Some(Segment::Lit(text)) if text.ends_with('\n') => "\n\n",
                _ => "\n\n\n",
            };
            vec![Segment::Lit(gap.to_string()), Segment::Cell]
        } else {
            vec![Segment::Cell, Segment::Lit("\n\n\n".to_string())]
        };
        self.segments.splice(at..at, new_segments);
        if positions.is_empty()
            && let Some(Segment::Lit(gap)) = self.segments.get_mut(at + 2)
            && gap == "\n\n"
        {
            // First cell into a cell-less file: the empty-file gap now sits
            // behind a cell block and becomes the standard pre-guard gap.
            *gap = "\n\n\n".to_string();
        }
        self.cells.insert(index, cell);
        self.metas.insert(index, meta);
    }

    fn remove_cell(&mut self, index: usize) -> (Cell, CellMeta) {
        let positions = self.cell_positions();
        let at = positions[index];
        let start = if at > 0 && matches!(self.segments[at - 1], Segment::Lit(_)) {
            at - 1
        } else {
            at
        };
        self.segments.drain(start..at + 1);
        (self.cells.remove(index), self.metas.remove(index))
    }

    /// Whether any cell binds `name` at its top level: a setup block makes it
    /// global, a plain app cell feeds it through the graph. Markdown cells
    /// (whose source is markdown text, not Python) and unparsable or broken
    /// regions cannot be analyzed and never count as definers.
    pub(super) fn defines(&self, name: &str) -> bool {
        self.cells
            .iter()
            .zip(&self.metas)
            .filter(|(_, meta)| {
                meta.md.is_none()
                    && matches!(
                        meta.kind,
                        Kind::AppCell | Kind::Setup | Kind::Function | Kind::Class
                    )
            })
            .any(|(cell, _)| {
                scope::analyze(&cell.source.text()).is_some_and(|info| info.defs.contains(name))
            })
    }

    /// Whether any cell is a markdown cell (a `mo.md(...)` wrapper).
    pub(super) fn has_markdown_cell(&self) -> bool {
        self.cells.iter().any(Cell::is_markdown)
    }

    /// Whether a setup cell binds `name` at its top level (so every cell can
    /// use it without a signature parameter).
    fn setup_defines(&self, name: &str) -> bool {
        self.cells
            .iter()
            .zip(&self.metas)
            .filter(|(_, meta)| matches!(meta.kind, Kind::Setup))
            .any(|(cell, _)| {
                scope::analyze(&cell.source.text()).is_some_and(|info| info.defs.contains(name))
            })
    }

    /// Each lintable cell's `(0-based marimo index, Python body)` for ruff.
    ///
    /// Parsing already strips the fixed indent and the trailing `return`, so a
    /// code app cell or a setup cell is just its dedented body; a markdown
    /// cell contributes its `mo.md(...)` wrapper call (the only Python it
    /// compiles to); a function/class block is linted verbatim (decorator and
    /// signature are real Python, not scaffolding); broken/unparsable cells
    /// have no valid body and are skipped. Indices stay 1:1 with the cell
    /// tools so the caller can pair a skipped index with a placeholder.
    pub(crate) fn lint_cells(&self) -> Vec<(usize, String)> {
        let mut lintable = Vec::new();
        for (index, (cell, meta)) in self.cells.iter().zip(&self.metas).enumerate() {
            let body = match meta.kind {
                Kind::Broken | Kind::Unparsable => continue,
                Kind::AppCell => match &meta.md {
                    Some(md) => markdown::wrap_call(&cell.source.text(), "", &md.tag),
                    None => cell.source.text().into_owned(),
                },
                Kind::Setup => cell.source.text().into_owned(),
                Kind::Function | Kind::Class => meta.block.clone(),
            };
            lintable.push((index, body));
        }
        lintable
    }

    /// The top-level names every analyzable cell defines (setup defs included).
    /// Together with [`Self::runtime_builtin`], a name the marimo runtime binds
    /// is not "undefined" just because the concatenated cell bodies do not
    /// bind it — the graph half of the ruff F821 filter's provided set.
    pub(crate) fn cell_definitions(&self) -> HashSet<String> {
        let mut names = HashSet::new();
        for (cell, meta) in self.cells.iter().zip(&self.metas) {
            if meta.md.is_some() {
                // A markdown cell's source is text, not Python.
                continue;
            }
            if let Some(info) = scope::analyze(&cell.source.text()) {
                names.extend(info.defs);
            }
        }
        names
    }

    /// Whether the marimo runtime provides `name` as a builtin, the runtime
    /// half of the ruff F821 filter's provided set (checked per name so the
    /// builtin list is never materialized per call).
    pub(crate) fn runtime_builtin(name: &str) -> bool {
        check::is_marimo_builtin(name)
    }

    /// Whether the file header binds `name` outside any cell: `import marimo`
    /// and `app = marimo.App(...)` — both required for the file to count as
    /// marimo at all — plus `__generated_with`. The header half of the ruff
    /// F821 filter's provided set: a cell body touching `app` (a
    /// `@app.function` block's decorator, say) references a real binding.
    pub(crate) fn header_binds(name: &str) -> bool {
        matches!(name, "marimo" | "app" | "__generated_with")
    }
}

impl Notebook for MarimoNotebook {
    fn format_label(&self) -> String {
        match &self.version {
            Some(version) => format!("marimo {version}"),
            None => "marimo".to_string(),
        }
    }

    fn cells(&self) -> &[Cell] {
        &self.cells
    }

    fn set_source(&mut self, index: usize, text: &str) -> Result<(), Error> {
        check_index(self.cells.len(), index)?;
        if !self.metas[index].kind.editable() {
            return Err(Error::invalid_input(
                if matches!(self.metas[index].kind, Kind::Broken) {
                    format!(
                        "cell {index} has a syntax error and is read-only; \
                     repair its Python syntax to edit it here"
                    )
                } else {
                    format!(
                        "cell {index} is a marimo {} block; editing it is not supported",
                        self.metas[index].kind.label()
                    )
                },
            ));
        }
        if self.metas[index].md.is_none() {
            // Python bodies remember which lines lacked the fixed indent; the
            // memory must follow the edit. Markdown sources are re-wrapped
            // wholesale and need none.
            let old_source = self.cells[index].source_text();
            let memory = indent::remap_indent_memory(
                &old_source,
                self.metas[index].indent_memory.as_deref(),
                text,
            );
            self.metas[index].indent_memory = memory;
        }
        self.cells[index].set_source_text(text);
        self.metas[index].edited = true;
        Ok(())
    }

    fn insert(&mut self, index: usize, cell_type: &str, source: &str) -> Result<(), Error> {
        if !matches!(cell_type, "code" | "markdown") {
            return Err(Error::invalid_input(format!(
                "marimo notebooks do not support {cell_type} cells"
            )));
        }
        if index > self.cells.len() {
            return Err(Error::invalid_input(format!(
                "insert index {index} out of bounds: notebook has {} cells",
                self.cells.len()
            )));
        }
        let (cell, meta) = if cell_type == "markdown" {
            self.new_markdown_cell(source)
        } else {
            new_app_cell(source)
        };
        self.insert_cell(index, cell, meta);
        Ok(())
    }

    fn delete(&mut self, index: usize) -> Result<Cell, Error> {
        check_index(self.cells.len(), index)?;
        Ok(self.remove_cell(index).0)
    }

    fn move_cell(&mut self, from: usize, to: usize) -> Result<(), Error> {
        check_index(self.cells.len(), from)?;
        if from == to {
            return Ok(());
        }
        let (cell, meta) = self.remove_cell(from);
        if to > self.cells.len() {
            return Err(Error::invalid_input(format!(
                "destination index {to} out of bounds: notebook has {} cells",
                self.cells.len()
            )));
        }
        self.insert_cell(to, cell, meta);
        Ok(())
    }

    fn set_type(&mut self, index: usize, cell_type: &str) -> Result<(), Error> {
        check_index(self.cells.len(), index)?;
        if !matches!(self.metas[index].kind, Kind::AppCell) {
            return Err(Error::invalid_input(format!(
                "cell {index} is a marimo {} block; its type cannot be changed",
                self.metas[index].kind.label()
            )));
        }
        match cell_type {
            "code" => {
                if self.metas[index].md.is_some() {
                    // The source (markdown text) becomes the Python source;
                    // the stale signature is regenerated by marimo on save.
                    self.metas[index].md = None;
                    self.metas[index].header = strip_hide_code(&self.metas[index].header);
                    self.cells[index].cell_type = "code".to_string();
                    self.metas[index].edited = true;
                }
            }
            "markdown" => {
                if self.metas[index].md.is_none() {
                    // The current Python source becomes the markdown content;
                    // the signature is reduced to the markdown cell's only
                    // ref (`mo`), unless the setup cell provides it.
                    let needs_mo = !self.setup_defines("mo");
                    let meta = &mut self.metas[index];
                    meta.header = to_markdown_header(&meta.header, needs_mo);
                    meta.md = Some(MdInfo {
                        tag: "r".to_string(),
                        comments: String::new(),
                    });
                    meta.return_suffix = Some(format!("{}return", meta.indent));
                    meta.edited = true;
                    self.cells[index].cell_type = "markdown".to_string();
                }
            }
            other => {
                return Err(Error::invalid_input(format!(
                    "marimo cells are code or markdown; cannot convert to {other}"
                )));
            }
        }
        Ok(())
    }

    fn clear_outputs(&mut self, _index: Option<usize>) -> Result<Vec<usize>, Error> {
        Err(Error::invalid_input(
            "marimo notebooks do not store outputs",
        ))
    }

    fn check(&self) -> Vec<Finding> {
        check::check(self)
    }

    fn serialize(&self) -> String {
        // The literal segments plus every cell's captured block length: a
        // close bound on the output (edited cells regenerate at roughly
        // their block's size), so the buffer allocates once.
        let capacity = self
            .segments
            .iter()
            .map(|segment| match segment {
                Segment::Lit(text) => text.len(),
                Segment::Cell => 0,
            })
            .sum::<usize>()
            + self
                .metas
                .iter()
                .map(|meta| meta.block.len())
                .sum::<usize>();
        let mut out = String::with_capacity(capacity);
        let mut cell = 0;
        for segment in &self.segments {
            match segment {
                Segment::Lit(text) => out.push_str(text),
                Segment::Cell => {
                    out.push_str(&self.cell_bytes(cell));
                    cell += 1;
                }
            }
        }
        out
    }
}
