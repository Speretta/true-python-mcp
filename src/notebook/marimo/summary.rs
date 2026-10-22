//! The reactive-graph name sets shared by drift detection and canonicalization.
//!
//! A marimo cell's `return` and signature are derived from four name sets —
//! which names are referenced, defined, setup-provided, or toplevel-bound
//! across the whole notebook. [`MarimoNotebook::graph_summary`] computes them
//! once; `check.rs` compares a stored `return`/signature against them (drift)
//! and `create.rs` writes them back (canonicalization), so both agree.

use std::collections::BTreeSet;

use super::check::is_marimo_builtin;
use super::scope::{self, CellInfo};
use super::{CellMeta, Kind, MarimoNotebook};

/// The name sets a notebook's reactive graph exposes, the single source of
/// truth for what counts as a referenced, defined, setup-provided, or
/// toplevel-bound name.
pub(super) struct GraphSummary {
    /// Names some cell references.
    used_refs: BTreeSet<String>,
    /// Names some cell defines.
    all_defs: BTreeSet<String>,
    /// Names the setup cell defines.
    setup_defs: BTreeSet<String>,
    /// Names bound by read-only `@app.function` / `@app.class_definition`
    /// blocks; they join the allowed refs.
    toplevel_names: BTreeSet<String>,
}

impl GraphSummary {
    /// The defs a cell's `return` should name, sorted: its top-level defs
    /// that some cell references.
    pub(super) fn returned<'a>(&self, info: &'a CellInfo) -> Vec<&'a str> {
        let mut names: Vec<&str> = info
            .defs
            .iter()
            .map(String::as_str)
            .filter(|name| self.used_refs.contains(*name))
            .collect();
        names.sort_unstable();
        names
    }

    /// The refs a cell's signature should take as parameters, sorted: every
    /// ref that is not an own def, a setup def, a toplevel name, or an
    /// unshadowed builtin.
    pub(super) fn params<'a>(&self, info: &'a CellInfo) -> Vec<&'a str> {
        let mut names: Vec<&str> = info
            .refs
            .iter()
            .map(String::as_str)
            .filter(|name| {
                let name = *name;
                !info.defs.contains(name)
                    && !self.setup_defs.contains(name)
                    && !self.toplevel_names.contains(name)
                    && !self.is_unshadowed_builtin(name)
            })
            .filter(|name| ruff_python_stdlib::identifiers::is_identifier(name))
            .collect();
        names.sort_unstable();
        names
    }

    /// Whether `name` is a builtin no cell shadows.
    pub(super) fn is_unshadowed_builtin(&self, name: &str) -> bool {
        is_marimo_builtin(name) && !self.all_defs.contains(name)
    }
}

impl MarimoNotebook {
    /// Analyze every cell body once into the [`GraphSummary`] sets, the same
    /// way drift detection's graph build does: markdown cells contribute `mo`,
    /// SQL cells are excluded, and read-only function/class blocks add their
    /// definition name to `toplevel_names`.
    pub(super) fn graph_summary(&self) -> GraphSummary {
        let mut summary = GraphSummary {
            used_refs: BTreeSet::new(),
            all_defs: BTreeSet::new(),
            setup_defs: BTreeSet::new(),
            toplevel_names: BTreeSet::new(),
        };
        for (cell, meta) in self.cells.iter().zip(&self.metas) {
            if matches!(meta.kind, Kind::Broken | Kind::Unparsable) {
                continue;
            }
            if matches!(meta.kind, Kind::Function | Kind::Class)
                && let Some(name) = block_definition_name(meta)
            {
                summary.toplevel_names.insert(name);
            }
            if meta.md.is_some() {
                summary.used_refs.insert("mo".to_string());
                continue;
            }
            let Some(info) = scope::analyze(&cell.source.text()) else {
                continue;
            };
            if info.is_sql {
                continue;
            }
            summary.used_refs.extend(info.refs.iter().cloned());
            summary.all_defs.extend(info.defs.iter().cloned());
            if matches!(meta.kind, Kind::Setup) {
                summary.setup_defs.extend(info.defs.iter().cloned());
            }
        }
        summary
    }
}

/// The name defined by a read-only `@app.function` / `@app.class_definition`
/// block (its names join the allowed refs, so other cells do not take them
/// as signature parameters).
fn block_definition_name(meta: &CellMeta) -> Option<String> {
    let parsed = ruff_python_parser::parse_module(&meta.block).ok()?;
    let mut name = None;
    for stmt in parsed.syntax().body.iter() {
        match stmt {
            ruff_python_ast::Stmt::FunctionDef(function) => {
                name = Some(function.name.to_string());
                break;
            }
            ruff_python_ast::Stmt::ClassDef(class) => {
                name = Some(class.name.to_string());
                break;
            }
            _ => {}
        }
    }
    super::parse::guarded_drop(parsed, meta.block.len());
    name
}
