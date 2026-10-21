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
//! deduplicated and ranked (not-stub before stub, not-reexported before
//! re-exported, reachable before unreachable, then kind, then source order) and
//! the first is reported. The `disambiguation` marker records *how* ty split
//! the symbol:
//!
//! - `unique` — exactly one definition, one signature;
//! - `overloads` — one callable with `@overload`s (one `signatures[]` entry per
//!   overload);
//! - `union` — a union of callables, one signature per element;
//! - `multiple` — several genuinely-distinct definitions (all listed in
//!   `candidates`);
//! - `reexport` — the terminal definition is itself a re-exported import;
//! - `stub_mapped` — reserved for Phase 2b stub→source mapping (not yet
//!   produced).
//!
//! A dotted path is walked hop by hop and the `resolution` trace carries one
//! entry per segment, so a dead-end is reported at the failing segment with its
//! receiver type — never a silent wrong answer.

use std::path::Path;

use ruff_db::parsed::parsed_module;
use ruff_db::source::source_text;
use ruff_python_ast as ast;
use ruff_source_file::LineIndex;
use ruff_text_size::{Ranged, TextSize};
use ty_python_core::definition::{Definition, DefinitionKind};
use ty_python_core::scope::NodeWithScopeKind;
use ty_python_semantic::types::ProgramEnvironment;
use ty_python_semantic::types::ide_support::{
    CallSignatureDetails, CallSignatureParameter, call_signature_details, constructor_signature,
    unreachable_ranges,
};
use ty_python_semantic::{
    Db as SemanticDb, HasType, ImportAliasResolution, ResolvedDefinition, SemanticModel,
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

/// One non-top definition that also matched, listed so the LLM sees what else
/// the dotted path could have resolved to.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DocCandidate {
    /// The definition's own name.
    pub resolved_name: String,
    /// Its kind, using the same coarse strings as [`DocSymbol::kind`].
    pub kind: String,
    /// The defining file and 1-based line, as `path:line`.
    pub source: String,
}

/// One resolved library symbol: what it is, how it is called, and its docs.
#[derive(Debug, Clone)]
pub struct DocSymbol {
    /// The terminal name of the resolved definition (e.g. `dumps`).
    pub resolved_name: String,
    /// `module`, `class`, `method`, `function`, `attribute`, `type_alias`, or
    /// `other`.
    pub kind: String,
    /// Rendered signatures — one per overload, or the constructor for a class
    /// (empty when the symbol is not callable).
    pub signatures: Vec<String>,
    /// Rendered return type, when the terminal is callable (first overload).
    pub return_type: Option<String>,
    /// Structured parameters, when the terminal is callable (first overload).
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
    /// Whether the terminal definition is itself a re-exported import.
    pub is_reexported: bool,
    /// `unique`, `overloads`, `union`, `multiple`, or `reexport` (see module
    /// docs; `stub_mapped` is reserved for a later phase).
    pub disambiguation: &'static str,
    /// One entry per dotted segment: `<segment> -> <kind> <prefix>`.
    pub resolution: Vec<String>,
    /// The non-top definitions that also matched, ranked, so the LLM sees what
    /// else the path could have resolved to (empty when unique).
    pub candidates: Vec<DocCandidate>,
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
    let env = model.program_environment();

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

    // Walk the intermediate hops first so a multi-segment dead-end is reported
    // at the failing segment, then resolve the terminal.
    let mut resolution = build_resolution(
        &db,
        &model,
        &env,
        parsed.suite(),
        &segments,
        is_module,
        symbol,
    )?;

    let terminal = resolve_expr(&model, &call.func)?;
    let candidates = rank_candidates(&db, &terminal);
    let first = candidates.first().cloned().ok_or_else(|| {
        let receiver = receiver_type_of(&db, &env, &model, &call.func);
        let segment = segments.last().map(String::as_str).unwrap_or(symbol);
        Error::analysis(format!(
            "symbol `{symbol}` did not resolve: `{segment}` has no definition on receiver `{receiver}`"
        ))
    })?;

