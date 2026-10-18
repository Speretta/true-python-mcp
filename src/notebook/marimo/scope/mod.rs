//! Scope-aware name analysis of a marimo cell body.
//!
//! This is a faithful port of marimo's `ScopedVisitor` (marimo/_ast/visitor.py)
//! reduced to what graph analysis needs: which names a cell defines at its top
//! level (`defs`), which names it references but does not define (`refs`), and
//! which referenced names it deletes (`deleted_refs`). Semantics mirrored:
//!
//! - function/class/lambda/comprehension bodies open a new block; names
//!   defined inside never leak to the cell level,
//! - decorators, default values and annotations are evaluated in the
//!   enclosing scope,
//! - `x = x` registers the ref before the def (value visited first),
//! - a name referenced before it is defined at the top level is a closure
//!   capture and is evicted from refs when the def arrives,
//! - `global x` binds at the top level,
//! - `except ... as e:` obscures `e` inside the handler only,
//! - names starting with `_` are cell-local: they are never refs and never
//!   exported defs (mirroring `is_local` and the nonlocals filter),
//! - `x += 1` defines `x` (Store ctx), like marimo.

mod visitor;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use std::collections::{HashMap, HashSet};

use ruff_python_ast::visitor::Visitor;
use ruff_python_ast::{Comprehension, Expr, Parameters, Pattern, TypeParam, TypeParams};
use ruff_python_parser::parse_module;

/// Stack-growth parameters for the AST walk, mirroring ruff's parser (which
/// uses the same crate): shallow nesting runs unchecked, deeper recursion
/// probes the remaining stack and grows into 1 MiB heap segments when it runs
/// low. Without the guard, deeply nested Python — which ruff's own guarded
/// parser happily accepts — overflows the 2 MiB tokio worker stacks and
/// SIGSEGVs the whole server during notebook_check.
const STACK_RED_ZONE: usize = 100 * 1024;
const STACK_SIZE: usize = 1024 * 1024;
const MAX_UNCHECKED_RECURSION_DEPTH: usize = 20;

/// A name is cell-local when it is `__` or starts with a single underscore
/// (mirroring marimo's `is_local`): never a ref, never an exported def.
fn is_local(name: &str) -> bool {
    name == "__" || (name.starts_with('_') && !name.starts_with("__"))
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Kind {
    #[default]
    Module,
    Function,
    Class,
    Comprehension,
    Annotation,
}

#[derive(Default)]
struct Block {
    id: usize,
    defs: HashSet<String>,
    global_names: HashSet<String>,
    kind: Kind,
}

struct RefData {
    deleted: bool,
    /// Ids of the blocks above the referencing block.
    parents: Vec<usize>,
}

/// Whether the referenced name was only captured in method position (mirrors
/// marimo's VariableData kind check that skips method captures).
enum DefKind {
    Variable,
    Function,
    Method,
    Class,
    Import,
}

/// Name analysis result for one cell body.
pub(super) struct CellInfo {
    /// Top-level definitions, locals excluded (marimo's `nonlocals`).
    pub defs: HashSet<String>,
    /// Referenced names never defined in this cell.
    pub refs: HashSet<String>,
    /// Referenced names that the cell `del`s.
    pub deleted_refs: HashSet<String>,
    /// The body is a SQL cell (`mo.sql(...)` / duckdb call), per marimo's
    /// brittle attribute check. SQL cells are excluded from graph analysis.
    pub is_sql: bool,
}

/// Analyze a cell body (dedented, return-stripped source). Returns `None`
/// when the body does not parse.
pub(super) fn analyze(code: &str) -> Option<CellInfo> {
    let parsed = parse_module(code).ok()?;
    let mut visitor = Analyzer {
        module: Block {
            id: 0,
            kind: Kind::Module,
            ..Block::default()
        },
        scopes: Vec::new(),
        next_id: 1,
        refs: HashMap::new(),
        obscured: Vec::new(),
        is_sql: false,
        recursion_depth: 0,
    };
    for stmt in parsed.syntax().body.iter() {
        visitor.visit_stmt(stmt);
    }
    let result = Some(CellInfo {
        defs: visitor
            .module
            .defs
            .iter()
            .filter(|name| !is_local(name))
            .cloned()
            .collect(),
        refs: visitor.refs.keys().cloned().collect(),
        deleted_refs: visitor
            .refs
            .iter()
            .filter(|(_, data)| data.iter().any(|entry| entry.deleted))
            .map(|(name, _)| name.clone())
            .collect(),
        is_sql: visitor.is_sql,
    });
    super::parse::guarded_drop(parsed, code.len());
    result
}

struct Analyzer<'a> {
    /// The cell's top-level block. Always present, so block access never
    /// needs a panic path.
    module: Block,
    /// Open nested scopes (function/class/comprehension/annotation),
    /// innermost last.
    scopes: Vec<Block>,
    next_id: usize,
    refs: HashMap<String, Vec<RefData>>,
    /// Exception handler names currently in scope (`except ... as name:`).
    obscured: Vec<&'a str>,
    is_sql: bool,
    /// Nesting depth of the AST walk, for the stack guard ([`grow_stack`]).
    recursion_depth: usize,
}

