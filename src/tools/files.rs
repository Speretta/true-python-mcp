//! Optional generic Python/text file tools (`--file-tools`).
//!
//! Coding hosts already ship native read/write/edit tools, so these are off
//! by default. They exist for hosts without file tools (e.g. plain Claude
//! Desktop) and add tpmcp's revision-based conflict safety: `py_write` and
//! `py_edit` require the current revision and refuse to clobber a file that
//! changed underneath the caller.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::json;

use crate::error::Error;
use crate::fs;
use crate::render::{self, Format};
use crate::server::Tpmcp;
use crate::tools::{apply_edit, blocking, mutation_result, resolve_text_or_file, respond};

/// Maximum lines rendered in markdown when no explicit range is given.
const MAX_RENDER_LINES: usize = 2000;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PyReadArgs {
    /// Path to the text/Python file.
    pub path: PathBuf,
    /// First line to return (1-based, inclusive).
    #[serde(default)]
    pub line_start: Option<usize>,
    /// Last line to return (1-based, inclusive).
    #[serde(default)]
    pub line_end: Option<usize>,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PyWriteArgs {
    /// Path to write.
    pub path: PathBuf,
    /// Full new file content; mutually exclusive with from_path.
    #[serde(default)]
    pub content: Option<String>,
    /// Read the full new content from this file instead (the stream path for
    /// huge payloads); mutually exclusive with content.
    #[serde(default)]
    pub from_path: Option<PathBuf>,
    /// Current revision to overwrite an existing file; omit to create a new
    /// file (fails if it already exists).
    #[serde(default)]
    pub revision: Option<String>,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StreamCopyArgs {
    /// Path of the source file to copy from.
    pub source: PathBuf,
    /// Destination path to write the copied bytes to.
    pub dest: PathBuf,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PyEditArgs {
    /// Path to the file.
    pub path: PathBuf,
    /// Exact text to find; must be unique unless replace_all is set.
    pub old_string: String,
    /// Replacement text.
    pub new_string: String,
    /// Replace every occurrence instead of requiring uniqueness.
    #[serde(default)]
    pub replace_all: bool,
    /// Current file revision (from py_read or a previous mutation).
    pub revision: String,
    /// Result format, "markdown" (default) or "json".
    #[serde(default)]
    pub format: Format,
}

#[tool_router(router = files_router, vis = "pub")]
impl Tpmcp {
    #[tool(
        title = "Read Python File",
        description = "Read a Python/text file with line numbers and its revision. Optional \
                       line_start/line_end select a 1-based inclusive range. Prefer the host's \
                       native read tool when available.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn py_read(
        &self,
        Parameters(args): Parameters<PyReadArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let snapshot = fs::read(&args.path)?;
            let all_lines = fs::logical_lines(&snapshot.content);
            let (range, lines) = select_range(&all_lines, args.line_start, args.line_end)?;
            let total = all_lines.len();
            Ok(respond(
                args.format,
                || read_markdown(&args.path, &snapshot.revision, total, range, &lines),
                || {
                    json!({
                        "path": args.path.display().to_string(),
                        "revision": snapshot.revision,
                        "line_count": total,
                        "range": [range.0, range.1],
                        "content": lines.join("\n"),
                    })
                },
            ))
        })
        .await
    }

    #[tool(
        title = "Write Python File",
        description = "Write a Python/text file. Omit `revision` to create a new file (fails if \
                       it exists); pass the current `revision` to overwrite. Provide the content \
                       inline, or via `from_path` to stream a large payload from disk without \
                       round-tripping it through your context. Returns the new revision. \
                       Prefer the host's native write tool when available.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn py_write(
        &self,
        Parameters(args): Parameters<PyWriteArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let content = resolve_text_or_file(
                args.content,
                args.from_path.as_deref(),
                "content",
                "from_path",
            )?;
            let creating = args.revision.is_none();
            let new_revision = fs::with_file_lock(&args.path, || {
                match &args.revision {
                    None => {
                        // Stat-based existence check: a read-based one treats
                        // "exists but unreadable" (non-UTF-8, oversized,
                        // permission-denied, directories) as "does not exist"
                        // and the rename below would silently destroy it.
                        if fs::exists(&args.path) {
                            return Err(Error::AlreadyExists {
                                path: args.path.clone(),
                            });
                        }
                        fs::create(&args.path, &content)
                    }
                    Some(expected) => {
                        fs::read_verified(&args.path, expected)?;
                        fs::write(&args.path, &content)
                    }
                }
            })?;
            let verb = if creating { "created" } else { "wrote" };
            Ok(mutation_result(
                args.format,
                format!("{verb} {}", args.path.display()),
                &args.path,
                &new_revision,
                || None,
                || json!({}),
            ))
        })
        .await
    }

    #[tool(
        title = "Edit Python File",
        description = "Edit a Python/text file: replace an exact old_string with new_string \
                       (like an editor's edit tool). old_string must be unique unless \
                       replace_all is set. Requires the current revision; returns a unified \
                       diff and the new revision. Prefer the host's native edit tool when \
                       available.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn py_edit(
        &self,
        Parameters(args): Parameters<PyEditArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let (diff, new_revision) = fs::with_file_lock(&args.path, || {
                let snapshot = fs::read_verified(&args.path, &args.revision)?;
                let updated = apply_edit(
                    &snapshot.content,
                    &args.old_string,
                    &args.new_string,
                    args.replace_all,
                    "file",
                )?;
                let new_revision = fs::write(&args.path, &updated)?;
                Ok((
                    render::unified_diff(&snapshot.content, &updated),
                    new_revision,
                ))
            })?;
            Ok(mutation_result(
                args.format,
                format!("edited {}", args.path.display()),
                &args.path,
                &new_revision,
                || Some(format!("```diff\n{diff}```")),
                || json!({"diff": diff.as_str()}),
            ))
        })
        .await
    }

    #[tool(
        title = "Stream Copy File",
        description = "Copy `source` to `dest` byte-for-byte through tpmcp, without loading \
                       the content into the caller's context (the agent passes only a path). \
                       The bytes are streamed in chunks and honor the same size cap as every \
                       other read; `dest` is written atomically (temp file + rename) so it is \
                       never observed half-written. Unlike py_write the content may be binary \
                       (not UTF-8). Returns the new revision of `dest`. Prefer the host's \
                       native copy tool when available.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn stream_copy(
        &self,
        Parameters(args): Parameters<StreamCopyArgs>,
    ) -> Result<CallToolResult, Error> {
        blocking(move || {
            let (new_revision, bytes) =
                fs::with_file_lock(&args.dest, || fs::copy_stream(&args.source, &args.dest))?;
            Ok(mutation_result(
                args.format,
                format!(
                    "copied {} to {}",
                    args.source.display(),
                    args.dest.display()
                ),
                &args.dest,
                &new_revision,
                || None,
                || {
                    json!({
                        "source": args.source.display().to_string(),
                        "bytes": bytes,
                    })
                },
            ))
        })
        .await
    }
}

