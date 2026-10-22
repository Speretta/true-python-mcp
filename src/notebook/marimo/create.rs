//! Construction of new marimo cells and files: the empty-file scaffold,
//! the app/markdown cell shapes tpmcp generates, the markdown-header surgery
//! behind `set_type`, and the `mo` definition markdown cells need to run.
//!
//! Everything here writes NEW blocks (created files, inserted or converted
//! cells); parsed files serialize from their captured bytes and never pass
//! through this module.

use std::collections::BTreeSet;

use ruff_python_ast::Stmt;
use ruff_python_parser::parse_module;
use ruff_text_size::Ranged;

use super::{CellMeta, Kind, MarimoNotebook, MdInfo, Segment, parse::guarded_drop, scope};
use crate::error::Error;
use crate::notebook::{Cell, Notebook};

/// Version stamped into newly created marimo files. Inert metadata: marimo
/// never validates it and refreshes it on its next save, so a stale value
/// cannot affect compatibility. Bump only for cosmetic freshness.
pub const MARIMO_VERSION: &str = "0.25.2";

/// The import markdown cells need: every `mo.md(...)` cell takes `mo` as a
/// graph reference, and marimo's own converter adds this cell when a file
/// gains markdown cells without a definition of `mo`.
const MO_IMPORT: &str = "import marimo as mo";