impl<'a> Analyzer<'a> {
    /// Run one recursive step of the AST walk, growing the stack once
    /// nesting gets deep (ruff's parser recursion-guard pattern; see the
    /// constants above).
    fn grow_stack<T>(&mut self, step: impl FnOnce(&mut Self) -> T) -> T {
        self.recursion_depth += 1;
        let result = if self.recursion_depth > MAX_UNCHECKED_RECURSION_DEPTH {
            stacker::maybe_grow(STACK_RED_ZONE, STACK_SIZE, || step(self))
        } else {
            step(self)
        };
        self.recursion_depth -= 1;
        result
    }

    /// The innermost open block; the module block when no nested scope is
    /// open. Structurally total: the module block cannot be popped.
    fn current(&self) -> &Block {
        self.scopes.last().unwrap_or(&self.module)
    }

    fn current_mut(&mut self) -> &mut Block {
        self.scopes.last_mut().unwrap_or(&mut self.module)
    }

    fn is_defined(&self, name: &str) -> bool {
        self.module.defs.contains(name) || self.scopes.iter().any(|block| block.defs.contains(name))
    }

    fn add_ref(&mut self, name: &str, deleted: bool) {
        // Ancestors of the current block: empty at module level, otherwise
        // the module plus all but the innermost scope.
        let parents: Vec<usize> = match self.scopes.split_last() {
            None => Vec::new(),
            Some((_, ancestors)) => std::iter::once(self.module.id)
                .chain(ancestors.iter().map(|block| block.id))
                .collect(),
        };
        let entries = self.refs.entry(name.to_string()).or_default();
        if let Some(entry) = entries.iter_mut().find(|entry| entry.parents == parents) {
            entry.deleted |= deleted;
        } else {
            entries.push(RefData { deleted, parents });
        }
    }

    /// Define `name` in block `block_index` (0 = module, i > 0 = scopes[i-1]);
    /// a define evicts earlier closure captures of the same name whose
    /// ancestor chain includes the defining block.
    fn define_in(&mut self, name: &str, block_index: usize, kind: DefKind) {
        let block_id = match block_index {
            0 => {
                self.module.defs.insert(name.to_string());
                self.module.id
            }
            index => {
                // Callers only pass live block indexes (define/walrus).
                let Some(block) = self.scopes.get_mut(index - 1) else {
                    return;
                };
                block.defs.insert(name.to_string());
                block.id
            }
        };
        if !matches!(kind, DefKind::Method)
            && let Some(entries) = self.refs.get_mut(name)
        {
            entries.retain(|entry| !entry.parents.contains(&block_id));
            if entries.is_empty() {
                self.refs.remove(name);
            }
        }
    }

    /// Define `name` in the current block (or the module block when it was
    /// declared `global` there).
    fn define(&mut self, name: &str, kind: DefKind) {
        let index = if self.current().global_names.contains(name) {
            0
        } else {
            self.scopes.len()
        };
        self.define_in(name, index, kind);
    }

    fn push(&mut self, kind: Kind) {
        let id = self.next_id;
        self.next_id += 1;
        self.scopes.push(Block {
            id,
            kind,
            ..Block::default()
        });
    }

    fn pop(&mut self) {
        self.scopes.pop();
    }

    /// `except ... as name:` hides the name inside the handler.
    fn obscures(&self, name: &str) -> bool {
        self.obscured.contains(&name)
    }

    fn visit_name_target(&mut self, name: &str) {
        self.define(name, DefKind::Variable);
    }

    /// Define every name bound by an assignment target (Name / destructuring).
    fn bind_target(&mut self, expr: &'a Expr) {
        self.grow_stack(|analyzer| analyzer.bind_target_inner(expr))
    }

