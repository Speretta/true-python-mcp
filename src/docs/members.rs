//! The `py_module_members` resolver: enumerate a module's public surface and,
//! for each member, resolve its kind, one-line signature and one-line
//! docstring — a two-pass walk over the same hand-assembled [`TyDb`] the
//! `py_docs` resolver uses.
//!
//! Pass 1 lists members through ty's *only* public enumeration surface: a
//! `from module import *` completion ([`SemanticModel::from_import_completions`];
//! `all_members`/`Member` are `pub(crate)`). Pass 2 re-resolves each capped
//! member through its own synthetic call (`module.member()`) so the existing
//! [`call_signature_details`]/[`constructor_signature`] helpers can split a
//! signature, reusing the `py_docs` resolution and ranking helpers.

use std::path::Path;

use ruff_db::parsed::parsed_module;
use ruff_python_ast as ast;
use ty_module_resolver::ModuleName;
use ty_python_semantic::NameKind;
use ty_python_semantic::types::ide_support::{call_signature_details, constructor_signature};
use ty_python_semantic::{Db as SemanticDb, SemanticModel};

use crate::error::Error;
use crate::ty::TyDb;

use super::enrich::resolve_docstring;
use super::rank::{is_stub_of, kind_of_resolved, rank_candidates};
use super::resolve::resolve_expr;
use super::signature::callable_output;

/// Cap on the number of members re-resolved for one module, so a huge package
/// (numpy) does not blow up the two-pass cost; the `truncated` flag reports it.
const MAX_MEMBERS: usize = 50;

/// Cap on a member's rendered docstring, so the listing stays an index rather
/// than a wall of prose.
const DOCSTRING_SUMMARY_BYTES: usize = 160;

/// One member of a module: its name, coarse kind, one-line signature and
/// one-line docstring.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DocModuleMember {
    /// The member's own name (`dumps`), as you would type `module.dumps`.
    pub name: String,
    /// `module`, `class`, `method`, `function`, `attribute`, `type_alias`, or
    /// `other` — the same coarse kind strings `py_docs` reports.
    pub kind: String,
    /// Rendered one-line signature (`def dumps(obj, ...) -> str`, or the
    /// constructor `class X(...)`); `None` when the member is not callable.
    pub signature: Option<String>,
    /// First line of the member's docstring, truncated.
    pub docstring_summary: Option<String>,
    /// When the member's docstring is missing, a short human-readable note
    /// explaining why and what was looked at — never a silent `None`.
    pub note: Option<String>,
}

/// The resolved module and its (possibly truncated) member listing.
#[derive(Debug, Clone)]
pub struct DocModule {
    /// The module name as passed in.
    pub module: String,
    /// The resolved module path (equal to `module` for a top-level name).
    pub resolved_name: String,
    /// Total distinct members enumerated, before the [`MAX_MEMBERS`] cap.
    pub member_count: usize,
    /// Whether the `members` list was truncated to [`MAX_MEMBERS`].
    pub truncated: bool,
    /// The capped, deterministically-ordered members.
    pub members: Vec<DocModuleMember>,
}

