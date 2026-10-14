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
fn is_marimo_builtin(name: &str) -> bool {
    ruff_python_stdlib::builtins::is_python_builtin(name, 13, false)
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
    let mut findings = Vec::new();
    let mut nodes: BTreeMap<usize, Node> = BTreeMap::new();
    let mut toplevel_names: BTreeSet<String> = BTreeSet::new();
    let mut setup_index: Option<usize> = None;

    for (index, (cell, meta)) in notebook.cells.iter().zip(&notebook.metas).enumerate() {
        match meta.kind {
            Kind::Broken => {
                findings.push(Finding {
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
                    toplevel_names.insert(name);
                }
            }
            Kind::Setup => setup_index = Some(index),
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
                findings.push(Finding {
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
        nodes.insert(index, Node { info, label });
    }

    // Name -> defining / referring / deleting cells.
    let mut definers: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    let mut referrers: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    let mut deleters: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, node) in &nodes {
        for name in &node.info.defs {
            definers.entry(name).or_default().push(*index);
        }
        for name in &node.info.refs {
            referrers.entry(name).or_default().push(*index);
        }
        for name in &node.info.deleted_refs {
            deleters.entry(name).or_default().push(*index);
        }
    }

    // Edges (marimo's compute_edges_for_cell, Python cells only).
    let empty: Vec<usize> = Vec::new();
    let mut parents: HashMap<usize, Vec<usize>> = HashMap::new();
    for (index, node) in &nodes {
        let mut set: BTreeSet<usize> = BTreeSet::new();
        for name in &node.info.refs {
            for definer in definers.get(name.as_str()).unwrap_or(&empty) {
                if definer != index {
                    set.insert(*definer);
                }
            }
            for deleter in deleters.get(name.as_str()).unwrap_or(&empty) {
                if deleter != index {
                    set.insert(*deleter);
                }
            }
        }
        for name in &node.info.deleted_refs {
            for referrer in referrers.get(name.as_str()).unwrap_or(&empty) {
                if referrer != index {
                    set.insert(*referrer);
                }
            }
        }
        parents.insert(*index, set.into_iter().collect());
    }

    // Multiple definitions.
    for (name, cells) in &definers {
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

    // Cycles.
    for component in cycles(&parents) {
        let mut vars: BTreeSet<&str> = BTreeSet::new();
        for to in &component {
            for from in parents.get(to).into_iter().flatten() {
                if !component.contains(from) {
                    continue;
                }
                // from -> to edge: to's refs meet from's defs (or deletes).
                for name in &nodes[to].info.refs {
                    if nodes[from].info.defs.contains(name) {
                        vars.insert(name);
                    }
                }
                for name in &nodes[to].info.deleted_refs {
                    if nodes[from].info.refs.contains(name) {
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

    // The setup cell must not depend on other cells.
    if let Some(setup) = setup_index
        && nodes.contains_key(&setup)
    {
        let mut ancestors: BTreeSet<usize> = BTreeSet::new();
        let mut queue: Vec<usize> = parents.get(&setup).cloned().unwrap_or_default();
        while let Some(cell) = queue.pop() {
            if ancestors.insert(cell) {
                queue.extend(parents.get(&cell).cloned().unwrap_or_default());
            }
        }
        if !ancestors.is_empty() {
            let names: BTreeSet<&str> = ancestors
                .iter()
                .flat_map(|ancestor| nodes[ancestor].info.defs.iter())
                .filter(|name| nodes[&setup].info.refs.contains(*name))
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

    // Informational drift: stale returns and signatures that marimo's own
    // save would regenerate.
    let all_defs: BTreeSet<&str> = nodes
        .values()
        .flat_map(|node| node.info.defs.iter())
        .map(String::as_str)
        .collect();
    let used_refs: BTreeSet<&str> = nodes
        .values()
        .flat_map(|node| node.info.refs.iter())
        .map(String::as_str)
        .collect();
    let setup_defs: BTreeSet<&str> = setup_index
        .and_then(|setup| nodes.get(&setup))
        .map(|node| node.info.defs.iter().map(String::as_str).collect())
        .unwrap_or_default();

    for (index, node) in &nodes {
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
        if stored_return.iter().map(String::as_str).collect::<Vec<_>>() != expected_return {
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
        let expected_params: Vec<&str> = {
            let mut names: Vec<&str> = node
                .info
                .refs
                .iter()
                .map(String::as_str)
                .filter(|name| {
                    !node.info.defs.contains(*name)
                        && !setup_defs.contains(name)
                        && !toplevel_names.contains(*name)
                        && !(is_marimo_builtin(name) && !all_defs.contains(name))
                })
                .filter(|name| ruff_python_stdlib::identifiers::is_identifier(name))
                .collect();
            names.sort_unstable();
            names
        };
        let stored_params = parse_params(&meta.header);
        if stored_params.iter().map(String::as_str).collect::<Vec<_>>() != expected_params {
            findings.push(Finding {
                severity: Severity::Info,
                cell: Some(*index),
                code: "stale-signature".to_string(),
                message: format!(
                    "{} takes ({}) but marimo would regenerate ({}); the signature is refreshed \
                     on marimo's next save",
                    node.label,
                    stored_params.join(", "),
                    expected_params.join(", ")
                ),
            });
        }
    }

    findings
}

/// The name defined by a read-only `@app.function` / `@app.class_definition`
/// block (its names join the allowed refs, so other cells do not take them
/// as signature parameters).
fn block_definition_name(meta: &CellMeta) -> Option<String> {
    let parsed = ruff_python_parser::parse_module(&meta.block).ok()?;
    for stmt in parsed.syntax().body.iter() {
        match stmt {
            ruff_python_ast::Stmt::FunctionDef(function) => {
                return Some(function.name.to_string());
            }
            ruff_python_ast::Stmt::ClassDef(class) => return Some(class.name.to_string()),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
#[path = "check_tests.rs"]
mod tests;