    let terminal_segment = segments.last().map(String::as_str).unwrap_or(symbol);
    let kind = kind_of_resolved(&db, &first);
    let resolved_name = resolved_name_of(&db, &first, terminal_segment);
    let is_stub = is_stub_of(&db, &first);
    let is_reexported = is_reexported_of(&db, &first);
    resolution.push(format!("{terminal_segment} -> {kind} {symbol}"));

    // Structured signature. A class terminal renders its constructor (so the
    // LLM learns how to instantiate it); a callable renders one signature per
    // overload through `call_signature_details`.
    let is_class = kind == "class";
    let (signatures, return_type, parameters, is_union) = if is_class {
        let signatures = constructor_signature(&model, call).into_iter().collect();
        (signatures, None, Vec::new(), false)
    } else {
        let details = call_signature_details(&model, call);
        let is_union = call
            .func
            .inferred_type(&model)
            .is_some_and(|ty| ty.as_union().is_some());
        let (signatures, return_type, parameters) =
            callable_output(&db, &env, &details, &resolved_name);
        (signatures, return_type, parameters, is_union)
    };
    let disambiguation = disambiguation_of(
        candidates.len(),
        is_class,
        is_union,
        signatures.len(),
        is_reexported,
    );

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

    let candidates = candidates
        .iter()
        .skip(1)
        .map(|resolved| DocCandidate {
            resolved_name: resolved_name_of(&db, resolved, terminal_segment),
            kind: kind_of_resolved(&db, resolved).to_string(),
            source: source_of(&db, resolved),
        })
        .collect();

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
        is_reexported,
        disambiguation,
        resolution,
        candidates,
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

/// One honesty entry per intermediate hop: resolve each prefix in order and
/// fail at the first dead-end, naming the failing segment and its receiver
/// type rather than silently reporting the terminal as unresolved.
fn build_resolution<'db>(
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
fn receiver_type_of<'db>(
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

/// Renders the callable output: one signature per overload and the structured
/// return type/parameters off the first overload.
fn callable_output<'db>(
    db: &'db TyDb,
    env: &ProgramEnvironment<'db>,
    details: &[CallSignatureDetails<'db>],
    resolved_name: &str,
) -> (Vec<String>, Option<String>, Vec<DocParam>) {
    let mut signatures = Vec::with_capacity(details.len());
    let mut return_type = None;
    let mut parameters = Vec::new();
    for (index, detail) in details.iter().enumerate() {
        signatures.push(format!("def {resolved_name}{}", detail.label));
        if index == 0 {
            return_type = return_type_of(&detail.label);
            parameters = doc_params(db, env, detail);
        }
    }
    (signatures, return_type, parameters)
}

// ---- candidate ranking ----

/// The deterministic sort key for one resolved definition: the ranking rules
/// applied in order, lower sorts first (better).
type RankKey = (bool, bool, bool, u8, String, TextSize);

/// Deduplicates ty's definitions by `(file, focus_range)` and ranks them:
/// not-stub before stub, not-reexported before re-exported, reachable before
/// unreachable, class/function/type_alias before bare assignment/import, then
/// source order as the final tiebreak.
fn rank_candidates<'db>(
    db: &TyDb,
    resolved: &[ResolvedDefinition<'db>],
) -> Vec<ResolvedDefinition<'db>> {
    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::with_capacity(resolved.len());
    for definition in resolved {
        if seen.insert(definition.focus_range(db)) {
            deduped.push(definition.clone());
        }
    }
    let mut ranked: Vec<(RankKey, ResolvedDefinition<'_>)> = deduped
        .into_iter()
        .map(|definition| (rank_key(db, &definition), definition))
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0));
    ranked
        .into_iter()
        .map(|(_, definition)| definition)
        .collect()
}

/// One candidate's sort key (see [`rank_candidates`]).
fn rank_key(db: &TyDb, resolved: &ResolvedDefinition<'_>) -> RankKey {
    let focus = resolved.focus_range(db);
    (
        is_stub_of(db, resolved),
        is_reexported_of(db, resolved),
        !is_reachable(db, resolved),
        kind_rank_of(db, resolved),
        focus.file().path(db).as_str().to_string(),
        focus.range().start(),
    )
}

