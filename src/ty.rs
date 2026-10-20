//! The ty integration: Astral's type checker compiled in (no interpreter,
//! no subprocess), exposed as a read-only whole-file verdict. The salsa `Db`
//! is hand-assembled here because the `ty_project` crate that would bundle it
//! is unpublished; this mirrors `ty_python_semantic`'s own `TestDb` pattern
//! and ruff's `ProjectDatabase`, reduced to: `OsSystem` + the vendored
//! typeshed stdlib + the site-packages of the environment discovered next to
//! the checked file.

use std::path::Path;
use std::sync::Arc;

use ruff_db::Db as SourceDb;
use ruff_db::diagnostic::{Diagnostic as TyDiagnostic, Severity, UnifiedFile};
use ruff_db::files::{File, Files, system_path_to_file};
use ruff_db::source::source_text;
use ruff_db::system::walk_directory::WalkDirectoryBuilder;
use ruff_db::system::{
    DirectoryEntry, Metadata, OsSystem, System, SystemPath, SystemPathBuf, SystemVirtualPath,
    SystemVirtualPathBuf, WhichResult, WritableSystem,
};
use ruff_db::vendored::VendoredFileSystem;
use ruff_notebook::{Notebook, NotebookError, NotebookIndex};
use ruff_python_ast::PythonVersion;
use ruff_source_file::{LineColumn, LineIndex, OneIndexed};
use ty_module_resolver::{Db as ModuleResolverDb, FallibleStrategy, SearchPathSettings};
use ty_python_core::Db as PythonCoreDb;
use ty_python_core::ProgramFile;
use ty_python_core::platform::PythonPlatform;
use ty_python_core::program::{Program, ProgramSettings};
use ty_python_semantic::dependency::DependencyMetadata;
use ty_python_semantic::lint::{LintRegistry, RuleSelection};
use ty_python_semantic::{
    AnalysisSettings, Db as SemanticDb, PythonVersionSource, PythonVersionWithSource,
    check_file_unwrap, default_lint_registry,
};

use crate::error::Error;
use crate::fs;
use crate::ruff::{Diagnostic, LineCol};

/// The virtual path of a notebook's concatenated cell source. A plain `.py`
/// name keeps the source type `Python` (a `.ipynb` virtual path would be
/// re-read as a notebook), and it need not live under a search root: absolute
/// imports already resolve against the notebook's parent directory, which is
/// the search root.
const NOTEBOOK_VIRTUAL_PATH: &str = "__tpmcp_ty_notebook__.py";

/// One in-memory `.py` source the database serves through its [`System`],
/// standing in for the temp file the old notebook path wrote to disk.
#[derive(Debug, Clone)]
struct VirtualSource {
    path: SystemVirtualPathBuf,
    content: Arc<str>,
}

/// [`OsSystem`] plus a single in-memory virtual file, so a notebook's
/// concatenated cell source is type-checked without materializing a temp `.py`
/// next to it (a read-only tool must not write into the caller's tree).
#[derive(Debug, Clone)]
struct TySystem {
    inner: OsSystem,
    virtual_source: Option<Arc<VirtualSource>>,
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
        program_inner(self)
    }
}

#[salsa::tracked(returns(copy))]
fn program_inner(db: &dyn ProgramDb) -> Program<'_> {
    Program::from_settings(db, db.program_settings())
}

