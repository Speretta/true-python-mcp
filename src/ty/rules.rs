//! ty rule configuration: `ty.toml` `[rules]` and `pyproject.toml`
//! `[tool.ty.rules]`.
//!
//! The ty half of what [`crate::ruff::settings`] does for ruff: `ty_check`
//! honors per-rule severities from the project's own configuration. Every
//! rule below was pinned against the real `ty` binary (0.0.19, the version of
//! the embedded engine) by probing it directly:
//!
//! - the config is the nearest directory — walking up from the anchor — that
//!   holds a `ty.toml` or a `pyproject.toml` with a `[tool.ty]` table. A
//!   `pyproject.toml` without `[tool.ty]` does not stop the walk, but one
//!   that fails to parse is a hard failure at every step, exactly like
//!   ty's "is not a valid `pyproject.toml`" error;
//! - in the same directory `ty.toml` wins over `[tool.ty]` (ty prints a
//!   warning; tpmcp carries it as a note);
//! - with no project config, the user-level `$XDG_CONFIG_HOME/ty/ty.toml`
//!   (`~/.config/ty/ty.toml`) applies; a project config replaces it whole,
//!   never merged;
//! - `[rules]` maps rule names to `"ignore" | "warn" | "error"`; any other
//!   value (or shape) is a hard failure naming the file;
//! - a rule name ty does not know is ignored without failing (ty does the
//!   same); tpmcp adds a note instead of swallowing it.
//!
//! Two deliberate divergences from the CLI, both disclosed rather than
//! hidden. `ty` discovers its config from its **cwd** — it will even apply
//! one from an unrelated tree — while tpmcp anchors the walk at the checked
//! file's directory, which is what a server serving arbitrary paths must do.
//! And sections other than the rules one (`environment`, `src`, ...) are
//! ignored with a note: tpmcp discovers the environment through its own
//! `python` parameter and `src_root`, not through ty's config.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use ruff_db::diagnostic::Severity;
use ty_python_semantic::default_lint_registry;
use ty_python_semantic::lint::{GetLintError, LintSource, RuleSelection};

use crate::error::Error;

/// One resolved rule configuration: the selection the database queries with,
/// where it came from, and what the config asked for that was not applied.
#[derive(Debug)]
pub(crate) struct RuleConfig {
    pub(crate) selection: RuleSelection,
    /// The config file in effect, or `None` for ty's built-in defaults.
    pub(crate) source: Option<PathBuf>,
    /// What the config asked for that this server answers differently (or
    /// that ty swallows): surfaced by `ty_check` as notes, never silent.
    pub(crate) notes: Vec<String>,
}

/// Which flavor of config file the walk found.
#[derive(Copy, Clone)]
enum Kind {
    TyToml,
    Pyproject,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::TyToml => "ty.toml",
            Self::Pyproject => "pyproject.toml",
        }
    }
}

/// The rule configuration in effect for `anchor`'s tree, with ty's built-in
/// defaults as the floor.
pub(crate) fn resolve(anchor: &Path) -> Result<RuleConfig, Error> {
    let Some((path, kind, mut notes)) = find(anchor)? else {
        return Ok(RuleConfig {
            selection: RuleSelection::from_registry(default_lint_registry()),
            source: None,
            notes: Vec::new(),
        });
    };
    let label = kind.label();
    let table = parse_table(&path, label)?;
    let mut selection = RuleSelection::from_registry(default_lint_registry());

    // The rules table lives at the root of a `ty.toml` and under `[tool.ty]`
    // of a `pyproject.toml`; everything else the file asked for is named and
    // ignored, because only the environment knobs could matter and tpmcp has
    // its own (`python`).
    let owner = match kind {
        Kind::TyToml => Some(&table),
        Kind::Pyproject => tool_ty_table(&table, &path)?,
    };
    let Some(owner) = owner else {
        // Only reachable if the file changed between the walk and this parse;
        // the defaults above are then the honest answer.
        return Ok(RuleConfig {
            selection,
            source: Some(path),
            notes,
        });
    };
    for key in owner.keys() {
        if key != "rules" {
            notes.push(format!(
                "`{key}` is not honored by tpmcp (only the rules section is); ignored"
            ));
        }
    }
    if let Some(rules) = owner.get("rules") {
        let rules = rules.as_table().ok_or_else(|| {
            Error::invalid_input(format!("{}: [rules] must be a table", path.display()))
        })?;
        for (name, value) in rules {
            // The level is validated before the name, mirroring ty: its TOML
            // value deserializes first, so a bad level fails even when the
            // rule name is unknown.
            let level = value
                .as_str()
                .filter(|level| matches!(*level, "ignore" | "warn" | "error"));
            let Some(level) = level else {
                return Err(Error::invalid_input(format!(
                    "{}: rule `{name}` has level {value:?}, expected one of `ignore`, `warn`, `error`",
                    path.display()
                )));
            };
            match default_lint_registry().get(name) {
                Ok(lint) => match level {
                    "ignore" => selection.disable(lint),
                    "warn" => selection.enable(lint, Severity::Warning, LintSource::File),
                    _ => selection.enable(lint, Severity::Error, LintSource::File),
                },
                Err(error) => notes.push(unusable_rule(name, &error)),
            }
        }
    }
    Ok(RuleConfig {
        selection,
        source: Some(path),
        notes,
    })
}

