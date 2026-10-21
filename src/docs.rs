//! The `py_docs` symbol resolver: resolve a dotted Python symbol —
//! `json.dumps`, `typing.Any`, `print` — to its kind, rendered signature,
//! structured parameters, return type and docstring, fully local, using ty's
//! IDE support on top of the hand-assembled [`TyDb`].
//!
//! Resolution is *anchor = environment*: `src_root` selects the search paths
//! (reusing `TyDb`'s discovery, so a first-party module shadows the installed
//! package exactly as `sys.path` would). The first segment is an import — or a
//! builtin, resolved through the same fallback — and the remaining segments are
//! walked as attributes on the inferred receiver type. The synthetic module
//! ends in a call (`json.dumps()`) so [`call_signature_details`] can split the
//! signature into structured parameters; a bare reference would expose only the
//! whole signature as one rendered string.
//!
//! ty never returns a single guess, so a symbol with several definitions is
//! ranked and the first is reported; the `disambiguation` marker records
//! whether there were several definitions (`multiple`), several overloads of
//! one definition (`overloads`), or exactly one (`unique`). Full ranking is a
//! later phase.

use std::path::Path;

use ruff_db::parsed::parsed_module;
use ruff_db::source::source_text;
use ruff_python_ast as ast;
use ruff_source_file::LineIndex;
use ruff_text_size::Ranged;
use ty_python_core::definition::{Definition, DefinitionKind};
use ty_python_semantic::types::ProgramEnvironment;
use ty_python_semantic::types::ide_support::{
    CallSignatureDetails, CallSignatureParameter, call_signature_details,
};
use ty_python_semantic::{
    Db as SemanticDb, ImportAliasResolution, ResolvedDefinition, SemanticModel,
    definitions_for_attribute, definitions_for_name,
};

use crate::error::Error;
use crate::ty::TyDb;

/// One parameter of a resolved callable, split from the rendered signature.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DocParam {
    /// Parameter name without the `*`/`**` prefix (the `kind` carries that).
    pub name: String,
    /// Rendered annotation (e.g. `int | str | None`).
    #[serde(rename = "type")]
    pub type_: String,
    /// `positional_only`, `positional_or_keyword`, `variadic`, `keyword_only`,
    /// or `keyword_variadic`.
    pub kind: &'static str,
    /// Rendered default value, when the signature declares one.
    pub default: Option<String>,
}

/// One resolved library symbol: what it is, how it is called, and its docs.
#[derive(Debug, Clone)]
pub struct DocSymbol {
    /// The terminal name of the resolved definition (e.g. `dumps`).
    pub resolved_name: String,
    /// `module`, `class`, `function`, `attribute`, `type_alias`, or `other`.
    pub kind: String,
    /// Rendered signatures (empty when the symbol is not callable).
    pub signatures: Vec<String>,
    /// Rendered return type, when the terminal is callable.
    pub return_type: Option<String>,
    /// Structured parameters, when the terminal is callable.
    pub parameters: Vec<DocParam>,
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
    /// `unique`, `overloads`, or `multiple` (see module docs).
    pub disambiguation: &'static str,
    /// One entry per dotted segment: `<segment> -> <kind> <prefix>`.
    pub resolution: Vec<String>,
}

/// Resolves `symbol` to its definition, signature and docstring, anchored at
/// `src_root` (the search-root directory; `None` defaults to the current
/// directory). No interpreter is required: the standard library and its
/// docstrings come from the vendored typeshed.
pub fn resolve_symbol(symbol: &str, src_root: Option<&Path>) -> Result<DocSymbol, Error> {
    resolve_symbol_with_python(symbol, src_root, None)
}

