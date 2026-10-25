//! Candidate deduplication, ranking, and the `disambiguation` marker, plus the
//! coarse `kind` strings the resolver and its trace report.

use ruff_db::files::File;
use ruff_db::source::source_text;
use ruff_source_file::LineIndex;
use ruff_text_size::{Ranged, TextSize};
use ty_python_core::definition::{Definition, DefinitionKind};
use ty_python_core::scope::NodeWithScopeKind;
use ty_python_semantic::types::ide_support::unreachable_ranges;
use ty_python_semantic::{Db as SemanticDb, ResolvedDefinition};

use crate::ty::TyDb;

// ---- candidate ranking ----

/// The deterministic sort key for one resolved definition: the ranking rules
/// applied in order, lower sorts first (better). The file tiebreak is `File`
/// (not its path string), so ranking costs no per-candidate allocation; `File`
/// orders by salsa-assigned id — stable within a process, not across runs.
type RankKey = (bool, bool, bool, u8, File, TextSize);

/// Deduplicates ty's definitions by identity and ranks them: not-stub before
/// stub, not-reexported before re-exported, reachable before unreachable,
/// class/function/type_alias before bare assignment/import, then source order
/// as the final tiebreak.
pub(super) fn rank_candidates<'db>(
    db: &TyDb,
    resolved: &[ResolvedDefinition<'db>],
) -> Vec<ResolvedDefinition<'db>> {
    // Dedup by the definition's identity, not its `focus_range`: two genuinely
    // distinct definitions (e.g. a re-export alias and its target, or two
    // overloads) can share one `(file, range)`, so a range key would collapse
    // them into one candidate. `ResolvedDefinition` is `Eq`, and its
    // `Definition` arm is a salsa-tracked handle that compares by `scope` +
    // `place` (the `kind` field is `#[no_eq]`), so equality is exact identity.
    // The scan is O(n²), and n is the candidate count for one symbol — 1-3 in
    // practice. A linear dedup is expressible: all three variant payloads
    // (`Definition`, `ProgramFile`, `FileRange`) impl `Hash`, so a small key
    // enum would do it. Hashing 1-3 items still costs more than scanning them,
    // and the key type would exist only to serve the scan, so the scan is the
    // choice here. Revisit with a key enum if a symbol ever fans out to many
    // candidates.
    let mut deduped: Vec<ResolvedDefinition<'db>> = Vec::with_capacity(resolved.len());
    for definition in resolved {
        if !deduped.contains(definition) {
            deduped.push(definition.clone());
        }
    }
    let mut ranked: Vec<(RankKey, ResolvedDefinition<'_>)> = deduped
        .into_iter()
        .map(|definition| (rank_key(db, &definition), definition))
        .collect();
    ranked.sort_by_key(|a| a.0);
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
        focus.file(),
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
pub(super) fn disambiguation_of(
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
pub(super) fn kind_of_resolved(db: &TyDb, resolved: &ResolvedDefinition<'_>) -> &'static str {
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
/// scope (not the function body) is the class for a method).
fn is_method(db: &TyDb, definition: Definition<'_>) -> bool {
    matches!(
        definition.scope(db).node(db),
        NodeWithScopeKind::Class(_) | NodeWithScopeKind::ClassTypeParameters(_)
    )
}

/// The terminal name of a resolved definition, falling back to the last
/// segment when ty cannot name it (modules and import ranges).
pub(super) fn resolved_name_of(
    db: &TyDb,
    resolved: &ResolvedDefinition<'_>,
    terminal: &str,
) -> String {
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
pub(super) fn is_stub_of(db: &TyDb, resolved: &ResolvedDefinition<'_>) -> bool {
    match resolved {
        ResolvedDefinition::Definition(definition) => definition.file(db).is_stub(db),
        ResolvedDefinition::Module(module) => module.file(db).is_stub(db),
        ResolvedDefinition::FileWithRange(range) => range.file().is_stub(db),
    }
}

/// Whether the terminal definition is itself a re-exported import. A name
/// defined directly (function, class, assignment) is not a re-export; only an
/// import alias that survived alias resolution is.
pub(super) fn is_reexported_of(db: &TyDb, resolved: &ResolvedDefinition<'_>) -> bool {
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
pub(super) fn source_of(db: &TyDb, resolved: &ResolvedDefinition<'_>) -> String {
    let focus = resolved.focus_range(db);
    let file = focus.file();
    let source = source_text(db, file);
    let index = LineIndex::from_source_text(&source);
    let line = index.line_index(focus.range().start()).get();
    format!("{}:{line}", file.path(db).as_str())
}
