//! The AST walk: a faithful port of marimo's ScopedVisitor reduced to
//! defs/refs/deleted-refs. See the parent module for the semantics notes.

use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};
use ruff_python_ast::{
    Alias, Decorator, ExceptHandler, Expr, ExprContext, InterpolatedStringElement, Pattern, Stmt,
};

use super::{Analyzer, DefKind, Kind, is_local};

impl<'a> Visitor<'a> for Analyzer<'a> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::FunctionDef(function) => {
                let name = function.name.as_str();
                let is_method = matches!(self.current().kind, Kind::Class);
                // Decorators evaluate in the enclosing scope.
                for decorator in function.decorator_list.iter() {
                    self.visit_decorator(decorator);
                }
                // The binding is visible inside its own body (self-reference
                // of nested private functions), unless in class namespace.
                if !is_method {
                    self.current_mut().defs.insert(name.to_string());
                }
                self.push(Kind::Function);
                if let Some(type_params) = &function.type_params {
                    self.visit_type_params(type_params);
                }
                self.visit_parameters(&function.parameters);
                if let Some(returns) = &function.returns {
                    self.visit_expr(returns);
                }
                for inner in function.body.iter() {
                    self.visit_stmt(inner);
                }
                self.pop();
                self.define(
                    name,
                    if is_method {
                        DefKind::Method
                    } else {
                        DefKind::Function
                    },
                );
            }
            Stmt::ClassDef(class) => {
                let name = class.name.as_str();
                for decorator in class.decorator_list.iter() {
                    self.visit_decorator(decorator);
                }
                for base in class.bases() {
                    self.visit_expr(base);
                }
                for keyword in class.keywords() {
                    self.visit_expr(&keyword.value);
                }
                self.push(Kind::Class);
                if let Some(type_params) = &class.type_params {
                    self.visit_type_params(type_params);
                }
                for inner in class.body.iter() {
                    self.visit_stmt(inner);
                }
                self.pop();
                self.define(name, DefKind::Class);
            }
            Stmt::Assign(assign) => {
                // Value before targets, so `x = x` registers the ref first.
                self.visit_expr(&assign.value);
                for target in assign.targets.iter() {
                    self.bind_target(target);
                }
            }
            Stmt::AugAssign(aug) => {
                self.visit_expr(&aug.value);
                self.bind_target(&aug.target);
            }
            Stmt::AnnAssign(ann) => {
                if let Some(value) = &ann.value {
                    self.visit_expr(value);
                }
                self.visit_expr(&ann.annotation);
                self.bind_target(&ann.target);
            }
            Stmt::Global(global) => {
                for name in global.names.iter() {
                    let name = name.as_str();
                    self.current_mut().global_names.insert(name.to_string());
                    if !self.module.defs.contains(name) {
                        self.add_ref(name, false);
                    }
                }
            }
            Stmt::Import(import) => {
                for alias in import.names.iter() {
                    let name = import_binding(alias);
                    if let Some(name) = name {
                        self.define(&name, DefKind::Import);
                    }
                }
            }
            Stmt::ImportFrom(import) => {
                for alias in import.names.iter() {
                    // `from x import *` is rejected by marimo at compile time;
                    // we simply bind nothing.
                    let name = import_binding(alias);
                    if let Some(name) = name {
                        self.define(&name, DefKind::Import);
                    }
                }
            }
            Stmt::Try(try_stmt) => {
                for inner in try_stmt.body.iter() {
                    self.visit_stmt(inner);
                }
                for handler in try_stmt.handlers.iter() {
                    let ExceptHandler::ExceptHandler(handler) = handler;
                    let name = handler.name.as_ref().map(|name| name.as_str());
                    if let Some(name) = name {
                        self.obscured.push(name);
                    }
                    if let Some(exception_type) = &handler.type_ {
                        self.visit_expr(exception_type);
                    }
                    for inner in handler.body.iter() {
                        self.visit_stmt(inner);
                    }
                    if name.is_some() {
                        self.obscured.pop();
                    }
                }
                for inner in try_stmt.orelse.iter() {
                    self.visit_stmt(inner);
                }
                for inner in try_stmt.finalbody.iter() {
                    self.visit_stmt(inner);
                }
            }
            Stmt::TypeAlias(alias) => {
                self.bind_target(&alias.name);
                self.push(Kind::Annotation);
                if let Some(type_params) = &alias.type_params {
                    self.visit_type_params(type_params);
                }
                self.visit_expr(&alias.value);
                self.pop();
            }
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Name(name) => {
                let id = name.id.as_str();
                if self.obscures(id) {
                    return;
                }
                match name.ctx {
                    ExprContext::Store => self.visit_name_target(id),
                    ExprContext::Load if !self.is_defined(id) && !is_local(id) => {
                        self.add_ref(id, false);
                    }
                    ExprContext::Del if !self.is_defined(id) && !is_local(id) => {
                        self.add_ref(id, true);
                    }
                    _ => {}
                }
            }
            Expr::Lambda(lambda) => {
                self.push(Kind::Function);
                if let Some(parameters) = &lambda.parameters {
                    self.visit_parameters(parameters);
                }
                self.visit_expr(&lambda.body);
                self.pop();
            }
            Expr::ListComp(comprehension) => {
                self.push(Kind::Comprehension);
                for generator in comprehension.generators.iter() {
                    self.visit_comprehension(generator);
                }
                self.visit_expr(&comprehension.elt);
                self.pop();
            }
            Expr::SetComp(comprehension) => {
                self.push(Kind::Comprehension);
                for generator in comprehension.generators.iter() {
                    self.visit_comprehension(generator);
                }
                self.visit_expr(&comprehension.elt);
                self.pop();
            }
            Expr::Generator(generator) => {
                self.push(Kind::Comprehension);
                for generator_comprehension in generator.generators.iter() {
                    self.visit_comprehension(generator_comprehension);
                }
                self.visit_expr(&generator.elt);
                self.pop();
            }
            Expr::DictComp(comprehension) => {
                self.push(Kind::Comprehension);
                for generator in comprehension.generators.iter() {
                    self.visit_comprehension(generator);
                }
                self.visit_expr(&comprehension.value);
                if let Some(key) = &comprehension.key {
                    self.visit_expr(key);
                }
                self.pop();
            }
            Expr::Named(named) => {
                self.visit_expr(&named.value);
                let in_comprehension = matches!(self.current().kind, Kind::Comprehension);
                if in_comprehension && let Expr::Name(name) = &*named.target {
                    // A walrus target inside a comprehension binds in the
                    // first enclosing non-comprehension block; the module
                    // block (index 0) when every open scope is one.
                    let index = self
                        .scopes
                        .iter()
                        .rposition(|block| block.kind != Kind::Comprehension)
                        .map_or(0, |position| position + 1);
                    self.define_in(name.id.as_str(), index, DefKind::Variable);
                    return;
                }
                self.visit_expr(&named.target);
            }
            Expr::Call(call) => {
                // marimo's brittle SQL-cell detection: a single-argument call
                // to mo.sql / marimo.sql / duckdb.execute / duckdb.sql.
                if call.arguments.args.len() == 1
                    && let Expr::Attribute(attribute) = &*call.func
                    && let Expr::Name(owner) = &*attribute.value
                {
                    let qualified = format!("{}.{}", owner.id.as_str(), attribute.attr.as_str());
                    if matches!(
                        qualified.as_str(),
                        "marimo.sql" | "mo.sql" | "duckdb.execute" | "duckdb.sql"
                    ) {
                        self.is_sql = true;
                    }
                }
                walk_expr(self, expr);
            }
            Expr::FString(fstring) => {
                // Visit interpolations only; literals hold no names.
                for part in fstring.value.iter() {
                    if let ruff_python_ast::FStringPartRef::FString(part) = part {
                        for element in part.elements.iter() {
                            if let InterpolatedStringElement::Interpolation(interpolation) = element
                            {
                                self.visit_expr(&interpolation.expression);
                            }
                        }
                    }
                }
            }
            _ => walk_expr(self, expr),
        }
    }

    fn visit_pattern(&mut self, pattern: &'a Pattern) {
        // Match patterns bind names; marimo defines them at parse time since
        // the bound subset is unknown statically.
        self.bind_pattern(pattern);
    }

    fn visit_decorator(&mut self, decorator: &'a Decorator) {
        self.visit_expr(&decorator.expression);
    }
}

/// The module-level name an import statement binds (mirroring
/// `_get_alias_name`): `import a.b` binds `a`, `import a as x` binds `x`,
/// `from m import a` binds `a`, `from m import a as x` binds `x`, and
/// `from m import *` binds nothing.
fn import_binding(alias: &Alias) -> Option<String> {
    if let Some(asname) = &alias.asname {
        return Some(asname.as_str().to_string());
    }
    let root = alias.name.as_str().split('.').next().unwrap_or("");
    if root == "*" {
        None
    } else {
        Some(root.to_string())
    }
}
