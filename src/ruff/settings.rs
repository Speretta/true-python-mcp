//! Resolution and process-wide caching of ruff [`Settings`].
//!
//! Split out of `ruff.rs` so the cache machinery stays isolated behind the
//! same quarantine doctrine: this is the only place that builds ruff
//! [`Settings`]. Resolution follows ruff's own precedence: the nearest project
//! config (`ruff.toml`, `.ruff.toml`, or a `pyproject.toml` carrying a
//! `[tool.ruff]` table) wins outright; the global user config is only a
//! fallback when no project config exists; ruff's [`Settings::default()`] comes
//! last. ruff never merges config files: the "closest" file is authoritative.
//! Neither do we.
//!
//! The cache is keyed by the resolved config-file path and validated with a
//! single stat (mtime + length) per call, so an edited config is picked up on
//! the next call while a stable config is parsed and built exactly once. The
//! no-config fallback (`Settings::default()`) lives behind its own `OnceLock`.
//!
//! Panic-free: `RwLock` poisoning is recovered with `PoisonError::into_inner`
//! (same policy as `src/fs.rs`). The map is only ever mutated by a single
//! `insert` under the write lock. That operation cannot leave it
//! inconsistent, so a poisoned lock still guards a valid map and recovery is
//! safe. A config we cannot stat for mtime simply falls back to a fresh,
//! uncached build rather than risking a stale entry.
//!
//! Environment variables are deliberately not read here. The real `RUFF_*`
//! variables are `RUFF_OUTPUT_FORMAT`, `RUFF_OUTPUT_FILE`, `RUFF_NO_CACHE`,
//! `RUFF_CACHE_DIR`, and `FORCE_COLOR`/`NO_COLOR` — and none of them apply to
//! tpmcp: it renders diagnostics itself (so the CLI's output format and output
//! file are irrelevant), it keeps no on-disk ruff cache (so there is no cache
//! to disable or relocate), and it emits no ANSI (so there is no color to force
//! or suppress). The `RUFF_CONFIG`, `RUFF_LINE_LENGTH` and `RUFF_TARGET_VERSION`
//! names that *would* matter for lint behavior do not exist as environment
//! variables in ruff — those knobs live in config files only, and this module
//! already resolves the config-file tiers.

use std::collections::HashMap;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::SystemTime;

use ruff_workspace::Settings;
use ruff_workspace::configuration::Configuration;
use ruff_workspace::pyproject::find_user_settings_toml;
use ruff_workspace::resolver::{
    ConfigurationOrigin, ConfigurationTransformer, resolve_root_settings,
};

use crate::error::Error;

// ---- cache ----

/// A cached build plus the `(mtime, length)` token that validates it.
type CacheEntry = (SystemTime, u64, Arc<Settings>);

fn cache() -> &'static RwLock<HashMap<PathBuf, CacheEntry>> {
    static CACHE: OnceLock<RwLock<HashMap<PathBuf, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Resolve the effective settings for `path` following ruff's precedence: the
/// nearest project config wins outright, the global user config is the fallback
/// when no project config exists, and ruff's defaults come last. The expensive
/// build (TOML parse + rule-table expansion + per-file-ignores compilation) is
/// cached per resolved config file and reused while that file is unchanged.
pub fn resolve_settings(path: &Path) -> Result<Arc<Settings>, Error> {
    let project = find_config(path);
    // The user config is only consulted when no project config exists: ruff
    // never merges, the "closest" file is authoritative.
    let user = if project.is_none() {
        find_user_settings_toml()
    } else {
        None
    };

    // The resolved config path doubles as the cache key; its origin (ancestor
    // for project, cwd for user) is applied inside `build_settings`.
    let Some(config) = project
        .as_ref()
        .map(|(path, _)| path.as_path())
        .or(user.as_deref())
    else {
        return Ok(default_settings());
    };

    let meta = project.as_ref().map(|(_, meta)| meta);
    let token = stat_token(config, meta);
    if let Some((mtime, len)) = token
        && let Some(settings) = lookup(config, mtime, len)
    {
        return Ok(settings);
    }

    let settings = Arc::new(build_settings(
        project.as_ref().map(|(path, _)| path.as_path()),
        user.as_deref(),
    )?);
    if let Some((mtime, len)) = token {
        insert(config.to_path_buf(), mtime, len, settings.clone());
    }
    Ok(settings)
}

/// The one-time default settings, used when no config exists anywhere.
fn default_settings() -> Arc<Settings> {
    static DEFAULT: OnceLock<Arc<Settings>> = OnceLock::new();
    DEFAULT
        .get_or_init(|| Arc::new(Settings::default()))
        .clone()
}

fn lookup(config_path: &Path, mtime: SystemTime, len: u64) -> Option<Arc<Settings>> {
    let cache = cache()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entry = cache.get(config_path)?;
    (entry.0 == mtime && entry.1 == len).then(|| entry.2.clone())
}

