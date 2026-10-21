//! CPython `Lib/*.py` docstring fallback for the docs resolver.
//!
//! Typeshed keeps a docstring for most stdlib definitions, but where a stub
//! omits it the honest answer is not a silent `None` — it is to look at the
//! interpreter's *real* CPython source (a venv's `lib/python3.X/` symlinks the
//! base stdlib, and a system Python owns `/usr/lib/python3.X/`). This module
//! maps a vendored typeshed stub path (`stdlib/json/__init__.pyi`) to the real
//! source (`<stdlib>/json/__init__.py`), parses it with ruff's already-linked
//! parser, and extracts the terminal definition's docstring. It is
//! filesystem-only: no subprocess, no interpreter execution, and no eager walk
//! — one file per symbol, cached for the process.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use ruff_db::files::FilePath;
use ruff_db::system::{SystemPath, SystemPathBuf};
use ruff_python_ast as ast;
use ruff_python_parser::parse_module;

use crate::ty::TyDb;

/// The docstring answer for one definition: the docstring itself, where it came
/// from, and — when there is none anywhere — an honest note explaining why and
/// what was looked at (never a silent gap).
pub(super) struct ResolvedDocstring {
    pub(super) docstring: Option<String>,
    /// `stub`, `source`, or `none`.
    pub(super) source: &'static str,
    pub(super) note: Option<String>,
}

/// The CPython-source fallback result.
enum Enrichment {
    Found { docstring: String },
    Missing { note: String },
}

/// Resolves a definition's docstring: the stub/source docstring wins, then the
/// real CPython source, then an honest note. `stub_docstring` is the docstring
/// already extracted from the resolved definition (which may be `None`);
/// `definition_file` names the file that definition lives in; `resolved_name`
/// is the terminal name to look up in the real source.
pub(super) fn resolve_docstring(
    db: &TyDb,
    definition_file: &FilePath,
    resolved_name: &str,
    is_stub: bool,
    stub_docstring: Option<String>,
) -> ResolvedDocstring {
    if let Some(docstring) = stub_docstring {
        return ResolvedDocstring {
            docstring: Some(docstring),
            source: if is_stub { "stub" } else { "source" },
            note: None,
        };
    }

    match enrich_from_cpython(db, definition_file, resolved_name, is_stub) {
        Enrichment::Found { docstring } => ResolvedDocstring {
            docstring: Some(docstring),
            source: "source",
            note: None,
        },
        Enrichment::Missing { note } => ResolvedDocstring {
            docstring: None,
            source: "none",
            note: Some(note),
        },
    }
}

/// Falls back to the real CPython source for a definition whose stub has no
/// docstring. Only vendored typeshed `stdlib/` stubs map to the interpreter's
/// `Lib/` tree; a third-party stub, a `.py` source, a builtin, or a C extension
/// without a stdlib stub has no source to fall back to and is reported as such.
fn enrich_from_cpython(
    db: &TyDb,
    definition_file: &FilePath,
    resolved_name: &str,
    is_stub: bool,
) -> Enrichment {
    let Some(stub_path) = definition_file
        .as_vendored_path()
        .map(|path| path.as_str())
        .filter(|path| path.starts_with("stdlib/"))
    else {
        // why: the fallback only knows how to map typeshed stdlib stubs; anything
        // else is honestly reported with its own reason rather than a bare None.
        let note = if is_stub {
            format!(
                "no docstring: the stub has none and no CPython source could be mapped \
                 (definition at `{}`)",
                definition_file.as_str()
            )
        } else {
            "no docstring: the definition has none (C extension or bare stub)".to_string()
        };
        return Enrichment::Missing { note };
    };

    let Some(stdlib) = db.real_stdlib_path() else {
        return Enrichment::Missing {
            note: format!(
                "no docstring: the stub has none and no CPython source was found to fall back to \
                 (looked at: `{stub_path}`)"
            ),
        };
    };

    let Some(source_path) = map_stub_to_source(stub_path, &stdlib) else {
        return Enrichment::Missing {
            note: format!(
                "no docstring: the stub has none and no CPython source could be mapped \
                 (looked at: `{stub_path}`)"
            ),
        };
    };

    let source_str = source_path.as_str().to_string();
    match extract_docstring(&source_path, resolved_name) {
        Some(docstring) => Enrichment::Found { docstring },
        None => {
            // Distinguish "the module is a C extension (no `.py` at all)" from
            // "the `.py` exists but the definition has no docstring", so the
            // note names what was actually checked.
            let note = if source_path.as_std_path().is_file() {
                format!(
                    "no docstring: the stub has none and the CPython source `{source_str}` has \
                     none to fall back to (looked at: `{stub_path}`, `{source_str}`)"
                )
            } else {
                format!(
                    "no docstring: the stub has none and no CPython source was found to fall \
                     back to (looked at: `{stub_path}`, `{source_str}`)"
                )
            };
            Enrichment::Missing { note }
        }
    }
}

