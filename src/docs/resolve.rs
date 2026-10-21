//! The synthetic-module build and the hop-by-hop resolution walk.
//!
//! The resolver turns a dotted symbol into a small Python module whose last
//! statement is a call on the target; this module walks the intermediate
//! attribute/name hops and records one trace entry per segment, failing at the
//! first dead-end.

use ruff_python_ast as ast;

use ty_python_semantic::types::ProgramEnvironment;
use ty_python_semantic::{
    HasType, ImportAliasResolution, ResolvedDefinition, SemanticModel, definitions_for_attribute,
    definitions_for_name,
};

use crate::error::Error;
use crate::ty::TyDb;

use super::rank::kind_of_resolved;

/// Splits a dotted symbol into its segments, rejecting empty components.
pub(super) fn split_segments(symbol: &str) -> Result<Vec<String>, Error> {
    let segments: Vec<String> = symbol.split('.').map(str::to_string).collect();
    if segments.iter().any(String::is_empty) {
        return Err(Error::invalid_input(format!(
            "invalid dotted symbol: {symbol:?}"
        )));
    }
    Ok(segments)
}

/// Builds the synthetic module: an import of the first segment (when it names
/// a module), one bare expression per non-terminal hop (so the trace can
/// report each hop's kind), and the terminal as a call.
pub(super) fn build_source(segments: &[String], is_module: bool) -> String {
    let mut source = String::new();
    if is_module {
        source.push_str(&format!("import {}\n", segments[0]));
    }
    for end in 1..segments.len() {
        source.push_str(&segments[..end].join("."));
        source.push('\n');
    }
    source.push_str(&segments.join("."));
    source.push_str("()\n");
    source
}

/// Resolves one expression (a name or an attribute) through ty's own IDE
/// resolution, reusing the builtins fallback for bare names.
pub(super) fn resolve_expr<'db>(
    model: &SemanticModel<'db>,
    expr: &ast::Expr,
) -> Result<Vec<ResolvedDefinition<'db>>, Error> {
    match expr {
        ast::Expr::Attribute(attribute) => Ok(definitions_for_attribute(model, attribute)),
        ast::Expr::Name(name) => Ok(definitions_for_name(
            model,
            name.id.as_str(),
            ast::AnyNodeRef::from(expr),
            ImportAliasResolution::ResolveAliases,
        )),
        _ => Err(Error::analysis(
            "symbol does not resolve to a name or attribute",
        )),
    }
}

/// One honesty entry per intermediate hop: resolve each prefix in order and
/// fail at the first dead-end, naming the failing segment and its receiver
/// type rather than silently reporting the terminal as unresolved.
pub(super) fn build_resolution<'db>(
    db: &'db TyDb,
    model: &SemanticModel<'db>,
    env: &ProgramEnvironment<'db>,
    suite: &[ast::Stmt],
    segments: &[String],
    is_module: bool,
    symbol: &str,
) -> Result<Vec<String>, Error> {
    let mut resolution = Vec::new();
    // The suite is `[import?] [prefix_1 .. prefix_{n-1}] [call]`; skip the
    // import and the call, and resolve the intermediate prefixes in order.
    let start = usize::from(is_module);
    let prefix_count = segments.len().saturating_sub(1);
    for (index, stmt) in suite.iter().enumerate().skip(start).take(prefix_count) {
        let hop = index - start;
        let Some(expr) = stmt.as_expr_stmt().map(|stmt| &stmt.value) else {
            continue;
        };
        let prefix = segments[..=hop].join(".");
        let resolved = resolve_expr(model, expr)?;
        let Some(first) = resolved.first() else {
            let receiver = receiver_type_of(db, env, model, expr);
            return Err(Error::analysis(format!(
                "symbol `{symbol}` did not resolve: segment `{}` of `{prefix}` has no definition on receiver `{receiver}`",
                segments[hop]
            )));
        };
        resolution.push(format!(
            "{} -> {} {prefix}",
            segments[hop],
            kind_of_resolved(db, first)
        ));
    }
    Ok(resolution)
}

/// Renders the receiver type of an unresolved attribute/name expression, used
/// to name the failing segment of a dead-ended dotted path.
pub(super) fn receiver_type_of<'db>(
    db: &'db TyDb,
    env: &ProgramEnvironment<'db>,
    model: &SemanticModel<'db>,
    expr: &ast::Expr,
) -> String {
    match expr {
        ast::Expr::Attribute(attribute) => attribute
            .value
            .inferred_type(model)
            .map(|ty| ty.display(db, env).to_string())
            .unwrap_or_else(|| "<unresolved>".to_string()),
        ast::Expr::Name(_) => "builtins".to_string(),
        _ => "<expression>".to_string(),
    }
}
