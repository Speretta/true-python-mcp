//! Python-environment discovery for `ty_check`: the venv lookup, the
//! ambiguity guard and the explicit `python` parameter.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::{codes, scaffold_venv, site_packages_dir, temp_dir};
use tpmcp::ty;

/// Cache-invalidation tripwire: the settings cache shares the discovery result
/// but never the file bytes. Write a clean assignment, check (this populates
/// the per-root discovery cache), overwrite with a type error, then re-check —
/// the second check reuses the cached [`ProgramSettings`] yet must still read
/// the file's fresh bytes and report `invalid-assignment`.
#[test]
fn cached_settings_still_read_fresh_bytes() {
    let dir = temp_dir("ty-cache-fresh");
    let path = dir.join("fresh.py");
    std::fs::write(&path, "x: int = 1\n").unwrap();
    assert!(ty::check_path(&path).unwrap().is_empty());
    std::fs::write(&path, "x: int = \"oops\"\n").unwrap();
    let diagnostics = ty::check_path(&path).unwrap();
    assert_eq!(
        codes(&diagnostics),
        vec!["invalid-assignment".to_string()],
        "a cached-settings check must read the file's fresh bytes"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The ancestor walk: a nested file (`root/src/mod.py`) finds `root/.venv` and
/// resolves its packages, so `import somepkg` no longer falls through to the
/// system Python and reports `unresolved-import`. Before the walk, discovery
/// only checked `src_root/.venv` — one level — and this file missed it.
#[test]
fn nested_file_resolves_ancestor_venv() {
    let dir = temp_dir("ty-ancestor");
    let root = dir.join("root");
    std::fs::create_dir_all(root.join("src")).unwrap();
    scaffold_venv(&root, ".venv", "somepkg");
    let path = root.join("src/mod.py");
    std::fs::write(&path, "import somepkg\n").unwrap();

    let diagnostics = ty::check_path(&path).unwrap();
    assert!(
        !codes(&diagnostics).iter().any(|c| c == "unresolved-import"),
        "the nested file must resolve the ancestor venv: {diagnostics:?}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Two virtual environments in the chain with no explicit signal (no `python`,
/// no VIRTUAL_ENV, no CONDA_PREFIX) are ambiguous: the walk cannot pick one, so
/// `check_path` errors instead of silently choosing.
#[test]
fn multiple_ancestor_venvs_are_ambiguous() {
    let dir = temp_dir("ty-ambiguous");
    let root = dir.join("root");
    std::fs::create_dir_all(&root).unwrap();
    scaffold_venv(&root, ".venv", "pkg_a");
    scaffold_venv(&root, "venv", "pkg_b");
    let path = root.join("mod.py");
    std::fs::write(&path, "import pkg_a\n").unwrap();

    let error = ty::check_path(&path).unwrap_err();
    assert_eq!(error.kind(), "analysis_error");
    assert!(
        error.to_string().contains("ambiguous Python environment"),
        "{error}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// An explicit `python` param pointing at a venv root resolves that venv's
/// packages even when the checked file has no ancestor venv of its own.
#[test]
fn explicit_python_param_resolves_venv() {
    let dir = temp_dir("ty-python-param");
    let root = dir.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let venv = scaffold_venv(&root, ".venv", "somepkg");

    // The file lives outside `root` so no ancestor walk can find `root/.venv`.
    let other = dir.join("other");
    std::fs::create_dir_all(&other).unwrap();
    let path = other.join("mod.py");
    std::fs::write(&path, "import somepkg\n").unwrap();

    let snapshot = tpmcp::fs::read(&path).unwrap();
    let diagnostics =
        ty::check_snapshot_with_python(&path, &snapshot, Some(venv.as_path())).unwrap();
    assert!(
        !codes(&diagnostics).iter().any(|c| c == "unresolved-import"),
        "the explicit python param must resolve the venv: {diagnostics:?}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The discovered venv's `pyvenv.cfg` `version` key drives the checker's target
/// Python version: PEP 695 `type X[T] = ...` (3.12+) parses under a 3.13 venv
/// but is a syntax error under a 3.11 venv, so the two checks pin both
/// directions of version adoption. (There is no silent 3.11 fallback to pin:
/// an unmeasurable version is now disclosed, not assumed.)
#[test]
fn venv_python_version_drives_syntax() {
    let dir = temp_dir("ty-version");
    let source = "type Alias[T] = list[T]\n";

    // A 3.13 venv parses the PEP 695 statement.
    let root313 = dir.join("root313");
    std::fs::create_dir_all(&root313).unwrap();
    let venv313 = root313.join(".venv");
    std::fs::create_dir_all(site_packages_dir(&venv313, "3.13")).unwrap();
    std::fs::write(
        venv313.join("pyvenv.cfg"),
        format!("home = {}\nversion = 3.13\n", root313.display()),
    )
    .unwrap();
    let path313 = root313.join("mod.py");
    std::fs::write(&path313, source).unwrap();
    let diagnostics = ty::check_path(&path313).unwrap();
    assert!(
        !codes(&diagnostics).iter().any(|c| c == "invalid-syntax"),
        "PEP 695 syntax must parse under a 3.13 venv: {diagnostics:?}"
    );

    // A 3.11 venv rejects the same statement.
    let root311 = dir.join("root311");
    std::fs::create_dir_all(&root311).unwrap();
    let venv311 = root311.join(".venv");
    std::fs::create_dir_all(site_packages_dir(&venv311, "3.11")).unwrap();
    std::fs::write(
        venv311.join("pyvenv.cfg"),
        format!("home = {}\nversion = 3.11\n", root311.display()),
    )
    .unwrap();
    let path311 = root311.join("mod.py");
    std::fs::write(&path311, source).unwrap();
    let diagnostics = ty::check_path(&path311).unwrap();
    assert!(
        codes(&diagnostics).iter().any(|c| c == "invalid-syntax"),
        "PEP 695 syntax must be a syntax error under a 3.11 venv: {diagnostics:?}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}