/// Coarse rank for the `kind` tiebreak: named, callable-like kinds first.
fn kind_rank_of(db: &TyDb, resolved: &ResolvedDefinition<'_>) -> u8 {
    match kind_of_resolved(db, resolved) {
        "class" | "method" | "function" | "type_alias" => 0,
        "attribute" => 1,
        "module" | "import" => 2,
        _ => 3,
    }
}

/// Whether a definition lies in a range ty's reachability analysis marks
/// unreachable. ty already filters most definitions to reachable ones; this
/// keeps the rule honest for import fallbacks that slip through.
fn is_reachable(db: &TyDb, resolved: &ResolvedDefinition<'_>) -> bool {
    let focus = resolved.focus_range(db);
    let program_file = SemanticDb::program_file(db, focus.file());
    let start = focus.range().start();
    !unreachable_ranges(db, program_file)
        .iter()
        .any(|range| range.range.contains(start))
}

/// `unique`, `overloads`, `union`, `multiple`, or `reexport`, reflecting how ty
/// split the symbol and its callable shape.
fn disambiguation_of(
    candidate_count: usize,
    is_class: bool,
    is_union: bool,
    signature_count: usize,
    is_reexported: bool,
) -> &'static str {
    if is_class {
        if candidate_count > 1 {
            "multiple"
        } else if is_reexported {
            "reexport"
        } else {
            "unique"
        }
    } else if is_union {
        "union"
    } else if signature_count > 1 {
        "overloads"
    } else if candidate_count > 1 {
        "multiple"
    } else if is_reexported {
        "reexport"
    } else {
        "unique"
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

/// Maps a definition's kind to the coarse `py_docs` kind string, splitting a
/// function bound inside a class body out as `method`.
fn kind_of_definition(db: &TyDb, definition: Definition<'_>) -> &'static str {
    match definition.kind(db) {
        DefinitionKind::Function(_) if is_method(db, definition) => "method",
        DefinitionKind::Function(_) => "function",
        DefinitionKind::Class(_) => "class",
        DefinitionKind::TypeAlias(_) => "type_alias",
        DefinitionKind::Assignment(_) | DefinitionKind::AnnotatedAssignment(_) => "attribute",
        DefinitionKind::ImportFromSubmodule(_) => "module",
        DefinitionKind::Import(_)
        | DefinitionKind::ImportFrom(_)
        | DefinitionKind::StarImport(_) => "import",
        _ => "other",
    }
}

/// Whether a function definition is a method: its name is bound in a class
/// body (ty records a function's name in the enclosing scope, so the enclosing
/// scope — not the function body — is the class for a method).
fn is_method(db: &TyDb, definition: Definition<'_>) -> bool {
    matches!(
        definition.scope(db).node(db),
        NodeWithScopeKind::Class(_) | NodeWithScopeKind::ClassTypeParameters(_)
    )
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

/// Whether the terminal definition is itself a re-exported import. A name
/// defined directly (function, class, assignment) is not a re-export; only an
/// import alias that survived alias resolution is.
fn is_reexported_of(db: &TyDb, resolved: &ResolvedDefinition<'_>) -> bool {
    let ResolvedDefinition::Definition(definition) = resolved else {
        return false;
    };
    let is_import = matches!(
        definition.kind(db),
        DefinitionKind::Import(_)
            | DefinitionKind::ImportFrom(_)
            | DefinitionKind::ImportFromSubmodule(_)
            | DefinitionKind::StarImport(_)
    );
    is_import && definition.is_reexported(db)
}

/// `path:line` for a definition, for the `candidates` list.
fn source_of(db: &TyDb, resolved: &ResolvedDefinition<'_>) -> String {
    let focus = resolved.focus_range(db);
    let file = focus.file();
    let source = source_text(db, file);
    let index = LineIndex::from_source_text(&source);
    let line = index.line_index(focus.range().start()).get();
    format!("{}:{line}", file.path(db).as_str())
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
