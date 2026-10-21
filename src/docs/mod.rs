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

mod enrich;
mod members;
mod rank;
mod resolve;
mod signature;

use std::path::Path;

use ruff_db::parsed::parsed_module;
use ruff_db::source::source_text;
use ruff_python_ast as ast;
use ruff_source_file::LineIndex;
use ruff_text_size::Ranged;
use ty_python_semantic::types::ide_support::{call_signature_details, constructor_signature};
use ty_python_semantic::{Db as SemanticDb, HasType, SemanticModel};

use crate::error::Error;
use crate::ty::TyDb;

use enrich::resolve_docstring;
pub use members::{DocModule, DocModuleMember, resolve_module_members};
use rank::{
    disambiguation_of, is_reexported_of, is_stub_of, kind_of_resolved, rank_candidates,
    resolved_name_of, source_of,
};
use resolve::{build_resolution, build_source, receiver_type_of, resolve_expr, split_segments};
use signature::callable_output;

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
    /// When the docstring is missing, a short human-readable note explaining
    /// why and what was looked at — never a silent `None`.
    pub note: Option<String>,
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

    let focus = first.focus_range(&db);
    let source_file = focus.file();
    let definition_path = source_file.path(&db).clone();
    let source_text = source_text(&db, source_file);
    let line_index = LineIndex::from_source_text(&source_text);
    let source_line = Some(line_index.line_index(focus.range().start()).get());

    // The overload's own docstring, falling back to the runtime implementation
    // (overload declarations often omit theirs) — and, when nothing has it, an
    // honest note instead of a silent `None`.
    let docstring = resolve_docstring(
        &definition_path,
        is_stub,
        first
            .docstring(&db)
            .or_else(|| first.implementation_docstring(&db)),
    );

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
        docstring: docstring.docstring,
        docstring_source: docstring.source,
        note: docstring.note,
        source_path: Some(definition_path.as_str().to_string()),
        source_line,
        is_stub,
        is_reexported,
        disambiguation,
        resolution,
        candidates,
    })
}