impl TyDb {
    /// Builds a database rooted at `src_root` (first-party modules) with the
    /// vendored typeshed stdlib, and the site-packages of the python
    /// environment discovered next to it (so third-party types like `numpy`
    /// resolve instead of reporting `unresolved-import`).
    fn new(src_root: &SystemPath, virtual_source: Option<VirtualSource>) -> Result<Self, Error> {
        let vendored = ty_vendored::file_system().clone();
        let system = TySystem {
            inner: OsSystem::new(src_root.to_path_buf()),
            virtual_source: virtual_source.map(Arc::new),
        };
        let mut search_path_settings = SearchPathSettings::new(vec![src_root.to_path_buf()]);
        if let Some(environment) =
            ty_site_packages::PythonEnvironment::discover(Some(src_root), &system).map_err(
                |error| Error::analysis(format!("environment discovery failed: {error}")),
            )?
        {
            search_path_settings.site_packages_paths = environment
                .site_packages_paths(&system)
                .map_err(|error| {
                    Error::analysis(format!("site-packages discovery failed: {error}"))
                })?
                .into_vec();
        }
        let search_paths = search_path_settings
            .to_search_paths(&system, &vendored, &FallibleStrategy)
            .map_err(|error| Error::analysis(format!("invalid search paths: {error}")))?;
        let program_settings = ProgramSettings {
            python_version: PythonVersionWithSource {
                version: PythonVersion::default(),
                source: PythonVersionSource::default(),
            },
            python_platform: PythonPlatform::default(),
            search_paths,
            python_environment: Ok(None),
        };
        let db = Self {
            storage: salsa::Storage::new(None),
            files: Files::default(),
            system,
            vendored,
            program_settings: Arc::new(program_settings),
            rule_selection: Arc::new(RuleSelection::from_registry(default_lint_registry())),
            analysis_settings: Arc::new(AnalysisSettings::default()),
        };
        db.program_settings
            .search_paths
            .try_register_static_roots(&db);
        Ok(db)
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
fn to_system_path(path: &Path) -> Option<SystemPathBuf> {
    let absolute = std::path::absolute(path).ok()?;
    SystemPath::from_std_path(&absolute).map(SystemPath::to_path_buf)
}

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
    if path.extension().is_some_and(|ext| ext == "ipynb") {
        return check_notebook(path, snapshot);
    }
    let src_root = path.parent().unwrap_or_else(|| Path::new("."));
    let src_root_sys = to_system_path(src_root).ok_or_else(|| {
        Error::invalid_input(format!("path is not representable: {}", src_root.display()))
    })?;
    let db = TyDb::new(src_root_sys.as_path(), None)?;
    let system_path = to_system_path(path).ok_or_else(|| {
        Error::invalid_input(format!("path is not representable: {}", path.display()))
    })?;
    let file = system_path_to_file(&db, &system_path)
        .map_err(|error| Error::analysis(format!("cannot read {system_path}: {error}")))?;
    check_file(&db, file)
}

/// Type-checks a `.ipynb` notebook from the caller's snapshot: parse it into
/// ruff's `Notebook` (concatenated cell source + offset index), serve that
/// source to ty as one virtual `.py` file, then map every concatenated-source
/// position back to a `cell` index and a cell-relative row. No temp file, no
/// write into the caller's tree, no second read.
fn check_notebook(path: &Path, snapshot: &fs::Snapshot) -> Result<Vec<Diagnostic>, Error> {
    let notebook = Notebook::from_source_code(&snapshot.content)
        .map_err(|error| Error::parse(path, format!("{error}")))?;
    let source = notebook.source_code().to_string();
    let index = notebook.index().clone();

    let src_root = path.parent().unwrap_or_else(|| Path::new("."));
    let src_root_sys = to_system_path(src_root).ok_or_else(|| {
        Error::invalid_input(format!("path is not representable: {}", src_root.display()))
    })?;

    let virtual_path = SystemVirtualPathBuf::from(NOTEBOOK_VIRTUAL_PATH.to_string());
    let db = TyDb::new(
        src_root_sys.as_path(),
        Some(VirtualSource {
            path: virtual_path.clone(),
            content: Arc::from(source),
        }),
    )?;
    let file = db.files().virtual_file(&db, virtual_path.as_path()).file();
    check_file(&db, file).map(|diagnostics| {
        diagnostics
            .into_iter()
            .map(|diagnostic| map_to_cell(diagnostic, &index))
            .collect()
    })
}

// ---- diagnostic mapping ----

/// Rewrites a diagnostic's concatenated-source positions to the notebook cell:
/// sets the 0-based `cell` index and makes `location`/`end_location`
/// cell-relative.
fn map_to_cell(mut diagnostic: Diagnostic, index: &NotebookIndex) -> Diagnostic {
    if let Some(location) = diagnostic.location {
        diagnostic.cell = index
            .cell(OneIndexed::new(location.row).unwrap_or(OneIndexed::MIN))
            .map(|cell| cell.get() - 1);
        diagnostic.location = Some(translate_line_column(index, location));
    }
    if let Some(end_location) = diagnostic.end_location {
        diagnostic.end_location = Some(translate_line_column(index, end_location));
    }
    diagnostic
}

fn translate_line_column(index: &NotebookIndex, location: LineCol) -> LineCol {
    let translated = index.translate_line_column(&LineColumn {
        line: OneIndexed::new(location.row).unwrap_or(OneIndexed::MIN),
        column: OneIndexed::new(location.column).unwrap_or(OneIndexed::MIN),
    });
    LineCol {
        row: translated.line.get(),
        column: translated.column.get(),
    }
}

/// Type-checks one interned file against an already-built database and returns
/// the diagnostics in tpmcp's stable shape.
fn check_file(db: &TyDb, file: File) -> Result<Vec<Diagnostic>, Error> {
    // The type check recurses through the semantic index and inference on the
    // small worker stack; grow it like the ruff linter/format walks do.
    let source = source_text(db, file);
    let diagnostics =
        crate::stack::guarded_walk(source.len(), || <TyDb as SemanticDb>::check_file(db, file));
    Ok(diagnostics
        .iter()
        .map(|diagnostic| map_diagnostic(db, diagnostic))
        .collect())
}

fn map_diagnostic(db: &TyDb, diagnostic: &TyDiagnostic) -> Diagnostic {
    let (location, end_location) = match diagnostic.primary_span() {
        Some(span) => match span.file() {
            UnifiedFile::Ty(file) => {
                let source = source_text(db, *file);
                let index = LineIndex::from_source_text(&source);
                match span.range() {
                    Some(range) => {
                        let start = index.line_column(range.start(), &source);
                        let end = index.line_column(range.end(), &source);
                        (
                            Some(LineCol {
                                row: start.line.get(),
                                column: start.column.get(),
                            }),
                            Some(LineCol {
                                row: end.line.get(),
                                column: end.column.get(),
                            }),
                        )
                    }
                    None => (None, None),
                }
            }
            // A Ruff span points into a source ty doesn't map (stdlib_comment);
            // leave it unlocated rather than inventing a position.
            UnifiedFile::Ruff(_) => (None, None),
        },
        None => (None, None),
    };
    Diagnostic {
        filename: None,
        cell: None,
        code: Some(diagnostic.secondary_code_or_id().to_string()),
        message: diagnostic.headline_message().to_string(),
        severity: match diagnostic.severity() {
            Severity::Error | Severity::Fatal => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
        .to_string(),
        location,
        end_location,
        fix: None,
        url: diagnostic.documentation_url().map(str::to_string),
    }
}
