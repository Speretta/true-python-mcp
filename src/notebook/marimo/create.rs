//! Construction of new marimo cells and files: the empty-file scaffold,
//! the app/markdown cell shapes tpmcp generates, the markdown-header surgery
//! behind `set_type`, and the `mo` definition markdown cells need to run.
//!
//! Everything here writes NEW blocks (created files, inserted or converted
//! cells); parsed files serialize from their captured bytes and never pass
//! through this module.

use super::{CellMeta, Kind, MarimoNotebook, MdInfo, Segment, scope};
use crate::error::Error;
use crate::notebook::{Cell, Notebook};

/// Version stamped into newly created marimo files. marimo never validates it
/// and refreshes it on its next save.
const MARIMO_VERSION: &str = "0.25.1";

/// The import markdown cells need: every `mo.md(...)` cell takes `mo` as a
/// graph reference, and marimo's own converter adds this cell when a file
/// gains markdown cells without a definition of `mo`.
const MO_IMPORT: &str = "import marimo as mo";

impl MarimoNotebook {
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

    /// A new markdown cell: `@app.cell(hide_code=True)` wrapping the content
    /// in `mo.md(r""")`, with `mo` as a parameter unless setup provides it.
    pub(super) fn new_markdown_cell(&self, source: &str) -> (Cell, CellMeta) {
        let indent = "    ".to_string();
        let params = if self.setup_defines("mo") { "" } else { "mo" };
        (
            Cell::from_text("markdown", source),
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

    /// Inject the `import marimo as mo` cell that marimo's own converter adds:
    /// markdown cells call `mo.md(...)`, and a file where no cell binds `mo`
    /// fails with a NameError in the marimo runtime even though it parses and
    /// compiles. Returns the warning announcing the added cell, or `None`
    /// when nothing was needed.
    pub(crate) fn ensure_mo_definition(&mut self) -> Result<Option<String>, Error> {
        let has_markdown = self.cells.iter().any(Cell::is_markdown);
        if !has_markdown || self.defines("mo") {
            return Ok(None);
        }
        self.insert(0, "code", MO_IMPORT)?;
        Ok(Some(format!(
            "added a first cell `{MO_IMPORT}`: markdown cells need a definition of `mo` \
             to run in marimo"
        )))
    }

    /// Whether any cell binds `name` at its top level: a setup block makes it
    /// global, a plain app cell feeds it through the graph. Markdown cells
    /// (whose source is markdown text, not Python) and unparsable or broken
    /// regions cannot be analyzed and never count as definers.
    fn defines(&self, name: &str) -> bool {
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
    /// What converting this file into another format leaves behind: literal
    /// text outside cells (module comments, PEP 723 dependency blocks, stray
    /// code) and `marimo.App(...)` configuration. One warning per category,
    /// only when present, so a conversion never drops them silently.
    pub(crate) fn conversion_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut stray_example: Option<String> = None;
        for segment in &self.segments {
            let Segment::Lit(text) = segment else {
                continue;
            };
            if let Some(configuration) = app_configuration(text) {
                warnings.push(format!(
                    "marimo.App configuration ({configuration}) is not carried over"
                ));
            }
            for line in text.lines() {
                if !line.trim().is_empty() && !is_scaffold_line(line) {
                    stray_example.get_or_insert_with(|| truncate_example(line));
                }
            }
        }
        if let Some(example) = stray_example {
            warnings.push(format!(
                "top-level lines outside cells (e.g. `{example}`) are not carried over"
            ));
        }
        warnings
    }
}

/// A new code cell: the plain `@app.cell` shape marimo writes for a body
/// without references of its own; marimo regenerates the signature and the
/// return on its next save.
pub(super) fn new_app_cell(source: &str) -> (Cell, CellMeta) {
    let indent = "    ".to_string();
    (
        Cell::from_text("code", source),
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

/// The arguments of a `marimo.App(...)` call in a literal segment, when it
/// carries any configuration.
fn app_configuration(text: &str) -> Option<String> {
    let after = text.split("marimo.App(").nth(1)?;
    let arguments = after.split(')').next().unwrap_or("").trim();
    (!arguments.is_empty()).then(|| arguments.to_string())
}

/// Lines of the standard file scaffold: the imports, the version stamp, the
/// app declaration and the run guard. Everything else in a literal segment
/// is user content a conversion would leave behind.
fn is_scaffold_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed == "import marimo"
        || trimmed.starts_with("__generated_with")
        || trimmed.starts_with("app = marimo.App")
        || trimmed.starts_with("if __name__")
        || trimmed == "app.run()"
}

/// One literal line shortened for a warning message (byte-safe).
fn truncate_example(line: &str) -> String {
    let trimmed = line.trim();
    let end = trimmed.floor_char_boundary(60);
    if end < trimmed.len() {
        format!("{}…", &trimmed[..end])
    } else {
        trimmed.to_string()
    }
}

/// Rebuild a header for a markdown cell: ensure `hide_code=True` on the
/// decorator and set the signature to the markdown cell's only reference
/// (`mo`), unless the setup cell provides it.
pub(super) fn to_markdown_header(header: &str, needs_mo: bool) -> String {
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
pub(super) fn strip_hide_code(header: &str) -> String {
    header
        .lines()
        .map(strip_hide_code_from_line)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}