fn insert(config_path: PathBuf, mtime: SystemTime, len: u64, settings: Arc<Settings>) {
    let mut cache = cache()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.insert(config_path, (mtime, len, settings));
}

// ---- resolution ----

/// The precedence rule in isolation: the project config (when present) is
/// authoritative and the user config is ignored; the user config applies only
/// when no project config exists. ruff never merges config files. Extracted so
/// the tests can exercise the rule with explicit paths, without touching
/// `~/.config` or environment variables.
fn build_settings(project: Option<&Path>, user: Option<&Path>) -> Result<Settings, Error> {
    match (project, user) {
        (Some(project), _) => root_settings(project, ConfigurationOrigin::Ancestor),
        (None, Some(user)) => root_settings(user, ConfigurationOrigin::UserSettings),
        (None, None) => Ok(Settings::default()),
    }
}

fn root_settings(config_path: &Path, origin: ConfigurationOrigin) -> Result<Settings, Error> {
    resolve_root_settings(config_path, &NoOpTransformer, origin)
        .map_err(|error| config_error(config_path, error))
}

fn config_error(path: &Path, error: anyhow::Error) -> Error {
    Error::analysis(format!("invalid ruff config {}: {error}", path.display()))
}

// ---- config discovery ----

/// Walk up from `path` to the nearest ruff config. Returns the config path
/// together with the single stat we already performed on it, so cache
/// validation costs no extra syscall.
fn find_config(path: &Path) -> Option<(PathBuf, Metadata)> {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let start = absolute.parent().unwrap_or(&absolute);
    for dir in start.ancestors() {
        for name in ["ruff.toml", ".ruff.toml"] {
            let candidate = dir.join(name);
            if let Some(meta) = file_meta(&candidate) {
                return Some((candidate, meta));
            }
        }
        let pyproject = dir.join("pyproject.toml");
        if let Some(meta) = file_meta(&pyproject)
            && pyproject_has_ruff(&pyproject)
        {
            return Some((pyproject, meta));
        }
    }
    None
}

/// The `(mtime, length)` cache token for a present config file, or `None` when
/// it cannot be stat'ed. `meta` is a stat the discoverer already performed (so
/// a project config costs no extra syscall); `None` triggers a fresh stat.
fn stat_token(path: &Path, meta: Option<&Metadata>) -> Option<(SystemTime, u64)> {
    let meta = meta.cloned().or_else(|| file_meta(path))?;
    let mtime = meta.modified().ok()?;
    Some((mtime, meta.len()))
}

/// `metadata` for `path` when it is an existing regular file, else `None`.
fn file_meta(path: &Path) -> Option<Metadata> {
    std::fs::metadata(path).ok().filter(|meta| meta.is_file())
}

/// Cheap check for a `[tool.ruff` table so unrelated pyproject.toml files do
/// not stop the upward search.
fn pyproject_has_ruff(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|text| text.contains("[tool.ruff"))
        .unwrap_or(false)
}

struct NoOpTransformer;

impl ConfigurationTransformer for NoOpTransformer {
    fn transform(&self, config: Configuration) -> Configuration {
        config
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod tests {
    use super::*;
    use ruff_linter::registry::Rule;
    use std::fs;

    /// A unique, cleaned scratch dir for one test (unit tests have no access to
    /// `tests/common::temp_dir`).
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tpmcp-settings-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// ruff's precedence is "closest wins", not a merge: a project config makes
    /// the user config irrelevant, so the user's `select = ["E501"]` is ignored
    /// (E501 stays off) while a default rule (F401) stays on.
    #[test]
    fn project_config_shadows_user() {
        let dir = scratch("shadow");
        let project = dir.join("project.toml");
        fs::write(&project, "line-length = 120\n").unwrap();
        let user = dir.join("user.toml");
        fs::write(&user, "line-length = 100\n[lint]\nselect = [\"E501\"]\n").unwrap();

        let settings = build_settings(Some(&project), Some(&user)).unwrap();
        assert_eq!(settings.linter.line_length.value(), 120);
        assert!(!settings.linter.rules.enabled(Rule::LineTooLong));
        assert!(settings.linter.rules.enabled(Rule::UnusedImport));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A global-only config is honored: its `line-length` and `select` apply
    /// (and a rule it did not select stays off).
    #[test]
    fn user_only_is_honored() {
        let dir = scratch("user-only");
        let user = dir.join("user.toml");
        fs::write(&user, "line-length = 100\n[lint]\nselect = [\"E501\"]\n").unwrap();

        let settings = build_settings(None, Some(&user)).unwrap();
        assert_eq!(settings.linter.line_length.value(), 100);
        assert!(settings.linter.rules.enabled(Rule::LineTooLong));
        assert!(!settings.linter.rules.enabled(Rule::UnusedImport));
        fs::remove_dir_all(&dir).unwrap();
    }
}