/// Maps a vendored typeshed stub path to the real CPython source:
/// `stdlib/json/__init__.pyi` -> `<stdlib>/json/__init__.py`.
fn map_stub_to_source(stub_path: &str, stdlib: &SystemPath) -> Option<SystemPathBuf> {
    let rest = stub_path.strip_prefix("stdlib/")?;
    let rest = rest.strip_suffix(".pyi")?;
    Some(stdlib.join(rest).with_extension("py"))
}

/// The terminal definition's docstring from the real source, or `None`.
fn extract_docstring(source_path: &SystemPath, resolved_name: &str) -> Option<String> {
    source_docstrings(source_path.as_std_path())
        .get(resolved_name)
        .cloned()
}

/// Process-wide cache of a real source file's `name -> docstring` map, keyed by
/// the source path. Stdlib files are read-only for the process lifetime, so a
/// stale entry is harmless; the cache keeps `py_module_members` from re-reading
/// the same `Lib/*.py` once per member.
type SourceDocstrings = RwLock<HashMap<PathBuf, Arc<HashMap<String, String>>>>;

fn source_docstrings(path: &Path) -> Arc<HashMap<String, String>> {
    if let Some(docstrings) = source_docstrings_cache()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(path)
    {
        return docstrings.clone();
    }
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let docstrings = Arc::new(parse_docstrings(&text));
    source_docstrings_cache()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(path.to_path_buf(), docstrings.clone());
    docstrings
}

fn source_docstrings_cache() -> &'static SourceDocstrings {
    static CACHE: OnceLock<SourceDocstrings> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Parses one CPython source file into a `name -> docstring` map over every
/// top-level function/class and each class's methods. First definition of a
/// name wins, matching source order.
fn parse_docstrings(text: &str) -> HashMap<String, String> {
    let Ok(parsed) = parse_module(text) else {
        return HashMap::new();
    };
    let mut docstrings = HashMap::new();
    collect_docstrings(&parsed.syntax().body, &mut docstrings);
    docstrings
}

fn collect_docstrings(body: &[ast::Stmt], out: &mut HashMap<String, String>) {
    for stmt in body {
        match stmt {
            ast::Stmt::FunctionDef(def) => {
                insert_body_docstring(out, def.name.as_str(), &def.body);
            }
            ast::Stmt::ClassDef(def) => {
                insert_body_docstring(out, def.name.as_str(), &def.body);
                // A method's docstring lives one level down; recurse so
                // `Class.method` can also fall back to the real source.
                collect_docstrings(&def.body, out);
            }
            _ => {}
        }
    }
}

fn insert_body_docstring(out: &mut HashMap<String, String>, name: &str, body: &[ast::Stmt]) {
    if let Some(docstring) = body_docstring(body) {
        out.entry(name.to_string()).or_insert(docstring);
    }
}

/// The first string literal of a function/class body, the Python docstring.
fn body_docstring(body: &[ast::Stmt]) -> Option<String> {
    let stmt = body.first()?;
    let value = &stmt.as_expr_stmt()?.value;
    value
        .as_string_literal_expr()
        .map(|expr| expr.value.to_str().to_string())
}
