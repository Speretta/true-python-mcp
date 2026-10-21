//! The `py_docs` tool: resolve a dotted Python symbol — `json.dumps`,
//! `typing.Any`, `print` — to its kind, rendered signature, structured
//! parameters, return type and docstring, using ty's IDE support. Read-only:
//! no interpreter, no subprocess, no write into the caller's tree.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::json;

use crate::docs::{self, DocModule, DocSymbol};
use crate::error::Error;
use crate::render::Format;
use crate::server::Tpmcp;
use crate::tools::{blocking, respond};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PyDocsArgs {
    /// Dotted Python symbol to resolve, e.g. `json.dumps`, `typing.Any`,
    /// `print`. The first segment is an import (or a builtin); the rest are
    /// walked as attributes.
    pub symbol: String,
    /// Optional anchor: a `.py`/`.ipynb` file or a directory. A file anchors
    /// at its parent directory, a directory anchors at itself — the search
    /// root and environment are discovered next to it, as `ty_check` does.
    /// Omit to anchor at the current directory.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// Optional path to a Python interpreter or a virtual-environment root to
    /// resolve against (its site-packages). Omit to auto-discover the
    /// environment: VIRTUAL_ENV, CONDA_PREFIX, then a `.venv`/`venv` ancestor
    /// walk, then the system Python.
    #[serde(default)]
    pub python: Option<PathBuf>,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PyModuleMembersArgs {
    /// A single Python module to enumerate, e.g. `json`, `pathlib`, `numpy`.
    /// The resolved members are listed with their kind, one-line signature and
    /// one-line docstring.
    pub module: String,
    /// Optional anchor: a `.py`/`.ipynb` file or a directory. A file anchors
    /// at its parent directory, a directory anchors at itself — the search
    /// root and environment are discovered next to it, as `ty_check` does.
    /// Omit to anchor at the current directory.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// Optional path to a Python interpreter or a virtual-environment root to
    /// resolve against (its site-packages). Omit to auto-discover the
    /// environment: VIRTUAL_ENV, CONDA_PREFIX, then a `.venv`/`venv` ancestor
    /// walk, then the system Python.
    #[serde(default)]
    pub python: Option<PathBuf>,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[tool_router(router = docs_router, vis = "pub")]
impl Tpmcp {
    #[tool(
        title = "Python Docs",
        description = "Resolve a dotted Python symbol (`json.dumps`, `typing.Any`, `print`) to \
                       its kind, signature, parameters, return type and docstring, using Astral's \
                       ty (compiled in, no interpreter, no subprocess). The first segment is an \
                       import or a builtin; the remaining segments are walked as attributes. Pass \
                       `path` to anchor the search root (a `.py`/`.ipynb` file or a directory) \
                       and `python` to resolve against a specific interpreter or virtual \
                       environment; omit both to anchor at the current directory and \
                       auto-discover the environment. The response records the resolution path \
                       hop by hop and whether the symbol was unique, overloaded, or ambiguous. \
                       Read-only.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn py_docs(
        &self,
        Parameters(args): Parameters<PyDocsArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let src_root = anchor_src_root(args.path.as_deref());
            let symbol = docs::resolve_symbol_with_python(
                &args.symbol,
                Some(src_root),
                args.python.as_deref(),
            )?;
            Ok(respond(
                args.format,
                || docs_markdown(&args.symbol, &symbol),
                || docs_json(&args.symbol, &symbol),
            ))
        })
        .await
    }

    #[tool(
        title = "Python Module Members",
        description = "Enumerate a Python module's members (`json`, `pathlib`, `numpy`) with each \
                       member's kind, a one-line signature and a one-line docstring, using \
                       Astral's ty (compiled in, no interpreter, no subprocess). The listing is \
                       ordered by ty's name convention (public names first, then dunder, then \
                       single-underscore) and lexicographically, and is capped for huge packages \
                       with an explicit `truncated` flag. Pass `path` to anchor the search root \
                       and `python` to resolve against a specific interpreter or virtual \
                       environment; omit both to anchor at the current directory and \
                       auto-discover the environment. Read-only.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn py_module_members(
        &self,
        Parameters(args): Parameters<PyModuleMembersArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let src_root = anchor_src_root(args.path.as_deref());
            let module =
                docs::resolve_module_members(&args.module, Some(src_root), args.python.as_deref())?;
            Ok(respond(
                args.format,
                || members_markdown(&module),
                || members_json(&module),
            ))
        })
        .await
    }
}

