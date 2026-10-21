//! The ty integration: Astral's type checker compiled in (no interpreter,
//! no subprocess), exposed as a read-only whole-file verdict. The salsa `Db`
//! is hand-assembled here because the `ty_project` crate that would bundle it
//! is unpublished; this mirrors `ty_python_semantic`'s own `TestDb` pattern
//! and ruff's `ProjectDatabase`, reduced to: `OsSystem` + the vendored
//! typeshed stdlib + the site-packages of the environment discovered next to
//! the checked file.

pub(crate) mod cache;
mod check;
mod db;
mod settings;

use std::path::Path;

use crate::error::Error;
use crate::fs;
use crate::ruff::Diagnostic;

pub(crate) use db::DOCS_VIRTUAL_PATH;
pub use db::TyDb;

/// Type-checks `path` (`.py`/`.pyi`/`.ipynb`) by reading it once through
/// [`crate::fs`] and handing those bytes to [`check_snapshot`]. Callers that
/// have already read the file should call [`check_snapshot`] directly so the
/// bytes are not read twice.
pub fn check_path(path: &Path) -> Result<Vec<Diagnostic>, Error> {
    let snapshot = fs::read(path)?;
    check_snapshot(path, &snapshot)
}

/// Type-checks the already-read `snapshot` of `path` and returns the
/// diagnostics in tpmcp's stable shape. Notebooks are checked cell-by-cell with
/// cell-relative positions; plain files are checked directly.
pub fn check_snapshot(path: &Path, snapshot: &fs::Snapshot) -> Result<Vec<Diagnostic>, Error> {
    check_snapshot_with_python(path, snapshot, None)
}

/// Like [`check_snapshot`], but pins the type-checking environment to `python`
/// — a path to a Python interpreter or a virtual-environment root — instead of
/// relying on the implicit discovery order. `None` behaves like
/// [`check_snapshot`].
pub fn check_snapshot_with_python(
    path: &Path,
    snapshot: &fs::Snapshot,
    python: Option<&Path>,
) -> Result<Vec<Diagnostic>, Error> {
    // The parsed AST's recursive Drop glue no longer runs here: the database
    // is cached (see `cache`), so its drop is deferred to eviction or thread
    // teardown, where `cache` re-applies `crate::stack::guarded` sized by the
    // largest source it served. The semantic-index/inference recursion itself
    // is still grown inside `check_file` (`guarded_walk`).
    check::check_snapshot_inner(path, snapshot, python)
}