/// [`resolve_symbol`], pinning the environment discovery to `python` (an
/// interpreter or venv root), so third-party symbols resolve against the
/// caller's environment exactly as `ty_check`'s `python` param does.
pub fn resolve_symbol_with_python(
    symbol: &str,
    src_root: Option<&Path>,
    python: Option<&Path>,
) -> Result<DocSymbol, Error> {
    let segments = split_segments(symbol)?;
    let src_root = src_root.unwrap_or_else(|| Path::new("."));

    // The first segment names a module to import, or a builtin to reference
    // bare. Probing needs only the search paths (settings), so the throwaway
    // database is cheap and shares the process-wide discovery cache.
    let is_module =
        TyDb::for_virtual_source_with_python(src_root, "", python)?.resolves_module(&segments[0]);

    let source = build_source(&segments, is_module);
    let db = TyDb::for_virtual_source_with_python(src_root, &source, python)?;
    let file = db.docs_file();
    let program_file = SemanticDb::program_file(&db, file);
    let model = SemanticModel::new(&db, program_file);
    let parsed = parsed_module(&db, model.python_file()).load(&db);

    // The synthetic module's last statement is a call whose `func` is the
    // target symbol; the call form lets `call_signature_details` split the
    // signature into structured parameters.
    let call = parsed
        .suite()
        .last()
        .and_then(ast::Stmt::as_expr_stmt)
        .map(|stmt| &stmt.value)
        .and_then(|value| value.as_call_expr())
        .ok_or_else(|| Error::analysis("synthetic symbol module has no call expression"))?;

    let terminal = resolve_expr(&model, &call.func)?;
    let first = terminal.first().ok_or_else(|| {
        Error::analysis(format!(
            "symbol `{symbol}` did not resolve to any definition"
        ))
    })?;

    let terminal_segment = segments.last().map(String::as_str).unwrap_or(symbol);
    let kind = kind_of_resolved(&db, first);
    let resolved_name = resolved_name_of(&db, first, terminal_segment);
    let is_stub = is_stub_of(&db, first);

    let resolution = build_resolution(
        &db,
        &model,
        parsed.suite(),
        &segments,
        is_module,
        kind,
        symbol,
    );

    // Structured signature, only when the terminal is callable.
    let details = call_signature_details(&model, call);
    let env = model.program_environment();
    let (signatures, return_type, parameters) = match details.first() {
        Some(detail) => {
            let signature = format!("def {resolved_name}{}", detail.label);
            let return_type = return_type_of(&detail.label);
            let parameters = doc_params(&db, &env, detail);
            (vec![signature], return_type, parameters)
        }
        None => (Vec::new(), None, Vec::new()),
    };
    let disambiguation = disambiguation_of(&terminal, details.len());

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
        return_type,
        parameters,
        docstring,
        docstring_source,
        source_path: Some(source_file.path(&db).as_str().to_string()),
        source_line,
        is_stub,
        disambiguation,
        resolution,
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
/// a module), one bare expression per non-terminal hop (so the trace can
/// report each hop's kind), and the terminal as a call.
fn build_source(segments: &[String], is_module: bool) -> String {
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
fn resolve_expr<'db>(
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

/// Maps a resolved definition to the coarse `py_docs` kind string.
fn kind_of_resolved(db: &TyDb, resolved: &ResolvedDefinition<'_>) -> &'static str {
    match resolved {
        ResolvedDefinition::Definition(definition) => kind_of_definition(db, *definition),
        ResolvedDefinition::Module(_) => "module",
        ResolvedDefinition::FileWithRange(_) => "import",
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

/// The terminal name of a resolved definition, falling back to the last
/// segment when ty cannot name it (modules and import ranges).
fn resolved_name_of(db: &TyDb, resolved: &ResolvedDefinition<'_>, terminal: &str) -> String {
    match resolved {
        ResolvedDefinition::Definition(definition) => {
            definition.name(db).unwrap_or_else(|| terminal.to_string())
        }
        ResolvedDefinition::Module(_) | ResolvedDefinition::FileWithRange(_) => {
            terminal.to_string()
        }
    }
}

/// Whether a resolved definition lives in a `.pyi` stub.
fn is_stub_of(db: &TyDb, resolved: &ResolvedDefinition<'_>) -> bool {
    match resolved {
        ResolvedDefinition::Definition(definition) => definition.file(db).is_stub(db),
        ResolvedDefinition::Module(module) => module.file(db).is_stub(db),
        ResolvedDefinition::FileWithRange(range) => range.file().is_stub(db),
    }
}

/// One honesty entry per hop: resolve each intermediate prefix and append the
/// terminal, so the caller can see exactly how the dotted path was walked.
fn build_resolution<'db>(
    db: &TyDb,
    model: &SemanticModel<'db>,
    suite: &[ast::Stmt],
    segments: &[String],
    is_module: bool,
    terminal_kind: &'static str,
    symbol: &str,
) -> Vec<String> {
    let mut resolution = Vec::new();
    // The suite is `[import?] [prefix_1 .. prefix_{n-1}] [call]`; skip the
    // import and the call, and resolve the intermediate prefixes in order.
    let start = usize::from(is_module);
    let prefix_count = segments.len().saturating_sub(1);
    for (index, stmt) in suite.iter().enumerate().skip(start).take(prefix_count) {
        let hop = index - start;
        let kind = stmt
            .as_expr_stmt()
            .map(|stmt| &stmt.value)
            .and_then(|expr| resolve_expr(model, expr).ok())
            .and_then(|resolved| {
                resolved
                    .first()
                    .map(|resolved| kind_of_resolved(db, resolved))
            })
            .unwrap_or("unknown");
        let prefix = segments[..=hop].join(".");
        resolution.push(format!("{} -> {kind} {prefix}", segments[hop]));
    }
    // The terminal hop resolved through `call.func` in the main flow.
    let terminal = segments.last().map(String::as_str).unwrap_or(symbol);
    resolution.push(format!("{terminal} -> {terminal_kind} {symbol}"));
    resolution
}

/// `unique`, `overloads`, or `multiple`, reflecting how ty split the symbol.
fn disambiguation_of(resolved: &[ResolvedDefinition<'_>], signature_count: usize) -> &'static str {
    if resolved.len() > 1 {
        "multiple"
    } else if signature_count > 1 {
        "overloads"
    } else {
        "unique"
    }
}

/// Renders the return type off a signature label (`(…) -> str` → `str`),
/// scanning for the top-level ` -> ` arrow so a callable return type like
/// `(int) -> str` is not split at its inner arrow.
fn return_type_of(label: &str) -> Option<String> {
    let bytes = label.as_bytes();
    let mut depth = 0usize;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b'-' if depth == 0
                && bytes.get(index + 1) == Some(&b'>')
                && bytes.get(index + 2) == Some(&b' ') =>
            {
                return Some(label[index + 3..].trim().to_string());
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// Splits a rendered signature's parameters into [`DocParam`]s, classifying
/// each parameter's kind from the variadic/positional flags and its position
/// relative to the keyword-only `*` marker in the label.
fn doc_params<'db>(
    db: &'db TyDb,
    env: &ProgramEnvironment<'db>,
    details: &CallSignatureDetails<'db>,
) -> Vec<DocParam> {
    let label = &details.label;
    let bare_star = find_bare_star(label);
    let mut after_variadic = false;
    let mut search_from = 0usize;
    details
        .parameters
        .iter()
        .map(|param| {
            let position = label[search_from..]
                .find(&param.label)
                .map(|offset| search_from + offset);
            if let Some(position) = position {
                search_from = position + param.label.len();
            }
            let after_star =
                position.is_some_and(|position| bare_star.is_some_and(|star| position > star));
            let kind = param_kind(param, after_variadic, after_star);
            after_variadic |= param.is_variadic;
            DocParam {
                name: param.name.trim_start_matches('*').to_string(),
                type_: param.ty.display(db, env).to_string(),
                kind,
                default: param_default(&param.label),
            }
        })
        .collect()
}

/// The byte offset of the bare `*` keyword-only separator in a signature label
/// (`*` immediately followed by `,`), distinguishing it from `*args` (followed
/// by the name) and `**kwargs` (followed by another `*`).
fn find_bare_star(label: &str) -> Option<usize> {
    let bytes = label.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'*' && bytes.get(index + 1) == Some(&b',') {
            return Some(index);
        }
    }
    None
}

/// Classifies one parameter's kind from ty's flags plus the inferred position.
fn param_kind(
    param: &CallSignatureParameter<'_>,
    after_variadic: bool,
    after_star: bool,
) -> &'static str {
    if param.is_variadic {
        "variadic"
    } else if param.is_keyword_variadic {
        "keyword_variadic"
    } else if param.is_positional_only {
        "positional_only"
    } else if after_variadic || after_star {
        "keyword_only"
    } else {
        "positional_or_keyword"
    }
}

/// The rendered default off a parameter label (`skipkeys: bool = False` →
/// `False`). A `=` never appears in an annotation, so the first one marks the
/// default.
fn param_default(label: &str) -> Option<String> {
    label
        .split_once('=')
        .map(|(_, default)| default.trim().to_string())
}
