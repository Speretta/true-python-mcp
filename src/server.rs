use rmcp::{
    ServerHandler,
    handler::server::router::tool::ToolRouter,
    model::{Implementation, ServerCapabilities, ServerConfig},
    tool_handler,
};

/// The `tpmcp` MCP server.
///
/// Holds a single [`ToolRouter`] assembled from the tool modules. Tool modules
/// attach their routes through `#[tool_router]` impl blocks and expose a
/// constructor that this type combines in [`Tpmcp::new`].
#[derive(Clone)]
pub struct Tpmcp {
    tool_router: ToolRouter<Self>,
}

impl Tpmcp {
    /// Build the server.
    ///
    /// `file_tools` toggles the generic `py_read`/`py_write`/`py_edit` tools,
    /// which are off by default because coding hosts already ship native file
    /// tools.
    pub fn new(file_tools: bool) -> Self {
        let mut router = Tpmcp::notebook_router() + Tpmcp::cell_router() + Tpmcp::analysis_router();
        if file_tools {
            router += Tpmcp::files_router();
        }
        Self {
            tool_router: router,
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Tpmcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Python and Jupyter/marimo notebook server. Read notebooks (Jupyter .ipynb and \
                 marimo .py) with notebook_read, never as raw JSON or raw Python; it also \
                 outlines (detail=outline), searches (query) and attaches validation and lint \
                 findings (diagnostics). Edit with cell_edit (surgical string replacement) or \
                 cell_set_source (whole-cell and line-range edits); cell_insert, cell_delete, \
                 cell_move, cell_set_type and cell_clear_outputs restructure them. \
                 notebook_create makes new notebooks or converts existing ones (from_path); \
                 the extension picks the format (.ipynb Jupyter, .py marimo, .md export). \
                 notebook_check validates without writing: Jupyter schema and git conflict \
                 markers, marimo reactive-graph errors (cycles, multiple definitions) and \
                 stale return/signature drift. ruff_check lints .py and .ipynb (notebook \
                 diagnostics carry a 0-based cell index) and ruff_format formats them; their \
                 responses report the current revision so fix and format calls can chain. \
                 Every mutating tool requires the current revision (returned by reads and \
                 mutations); on a conflict error, re-read and retry. Files saved by Jupyter, \
                 nbformat or VS Code stay byte-exact; other layouts normalize once on the \
                 first write and stay byte-stable after, so git diffs show only the lines you \
                 changed. marimo cells render body-only (the trailing return is regenerated); \
                 setup/function/class blocks are read-only; cells in a syntax-error file are \
                 marked broken; mo.md cells are markdown cells and re-wrap on edit.",
            )
    }
}
