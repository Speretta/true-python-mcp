//! The `ty_check` tool: Astral's type checker compiled in (no interpreter, no
//! subprocess), read-only. It type-checks a whole `.py`/`.pyi` file against
//! its annotations and imports and reports the diagnostics; notebooks are
//! checked cell-by-cell with cell-relative positions.

use std::path::{Path, PathBuf};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::json;

use crate::error::Error;
use crate::fs;
use crate::render::{self, Format};
use crate::ruff::Diagnostic;
use crate::server::Tpmcp;
use crate::tools::{blocking, respond};
use crate::ty;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TyCheckArgs {
    /// Path to a .py/.pyi file or a .ipynb notebook to type-check.
    pub path: PathBuf,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[tool_router(router = ty_router, vis = "pub")]
impl Tpmcp {
    #[tool(
        title = "Type Check",
        description = "Type-check a Python file or Jupyter notebook with Astral's ty (compiled \
                       in, no interpreter, no subprocess). Verifies types against annotations, \
                       imports and the standard library. Notebook diagnostics carry a 0-based \
                       `cell` index and a cell-relative line. marimo .py files are checked as \
                       plain Python (their cells are untyped defs, so cross-cell types are not \
                       statically visible). Reports the current revision; read-only.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn ty_check(
        &self,
        Parameters(args): Parameters<TyCheckArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            // Single read (CPU-F2): one snapshot supplies both the reported
            // revision and the bytes the checker parses, so a notebook's
            // diagnostics are computed from exactly these bytes.
            let snapshot = fs::read(&args.path)?;
            let diagnostics = ty::check_snapshot(&args.path, &snapshot)?;
            Ok(respond(
                args.format,
                || ty_markdown(&args.path, &snapshot.revision, &diagnostics),
                || ty_json(&args.path, &snapshot.revision, &diagnostics),
            ))
        })
        .await
    }
}

// ---- rendering ----

fn ty_markdown(path: &Path, revision: &str, diagnostics: &[Diagnostic]) -> String {
    let mut out = String::new();
    out.push_str(&format!("ty check: {}\n", path.display()));
    out.push_str(&format!("revision: {revision}\n"));
    if diagnostics.is_empty() {
        out.push_str("no type errors found\n");
        return out;
    }
    out.push_str(&format!("{} diagnostic(s)\n", diagnostics.len()));
    for diagnostic in diagnostics {
        out.push_str(&format!("\n{}", render::diagnostic_line(diagnostic)));
    }
    out.push('\n');
    out
}

fn ty_json(path: &Path, revision: &str, diagnostics: &[Diagnostic]) -> serde_json::Value {
    json!({
        "path": path.display().to_string(),
        "revision": revision,
        "diagnostic_count": diagnostics.len(),
        "diagnostics": diagnostics,
    })
}
