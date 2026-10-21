use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use ruff_db::system::{OsSystem, System, SystemPath, SystemPathBuf};
use ruff_python_ast::PythonVersion;
use ty_module_resolver::{FallibleStrategy, SearchPathSettings};
use ty_python_core::platform::PythonPlatform;
use ty_python_core::program::ProgramSettings;
use ty_python_semantic::{PythonVersionSource, PythonVersionWithSource};
use ty_site_packages::{PythonEnvironment, SitePackagesPaths, SysPrefixPathOrigin};

use crate::error::Error;

use super::db::{TySystem, to_system_path};

// ---- discovery + settings cache ----

/// Cache key: the checked file's source root plus an explicit `python` override
/// (when one was given).
type SettingsCacheKey = (PathBuf, Option<PathBuf>);

/// Process-wide cache of the discovery result, keyed by [`SettingsCacheKey`].
type SettingsCache = RwLock<HashMap<SettingsCacheKey, Arc<ProgramSettings>>>;

/// Process-wide cache of the discovery result, keyed by the checked file's
/// parent directory (the source root) and the explicit `python` override (if
/// any). Discovery — the environment lookup, the site-packages walk, and
/// search-path resolution — is the expensive half of [`TyDb::new`]. Its product
/// is plain `Send + Sync` data, so it is safe to share; the database itself is
/// *not* (salsa's `Storage` is per-thread state and must never be shared across
/// checks). The `python` path participates in the key so an explicit-python
/// check and an implicit (ancestor-walk) check of the same directory never
/// collide on one entry.
fn settings_cache() -> &'static SettingsCache {
    static CACHE: OnceLock<SettingsCache> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Resolve the [`ProgramSettings`] for a file rooted at `src_root`, optionally
/// pinning the type-checking environment with `python`, caching the result per
/// (source root, python) pair so repeat checks of files in the same directory
/// skip the filesystem walk. The cache holds settings only — never the salsa
/// database — so a stale entry is harmless: each check still builds a fresh
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
        .read()
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
    // Resolve the environment's site-packages once: they feed both the search
    // paths and the version measurement below.
    let site_packages = match &environment {
        Some(environment) => Some(environment.site_packages_paths(&system).map_err(|error| {
            Error::analysis(format!("site-packages discovery failed: {error}"))
        })?),
        None => None,
    };
    // Target the discovered environment's Python version: a venv's `pyvenv.cfg`
    // records the interpreter version that created it, and the site-packages
    // layout (`<prefix>/lib/pythonX.Y/site-packages`) names it for a system
    // environment. Only when neither is measurable do we fall back — and that
    // fallback is disclosed, never a silent 3.11 (see [`PythonTarget`]).
    let python_version = resolve_python_version(&environment, &site_packages);
    if let Some(site_packages) = site_packages {
        search_path_settings.site_packages_paths = site_packages.into_vec();
    }
    let search_paths = search_path_settings
        .to_search_paths(&system, &vendored, &FallibleStrategy)
        .map_err(|error| Error::analysis(format!("invalid search paths: {error}")))?;
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
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, settings.clone());
    Ok(settings)
}

/// The Python version and environment a check or docs resolution actually
/// targeted, disclosed so the caller never gets a silent static assumption.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PythonTarget {
    /// Rendered `major.minor`, e.g. "3.11".
    pub python_version: String,
    /// Where the version was measured: `pyvenv_cfg`, `site_packages_layout`,
    /// `config_file`, `script_metadata`, `cli`, `editor`, or `unknown` when it
    /// fell back to the default without a measurement.
    pub version_source: String,
    /// The discovered environment's `sys.prefix`, when one was found.
    pub environment: Option<String>,
    /// An honest note when the version was not measured (default fallback).
    pub note: Option<String>,
}

/// Discloses the Python target for `src_root` (a directory) with the same
/// discovery order `ty_check` uses. Discovery is the cached half of
/// [`TyDb::new`], so calling this after a check is effectively free.
pub fn version_info(src_root: &Path, python: Option<&Path>) -> Result<PythonTarget, Error> {
    let src_root_sys = to_system_path(src_root).ok_or_else(|| {
        Error::invalid_input(format!(
            "source root is not representable: {}",
            src_root.display()
        ))
    })?;
    let settings = discover_settings(src_root_sys.as_path(), python)?;
    Ok(python_target_from_settings(&settings))
}

/// Reads the disclosure fields back off a resolved [`ProgramSettings`].
fn python_target_from_settings(settings: &ProgramSettings) -> PythonTarget {
    let version = settings.python_version.version;
    let version_source = version_source_name(&settings.python_version.source);
    let environment = settings
        .python_environment
        .as_ref()
        .ok()
        .and_then(|environment| environment.as_ref())
        .map(|environment| environment.sys_prefix().as_str().to_string());
    let note = (version_source == "unknown").then(|| {
        format!("targeting Python {version} (default fallback; interpreter version not measured)")
    });
    PythonTarget {
        python_version: version.to_string(),
        version_source: version_source.to_string(),
        environment,
        note,
    }
}

/// Measures the Python version from the discovered environment, most
/// trustworthy first: the `pyvenv.cfg` metadata (a venv records the interpreter
/// that made it), then the site-packages layout (`<prefix>/lib/pythonX.Y/
/// site-packages`, which names the version for a system environment on Unix).
/// Only when neither is measurable does this fall back to the 3.11 default with
/// `source: Default`, which [`python_target_from_settings`] discloses as
/// `unknown` rather than passing it off as measured.
fn resolve_python_version(
    environment: &Option<PythonEnvironment>,
    site_packages: &Option<SitePackagesPaths>,
) -> PythonVersionWithSource {
    if let Some(version) = environment
        .as_ref()
        .and_then(PythonEnvironment::python_version_from_metadata)
    {
        return version.clone();
    }
    if let Some(version) = site_packages
        .as_ref()
        .and_then(SitePackagesPaths::python_version_from_layout)
    {
        return version;
    }
    PythonVersionWithSource {
        version: PythonVersion::default(),
        source: PythonVersionSource::default(),
    }
}

/// The [`PythonVersionSource`] rendered for disclosure.
/// [`PythonVersionSource::Default`] is reported as `unknown` — it means the
/// version was never measured, not that it is known to be the default.
fn version_source_name(source: &PythonVersionSource) -> &'static str {
    match source {
        PythonVersionSource::ConfigFile(_) => "config_file",
        PythonVersionSource::ScriptMetadata(_) => "script_metadata",
        PythonVersionSource::PyvenvCfgFile(_) => "pyvenv_cfg",
        PythonVersionSource::InstallationDirectoryLayout { .. } => "site_packages_layout",
        PythonVersionSource::Cli => "cli",
        PythonVersionSource::Editor => "editor",
        PythonVersionSource::Default => "unknown",
    }
}

/// The environment discovery order. An explicit signal always wins without
/// ambiguity; only the ancestor walk — reached when nothing else pinned a
/// choice — can be ambiguous:
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
    // semantics match the param — it may name an interpreter *or* a
    // `sys.prefix` dir — unlike `VirtualEnvVar`/`LocalVenv`, which demand a
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
/// `venv` that actually holds a `pyvenv.cfg` — a real virtual environment, not a
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