impl MarimoNotebook {
    /// A new empty marimo notebook (header + run guard, no cells). When
    /// `dependencies` or `requires_python` is supplied, a PEP 723
    /// `# /// script` block is emitted above `import marimo`.
    pub(crate) fn create(dependencies: Option<&[String]>, requires_python: Option<&str>) -> Self {
        let header = pep723_header(dependencies, requires_python).unwrap_or_default();
        let head = format!(
            "{header}import marimo\n\n__generated_with = \"{MARIMO_VERSION}\"\napp = marimo.App()\n"
        );
        Self {
            cells: Vec::new(),
            metas: Vec::new(),
            segments: vec![
                Segment::Lit(head),
                // The header literal ends with a newline, so the gap to the
                // run guard is two newlines (two blank lines), the spacing
                // marimo writes between its header and the first block.
                Segment::Lit("\n\n".to_string()),
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
        // The same predicate check.rs's missing-mo finding guards on, so the
        // auto-fix repairs exactly the notebooks the check flags.
        if self.has_markdown_cell() && !self.defines("mo") {
            self.insert(0, "code", MO_IMPORT)?;
            return Ok(Some(format!(
                "added a first cell `{MO_IMPORT}`: markdown cells need a definition of `mo` \
                 to run in marimo"
            )));
        }
        Ok(None)
    }

    /// Rewrite the generated cells' trailing `return` and signature the way
    /// marimo's serializer writes them: `return` names exactly the cell's
    /// defs that some cell refs (sorted), and the signature takes exactly
    /// the refs that are not allowed (own defs, setup defs, unshadowed
    /// builtins), sorted, the same expectations check.rs reports drift
    /// against, so a freshly built file carries no stale-return or
    /// stale-signature findings. Parsed cells serialize from their captured
    /// bytes and are never touched, so roundtrips stay byte-exact. Very long
    /// name lists keep the one-line shape (marimo wraps them into multiline
    /// tuples and rewraps ours on its next save).
    pub(crate) fn canonicalize_generated_cells(&mut self) {
        let mut used_refs: BTreeSet<String> = BTreeSet::new();
        let mut all_defs: BTreeSet<String> = BTreeSet::new();
        let mut setup_defs: BTreeSet<String> = BTreeSet::new();
        let mut infos: Vec<Option<scope::CellInfo>> = Vec::with_capacity(self.cells.len());
        for (cell, meta) in self.cells.iter().zip(&self.metas) {
            // A markdown cell's wrapper call refs `mo`; its source is not Python.
            if meta.md.is_some() {
                used_refs.insert("mo".to_string());
                infos.push(None);
                continue;
            }
            let info = scope::analyze(&cell.source.text());
            if let Some(info) = &info {
                used_refs.extend(info.refs.iter().cloned());
                all_defs.extend(info.defs.iter().cloned());
                if matches!(meta.kind, Kind::Setup) {
                    setup_defs.extend(info.defs.iter().cloned());
                }
            }
            infos.push(info);
        }
        for (index, meta) in self.metas.iter_mut().enumerate() {
            let generated = meta.block.is_empty()
                && meta.edited
                && meta.md.is_none()
                && matches!(meta.kind, Kind::AppCell);
            if !generated {
                continue;
            }
            let Some(info) = infos[index].as_ref() else {
                continue;
            };
            if info.is_sql {
                continue; // marimo keeps SQL cells' defs out of the return
            }
            let mut exported: Vec<&str> = info
                .defs
                .iter()
                .map(String::as_str)
                .filter(|name| used_refs.contains(*name))
                .collect();
            exported.sort_unstable();
            let mut params: Vec<&str> = info
                .refs
                .iter()
                .map(String::as_str)
                .filter(|name| {
                    !info.defs.contains(*name)
                        && !setup_defs.contains(*name)
                        && !(super::check::is_marimo_builtin(name) && !all_defs.contains(*name))
                })
                .filter(|name| ruff_python_stdlib::identifiers::is_identifier(name))
                .collect();
            params.sort_unstable();
            let blank_line = meta
                .return_suffix
                .as_deref()
                .is_some_and(|suffix| suffix.starts_with('\n'));
            meta.return_suffix = Some(return_statement(&meta.indent, &exported, blank_line));
            meta.header = format!("@app.cell\ndef _({}):\n", params.join(", "));
        }
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
    let return_suffix = Some(format!(
        "{}{indent}return",
        if blank_before_return(source) {
            "\n"
        } else {
            ""
        }
    ));
    (
        Cell::from_text("code", source),
        CellMeta {
            kind: Kind::AppCell,
            block: String::new(),
            header: "@app.cell\ndef _():\n".to_string(),
            indent,
            indent_memory: None,
            return_suffix,
            md: None,
            edited: true,
        },
    )
}

/// Whether marimo's serializer separates the trailing `return` with a blank
/// line: its `_needs_trailing_blank_line` puts one after a final import,
/// function or class definition, and only when that statement is truly the
/// last content (no trailing comment lines).
fn blank_before_return(source: &str) -> bool {
    let Ok(parsed) = parse_module(source) else {
        return false;
    };
    let needs_blank = match parsed.syntax().body.last() {
        Some(
            stmt @ (Stmt::Import(_)
            | Stmt::ImportFrom(_)
            | Stmt::FunctionDef(_)
            | Stmt::ClassDef(_)),
        ) => {
            let code_lines = source.trim_end().matches('\n').count() + 1;
            let statement_end = stmt.range().end().to_usize();
            source[..statement_end].matches('\n').count() + 1 == code_lines
        }
        _ => false,
    };
    guarded_drop(parsed, source.len());
    needs_blank
}

/// marimo's `return` shape (`format_tuple_elements` with a naked empty and
/// single-name forms): bare, `(name,)` for one, paren-less for several.
fn return_statement(indent: &str, names: &[&str], blank_line: bool) -> String {
    let prefix = if blank_line { "\n" } else { "" };
    match names {
        [] => format!("{prefix}{indent}return"),
        [one] => format!("{prefix}{indent}return ({one},)"),
        many => format!("{prefix}{indent}return {}", many.join(", ")),
    }
}

/// The arguments of a `marimo.App(...)` call in a literal segment, when it
/// carries any configuration.
fn app_configuration(text: &str) -> Option<String> {
    let after = text.split("marimo.App(").nth(1)?;
    let arguments = after.split(')').next().unwrap_or("").trim();
    (!arguments.is_empty()).then(|| arguments.to_string())
}

/// The PEP 723 inline-metadata header a created marimo file carries when the
/// caller supplies `dependencies` or `requires_python`. Emitted above
/// `import marimo`, matching marimo's own serializer (`tomlkit.dumps` wrapped
/// in the `# /// script` markers): a double-quoted `requires-python`, a
/// single-line double-quoted `dependencies` array, the closing marker, and the
/// one blank line marimo writes between the header and the import. `None`
/// when neither is supplied, so the default scaffold stays byte-identical.
fn pep723_header(dependencies: Option<&[String]>, requires_python: Option<&str>) -> Option<String> {
    if dependencies.is_none() && requires_python.is_none() {
        return None;
    }
    let mut lines = vec!["# /// script".to_string()];
    if let Some(python) = requires_python {
        lines.push(format!("# requires-python = {}", toml_string(python)));
    }
    if let Some(deps) = dependencies {
        let quoted: Vec<String> = deps.iter().map(|dep| toml_string(dep)).collect();
        lines.push(format!("# dependencies = [{}]", quoted.join(", ")));
    }
    lines.push("# ///".to_string());
    Some(format!("{}\n\n", lines.join("\n")))
}

/// Quote a value for a double-quoted TOML basic string: escape `\` and `"`,
/// the only escapes a PEP 723 dependency spec or version range can contain.
fn toml_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

/// Lines of the standard file scaffold: the imports, the version stamp, the
/// app declaration, the run guard and the PEP 723 header marimo writes above
/// the import. Everything else in a literal segment is user content a
/// conversion would leave behind.
fn is_scaffold_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed == "import marimo"
        || trimmed.starts_with("__generated_with")
        || trimmed.starts_with("app = marimo.App")
        || trimmed.starts_with("if __name__")
        || trimmed == "app.run()"
        // A PEP 723 inline-metadata block. The closing `# ///` is also the
        // prefix of the opening `# /// script`, so match the opening form
        // first; `# requires-python =` and `# dependencies =` cover the
        // single-line key/value shape this project emits.
        || trimmed == "# /// script"
        || trimmed == "# ///"
        || trimmed.starts_with("# requires-python =")
        || trimmed.starts_with("# dependencies =")
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
