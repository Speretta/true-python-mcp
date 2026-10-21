use std::path::Path;
use std::sync::Arc;

use ruff_db::Db as SourceDb;
use ruff_db::diagnostic::Diagnostic as TyDiagnostic;
use ruff_db::files::{File, Files};
use ruff_db::system::walk_directory::WalkDirectoryBuilder;
use ruff_db::system::{
    DirectoryEntry, Metadata, OsSystem, System, SystemPath, SystemPathBuf, SystemVirtualPath,
    SystemVirtualPathBuf, WhichResult, WritableSystem,
};
use ruff_db::vendored::VendoredFileSystem;
use ruff_notebook::{Notebook, NotebookError};
use ruff_python_ast::PythonVersion;
use ty_module_resolver::{Db as ModuleResolverDb, ModuleName, resolve_module_confident};
use ty_python_core::Db as PythonCoreDb;
use ty_python_core::ProgramFile;
use ty_python_core::program::{Program, ProgramSettings};
use ty_python_semantic::dependency::DependencyMetadata;
use ty_python_semantic::lint::{LintRegistry, RuleSelection};
use ty_python_semantic::{
    AnalysisSettings, Db as SemanticDb, PythonVersionWithSource, check_file_unwrap,
    default_lint_registry,
};
use ty_site_packages::PythonEnvironment;

use crate::error::Error;

use super::settings::discover_settings;

/// The virtual path of a notebook's concatenated cell source. A plain `.py`
/// name keeps the source type `Python` (a `.ipynb` virtual path would be
/// re-read as a notebook), and it need not live under a search root: absolute
/// imports already resolve against the notebook's parent directory, which is
/// the search root.
pub(super) const NOTEBOOK_VIRTUAL_PATH: &str = "__tpmcp_ty_notebook__.py";

/// The virtual path of the synthetic module [`crate::docs::resolve_symbol`]
/// resolves a dotted symbol through. Like [`NOTEBOOK_VIRTUAL_PATH`], a plain
/// `.py` name keeps the source type `Python` and need not live under a search
/// root: absolute imports already resolve against the configured search paths.
pub(crate) const DOCS_VIRTUAL_PATH: &str = "__tpmcp_docs__.py";

/// One in-memory `.py` source the database serves through its [`System`],
/// standing in for the temp file the old notebook path wrote to disk.
#[derive(Debug, Clone)]
pub(super) struct VirtualSource {
    pub(super) path: SystemVirtualPathBuf,
    pub(super) content: Arc<str>,
}

/// [`OsSystem`] plus a single in-memory virtual file, so a notebook's
/// concatenated cell source is type-checked without materializing a temp `.py`
/// next to it (a read-only tool must not write into the caller's tree).
#[derive(Debug, Clone)]
pub(super) struct TySystem {
    pub(super) inner: OsSystem,
    pub(super) virtual_source: Option<Arc<VirtualSource>>,
}

impl System for TySystem {
    fn path_metadata(&self, path: &SystemPath) -> std::io::Result<Metadata> {
        self.inner.path_metadata(path)
    }

    fn canonicalize_path(&self, path: &SystemPath) -> std::io::Result<SystemPathBuf> {
        self.inner.canonicalize_path(path)
    }

    fn is_same_file(&self, first: &SystemPath, second: &SystemPath) -> std::io::Result<bool> {
        self.inner.is_same_file(first, second)
    }

    fn which(&self, binary_name: &str) -> WhichResult {
        self.inner.which(binary_name)
    }

    fn read_to_string(&self, path: &SystemPath) -> std::io::Result<String> {
        self.inner.read_to_string(path)
    }

    fn read_to_notebook(&self, path: &SystemPath) -> std::result::Result<Notebook, NotebookError> {
        self.inner.read_to_notebook(path)
    }

    fn read_virtual_path_to_string(&self, path: &SystemVirtualPath) -> std::io::Result<String> {
        match &self.virtual_source {
            Some(source) if source.path.as_path() == path => Ok(source.content.to_string()),
            _ => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        }
    }

    fn read_virtual_path_to_notebook(
        &self,
        _path: &SystemVirtualPath,
    ) -> std::result::Result<Notebook, NotebookError> {
        Err(NotebookError::from(std::io::Error::from(
            std::io::ErrorKind::NotFound,
        )))
    }

    fn current_directory(&self) -> &SystemPath {
        self.inner.current_directory()
    }

    fn user_config_directory(&self) -> Option<SystemPathBuf> {
        self.inner.user_config_directory()
    }

    fn cache_dir(&self) -> Option<SystemPathBuf> {
        self.inner.cache_dir()
    }

