//! marimo notebooks (`.py`).
//!
//! The file is modeled as a sequence of [`Segment`]s: verbatim literals and
//! cells. An unedited cell serializes from its captured [`CellMeta::block`],
//! so parse→serialize is byte-identical to the original. Editing a cell
//! regenerates just its body (re-indented) between the preserved header and
//! trailing `return`; marimo recomputes signatures and returns on its next
//! save, so a stale `return` is harmless.

mod check;
mod cycles;
mod indent;
pub(super) mod markdown;
pub(super) mod parse;
mod scan;
pub(super) mod scope;

use super::{Cell, Finding, Notebook, SourceText, check_index};

/// Version stamped into newly created marimo files. marimo never validates it
/// and refreshes it on the next save.
const MARIMO_VERSION: &str = "0.25.1";

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

    /// A new empty marimo notebook (header + run guard, no cells).
    pub(crate) fn create() -> Self {
        let head = format!(
            "import marimo\n\n__generated_with = \"{MARIMO_VERSION}\"\napp = marimo.App()\n"
        );
        Self {
            cells: Vec::new(),
            metas: Vec::new(),
            segments: vec![
                Segment::Lit(head),
                Segment::Lit("\n\n\n".to_string()),
                Segment::Lit("if __name__ == \"__main__\":\n    app.run()\n".to_string()),
            ],
            version: Some(MARIMO_VERSION.to_string()),
        }
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

    fn cell_bytes(&self, index: usize) -> String {
        let meta = &self.metas[index];
        if !meta.edited {
            return meta.block.clone();
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
        match meta.kind {
            Kind::AppCell => format!(
                "{}{}{}",
                meta.header,
                body,
                meta.return_suffix.clone().unwrap_or_default()
            ),
            Kind::Setup => format!("{}{}", meta.header, body),
            _ => meta.block.clone(),
        }
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

    /// A new markdown cell: `@app.cell(hide_code=True)` wrapping the content
    /// in `mo.md(r""")`, with `mo` as a parameter unless setup provides it.
    fn new_markdown_cell(&self, source: &str) -> (Cell, CellMeta) {
        let indent = "    ".to_string();
        let params = if self.setup_defines("mo") { "" } else { "mo" };
        (
            Cell {
                cell_type: "markdown".to_string(),
                source: SourceText::Lines(SourceText::split_canonical(source)),
                extra: Default::default(),
            },
            CellMeta {
                kind: Kind::AppCell,
                block: String::new(),
                header: format!("@app.cell(hide_code=True)\ndef _({params}):\n"),
                indent,
                indent_memory: None,
                return_suffix: Some("    return".to_string()),
                md: Some(MdInfo {
                    tag: "r".to_string(),
                    comments: String::new(),
                }),
                edited: true,
            },
        )
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

    fn set_source(&mut self, index: usize, text: &str) -> Result<(), String> {
        check_index(self.cells.len(), index)?;
        if !self.metas[index].kind.editable() {
            return Err(if matches!(self.metas[index].kind, Kind::Broken) {
                format!(
                    "cell {index} has a syntax error and is read-only; \
                     repair its Python syntax to edit it here"
                )
            } else {
                format!(
                    "cell {index} is a marimo {} block; editing it is not supported",
                    self.metas[index].kind.label()
                )
            });
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

    fn insert(&mut self, index: usize, cell_type: &str, source: &str) -> Result<(), String> {
        if !matches!(cell_type, "code" | "markdown") {
            return Err(format!("marimo notebooks do not support {cell_type} cells"));
        }
        if index > self.cells.len() {
            return Err(format!(
                "insert index {index} out of bounds: notebook has {} cells",
                self.cells.len()
            ));
        }
        let (cell, meta) = if cell_type == "markdown" {
            self.new_markdown_cell(source)
        } else {
            new_app_cell(source)
        };
        self.insert_cell(index, cell, meta);
        Ok(())
    }

    fn delete(&mut self, index: usize) -> Result<Cell, String> {
        check_index(self.cells.len(), index)?;
        Ok(self.remove_cell(index).0)
    }

    fn move_cell(&mut self, from: usize, to: usize) -> Result<(), String> {
        check_index(self.cells.len(), from)?;
        if from == to {
            return Ok(());
        }
        let (cell, meta) = self.remove_cell(from);
        if to > self.cells.len() {
            return Err(format!(
                "destination index {to} out of bounds: notebook has {} cells",
                self.cells.len()
            ));
        }
        self.insert_cell(to, cell, meta);
        Ok(())
    }

    fn set_type(&mut self, index: usize, cell_type: &str) -> Result<(), String> {
        check_index(self.cells.len(), index)?;
        if !matches!(self.metas[index].kind, Kind::AppCell) {
            return Err(format!(
                "cell {index} is a marimo {} block; its type cannot be changed",
                self.metas[index].kind.label()
            ));
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
                return Err(format!(
                    "marimo cells are code or markdown; cannot convert to {other}"
                ));
            }
        }
        Ok(())
    }

    fn clear_outputs(&mut self, _index: Option<usize>) -> Result<Vec<usize>, String> {
        Err("marimo notebooks do not store outputs".to_string())
    }

    fn check(&self) -> Vec<Finding> {
        check::check(self)
    }

    fn serialize(&self) -> String {
        let mut out = String::new();
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

fn new_app_cell(source: &str) -> (Cell, CellMeta) {
    let indent = "    ".to_string();
    (
        Cell {
            cell_type: "code".to_string(),
            source: SourceText::Lines(SourceText::split_canonical(source)),
            extra: Default::default(),
        },
        CellMeta {
            kind: Kind::AppCell,
            block: String::new(),
            header: "@app.cell\ndef _():\n".to_string(),
            indent,
            indent_memory: None,
            return_suffix: Some("    return".to_string()),
            md: None,
            edited: true,
        },
    )
}

/// Rebuild a header for a markdown cell: ensure `hide_code=True` on the
/// decorator and set the signature to the markdown cell's only reference
/// (`mo`), unless the setup cell provides it.
fn to_markdown_header(header: &str, needs_mo: bool) -> String {
    let params = if needs_mo { "mo" } else { "" };
    let mut out = String::new();
    for line in header.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('@') && !trimmed.contains("hide_code") {
            out.push_str(&add_hide_code_to_decorator(line));
        } else if trimmed.starts_with("def ") || trimmed.starts_with("async def ") {
            let indent = &line[..line.len() - trimmed.len()];
            let (prefix, rest) = match trimmed.strip_prefix("async def ") {
                Some(rest) => ("async def ", rest),
                None => ("def ", &trimmed[4..]),
            };
            let name = rest.split('(').next().unwrap_or("_").trim();
            out.push_str(&format!("{indent}{prefix}{name}({params}):"));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// One decorator line of a header, with `hide_code=True` appended to its
/// argument list (the decorator gains parentheses when it had none).
fn add_hide_code_to_decorator(line: &str) -> String {
    match line.strip_suffix(')') {
        Some(before_close) => format!("{before_close}, hide_code=True)"),
        None => format!("{line}(hide_code=True)"),
    }
}

/// Decorator fragments carrying `hide_code=True`, paired with what replaces
/// them (keeping any remaining decorator arguments intact).
const HIDE_CODE_FRAGMENTS: [(&str, &str); 3] = [
    ("(hide_code=True, ", "("),
    (", hide_code=True)", ")"),
    ("(hide_code=True)", ""),
];

fn strip_hide_code_from_line(line: &str) -> String {
    if !line.trim_start().starts_with('@') {
        return line.to_string();
    }
    for (pattern, replacement) in HIDE_CODE_FRAGMENTS {
        if line.contains(pattern) {
            return line.replacen(pattern, replacement, 1);
        }
    }
    line.to_string()
}

/// Remove `hide_code=True` from a header's decorator (markdown → code).
/// The `join("\n") + "\n"` tail is load-bearing: an empty header yields
/// exactly "\n".
fn strip_hide_code(header: &str) -> String {
    header
        .lines()
        .map(strip_hide_code_from_line)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}