    fn bind_target_inner(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Name(name) => self.visit_name_target(name.id.as_str()),
            Expr::Tuple(tuple) => tuple
                .elts
                .iter()
                .for_each(|element| self.bind_target(element)),
            Expr::List(list) => list
                .elts
                .iter()
                .for_each(|element| self.bind_target(element)),
            Expr::Starred(starred) => self.bind_target(&starred.value),
            // Subscript/Attribute targets are loads of their value, not binds.
            other => self.visit_expr(other),
        }
    }

    fn visit_parameters(&mut self, parameters: &'a Parameters) {
        // Defaults are evaluated in the enclosing scope, before the bindings.
        for argument in parameters.kwonlyargs.iter() {
            if let Some(default) = &argument.default {
                self.visit_expr(default);
            }
        }
        for argument in parameters.posonlyargs.iter().chain(parameters.args.iter()) {
            if let Some(default) = &argument.default {
                self.visit_expr(default);
            }
        }
        for argument in parameters
            .posonlyargs
            .iter()
            .chain(parameters.args.iter())
            .chain(parameters.kwonlyargs.iter())
        {
            self.define(argument.parameter.name.as_str(), DefKind::Variable);
            if let Some(annotation) = &argument.parameter.annotation {
                self.visit_expr(annotation);
            }
        }
        if let Some(vararg) = &parameters.vararg {
            self.define(vararg.name.as_str(), DefKind::Variable);
            if let Some(annotation) = &vararg.annotation {
                self.visit_expr(annotation);
            }
        }
        if let Some(kwarg) = &parameters.kwarg {
            self.define(kwarg.name.as_str(), DefKind::Variable);
            if let Some(annotation) = &kwarg.annotation {
                self.visit_expr(annotation);
            }
        }
    }

    fn visit_type_params(&mut self, type_params: &'a TypeParams) {
        for parameter in type_params.type_params.iter() {
            match parameter {
                TypeParam::TypeVar(type_var) => {
                    self.define(type_var.name.as_str(), DefKind::Variable);
                    if let Some(bound) = &type_var.bound {
                        self.visit_expr(bound);
                    }
                }
                TypeParam::ParamSpec(param_spec) => {
                    self.define(param_spec.name.as_str(), DefKind::Variable);
                }
                TypeParam::TypeVarTuple(type_var_tuple) => {
                    self.define(type_var_tuple.name.as_str(), DefKind::Variable);
                }
            }
        }
    }

    fn visit_comprehension(&mut self, comprehension: &'a Comprehension) {
        // Python visits iter first (outer scope refs), then the target, then
        // the ifs; marimo mirrors that inside the comprehension block.
        self.visit_expr(&comprehension.iter);
        self.bind_target(&comprehension.target);
        for condition in comprehension.ifs.iter() {
            self.visit_expr(condition);
        }
    }

    fn bind_pattern(&mut self, pattern: &'a Pattern) {
        self.grow_stack(|analyzer| analyzer.bind_pattern_inner(pattern))
    }

    fn bind_pattern_inner(&mut self, pattern: &'a Pattern) {
        match pattern {
            Pattern::MatchAs(pattern) => {
                if let Some(name) = &pattern.name {
                    self.define(name.as_str(), DefKind::Variable);
                }
                if let Some(nested) = &pattern.pattern {
                    self.bind_pattern(nested);
                }
            }
            Pattern::MatchMapping(pattern) => {
                if let Some(rest) = &pattern.rest {
                    self.define(rest.as_str(), DefKind::Variable);
                }
                for key in pattern.keys.iter() {
                    self.visit_expr(key);
                }
                for nested in pattern.patterns.iter() {
                    self.bind_pattern(nested);
                }
            }
            Pattern::MatchStar(pattern) => {
                if let Some(name) = &pattern.name {
                    self.define(name.as_str(), DefKind::Variable);
                }
            }
            Pattern::MatchClass(pattern) => {
                self.visit_expr(&pattern.cls);
                for argument in pattern.arguments.patterns.iter() {
                    self.bind_pattern(argument);
                }
                for keyword in pattern.arguments.keywords.iter() {
                    self.bind_pattern(&keyword.pattern);
                }
            }
            Pattern::MatchOr(pattern) => {
                for alternative in pattern.patterns.iter() {
                    self.bind_pattern(alternative);
                }
            }
            Pattern::MatchSequence(pattern) => {
                for nested in pattern.patterns.iter() {
                    self.bind_pattern(nested);
                }
            }
            Pattern::MatchValue(pattern) => {
                self.visit_expr(&pattern.value);
            }
            Pattern::MatchSingleton(_) => {}
        }
    }
}
