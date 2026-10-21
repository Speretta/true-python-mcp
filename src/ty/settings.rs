use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use ruff_db::system::{OsSystem, System, SystemPath, SystemPathBuf};
use ruff_python_ast::PythonVersion;
use ty_module_resolver::{FallibleStrategy, SearchPathSettings};
use ty_python_core::platform::PythonPlatform;
use ty_python_core::program::ProgramSettings;
use ty_python_semantic::{PythonVersionSource, PythonVersionWithSource};
use ty_site_packages::{PythonEnvironment, SysPrefixPathOrigin};

use crate::error::Error;

use super::db::{TySystem, to_system_path};

// ---- discovery + settings cache ----

/// Cache key: the checked file's source root plus an explicit `python` override
/// (when one was given).
type SettingsCacheKey = (PathBuf, Option<PathBuf>);

/// Process-wide cache of the discovery result, keyed by [`SettingsCacheKey`].
type SettingsCache = Mutex<HashMap<SettingsCacheKey, Arc<ProgramSettings>>>;

/// Process-wide cache of the discovery result, keyed by the checked file's
/// parent directory (the source root) and the explicit `python` override (if
/// any). Discovery (the environment lookup, the site-packages walk, and
/// search-path resolution) is the expensive half of [`TyDb::new`]. Its product
/// is plain `Send + Sync` data, so it is safe to share; the database itself is
/// *not* (salsa's `Storage` is per-thread state and must never be shared across
/// checks). The `python` path participates in the key so an explicit-python
/// check and an implicit (ancestor-walk) check of the same directory never
/// collide on one entry.
fn settings_cache() -> &'static SettingsCache {
    static CACHE: OnceLock<SettingsCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve the [`ProgramSettings`] for a file rooted at `src_root`, optionally
/// pinning the type-checking environment with `python`, caching the result per
/// (source root, python) pair so repeat checks of files in the same directory
/// skip the filesystem walk. The cache holds settings only, never the salsa
/// database, so a stale entry is harmless: each check still builds a fresh
/// [`TyDb`] that reads the checked file's current bytes.
pub(super) fn discover_settings(
    src_root: &SystemPath,
    python: Option<&Path>,
) -> Result<Arc<ProgramSettings>, Error> {
    let key = (
        src_root.as_std_path().to_path_buf(),
        python.map(Path::to_path_buf),
    );
    // A poisoned lock still guards a valid map: the only mutation is a single
    // insert under the lock, which cannot leave the map inconsistent.
    if let Some(settings) = settings_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
    {
        return Ok(settings.clone());
    }

    // A throwaway walk system with no virtual source: discovery never reads a
    // notebook's in-memory cell source, so a per-root cache entry is never
    // bound to any one check's virtual file.
    let system = TySystem {
        inner: OsSystem::new(src_root.to_path_buf()),
        virtual_source: None,
    };
    let vendored = ty_vendored::file_system().clone();
    let mut search_path_settings = SearchPathSettings::new(vec![src_root.to_path_buf()]);
    let environment = discover_environment(src_root, python, &system)?;
    if let Some(environment) = &environment {
        search_path_settings.site_packages_paths = environment
            .site_packages_paths(&system)
            .map_err(|error| Error::analysis(format!("site-packages discovery failed: {error}")))?
            .into_vec();
    }
    let search_paths = search_path_settings
        .to_search_paths(&system, &vendored, &FallibleStrategy)
        .map_err(|error| Error::analysis(format!("invalid search paths: {error}")))?;
    // Target the discovered environment's Python version: a venv's `pyvenv.cfg`
    // records the interpreter version that created it, and that grammar is what
    // the checker must accept. A system environment has no such metadata, so it
    // falls back to the 3.11 default the CLI would use.
    let python_version = environment
        .as_ref()
        .and_then(|env| env.python_version_from_metadata())
        .cloned()
        .unwrap_or_else(|| PythonVersionWithSource {
            version: PythonVersion::default(),
            source: PythonVersionSource::default(),
        });
    // Keep the discovered environment on the settings: ty's own `ProgramSettings`
    // carries `python_environment` so queries can reuse it instead of re-walking
    // discovery. The docs resolver reads it back for `real_stdlib_path`.
    let settings = Arc::new(ProgramSettings {
        python_version,
        python_platform: PythonPlatform::default(),
        search_paths,
        python_environment: Ok(environment),
    });

    settings_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, settings.clone());
    Ok(settings)
}

