//! Resolution and process-wide caching of ruff [`Settings`].
//!
//! Split out of `ruff.rs` so the cache machinery stays isolated behind the
//! same quarantine doctrine: this is the only place that builds ruff
//! [`Settings`]. The cache is keyed by the resolved config-file path and
//! validated with a single stat (mtime + length) per call, so an edited
//! config is picked up on the next call while a stable config is parsed and
//! built exactly once. The no-config fallback (`Settings::default()`) lives
//! behind its own `OnceLock`.
//!
//! Panic-free: `RwLock` poisoning is recovered with `PoisonError::into_inner`
//! (same policy as `src/fs.rs`). The map is only ever mutated by a single
//! `insert` under the write lock — an operation that cannot leave it
//! inconsistent — so a poisoned lock still guards a valid map and recovery is
//! safe. A config we cannot stat for mtime simply falls back to a fresh,
//! uncached build rather than risking a stale entry.

use std::collections::HashMap;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::SystemTime;

use ruff_workspace::Settings;
use ruff_workspace::configuration::Configuration;
use ruff_workspace::resolver::{
    ConfigurationOrigin, ConfigurationTransformer, resolve_root_settings,
};

use crate::error::Error;

/// A cached build plus the `(mtime, length)` token that validates it.
type CacheEntry = (SystemTime, u64, Arc<Settings>);

fn cache() -> &'static RwLock<HashMap<PathBuf, CacheEntry>> {
    static CACHE: OnceLock<RwLock<HashMap<PathBuf, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Resolve the effective settings for `path` by walking up to the nearest ruff
/// config (`ruff.toml`, `.ruff.toml`, or a `pyproject.toml` carrying a
/// `[tool.ruff]` table), falling back to ruff defaults. The expensive build
/// (TOML parse + rule-table expansion + per-file-ignores compilation) is
/// cached per config file and reused while that file is unchanged.
pub fn resolve_settings(path: &Path) -> Result<Arc<Settings>, Error> {
    let Some((config_path, meta)) = find_config(path) else {
        return Ok(default_settings());
    };
    let len = meta.len();
    // mtime + length are the validity token. If mtime is unavailable, fall
    // back to a fresh (uncached) build rather than cache against a bad key.
    let Ok(mtime) = meta.modified() else {
        return root_settings(&config_path).map(Arc::new);
    };
    if let Some(settings) = lookup(&config_path, mtime, len) {
        return Ok(settings);
    }
    let settings = Arc::new(root_settings(&config_path)?);
    insert(config_path, mtime, len, settings.clone());
    Ok(settings)
}

/// The one-time default settings, used when no config exists in the ancestry.
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

/// `metadata` for `path` when it is an existing regular file, else `None`.
fn file_meta(path: &Path) -> Option<Metadata> {
    std::fs::metadata(path).ok().filter(|meta| meta.is_file())
}

fn root_settings(config_path: &Path) -> Result<Settings, Error> {
    resolve_root_settings(config_path, &NoOpTransformer, ConfigurationOrigin::Ancestor).map_err(
        |error| {
            Error::analysis(format!(
                "invalid ruff config {}: {error}",
                config_path.display()
            ))
        },
    )
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
