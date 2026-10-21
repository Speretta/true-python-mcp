//! The `py_docs` symbol resolver (Phase 0 spike): resolve a dotted Python
//! symbol — `json.dumps`, `typing.Any`, `print` — to its kind, rendered
//! signature and docstring, fully local, using ty's IDE support on top of the
//! hand-assembled [`TyDb`]. This is the resolution half of the eventual
//! `py_docs` tool; it exposes one function and is exercised by `tests/docs.rs`.
//!
//! Resolution is *anchor = environment*: `src_root` selects the search paths
//! (reusing `TyDb`'s discovery, so a first-party module shadows the installed
//! package exactly as `sys.path` would). The first segment is an import — or a
//! builtin, resolved through the same fallback — and the remaining segments are
//! walked as attributes on the inferred receiver type. ty never returns a
//! single guess, so a symbol with several definitions is ranked and the first
//! is reported here; ambiguity handling proper is Phase 1.

use std::path::Path;

use ruff_db::parsed::parsed_module;
use ruff_db::source::source_text;
use ruff_python_ast as ast;
use ruff_source_file::LineIndex;
use ruff_text_size::Ranged;
use ty_python_core::definition::{Definition, DefinitionKind};
use ty_python_semantic::{
    Db as SemanticDb, HasType, ImportAliasResolution, ResolvedDefinition, SemanticModel,
    definitions_for_attribute, definitions_for_name,
};

use crate::error::Error;
use crate::ty::TyDb;

/// One resolved library symbol: what it is, how it is called, and its docs.
#[derive(Debug, Clone)]
pub struct DocSymbol {
    /// The terminal name of the resolved definition (e.g. `dumps`).
    pub resolved_name: String,
    /// `module`, `class`, `function`, `attribute`, `type_alias`, or `other`.
    pub kind: String,
    /// Rendered signatures (empty when the symbol is not a function).
    pub signatures: Vec<String>,
    /// The symbol's docstring, if any.
    pub docstring: Option<String>,
    /// Where the docstring came from: `source`, `stub`, or `none`.
    pub docstring_source: &'static str,
    /// Path of the defining file (system, vendored, or virtual), if known.
    pub source_path: Option<String>,
    /// 1-based line of the definition, if known.
    pub source_line: Option<usize>,
    /// Whether the definition lives in a `.pyi` stub.
    pub is_stub: bool,
}