/// The environment discovery order. An explicit signal always wins without
/// ambiguity; only the ancestor walk (reached when nothing else pinned a
/// choice) can be ambiguous:
///
/// 1. an explicit `python` param (an interpreter or a venv root);
/// 2. the `VIRTUAL_ENV` env var ("the choice is already made");
/// 3. the `CONDA_PREFIX` env var, delegated to [`PythonEnvironment::discover`]
///    (conda is an explicit signal, but which env to use is conda's business);
/// 4. an ancestor walk upward from `src_root` for `.venv`/`venv` dirs holding a
///    `pyvenv.cfg`: none → system fallback; one → that venv; several → error.
fn discover_environment(
    src_root: &SystemPath,
    python: Option<&Path>,
    system: &TySystem,
) -> Result<Option<PythonEnvironment>, Error> {
    // (1) Explicit `python` wins. `PythonCliFlag` is the origin whose path
    // semantics match the param (it may name an interpreter *or* a
    // `sys.prefix` dir), unlike `VirtualEnvVar`/`LocalVenv`, which demand a
    // venv root and reject a bare interpreter.
    if let Some(python) = python {
        let python_sys = to_system_path(python).ok_or_else(|| {
            Error::invalid_input(format!(
                "python path is not representable: {}",
                python.display()
            ))
        })?;
        return PythonEnvironment::new(
            python_sys.as_path(),
            SysPrefixPathOrigin::PythonCliFlag,
            system,
        )
        .map(Some)
        .map_err(|error| Error::analysis(format!("environment discovery failed: {error}")));
    }

    // (2) `VIRTUAL_ENV` is the unambiguous "activated" signal.
    if let Ok(virtual_env) = system.env_var("VIRTUAL_ENV")
        && !virtual_env.is_empty()
    {
        let virtual_env = SystemPathBuf::from(virtual_env);
        return PythonEnvironment::new(&virtual_env, SysPrefixPathOrigin::VirtualEnvVar, system)
            .map(Some)
            .map_err(|error| Error::analysis(format!("environment discovery failed: {error}")));
    }

    // (3) `CONDA_PREFIX` is an explicit conda signal; `discover` decides between
    // the child (activated) env and the base env without an ancestor walk.
    if let Ok(conda_prefix) = system.env_var("CONDA_PREFIX")
        && !conda_prefix.is_empty()
    {
        return PythonEnvironment::discover(Some(src_root), system)
            .map_err(|error| Error::analysis(format!("environment discovery failed: {error}")));
    }

    // (4) Ancestor walk: the only step that can be ambiguous, and only because
    // no explicit signal was given.
    let venvs = ancestor_venvs(src_root, system);
    match venvs.as_slice() {
        [] => PythonEnvironment::discover(Some(src_root), system)
            .map_err(|error| Error::analysis(format!("environment discovery failed: {error}"))),
        [venv] => PythonEnvironment::new(venv, SysPrefixPathOrigin::LocalVenv, system)
            .map(Some)
            .map_err(|error| Error::analysis(format!("environment discovery failed: {error}"))),
        _ => {
            let paths = venvs
                .iter()
                .map(|path| path.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(Error::analysis(format!(
                "ambiguous Python environment: found {} virtual environments ({paths}); \
                 pass `python` to disambiguate or set VIRTUAL_ENV",
                venvs.len()
            )))
        }
    }
}

/// Walks `src_root`'s ancestor chain and collects every child named `.venv` or
/// `venv` that actually holds a `pyvenv.cfg`, a real virtual environment, not a
/// bare directory with a suggestive name.
fn ancestor_venvs(src_root: &SystemPath, system: &TySystem) -> Vec<SystemPathBuf> {
    let mut found = Vec::new();
    for ancestor in src_root.ancestors() {
        for name in [".venv", "venv"] {
            let candidate = ancestor.join(name);
            if system.is_file(&candidate.join("pyvenv.cfg")) {
                found.push(candidate);
            }
        }
    }
    found
}