/// Resolves `module` (a single importable segment) to its member listing,
/// anchored at `src_root` with `python` pinning the environment, exactly like
/// [`super::resolve_symbol_with_python`].
pub fn resolve_module_members(
    module: &str,
    src_root: Option<&Path>,
    python: Option<&Path>,
) -> Result<DocModule, Error> {
    // A member listing is per-module, so only a single import segment is
    // accepted; empty or dotted input is a caller error, not a resolution
    // failure. `ModuleName::new` enforces Python identifier syntax.
    if module.is_empty() || module.contains('.') || ModuleName::new(module).is_none() {
        return Err(Error::invalid_input(format!(
            "module must be a single importable module name, got {module:?}"
        )));
    }

    let src_root = src_root.unwrap_or_else(|| Path::new("."));

    // ---- pass 1: enumerate members ----

    // `from module import *` is the only public way to list a module's members
    // (`all_members`/`Member` in ty are `pub(crate)`). The list ty returns is
    // every end-of-scope symbol — it is NOT filtered by `__all__` or
    // star-import privacy, so private (`_`-prefixed) and dunder names appear;
    // they are demoted to the tail of the ordering below.
    let probe_source = format!("from {module} import *\n");
    let db = TyDb::for_virtual_source_with_python(src_root, &probe_source, python)?;
    let file = db.docs_file();
    let program_file = SemanticDb::program_file(&db, file);
    let model = SemanticModel::new(&db, program_file);
    let parsed = parsed_module(&db, model.python_file()).load(&db);

    let resolved_name = match model.resolve_module(Some(module), 0) {
        Some(resolved) => resolved.name(model.db()).as_str().to_string(),
        None => {
            return Err(Error::analysis(format!(
                "module `{module}` did not resolve"
            )));
        }
    };

    let import = parsed
        .suite()
        .first()
        .and_then(ast::Stmt::as_import_from_stmt)
        .ok_or_else(|| Error::analysis("synthetic member probe has no import"))?;
    let completions = model.from_import_completions(import);

    // Deduplicate (a name can surface from both the member and the submodule
    // listing) and order deterministically: ty's `NameKind` convention (Normal
    // first, then Dunder, then Sunder — the derived `Ord` of the enum, matching
    // ty's own completion ordering), then lexicographic.
    let mut names: Vec<String> = Vec::with_capacity(completions.len());
    for completion in &completions {
        let name = completion.name.to_string();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names.sort_by(|a, b| {
        NameKind::classify(a)
            .cmp(&NameKind::classify(b))
            .then_with(|| a.cmp(b))
    });

    let member_count = names.len();
    let truncated = member_count > MAX_MEMBERS;
    names.truncate(MAX_MEMBERS);

    // ---- pass 2: re-resolve each member ----

    // One synthetic call per capped member, in a single module, so discovery
    // and parsing happen once regardless of the member count.
    let mut source = format!("import {module}\n");
    for name in &names {
        source.push_str(&format!("{module}.{name}()\n"));
    }
    let db = TyDb::for_virtual_source_with_python(src_root, &source, python)?;
    let file = db.docs_file();
    let program_file = SemanticDb::program_file(&db, file);
    let model = SemanticModel::new(&db, program_file);
    let parsed = parsed_module(&db, model.python_file()).load(&db);
    let env = model.program_environment();

    // The suite is `[import, name0(), name1(), ...]`; skip the import and pair
    // each call with its member name in order.
    let calls = parsed
        .suite()
        .iter()
        .skip(1)
        .filter_map(ast::Stmt::as_expr_stmt)
        .map(|stmt| &stmt.value)
        .filter_map(|value| value.as_call_expr());

    let members = names
        .iter()
        .zip(calls)
        .map(|(name, call)| resolve_member(&db, &env, &model, name, call))
        .collect();

    Ok(DocModule {
        module: module.to_string(),
        resolved_name,
        member_count,
        truncated,
        members,
    })
}

/// Resolves one member to its kind, one-line signature and docstring. A member
/// that ty lists but that fails to resolve still appears, as `other` with no
/// signature — hiding it would silently contradict the enumeration.
fn resolve_member<'db>(
    db: &'db TyDb,
    env: &ty_python_semantic::types::ProgramEnvironment<'db>,
    model: &SemanticModel<'db>,
    name: &str,
    call: &ast::ExprCall,
) -> DocModuleMember {
    let mut member = DocModuleMember {
        name: name.to_string(),
        kind: "other".to_string(),
        signature: None,
        docstring_summary: None,
        note: None,
    };

    let Ok(resolved) = resolve_expr(model, &call.func) else {
        // why: the member was enumerated, so hiding it would silently contradict
        // pass 1; report the dead end instead of a bare `other`.
        member.note = Some(format!("member `{name}` did not resolve to a definition"));
        return member;
    };
    let candidates = rank_candidates(db, &resolved);
    let Some(first) = candidates.first() else {
        member.note = Some(format!("member `{name}` has no ranked definition"));
        return member;
    };

    let kind = kind_of_resolved(db, first);
    member.kind = kind.to_string();

    // A class member renders its constructor; anything else renders one
    // signature per overload and keeps only the first. The listing collapses
    // the signature to one line below, so it stays an index.
    let raw_signature = if kind == "class" {
        constructor_signature(model, call)
    } else {
        let details = call_signature_details(model, call);
        let (signatures, _, _) = callable_output(db, env, &details, name);
        signatures.into_iter().next()
    };
    member.signature = raw_signature.map(|signature| one_line_signature(&signature));

    let docstring = resolve_docstring(
        db,
        &first.focus_range(db).file().path(db).clone(),
        name,
        is_stub_of(db, first),
        first
            .docstring(db)
            .or_else(|| first.implementation_docstring(db)),
    );
    member.docstring_summary = docstring.docstring.as_deref().map(docstring_summary);
    member.note = docstring.note;

    member
}

/// Collapses a rendered signature to a single line and keeps only the first
/// overload. Constructor signatures render one parameter per line, and ty
/// joins multiple constructors with `\nclass `, so the split drops any later
/// overloads before the newlines/indentation are folded into spaces.
fn one_line_signature(signature: &str) -> String {
    let first = signature.split("\nclass ").next().unwrap_or(signature);
    first.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first non-empty line of a docstring, trimmed and capped to a char
/// boundary ([`DOCSTRING_SUMMARY_BYTES`]), so the listing stays one line.
fn docstring_summary(docstring: &str) -> String {
    let first_line = docstring
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if first_line.len() <= DOCSTRING_SUMMARY_BYTES {
        first_line.to_string()
    } else {
        first_line[..first_line.floor_char_boundary(DOCSTRING_SUMMARY_BYTES)].to_string()
    }
}