/// Resolve an optional 1-based inclusive range to `(start, end)` (1-based)
/// and the selected lines. Defaults to the whole file.
fn select_range<'a>(
    all: &'a [&'a str],
    start: Option<usize>,
    end: Option<usize>,
) -> Result<((usize, usize), Vec<&'a str>), Error> {
    let total = all.len();
    let (start, end) = match (start, end) {
        // A whole-file read of an empty file is an empty view; an EXPLICIT
        // range must hit the bounds error below like any other out-of-range
        // request, instead of silently returning nothing.
        (None, None) if total == 0 => return Ok(((0, 0), Vec::new())),
        (None, None) => (1, total),
        (Some(s), Some(e)) => (s, e),
        (Some(s), None) => (s, total),
        (None, Some(e)) => (1, e),
    };
    if start < 1 || start > end || end > total {
        return Err(Error::invalid_input(format!(
            "line range {start}..{end} out of bounds: file has {total} lines"
        )));
    }
    Ok(((start, end), all[start - 1..end].to_vec()))
}

fn read_markdown(
    path: &Path,
    revision: &str,
    total: usize,
    range: (usize, usize),
    lines: &[&str],
) -> String {
    let mut out = format!(
        "file: {}\nrevision: {revision}\nlines: {total}\n",
        path.display()
    );
    let shown = lines.len().min(MAX_RENDER_LINES);
    out.push_str("```\n");
    for (offset, line) in lines.iter().take(shown).enumerate() {
        // Format straight into the output buffer: a per-line format! would
        // allocate a temp String only to copy it again. Writing into a
        // String is infallible.
        let _ = writeln!(out, "{:>5} | {line}", range.0 + offset);
    }
    if shown < lines.len() {
        let _ = writeln!(
            out,
            "... {} more lines (use line_start/line_end)",
            lines.len() - shown
        );
    }
    out.push_str("```\n");
    out
}
