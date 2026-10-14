//! tpmcp: a Python and Jupyter notebook MCP server.
//!
//! The crate is organized around a small set of focused modules:
//!
//! - [`error`] — the domain error taxonomy, rendered to LLM-visible tool errors
//! - [`fs`] — content-addressed revisions, atomic writes and per-path locking
//! - [`notebook`] — the format-agnostic notebook API; `notebook::jupyter`
//!   implements byte-exact `.ipynb` handling
//! - [`render`] — markdown/JSON rendering of notebooks and diagnostics
//! - [`ruff`] — the isolated ruff integration (lint, fix, format)
//! - [`server`] — the rmcp [`ServerHandler`] implementation
//!
//! Tool implementations live in `tools/` and are wired into the server's tool
//! router; each module is self-contained and stays under 500 lines.

pub mod error;
pub mod fs;
pub mod notebook;
pub mod render;
pub mod ruff;
pub mod server;
pub mod tools;
