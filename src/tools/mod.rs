//! Shared plumbing for tool implementations.
//!
//! Tools are methods on [`crate::server::Tpmcp`], grouped per domain; each
//! module exposes a `#[tool_router]`-generated router that `server.rs`
//! combines. Mutating tools follow one cycle: lock the path, verify the
//! caller's revision, mutate the parsed document, write it back byte-exact,
//! return the new revision.

pub mod analysis;
pub mod cell;
pub mod docs;
pub mod files;
pub mod notebook;
pub mod ty;

use std::path::Path;

use rmcp::model::{CallToolResult, ContentBlock};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::Error;
use crate::fs::{self, Snapshot};
use crate::notebook::{AnyNotebook, Notebook};
use crate::render::Format;

/// Run one synchronous tool body on the runtime's blocking pool.
///
/// Tool bodies block for real work: cross-process file locks wait on other
/// processes, and ruff lint/format passes take seconds on large files. The
/// current-thread runtime pumps stdio on its only worker, so bodies must
/// never run on it directly. A panic inside a body (only possible in
/// dependencies, since tpmcp's own code is panic-free) surfaces as an
/// [`Error::Internal`] tool result instead of killing the server.
pub async fn blocking(
    work: impl FnOnce() -> Result<CallToolResult, Error> + Send + 'static,
) -> Result<CallToolResult, Error> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(Error::internal)?
}

/// Cell type accepted by cell-creating and cell-mutating tools.
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CellType {
    Code,
    Markdown,
    Raw,
}

impl CellType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Markdown => "markdown",
            Self::Raw => "raw",
        }
    }
}

/// Read and parse a notebook. Reads need no lock: writes land via atomic
/// rename, so a reader always sees a complete file.
pub fn read_notebook(path: &Path) -> Result<(Snapshot, AnyNotebook), Error> {
    let snapshot = fs::read(path)?;
    let notebook =
        crate::notebook::open(&snapshot.content).map_err(|message| Error::parse(path, message))?;
    Ok((snapshot, notebook))
}

/// Lock, verify `revision`, parse, mutate, write back. Returns the
/// callback's value plus the new file revision.
pub fn with_notebook<T>(
    path: &Path,
    revision: &str,
    mutate: impl FnOnce(&mut AnyNotebook) -> Result<T, Error>,
) -> Result<(T, String), Error> {
    fs::with_file_lock(path, || {
        let snapshot = fs::read_verified(path, revision)?;
        let mut notebook = crate::notebook::open(&snapshot.content)
            .map_err(|message| Error::parse(path, message))?;
        let value = mutate(&mut notebook)?;
        let new_revision = fs::write(path, &notebook.serialize())?;
        Ok((value, new_revision))
    })
}

pub fn check_cell_index(notebook: &AnyNotebook, cell: usize) -> Result<(), Error> {
    crate::notebook::check_index(notebook.cells().len(), cell)
}

/// Resolve a text payload given either inline or as a path to a file to
/// read (the "stream" path for huge content): exactly one must be set. When
/// `file` is set the bytes are read by [`fs::read`] — sharing its size cap
/// and UTF-8 check — instead of trusting the inline text, so a large blob
/// never round-trips through the caller's context.
pub fn resolve_text_or_file(
    inline: Option<String>,
    file: Option<&Path>,
    inline_name: &str,
    file_name: &str,
) -> Result<String, Error> {
    match (inline, file) {
        (Some(_), Some(_)) => Err(Error::invalid_input(format!(
            "{inline_name} and {file_name} are mutually exclusive"
        ))),
        (Some(text), None) => Ok(text),
        (None, Some(path)) => Ok(fs::read(path)?.content),
        (None, None) => Err(Error::invalid_input(format!(
            "provide either {inline_name} or {file_name}"
        ))),
    }
}

/// Shared logic of the surgical edit tools (`cell_edit`, `py_edit`):
/// identical-string rejection, uniqueness enforcement, and the replacement.
/// `context` names the edited region in error messages ("cell 3", "file").
pub fn apply_edit(
    source: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
    context: &str,
) -> Result<String, Error> {
    if old_string == new_string {
        return Err(Error::invalid_input(
            "old_string and new_string are identical",
        ));
    }
    let occurrences = source.matches(old_string).count();
    if occurrences == 0 {
        return Err(Error::invalid_input(format!(
            "old_string not found in {context}"
        )));
    }
    if occurrences > 1 && !replace_all {
        return Err(Error::invalid_input(format!(
            "old_string occurs {occurrences} times in {context}; add more context \
             or set replace_all=true"
        )));
    }
    Ok(if replace_all {
        source.replace(old_string, new_string)
    } else {
        source.replacen(old_string, new_string, 1)
    })
}

/// Build the tool result for the requested format, computing only the
/// branch that is needed.
pub fn respond(
    format: Format,
    markdown: impl FnOnce() -> String,
    json: impl FnOnce() -> Value,
) -> CallToolResult {
    match format {
        Format::Markdown => CallToolResult::success(vec![ContentBlock::text(markdown())]),
        Format::Json => CallToolResult::structured(json()),
    }
}

/// Standard confirmation returned by mutating tools: what happened, where,
/// and the new revision to use for the next mutation. The extras are lazy:
/// each format branch builds only its own payload, so a markdown caller
/// never pays for the JSON extra (a rendered diff, cloned outputs) or vice
/// versa.
pub fn mutation_result(
    format: Format,
    summary: String,
    path: &Path,
    new_revision: &str,
    extra_markdown: impl FnOnce() -> Option<String>,
    extra_json: impl FnOnce() -> Value,
) -> CallToolResult {
    respond(
        format,
        || {
            let mut out = format!(
                "{summary}\npath: {}\nrevision: {new_revision}\n",
                path.display()
            );
            if let Some(extra) = extra_markdown() {
                out.push('\n');
                out.push_str(&extra);
            }
            out
        },
        || {
            let mut payload = serde_json::Map::new();
            payload.insert("ok".into(), Value::Bool(true));
            payload.insert("summary".into(), Value::String(summary.clone()));
            payload.insert("path".into(), json!(path.display().to_string()));
            payload.insert("revision".into(), Value::String(new_revision.to_string()));
            if let Value::Object(extra) = extra_json() {
                for (key, value) in extra {
                    payload.insert(key, value);
                }
            }
            Value::Object(payload)
        },
    )
}
