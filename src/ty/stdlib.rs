//! The real-stdlib layout of a CPython installation: where the standard
//! library's `.py` sources live for a given platform and version, and how
//! to find them behind a virtual environment. Split out of `db.rs` (the
//! database assembly) because this is installation archaeology, not
//! database wiring.

use ruff_db::system::{System, SystemPath, SystemPathBuf};
use ruff_python_ast::PythonVersion;
use ty_site_packages::PythonEnvironment;

/// The on-disk layout of a CPython stdlib, keyed by the host platform. A
/// parameter rather than a `cfg!` gate so the Linux test suite can exercise the
/// Windows `Lib` layout without a Windows runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StdlibLayout {
    /// The stdlib lives directly under `<prefix>/Lib`.
    Windows,
    /// The stdlib lives under `<prefix>/lib/python{major}.{minor}`.
    Unix,
}

impl StdlibLayout {
    /// The layout of the platform tpmcp is actually running on.
    pub(super) fn host() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// The real stdlib source directory for a CPython installation rooted at
/// `prefix`, laid out for the given platform and Python version.
pub(super) fn stdlib_source_dir(
    prefix: &SystemPath,
    layout: StdlibLayout,
    version: PythonVersion,
) -> SystemPathBuf {
    match layout {
        StdlibLayout::Windows => prefix.join("Lib"),
        StdlibLayout::Unix => prefix.join(format!("lib/python{version}")),
    }
}

/// The base interpreter's `sys.prefix` — the root that actually owns the
/// stdlib. For a system environment that is the environment's own prefix; for a
/// virtual environment it is the `home` value recorded in `pyvenv.cfg`, whose
/// meaning differs by layout: on Windows `home` is already the base prefix, on
/// Unix it is `<prefix>/bin`. `layout` drives that interpretation so the Linux
/// suite can exercise the Windows branch.
pub(super) fn base_interpreter_prefix(
    environment: &PythonEnvironment,
    system: &dyn System,
    layout: StdlibLayout,
) -> Option<SystemPathBuf> {
    let prefix = environment.sys_prefix().to_path_buf();
    if !environment.is_virtual() {
        return Some(prefix);
    }

    let pyvenv_cfg = prefix.join("pyvenv.cfg");
    let contents = system.read_to_string(&pyvenv_cfg).ok()?;
    let home = SystemPathBuf::from(pyvenv_home(&contents)?);
    match layout {
        StdlibLayout::Windows => Some(home),
        StdlibLayout::Unix => home.parent().map(SystemPath::to_path_buf),
    }
}

/// The `home` value from a `pyvenv.cfg` file, or `None` when it is absent or
/// blank.
fn pyvenv_home(contents: &str) -> Option<&str> {
    contents.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "home")
            .then(|| value.trim())
            .filter(|value| !value.is_empty())
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom

    use ruff_db::system::OsSystem;

    use super::super::db::to_system_path;
    use super::*;

    #[test]
    fn stdlib_source_dir_windows_uses_lib_layout() {
        let prefix = SystemPath::new("C:/hostedtoolcache/windows/Python/3.12.10/x64");
        let dir = stdlib_source_dir(prefix, StdlibLayout::Windows, PythonVersion::from((3, 12)));
        assert_eq!(dir, prefix.join("Lib"));
    }

    #[test]
    fn stdlib_source_dir_unix_uses_versioned_layout() {
        let prefix = SystemPath::new("/opt/python");
        let version = PythonVersion::from((3, 12));
        let dir = stdlib_source_dir(prefix, StdlibLayout::Unix, version);
        assert_eq!(dir, prefix.join(format!("lib/python{version}")));
    }

    #[test]
    fn pyvenv_home_parses_the_home_key() {
        // Unix `\n` line endings.
        assert_eq!(
            pyvenv_home("home = C:\\Python312\nversion_info = 3.12\n"),
            Some("C:\\Python312")
        );
        // Windows `\r\n` line endings trim to the same value.
        assert_eq!(
            pyvenv_home("home = C:\\Python312\r\nversion_info = 3.12\r\n"),
            Some("C:\\Python312")
        );
    }