/// A cheap fingerprint of the config in effect for `anchor` (`0` without one,
/// `u64::MAX` while it is broken), folded into
/// [`crate::ty::cache::CacheKey`] so editing the config builds a fresh
/// database instead of serving the old severities.
pub(crate) fn token(anchor: &Path) -> u64 {
    match find(anchor) {
        Ok(Some((path, _, _))) => fingerprint(&path),
        Ok(None) => 0,
        Err(_) => u64::MAX,
    }
}

fn fingerprint(path: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    if let Ok(meta) = std::fs::metadata(path) {
        let modified = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_nanos());
        meta.len().hash(&mut hasher);
        modified.hash(&mut hasher);
    }
    hasher.finish()
}

/// Walks up from `anchor` for the nearest config marker, with the user-level
/// config as the fallback; the semantics are the ones pinned against the `ty`
/// binary in the module docs.
fn find(anchor: &Path) -> Result<Option<(PathBuf, Kind, Vec<String>)>, Error> {
    let mut notes = Vec::new();
    for dir in anchor.ancestors() {
        let pyproject = dir.join("pyproject.toml");
        // ty parses every `pyproject.toml` it walks past — even one without
        // `[tool.ty]`, even when `ty.toml` in the same directory wins — so a
        // syntax error stops the walk just like ty's failure does.
        let mut has_tool_ty = false;
        if pyproject.is_file() {
            let table = parse_table(&pyproject, "pyproject.toml")?;
            has_tool_ty = tool_ty_table(&table, &pyproject)?.is_some();
        }
        let ty_toml = dir.join("ty.toml");
        if ty_toml.is_file() {
            if has_tool_ty {
                notes.push(
                    "ty.toml takes precedence over the pyproject.toml [tool.ty] section here; \
                     the [tool.ty] section is ignored"
                        .to_string(),
                );
            }
            return Ok(Some((ty_toml, Kind::TyToml, notes)));
        }
        if has_tool_ty {
            return Ok(Some((pyproject, Kind::Pyproject, notes)));
        }
    }
    Ok(user_config().map(|path| (path, Kind::TyToml, notes)))
}

/// The user-level config: `$XDG_CONFIG_HOME/ty/ty.toml`, else
/// `~/.config/ty/ty.toml` (both verified against the ty binary).
fn user_config() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    let path = base.join("ty").join("ty.toml");
    path.is_file().then_some(path)
}

/// The `[tool.ty]` table of a `pyproject.toml`, if the file has one. A
/// `tool.ty` key of a non-table type is rejected the way ty's config
/// deserializer would (inferred from its schema; not probed).
fn tool_ty_table<'a>(
    table: &'a toml::Table,
    path: &Path,
) -> Result<Option<&'a toml::Table>, Error> {
    let Some(tool) = table.get("tool").and_then(toml::Value::as_table) else {
        return Ok(None);
    };
    match tool.get("ty") {
        None => Ok(None),
        Some(value) => value.as_table().map(Some).ok_or_else(|| {
            Error::invalid_input(format!("{}: tool.ty must be a table", path.display()))
        }),
    }
}

fn parse_table(path: &Path, label: &str) -> Result<toml::Table, Error> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| Error::internal(format!("cannot read {}: {error}", path.display())))?;
    toml::from_str(&content).map_err(|error| {
        Error::invalid_input(format!(
            "{} is not a valid `{label}`: {error}",
            path.display()
        ))
    })
}

