//! Reactive-graph validation for marimo notebooks.
//!
//! Mirrors what the marimo runtime checks when a notebook loads
//! (`marimo/_lint/validate_graph.py` + the dataflow graph): a name defined by
//! more than one cell, dependency cycles, a setup cell that depends on other
//! cells, and unparsable regions. On top of that we report drift that marimo's
//! own save would silently regenerate — stale `return` tuples and stale
//! signature parameters — as informational findings, because an LLM editing
//! cells should know the file no longer matches what marimo would write.
//!
//! SQL cells (`mo.sql(...)`) are excluded from graph analysis: their defs and
//! refs come from SQL parsing, which V1 does not implement.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::cycles::cycles;

use super::scope::{self, CellInfo};
use super::{CellMeta, Kind, MarimoNotebook};
use crate::notebook::{Finding, Severity};

/// BUILTINS as marimo sees them: the interpreter's builtins plus the module
/// dunders and special names in `builtins.__dict__` (marimo/_ast/variables.py).
///
/// ASSUMPTION (honest): marimo's real set is the `builtins.__dict__` of the
/// interpreter that *runs* marimo, which we cannot read without one. We
/// approximate it with ruff's CPython builtins pinned to Python 3.13
/// ([`MARIMO_BUILTIN_MINOR_VERSION`]), independent of the environment `ty_check`
/// discovers. The stale-signature finding names this when it shapes a result.
pub(super) fn is_marimo_builtin(name: &str) -> bool {
    ruff_python_stdlib::builtins::is_python_builtin(name, MARIMO_BUILTIN_MINOR_VERSION, false)
        || matches!(
            name,
            "__builtin__"
                | "__file__"
                | "__builtins__"
                | "__class__"
                | "__name__"
                | "__doc__"
                | "__package__"
                | "__loader__"
                | "__spec__"
                | "__debug__"
                | "__build_class__"
                | "__import__"
                | "_IncompleteInputError"
        )
}

/// The CPython minor version whose builtin set [`is_marimo_builtin`] assumes —
/// the newest the linked ruff knows, not the discovered environment's.
const MARIMO_BUILTIN_MINOR_VERSION: u8 = 13;

/// A cell prepared for graph analysis.
struct Node {
    info: CellInfo,
    /// Display label used in messages, e.g. "cell 2" or "the setup cell".
    label: String,
}

