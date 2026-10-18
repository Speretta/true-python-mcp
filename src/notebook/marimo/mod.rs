//! marimo notebooks (`.py`).
//!
//! The file is modeled as a sequence of [`Segment`]s: verbatim literals and
//! cells. An unedited cell serializes from its captured [`CellMeta::block`],
//! so parse→serialize is byte-identical to the original. Editing a cell
//! regenerates just its body (re-indented) between the preserved header and
//! trailing `return`; marimo recomputes signatures and returns on its next
//! save, so a stale `return` is harmless.

mod check;
mod create;
mod cycles;
mod indent;
pub(super) mod markdown;
pub(super) mod parse;
mod scan;
pub(super) mod scope;

use std::borrow::Cow;

use super::{Cell, Finding, Notebook, check_index};
use crate::error::Error;
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
    /// interiors, stray comments) — written back verbatim so string values
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
    /// cell borrows its captured block (no copy — serialize writes it into
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
                meta.return_suffix.clone().unwrap_or_default()
            ),
            Kind::Setup => format!("{}{}", meta.header, body),
            _ => meta.block.clone(),
        };
        Cow::Owned(block)
    }

    fn insert_cell(&mut self, index: usize, cell: Cell, meta: CellMeta) {
        let separator = Segment::Lit("\n\n\n".to_string());
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
        let new_segments = if index < positions.len() {
            vec![Segment::Cell, separator]
        } else {
            vec![separator, Segment::Cell]
        };
        self.segments.splice(at..at, new_segments);
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