/// The three ways a config can name a rule ty will not apply. ty ignores all
/// of them silently; tpmcp says so.
fn unusable_rule(name: &str, error: &GetLintError) -> String {
    match error {
        GetLintError::Removed(_) => format!("rule `{name}` was removed from ty; ignored"),
        GetLintError::Unknown { suggestion, .. } => match suggestion {
            Some(suggestion) => format!(
                "rule `{name}` is not a known ty rule (did you mean `{suggestion}`?); ignored"
            ),
            None => format!("rule `{name}` is not a known ty rule; ignored"),
        },
        GetLintError::PrefixedWithCategory { suggestion, .. } => format!(
            "rule `{name}` should be written `{suggestion}` without the `lint:` prefix; ignored"
        ),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom

    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tpmcp-rules-tests-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn level_of(config: &RuleConfig, rule: &str) -> Option<Severity> {
        config
            .selection
            .iter()
            .find(|(lint, _)| lint.name().as_str() == rule)
            .map(|(_, severity)| severity)
    }

    #[test]
    fn ty_toml_levels_apply() {
        let dir = dir("levels");
        write(&dir, "ty.toml", "[rules]\nunresolved-import = \"ignore\"\n");
        let config = resolve(&dir).unwrap();
        assert_eq!(level_of(&config, "unresolved-import"), None);
        assert_eq!(
            config.source.as_deref(),
            Some(dir.join("ty.toml").as_path()),
            "the config file must be disclosed"
        );
    }

    #[test]
    fn warn_and_error_map_to_severities() {
        let dir = dir("severities");
        write(&dir, "ty.toml", "[rules]\nunresolved-import = \"warn\"\n");
        let config = resolve(&dir).unwrap();
        assert_eq!(
            level_of(&config, "unresolved-import"),
            Some(Severity::Warning)
        );

        write(&dir, "ty.toml", "[rules]\nunresolved-import = \"error\"\n");
        let config = resolve(&dir).unwrap();
        assert_eq!(
            level_of(&config, "unresolved-import"),
            Some(Severity::Error)
        );
    }

    #[test]
    fn pyproject_tool_ty_rules_apply() {
        let dir = dir("pyproject");
        write(
            &dir,
            "pyproject.toml",
            "[project]\nname = \"x\"\n[tool.ty.rules]\nunresolved-import = \"ignore\"\n",
        );
        let config = resolve(&dir).unwrap();
        assert_eq!(level_of(&config, "unresolved-import"), None);
        assert_eq!(
            config.source.as_deref(),
            Some(dir.join("pyproject.toml").as_path())
        );
    }

    #[test]
    fn an_unhonored_section_is_named_not_swallowed() {
        let dir = dir("unhonored");
        write(
            &dir,
            "ty.toml",
            "[environment]\npython-version = \"3.12\"\n[rules]\nunresolved-import = \"warn\"\n",
        );
        let config = resolve(&dir).unwrap();
        assert_eq!(config.notes.len(), 1);
        assert!(
            config.notes[0].contains("`environment`"),
            "the ignored section must be named: {:?}",
            config.notes
        );
    }

    #[test]
    fn ty_toml_beats_pyproject_and_says_so() {
        let dir = dir("precedence");
        write(&dir, "ty.toml", "[rules]\nunresolved-import = \"ignore\"\n");
        write(
            &dir,
            "pyproject.toml",
            "[tool.ty.rules]\nunresolved-import = \"error\"\n",
        );
        let config = resolve(&dir).unwrap();
        assert_eq!(level_of(&config, "unresolved-import"), None);
        assert!(
            config
                .notes
                .iter()
                .any(|note| note.contains("takes precedence")),
            "precedence must be disclosed: {:?}",
            config.notes
        );
    }

    #[test]
    fn unknown_rules_are_noted_with_a_suggestion() {
        let dir = dir("unknown");
        write(&dir, "ty.toml", "[rules]\nunresolved-improt = \"ignore\"\n");
        let config = resolve(&dir).unwrap();
        assert!(
            config
                .notes
                .iter()
                .any(|note| note.contains("did you mean `unresolved-import`")),
            "a typo must come back with a suggestion: {:?}",
            config.notes
        );
    }

    #[test]
    fn a_bad_level_fails_naming_the_file() {
        let dir = dir("badlevel");
        write(&dir, "ty.toml", "[rules]\nunresolved-import = \"banana\"\n");
        let error = resolve(&dir).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("ty.toml"), "{message}");
        assert!(
            message.contains("expected one of `ignore`, `warn`, `error`"),
            "{message}"
        );
    }

    #[test]
    fn a_broken_pyproject_fails_the_walk_even_without_tool_ty() {
        let dir = dir("broken");
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        write(&dir, "inner/pyproject.toml", "[project\n");
        write(&dir, "ty.toml", "[rules]\nunresolved-import = \"ignore\"\n");
        let error = resolve(&dir.join("inner")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("is not a valid `pyproject.toml`")
        );
    }

    #[test]
    fn a_pyproject_without_tool_ty_does_not_stop_the_walk() {
        let dir = dir("walk");
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        write(&dir, "inner/pyproject.toml", "[project]\nname = \"x\"\n");
        write(&dir, "ty.toml", "[rules]\nunresolved-import = \"ignore\"\n");
        let config = resolve(&dir.join("inner")).unwrap();
        assert_eq!(level_of(&config, "unresolved-import"), None);
    }

    #[test]
    fn the_token_tracks_the_config_file() {
        let dir = dir("token");
        assert_eq!(token(&dir), 0, "no config means no token");
        write(&dir, "ty.toml", "[rules]\nunresolved-import = \"ignore\"\n");
        let first = token(&dir);
        assert_ne!(first, 0);
        assert_eq!(token(&dir), first, "unchanged file, unchanged token");
        write(&dir, "ty.toml", "[rules]\nunresolved-import = \"warn\"\n");
        assert_ne!(token(&dir), first, "an edited config must move the token");
    }
}