    fn read_directory(
        &self,
        path: &SystemPath,
    ) -> std::io::Result<Box<dyn Iterator<Item = std::io::Result<DirectoryEntry>> + '_>> {
        self.inner.read_directory(path)
    }

    fn walk_directory(&self, path: &SystemPath) -> WalkDirectoryBuilder {
        self.inner.walk_directory(path)
    }

    // Delegated, not defaulted, because site-packages discovery reads env vars
    // (VIRTUAL_ENV, CONDA_*); a silent NotPresent here would disable discovery.
    fn env_var(&self, name: &str) -> std::result::Result<String, std::env::VarError> {
        self.inner.env_var(name)
    }

    fn as_writable(&self) -> Option<&dyn WritableSystem> {
        self.inner.as_writable()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn dyn_clone(&self) -> Box<dyn System> {
        Box::new(self.clone())
    }
}

/// Hand-assembled salsa database (stands in for the unpublished ty_project Db).
#[salsa::db]
#[derive(Clone)]
pub struct TyDb {
    storage: salsa::Storage<Self>,
    files: Files,
    system: TySystem,
    vendored: VendoredFileSystem,
    program_settings: Arc<ProgramSettings>,
    rule_selection: Arc<RuleSelection>,
    analysis_settings: Arc<AnalysisSettings>,
}

/// Provides the interned `Program` query, mirroring ty_python_core's
/// (testing-gated) `TestProgramDb` pattern since `program` is not part of any
/// published trait.
#[salsa::db]
pub trait ProgramDb: SemanticDb {
    fn program_settings(&self) -> &ProgramSettings;

    fn program(&self) -> Program<'_>
    where
        Self: Sized,
    {
        Program::from_settings(self, self.program_settings())
    }
}

impl TyDb {
    /// Builds a database rooted at `src_root` (first-party modules) with the
    /// vendored typeshed stdlib, and the site-packages of the python
    /// environment discovered next to it (so third-party types like `numpy`
    /// resolve instead of reporting `unresolved-import`). `python` pins the
    /// environment explicitly; `None` falls back to the implicit discovery
    /// order in [`discover_settings`].
    pub(super) fn new(src_root: &SystemPath, python: Option<&Path>) -> Result<Self, Error> {
        let vendored = ty_vendored::file_system().clone();
        let system = TySystem {
            inner: OsSystem::new(src_root.to_path_buf()),
            virtual_source: None,
        };
        let program_settings = discover_settings(src_root, python)?;
        let db = Self {
            storage: salsa::Storage::new(None),
            files: Files::default(),
            system,
            vendored,
            program_settings,
            rule_selection: Arc::new(RuleSelection::from_registry(default_lint_registry())),
            analysis_settings: Arc::new(AnalysisSettings::default()),
        };
        db.program_settings
            .search_paths
            .try_register_static_roots(&db);
        Ok(db)
    }

    /// Builds a database rooted at `src_root` with no virtual source; the
    /// caller supplies the in-memory source via [`TyDb::serve_virtual`] before
    /// querying. `python` pins the environment discovery (an interpreter or
    /// venv root), exactly as documented on [`TyDb::new`].
    pub(crate) fn for_src_root(src_root: &Path, python: Option<&Path>) -> Result<Self, Error> {
        let src_root_sys = to_system_path(src_root).ok_or_else(|| {
            Error::invalid_input(format!(
                "source root is not representable: {}",
                src_root.display()
            ))
        })?;
        Self::new(src_root_sys.as_path(), python)
    }

    /// Serves `content` at the virtual `path`, replacing the database's single
    /// in-memory virtual source, then returns the interned [`File`] to query
    /// against. On first use the file is registered fresh; on reuse its
    /// revision is bumped so any memoized parse of the previous source is
    /// invalidated and the new bytes are read. Callers must set the source
    /// before every query: the database holds at most one virtual source at a
    /// time, so the previous one does not survive a cache hit.
    pub(crate) fn serve_virtual(&mut self, path: &str, content: &str) -> File {
        let path = SystemVirtualPathBuf::from(path.to_string());
        self.system.virtual_source = Some(Arc::new(VirtualSource {
            path: path.clone(),
            content: Arc::from(content),
        }));
        let virtual_file = match self.files().try_virtual_file(path.as_path()) {
            Some(virtual_file) => virtual_file,
            None => self.files().virtual_file(self, path.as_path()),
        };
        virtual_file.sync(self);
        virtual_file.file()
    }

    /// The interpreter's real stdlib directory (`<prefix>/lib/python3.X` on
    /// Unix, `<prefix>/Lib` on Windows), when an environment was discovered.
    /// The docs resolver uses this to fall back to the real CPython `Lib/*.py`
    /// source for docstrings the typeshed stub omits. Read-only: it is the same
    /// discovery [`discover_settings`] already performed; never a subprocess
    /// or interpreter execution.
    ///
    /// The crate's discovery is authoritative; when it comes up empty we lay the
    /// stdlib out ourselves from the base interpreter's prefix (`pyvenv.cfg`'s
    /// `home` for a venv, `sys.prefix` for a system env) using the host
    /// platform's layout.
    pub(crate) fn real_stdlib_path(&self) -> Option<SystemPathBuf> {
        let environment = self.program_settings.python_environment.as_ref().ok()?;
        let environment = environment.as_ref()?;

        if let Ok(path) = environment.real_stdlib_path(&self.system) {
            return Some(path);
        }

        self.stdlib_path_for_layout(environment, StdlibLayout::host())
    }