/// Resolves `symbol` to its definition, signature and docstring, anchored at
/// `src_root` (the search-root directory; `None` defaults to the current
/// directory). No interpreter is required: the standard library and its
/// docstrings come from the vendored typeshed.
pub fn resolve_symbol(symbol: &str, src_root: Option<&Path>) -> Result<DocSymbol, Error> {
    let segments = split_segments(symbol)?;
    let src_root = src_root.unwrap_or_else(|| Path::new("."));

    // The first segment names a module to import, or a builtin to reference
    // bare. Probing needs only the search paths (settings), so the throwaway
    // database is cheap and shares the process-wide discovery cache.
    let is_module = TyDb::for_virtual_source(src_root, "")?.resolves_module(&segments[0]);

    let source = build_source(&segments, is_module);
    let db = TyDb::for_virtual_source(src_root, &source)?;
    let file = db.docs_file();
    let program_file = SemanticDb::program_file(&db, file);
    let model = SemanticModel::new(&db, program_file);
    let parsed = parsed_module(&db, model.python_file()).load(&db);

    // The synthetic module's last statement is the target expression, so the
    // walk reuses ty's own resolution instead of hand-rolling a scope lookup.
    let expr = parsed
        .suite()
        .last()
        .and_then(ast::Stmt::as_expr_stmt)
        .map(|stmt| &stmt.value)
        .ok_or_else(|| Error::analysis("synthetic symbol module has no expression"))?;

    let resolved = match &**expr {
        ast::Expr::Attribute(attribute) => definitions_for_attribute(&model, attribute),
        ast::Expr::Name(name) => definitions_for_name(
            &model,
            name.id.as_str(),
            ast::AnyNodeRef::from(expr.as_ref()),
            ImportAliasResolution::ResolveAliases,
        ),
        _ => {
            return Err(Error::analysis(
                "symbol does not resolve to a name or attribute",
            ));
        }
    };

    let first = resolved.first().ok_or_else(|| {
        Error::analysis(format!(
            "symbol `{symbol}` did not resolve to any definition"
        ))
    })?;

    let terminal = segments.last().map(String::as_str).unwrap_or(symbol);
    let kind: &'static str = match first {
        ResolvedDefinition::Definition(definition) => kind_of_definition(&db, *definition),
        ResolvedDefinition::Module(_) => "module",
        ResolvedDefinition::FileWithRange(_) => "import",
    };
    let resolved_name = match first {
        ResolvedDefinition::Definition(definition) => {
            definition.name(&db).unwrap_or_else(|| terminal.to_string())
        }
        ResolvedDefinition::Module(_) | ResolvedDefinition::FileWithRange(_) => {
            terminal.to_string()
        }
    };
    let is_stub = match first {
        ResolvedDefinition::Definition(definition) => definition.file(&db).is_stub(&db),
        ResolvedDefinition::Module(module) => module.file(&db).is_stub(&db),
        ResolvedDefinition::FileWithRange(range) => range.file().is_stub(&db),
    };

    // A function's rendered type carries the signature (`def dumps(obj: Any,
    // *, ...) -> str`); non-callables have no signature to show.
    let signatures = if kind == "function" {
        expr.inferred_type(&model)
            .map(|ty| {
                let env = model.program_environment();
                vec![ty.display(&db, &env).to_string()]
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    // The overload's own docstring, falling back to the runtime implementation
    // (overload declarations often omit theirs).
    let docstring = first
        .docstring(&db)
        .or_else(|| first.implementation_docstring(&db));
    let docstring_source = match (&docstring, is_stub) {
        (Some(_), true) => "stub",
        (Some(_), false) => "source",
        (None, _) => "none",
    };

    let focus = first.focus_range(&db);
    let source_file = focus.file();
    let source_text = source_text(&db, source_file);
    let line_index = LineIndex::from_source_text(&source_text);
    let source_line = Some(line_index.line_index(focus.range().start()).get());

    Ok(DocSymbol {
        resolved_name,
        kind: kind.to_string(),
        signatures,
        docstring,
        docstring_source,
        source_path: Some(source_file.path(&db).as_str().to_string()),
        source_line,
        is_stub,
    })
}

// ---- helpers ----

/// Splits a dotted symbol into its segments, rejecting empty components.
fn split_segments(symbol: &str) -> Result<Vec<String>, Error> {
    let segments: Vec<String> = symbol.split('.').map(str::to_string).collect();
    if segments.iter().any(String::is_empty) {
        return Err(Error::invalid_input(format!(
            "invalid dotted symbol: {symbol:?}"
        )));
    }
    Ok(segments)
}

/// Builds the synthetic module: an import of the first segment (when it names
/// a module) followed by the full dotted path as an expression statement.
fn build_source(segments: &[String], is_module: bool) -> String {
    let dotted = segments.join(".");
    if is_module {
        format!("import {}\n{dotted}\n", segments[0])
    } else {
        format!("{dotted}\n")
    }
}

/// Maps a definition's kind to the coarse `py_docs` kind string.
fn kind_of_definition(db: &TyDb, definition: Definition<'_>) -> &'static str {
    match definition.kind(db) {
        DefinitionKind::Function(_) => "function",
        DefinitionKind::Class(_) => "class",
        DefinitionKind::TypeAlias(_) => "type_alias",
        DefinitionKind::Assignment(_) | DefinitionKind::AnnotatedAssignment(_) => "attribute",
        DefinitionKind::Import(_)
        | DefinitionKind::ImportFrom(_)
        | DefinitionKind::ImportFromSubmodule(_)
        | DefinitionKind::StarImport(_) => "import",
        _ => "other",
    }
}
