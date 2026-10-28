//! marimo's lint view: the bodies a linter sees per cell and the
//! false-positive rules the file's shape triggers.
//!
//! Cell bodies are linted as one fabricated module (the only Python they
//! share), which makes a linter report what marimo's runtime never would:
//! names the runtime or the file header provides, `mo` redefinitions that
//! are the reactive graph's normal shape, and import-order findings that
//! exist only at module level. [`LintPolicy`] answers per finding; the
//! lint glue owns the engine calls.

use std::collections::HashSet;

use super::{Kind, MarimoNotebook, check, create, markdown, scope};

/// One lint pass's policy for a marimo file: the per-cell bodies and the
/// provided-name set the file carries.
pub(crate) struct LintPolicy {
    /// One slot per cell: `Some(body)` for a lintable body, `None` for a
    /// cell with none (broken/unparsable), so indices stay 1:1 with the
    /// cell tools and the caller pairs a skipped slot with a placeholder.
    slots: Vec<Option<String>>,
    /// The top-level names every analyzable cell defines (setup defs
    /// included).
    cell_definitions: HashSet<String>,
    /// The cell holding marimo's scaffold import, if any.
    scaffold_import_cell: Option<usize>,
}

/// What the policy says about one lint finding.
pub(crate) enum Verdict {
    /// A genuine finding.
    Keep,
    /// An artifact of linting cell bodies as one module.
    Drop,
    /// Genuine only if it also fires on the body where marimo nests it
    /// (inside the cell's `def`); the glue runs that re-check.
    RecheckNested,
}

impl LintPolicy {
    /// The policy for this file's current cells.
    pub(crate) fn new(notebook: &MarimoNotebook) -> Self {
        Self {
            slots: lint_slots(notebook),
            cell_definitions: cell_definitions(notebook),
            scaffold_import_cell: scaffold_import_cell(notebook),
        }
    }

    /// The lint bodies by cell index; see [`LintPolicy::slots`].
    pub(crate) fn slots(&self) -> &[Option<String>] {
        &self.slots
    }

    /// The module the cell body nests into in the file: the body inside
    /// its `def`, so an engine can re-check it where marimo puts it.
    pub(crate) fn nested_module(&self, cell: usize) -> String {
        let body = self
            .slots
            .get(cell)
            .and_then(Option::as_deref)
            .unwrap_or_default();
        let mut module = String::from("async def _():\n");
        for line in body.lines() {
            module.push_str("    ");
            module.push_str(line);
            module.push('\n');
        }
        module
    }

    /// What the format knows about one lint finding: the codes the file's
    /// shape makes unreliable when the bodies are linted as one module.
    pub(crate) fn verdict(&self, code: &str, token: Option<&str>, cell: Option<usize>) -> Verdict {
        match code {
            // The scaffold import cell is marimo's own, so an unused
            // `marimo` import there is scaffolding. Every other F401 naming
            // `marimo` sits in a user cell and survives.
            "F401" => match token {
                Some(token) if token == "marimo" || token.starts_with("marimo.") => {
                    match self.scaffold_import_cell {
                        Some(scaffold) if cell == Some(scaffold) => Verdict::Drop,
                        _ => Verdict::Keep,
                    }
                }
                _ => Verdict::Keep,
            },
            // `mo`/`marimo` redefinitions across cells are the graph's
            // normal shape, and a real duplicate surfaces as the graph's own
            // multiple-definition finding, so the lint echo is dropped whole
            // (the message names only the redefinition site, which does not
            // say which instance is the scaffold's).
            "F811" => match token {
                Some(token) if token == "mo" || token == "marimo" => Verdict::Drop,
                _ => Verdict::Keep,
            },
            // A name the file provides is not "undefined": marimo's
            // builtins, the header's bindings, and cell defs.
            "F821" => {
                if token.is_some_and(|token| self.provides(token)) {
                    Verdict::Drop
                } else {
                    Verdict::Keep
                }
            }
            // Only the flavor ruff-on-the-file would report survives; the
            // glue re-checks the body where marimo nests it.
            "I001" => Verdict::RecheckNested,
            _ => Verdict::Keep,
        }
    }

    /// Whether the file provides `name` to its cells: marimo's runtime, the
    /// file header, or some cell's top-level defs.
    fn provides(&self, name: &str) -> bool {
        runtime_builtin(name) || header_binds(name) || self.cell_definitions.contains(name)
    }
}

/// The lint bodies, one slot per cell. Parsing already strips the fixed
/// indent and the trailing `return`, so a code app cell or a setup cell is
/// just its dedented body; a markdown cell contributes its `mo.md(...)`
/// wrapper call (the only Python it compiles to); a function/class block is
/// linted verbatim (decorator and signature are real Python, not
/// scaffolding); broken/unparsable cells have no valid body and are skipped.
fn lint_slots(notebook: &MarimoNotebook) -> Vec<Option<String>> {
    notebook
        .cells
        .iter()
        .zip(&notebook.metas)
        .map(|(cell, meta)| match meta.kind {
            Kind::Broken | Kind::Unparsable => None,
            Kind::AppCell => Some(match &meta.md {
                Some(md) => markdown::wrap_call(&cell.source.text(), "", &md.tag),
                None => cell.source.text().into_owned(),
            }),
            Kind::Setup => Some(cell.source.text().into_owned()),
            Kind::Function | Kind::Class => Some(meta.block.clone()),
        })
        .collect()
}

/// The top-level names every analyzable cell defines (setup defs included).
/// Together with [`runtime_builtin`], a name the marimo runtime binds is
/// not "undefined" just because the concatenated cell bodies do not bind
/// it: the graph half of the F821 rule's provided set.
fn cell_definitions(notebook: &MarimoNotebook) -> HashSet<String> {
    let mut names = HashSet::new();
    for (cell, meta) in notebook.cells.iter().zip(&notebook.metas) {
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

/// The cell holding marimo's scaffold import (exactly [`create::MO_IMPORT`],
/// the shape `ensure_mo_definition` injects): an unused `marimo` import in
/// *that* cell is scaffolding, not the user's. The first such cell (where
/// the auto-fix would place it). `None` when no cell carries the shape; a
/// user cell repeating the exact scaffold line in a notebook with no
/// scaffold is indistinguishable and counted as scaffolding.
fn scaffold_import_cell(notebook: &MarimoNotebook) -> Option<usize> {
    notebook
        .cells
        .iter()
        .zip(&notebook.metas)
        .position(|(cell, meta)| {
            meta.md.is_none() && cell.source.text().trim() == create::MO_IMPORT
        })
}

/// Whether the marimo runtime provides `name` as a builtin, the runtime
/// half of the F821 rule's provided set (checked per name so the builtin
/// list is never materialized per call).
fn runtime_builtin(name: &str) -> bool {
    check::is_marimo_builtin(name)
}

/// Whether the file header binds `name` outside any cell: `import marimo`
/// and `app = marimo.App(...)`, both required for the file to count as
/// marimo at all, plus `__generated_with`. The header half of the F821
/// rule's provided set: a cell body touching `app` (a `@app.function`
/// block's decorator, say) references a real binding.
fn header_binds(name: &str) -> bool {
    matches!(name, "marimo" | "app" | "__generated_with")
}