/// Names bound by a `return (a, b)` / `return x` suffix.
fn parse_return(suffix: &str) -> Vec<String> {
    let rest = suffix.trim();
    let Some(rest) = rest.strip_prefix("return") else {
        return Vec::new();
    };
    let rest = rest.trim();
    if rest.is_empty() {
        return Vec::new();
    }
    split_top_level(rest.trim_matches(|c| c == '(' || c == ')'))
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parameter names of a `def _(a, b: int):` signature line.
fn parse_params(header: &str) -> Vec<String> {
    let Some(line) = header.lines().find(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("def ") || trimmed.starts_with("async def ")
    }) else {
        return Vec::new();
    };
    let Some(open) = line.find('(') else {
        return Vec::new();
    };
    let Some(close) = line.rfind(')') else {
        return Vec::new();
    };
    if close <= open {
        return Vec::new();
    }
    split_top_level(&line[open + 1..close])
        .into_iter()
        .map(|part| part.split(':').next().unwrap_or("").trim())
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// Split on commas that are not nested in brackets.
fn split_top_level(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, character) in text.char_indices() {
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

fn cells_list(cells: &[usize]) -> String {
    cells
        .iter()
        .map(|index| index.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Validate the notebook. Deterministic order: syntax, multiple definitions,
/// cycles, setup root, then informational drift by cell.
pub(super) fn check(notebook: &MarimoNotebook) -> Vec<Finding> {
    let mut graph = Graph::build(notebook);
    let mut findings = std::mem::take(&mut graph.syntax_findings);
    findings.extend(graph.multiple_definition_findings());
    findings.extend(graph.cycle_findings());
    findings.extend(graph.setup_root_findings());
    findings.extend(graph.drift_findings(notebook));
    findings
}

/// The reactive graph: analyzed cells, name indexes, and dependency edges —
/// everything the finding passes read. Built once per check.
struct Graph {
    /// Analyzed Python cells by index (SQL cells excluded, like marimo).
    nodes: BTreeMap<usize, Node>,
    /// Names bound by read-only `@app.function` / `@app.class_definition`
    /// blocks; they join the allowed refs.
    toplevel_names: BTreeSet<String>,
    setup_index: Option<usize>,
    /// Broken cells and unparsable bodies, in cell order.
    syntax_findings: Vec<Finding>,
    /// Name -> defining / referring / deleting cells.
    definers: BTreeMap<String, Vec<usize>>,
    referrers: BTreeMap<String, Vec<usize>>,
    deleters: BTreeMap<String, Vec<usize>>,
    /// marimo's `compute_edges_for_cell`: cell -> the cells it depends on.
    parents: HashMap<usize, Vec<usize>>,
}

impl Graph {
    fn build(notebook: &MarimoNotebook) -> Self {
        let mut graph = Self {
            nodes: BTreeMap::new(),
            toplevel_names: BTreeSet::new(),
            setup_index: None,
            syntax_findings: Vec::new(),
            definers: BTreeMap::new(),
            referrers: BTreeMap::new(),
            deleters: BTreeMap::new(),
            parents: HashMap::new(),
        };
        graph.build_nodes(notebook);
        graph.index_names();
        graph.compute_edges();
        graph
    }

    /// Analyze every cell into a node, collecting syntax findings and the
    /// setup / toplevel-name indexes along the way, in cell order.
    fn build_nodes(&mut self, notebook: &MarimoNotebook) {
        for (index, (cell, meta)) in notebook.cells.iter().zip(&notebook.metas).enumerate() {
            match meta.kind {
                Kind::Broken => {
                    self.syntax_findings.push(Finding {
                        severity: Severity::Error,
                        cell: Some(index),
                        code: "syntax-error".to_string(),
                        message: format!(
                            "cell {index} contains a Python syntax error; marimo cannot run this \
                             notebook until it is repaired"
                        ),
                    });
                    continue;
                }
                // The runtime registers unparsable cells without compiling them.
                Kind::Unparsable => continue,
                Kind::Function | Kind::Class => {
                    if let Some(name) = block_definition_name(meta) {
                        self.toplevel_names.insert(name);
                    }
                }
                Kind::Setup => self.setup_index = Some(index),
                Kind::AppCell => {}
            }
            let label = match meta.kind {
                Kind::Setup => "the setup cell".to_string(),
                _ => format!("cell {index}"),
            };
            let info = if meta.md.is_some() {
                // A markdown cell is exactly `mo.md(constant)`: its only ref is mo.
                CellInfo {
                    defs: HashSet::new(),
                    refs: HashSet::from(["mo".to_string()]),
                    deleted_refs: HashSet::new(),
                    is_sql: false,
                }
            } else {
                let Some(info) = scope::analyze(&cell.source.text()) else {
                    self.syntax_findings.push(Finding {
                        severity: Severity::Error,
                        cell: Some(index),
                        code: "syntax-error".to_string(),
                        message: format!("cell {index} does not parse as Python"),
                    });
                    continue;
                };
                info
            };
            if info.is_sql {
                continue;
            }
            self.nodes.insert(index, Node { info, label });
        }
    }

    /// Build the name -> defining / referring / deleting cells indexes.
    fn index_names(&mut self) {
        for (index, node) in &self.nodes {
            for name in &node.info.defs {
                self.definers.entry(name.clone()).or_default().push(*index);
            }
            for name in &node.info.refs {
                self.referrers.entry(name.clone()).or_default().push(*index);
            }
            for name in &node.info.deleted_refs {
                self.deleters.entry(name.clone()).or_default().push(*index);
            }
        }
    }

    /// Dependency edges (marimo's compute_edges_for_cell, Python cells only).
    fn compute_edges(&mut self) {
        let empty: Vec<usize> = Vec::new();
        for (index, node) in &self.nodes {
            let mut set: BTreeSet<usize> = BTreeSet::new();
            for name in &node.info.refs {
                for definer in self.definers.get(name.as_str()).unwrap_or(&empty) {
                    if definer != index {
                        set.insert(*definer);
                    }
                }
                for deleter in self.deleters.get(name.as_str()).unwrap_or(&empty) {
                    if deleter != index {
                        set.insert(*deleter);
                    }
                }
            }
            for name in &node.info.deleted_refs {
                for referrer in self.referrers.get(name.as_str()).unwrap_or(&empty) {
                    if referrer != index {
                        set.insert(*referrer);
                    }
                }
            }
            self.parents.insert(*index, set.into_iter().collect());
        }
    }

    /// A name defined by more than one cell, in name order.
    fn multiple_definition_findings(&self) -> Vec<Finding> {
        let mut findings = Vec::new();
        for (name, cells) in &self.definers {
            if cells.len() > 1 {
                findings.push(Finding {
                    severity: Severity::Error,
                    cell: Some(cells[0]),
                    code: "multiple-definitions".to_string(),
                    message: format!(
                        "`{name}` is defined by multiple cells ({}); marimo only keeps one",
                        cells_list(cells)
                    ),
                });
            }
        }
        findings
    }

    /// Dependency cycles (strongly connected components), in component order.
    fn cycle_findings(&self) -> Vec<Finding> {
        let mut findings = Vec::new();
        for component in cycles(&self.parents) {
            let mut vars: BTreeSet<&str> = BTreeSet::new();
            for to in &component {
                for from in self.parents.get(to).into_iter().flatten() {
                    if !component.contains(from) {
                        continue;
                    }
                    // from -> to edge: to's refs meet from's defs (or deletes).
                    for name in &self.nodes[to].info.refs {
                        if self.nodes[from].info.defs.contains(name) {
                            vars.insert(name);
                        }
                    }
                    for name in &self.nodes[to].info.deleted_refs {
                        if self.nodes[from].info.refs.contains(name) {
                            vars.insert(name);
                        }
                    }
                }
            }
            let via = if vars.is_empty() {
                String::new()
            } else {
                format!(" via {}", vars.into_iter().collect::<Vec<_>>().join(", "))
            };
            findings.push(Finding {
                severity: Severity::Error,
                cell: Some(component[0]),
                code: "cycle".to_string(),
                message: format!(
                    "cells [{}] form a dependency cycle{via}",
                    cells_list(&component)
                ),
            });
        }
        findings
    }

    /// The setup cell must not depend on other cells.
    fn setup_root_findings(&self) -> Vec<Finding> {
        let mut findings = Vec::new();
        if let Some(setup) = self.setup_index
            && self.nodes.contains_key(&setup)
        {
            let mut ancestors: BTreeSet<usize> = BTreeSet::new();
            let mut queue: Vec<usize> = self.parents.get(&setup).cloned().unwrap_or_default();
            while let Some(cell) = queue.pop() {
                if ancestors.insert(cell) {
                    queue.extend(self.parents.get(&cell).cloned().unwrap_or_default());
                }
            }
            if !ancestors.is_empty() {
                let names: BTreeSet<&str> = ancestors
                    .iter()
                    .flat_map(|ancestor| self.nodes[ancestor].info.defs.iter())
                    .filter(|name| self.nodes[&setup].info.refs.contains(*name))
                    .map(String::as_str)
                    .collect();
                findings.push(Finding {
                    severity: Severity::Error,
                    cell: Some(setup),
                    code: "setup-root".to_string(),
                    message: format!(
                        "the setup cell must not depend on other cells, but it uses {} \
                         defined by cells [{}]",
                        names.into_iter().collect::<Vec<_>>().join(", "),
                        cells_list(&ancestors.into_iter().collect::<Vec<_>>())
                    ),
                });
            }
        }
        findings
    }

    /// Informational drift: stale returns and signatures that marimo's own
    /// save would regenerate, by cell.
    fn drift_findings(&self, notebook: &MarimoNotebook) -> Vec<Finding> {
        let mut findings = Vec::new();
        let all_defs: BTreeSet<&str> = self
            .nodes
            .values()
            .flat_map(|node| node.info.defs.iter())
            .map(String::as_str)
            .collect();
        let used_refs: BTreeSet<&str> = self
            .nodes
            .values()
            .flat_map(|node| node.info.refs.iter())
            .map(String::as_str)
            .collect();
        let setup_defs: BTreeSet<&str> = self
            .setup_index
            .and_then(|setup| self.nodes.get(&setup))
            .map(|node| node.info.defs.iter().map(String::as_str).collect())
            .unwrap_or_default();

        for (index, node) in &self.nodes {
            let meta: &CellMeta = &notebook.metas[*index];
            if !matches!(meta.kind, Kind::AppCell) {
                continue;
            }
            // Stale return: marimo writes `return` with exactly the defs that are
            // referenced somewhere, sorted.
            let expected_return: Vec<&str> = {
                let mut names: Vec<&str> = node
                    .info
                    .defs
                    .iter()
                    .map(String::as_str)
                    .filter(|name| used_refs.contains(name))
                    .collect();
                names.sort_unstable();
                names
            };
            let stored_return = meta
                .return_suffix
                .as_deref()
                .map(parse_return)
                .unwrap_or_default();
            if !stored_return
                .iter()
                .map(String::as_str)
                .eq(expected_return.iter().copied())
            {
                findings.push(Finding {
                    severity: Severity::Info,
                    cell: Some(*index),
                    code: "stale-return".to_string(),
                    message: format!(
                        "{} returns [{}] but marimo would regenerate [{}]; the return is refreshed \
                         on marimo's next save",
                        node.label,
                        stored_return.join(", "),
                        expected_return.join(", ")
                    ),
                });
            }
            // Stale signature: marimo writes parameters for exactly the refs that
            // are not allowed (setup defs, unshadowed builtins, toplevel names)
            // and not defined by the cell itself, sorted.
            let mut assumed_builtins: Vec<&str> = Vec::new();
            let expected_params: Vec<&str> = {
                let mut names: Vec<&str> = node
                    .info
                    .refs
                    .iter()
                    .map(String::as_str)
                    .filter(|name| {
                        let assumed_builtin = is_marimo_builtin(name) && !all_defs.contains(name);
                        if assumed_builtin {
                            assumed_builtins.push(*name);
                        }
                        !node.info.defs.contains(*name)
                            && !setup_defs.contains(name)
                            && !self.toplevel_names.contains(*name)
                            && !assumed_builtin
                    })
                    .filter(|name| ruff_python_stdlib::identifiers::is_identifier(name))
                    .collect();
                names.sort_unstable();
                names
            };
            let stored_params = parse_params(&meta.header);
            if !stored_params
                .iter()
                .map(String::as_str)
                .eq(expected_params.iter().copied())
            {
                // The builtin set is approximated as Python 3.13's (see
                // [`is_marimo_builtin`]); say so only when it actually shaped
                // this finding, so the assumption is never silent.
                let builtin_note = if assumed_builtins.is_empty() {
                    String::new()
                } else {
                    assumed_builtins.sort_unstable();
                    format!(
                        " (assumed builtin {} as of Python 3.13, not the discovered environment)",
                        assumed_builtins.join(", ")
                    )
                };
                findings.push(Finding {
                    severity: Severity::Info,
                    cell: Some(*index),
                    code: "stale-signature".to_string(),
                    message: format!(
                        "{} takes ({}) but marimo would regenerate ({}); the signature is refreshed \
                         on marimo's next save{builtin_note}",
                        node.label,
                        stored_params.join(", "),
                        expected_params.join(", ")
                    ),
                });
            }
        }
        findings
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

#[cfg(test)]
#[path = "check_tests.rs"]
mod tests;