    /// Scaffolds a base install and a venv whose `pyvenv.cfg` `home` uses
    /// `layout`'s convention, returning the temp root and the base/stdlib
    /// `SystemPathBuf`s. A real stdlib source file (`antigravity.py`) is laid
    /// out so the caller can assert the end-to-end fallback, not just the
    /// one-line `stdlib_source_dir`.
    fn scaffold_venv_for_layout(
        tag: &str,
        layout: StdlibLayout,
    ) -> (std::path::PathBuf, SystemPathBuf, SystemPathBuf) {
        let root =
            std::env::temp_dir().join(format!("tpmcp-db-stdlib-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        // The base interpreter owns the stdlib. Its `pyvenv.cfg` `home` is the
        // prefix itself on Windows and `<prefix>/bin` on Unix.
        let base = root.join("base");
        let stdlib = match layout {
            StdlibLayout::Windows => base.join("Lib"),
            StdlibLayout::Unix => base.join("lib/python3.12"),
        };
        let home = match layout {
            StdlibLayout::Windows => base.clone(),
            StdlibLayout::Unix => base.join("bin"),
        };
        std::fs::create_dir_all(&stdlib).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            stdlib.join("antigravity.py"),
            "def geohash():\n    '''compute geohash()'''\n",
        )
        .unwrap();

        let venv = root.join("venv");
        std::fs::create_dir_all(&venv).unwrap();
        std::fs::write(
            venv.join("pyvenv.cfg"),
            format!("home = {}\nversion_info = 3.12\n", home.display()),
        )
        .unwrap();

        let base = to_system_path(&base).unwrap();
        let stdlib = to_system_path(stdlib.as_path()).unwrap();
        (root, base, stdlib)
    }

    #[test]
    fn windows_layout_fallback_resolves_from_scaffolded_venv() {
        let (root, base, stdlib) = scaffold_venv_for_layout("windows", StdlibLayout::Windows);
        let system = OsSystem::new(root.to_str().unwrap());
        let venv = to_system_path(&root.join("venv")).unwrap();
        let environment = PythonEnvironment::new(
            venv.as_path(),
            ty_site_packages::SysPrefixPathOrigin::PythonCliFlag,
            &system,
        )
        .unwrap();
        assert!(environment.is_virtual());

        // `home` is already the base prefix on Windows; the derived prefix must
        // be it verbatim, and the stdlib must live directly under it as `Lib`.
        let base_prefix =
            base_interpreter_prefix(&environment, &system, StdlibLayout::Windows).unwrap();
        assert_eq!(base_prefix, base);
        let version = environment
            .python_version_from_metadata()
            .map(|with_source| with_source.version)
            .unwrap_or_default();
        let resolved = stdlib_source_dir(&base_prefix, StdlibLayout::Windows, version);
        assert_eq!(resolved, stdlib);
        assert!(system.is_directory(&resolved));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn unix_layout_fallback_resolves_from_scaffolded_venv() {
        let (root, base, stdlib) = scaffold_venv_for_layout("unix", StdlibLayout::Unix);
        let system = OsSystem::new(root.to_str().unwrap());
        let venv = to_system_path(&root.join("venv")).unwrap();
        let environment = PythonEnvironment::new(
            venv.as_path(),
            ty_site_packages::SysPrefixPathOrigin::PythonCliFlag,
            &system,
        )
        .unwrap();
        assert!(environment.is_virtual());

        // `home` is `<prefix>/bin` on Unix; the derived prefix is its parent,
        // and the stdlib lives under it as `lib/pythonX.Y`.
        let base_prefix =
            base_interpreter_prefix(&environment, &system, StdlibLayout::Unix).unwrap();
        assert_eq!(base_prefix, base);
        let version = environment
            .python_version_from_metadata()
            .map(|with_source| with_source.version)
            .unwrap_or_default();
        let resolved = stdlib_source_dir(&base_prefix, StdlibLayout::Unix, version);
        assert_eq!(resolved, stdlib);
        assert!(system.is_directory(&resolved));

        std::fs::remove_dir_all(&root).unwrap();
    }
}