    /// The stdlib source directory laid out for `layout`, recomputed from the
    /// base interpreter's prefix when the crate's own discovery came up empty.
    /// `layout` is a parameter rather than a `cfg!` gate so the Linux test suite
    /// exercises the Windows `Lib` layout end-to-end without a Windows runner.
    fn stdlib_path_for_layout(
        &self,
        environment: &PythonEnvironment,
        layout: StdlibLayout,
    ) -> Option<SystemPathBuf> {
        let base = base_interpreter_prefix(environment, &self.system, layout)?;
        let version = environment
            .python_version_from_metadata()
            .map(|with_source| with_source.version)
            .unwrap_or_default();
        let stdlib = stdlib_source_dir(&base, layout, version);
        self.system.is_directory(&stdlib).then_some(stdlib)
    }

    /// Whether `name` resolves to an importable module in this database's
    /// search paths. The symbol resolver probes this to decide whether a
    /// dotted symbol's first segment is an import (vs a builtin name, which
    /// must be referenced bare so the builtins fallback can find it).
    pub(crate) fn resolves_module(&self, name: &str) -> bool {
        let Some(module_name) = ModuleName::new(name) else {
            return false;
        };
        let program = <Self as ProgramDb>::program(self);
        let environment = program.resolver_environment(self);
        resolve_module_confident(self, environment, &module_name).is_some()
    }
}

#[salsa::db]
impl SourceDb for TyDb {
    fn vendored(&self) -> &VendoredFileSystem {
        &self.vendored
    }

    fn system(&self) -> &dyn System {
        &self.system
    }

    fn files(&self) -> &Files {
        &self.files
    }
}

#[salsa::db]
impl ModuleResolverDb for TyDb {}

#[salsa::db]
impl PythonCoreDb for TyDb {
    fn should_check_file(&self, file: File) -> bool {
        !file.path(self).is_vendored_path()
    }
}

#[salsa::db]
impl ProgramDb for TyDb {
    fn program_settings(&self) -> &ProgramSettings {
        &self.program_settings
    }
}

#[salsa::db]
impl SemanticDb for TyDb {
    fn check_file(&self, file: File) -> Vec<TyDiagnostic> {
        if !PythonCoreDb::should_check_file(self, file) {
            return Vec::new();
        }
        check_file_unwrap(self, self.program_file(file))
    }

    fn program_file(&self, file: File) -> ProgramFile<'_> {
        ProgramDb::program(self).program_file(self, file)
    }

    fn python_version_with_source(&self, _file: File) -> &PythonVersionWithSource {
        &self.program_settings.python_version
    }

    fn rule_selection(&self, _file: File) -> &RuleSelection {
        &self.rule_selection
    }

    fn lint_registry(&self) -> &LintRegistry {
        default_lint_registry()
    }

    fn analysis_settings(&self, _file: File) -> &AnalysisSettings {
        &self.analysis_settings
    }

    fn dependency_metadata(&self, _file: File) -> Option<&DependencyMetadata> {
        None
    }

    fn verbose(&self) -> bool {
        false
    }

    fn is_open_file(&self, _file: File) -> bool {
        false
    }

    fn dyn_clone(&self) -> Box<dyn SemanticDb> {
        Box::new(self.clone())
    }
}

#[salsa::db]
impl salsa::Database for TyDb {}

/// std `Path` and ty's `SystemPath` are two address spaces; this is the single
/// conversion between them, failing when the path is not representable.
pub(super) fn to_system_path(path: &Path) -> Option<SystemPathBuf> {
    let absolute = std::path::absolute(path).ok()?;
    SystemPath::from_std_path(&absolute).map(SystemPath::to_path_buf)
}

/// The on-disk layout of a CPython stdlib, keyed by the host platform. A
/// parameter rather than a `cfg!` gate so the Linux test suite can exercise the
/// Windows `Lib` layout without a Windows runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StdlibLayout {
    /// The stdlib lives directly under `<prefix>/Lib`.
    Windows,
    /// The stdlib lives under `<prefix>/lib/python{major}.{minor}`.
    Unix,
}

impl StdlibLayout {
    /// The layout of the platform tpmcp is actually running on.
    fn host() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// The real stdlib source directory for a CPython installation rooted at
/// `prefix`, laid out for the given platform and Python version.
fn stdlib_source_dir(
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
fn base_interpreter_prefix(
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