// ---- rendering ----

/// The search-root directory implied by an optional anchor: a directory
/// anchors at itself, a file at its parent (the project directory), matching
/// `ty_check`'s source-root selection.
fn anchor_src_root(path: Option<&Path>) -> &Path {
    match path {
        Some(path) if path.is_dir() => path,
        Some(path) => path.parent().unwrap_or_else(|| Path::new(".")),
        None => Path::new("."),
    }
}

fn docs_markdown(symbol: &str, resolved: &DocSymbol) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "py_docs: {symbol}");
    let _ = writeln!(out, "resolved_name: {}", resolved.resolved_name);
    let _ = writeln!(out, "kind: {} ({})", resolved.kind, resolved.disambiguation);
    if resolved.signatures.is_empty() {
        out.push_str("signatures: not callable\n");
    } else {
        for signature in &resolved.signatures {
            let _ = writeln!(out, "signature: {signature}");
        }
    }
    if let Some(return_type) = &resolved.return_type {
        let _ = writeln!(out, "return_type: {return_type}");
    }
    if !resolved.parameters.is_empty() {
        out.push_str("parameters:\n");
        for param in &resolved.parameters {
            let _ = write!(out, "  {}: {}", param.name, param.type_);
            if let Some(default) = &param.default {
                let _ = write!(out, " = {default}");
            }
            let _ = writeln!(out, " ({})", param.kind);
        }
    }
    match &resolved.docstring {
        Some(docstring) => {
            let _ = write!(
                out,
                "docstring ({}):\n{docstring}\n",
                resolved.docstring_source
            );
        }
        None => out.push_str("docstring: none\n"),
    }
    match &resolved.source_path {
        Some(path) => {
            let line = resolved
                .source_line
                .map(|line| line.to_string())
                .unwrap_or_else(|| "?".to_string());
            let _ = writeln!(out, "source: {path}:{line}");
        }
        None => out.push_str("source: unknown\n"),
    }
    if resolved.is_stub {
        out.push_str("is_stub: true\n");
    }
    if resolved.is_reexported {
        out.push_str("is_reexported: true\n");
    }
    if !resolved.candidates.is_empty() {
        out.push_str("candidates:\n");
        for candidate in &resolved.candidates {
            let _ = writeln!(
                out,
                "  {}: {} ({})",
                candidate.resolved_name, candidate.kind, candidate.source
            );
        }
    }
    out.push_str("resolution:\n");
    for hop in &resolved.resolution {
        let _ = writeln!(out, "  {hop}");
    }
    out
}

fn docs_json(symbol: &str, resolved: &DocSymbol) -> serde_json::Value {
    json!({
        "symbol": symbol,
        "resolved_name": resolved.resolved_name,
        "kind": resolved.kind,
        "disambiguation": resolved.disambiguation,
        "signatures": resolved.signatures,
        "return_type": resolved.return_type,
        "parameters": resolved.parameters,
        "docstring": resolved.docstring,
        "docstring_source": resolved.docstring_source,
        "source": resolved.source_path,
        "source_line": resolved.source_line,
        "is_stub": resolved.is_stub,
        "is_reexported": resolved.is_reexported,
        "resolution": resolved.resolution,
        "candidates": resolved.candidates,
    })
}

fn members_markdown(module: &DocModule) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "py_module_members: {}", module.module);
    let _ = writeln!(out, "resolved_name: {}", module.resolved_name);
    let _ = write!(out, "member_count: {}", module.member_count);
    if module.truncated {
        let _ = write!(out, " (showing {})", module.members.len());
    }
    out.push('\n');
    out.push_str("members:\n");
    for member in &module.members {
        let _ = writeln!(out, "  {}: {}", member.name, member.kind);
        if let Some(signature) = &member.signature {
            let _ = writeln!(out, "    signature: {signature}");
        }
        if let Some(summary) = &member.docstring_summary {
            let _ = writeln!(out, "    docstring: {summary}");
        }
    }
    out
}

fn members_json(module: &DocModule) -> serde_json::Value {
    json!({
        "module": module.module,
        "resolved_name": module.resolved_name,
        "member_count": module.member_count,
        "truncated": module.truncated,
        "members": module.members,
    })
}
