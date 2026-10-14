//! Optional generic Python/text file tools (`--file-tools`).
//!
//! Coding hosts already ship native read/write/edit tools, so these are off
//! by default. They exist for hosts without file tools (e.g. plain Claude
//! Desktop) and add tpmcp's revision-based conflict safety: `py_write` and
//! `py_edit` require the current revision and refuse to clobber a file that
//! changed underneath the caller.

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
use crate::tools::{apply_edit, mutation_result, respond};

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
    /// Full new file content.
    pub content: String,
    /// Current revision to overwrite an existing file; omit to create a new
    /// file (fails if it already exists).
    #[serde(default)]
    pub revision: Option<String>,
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
        description = "Read a Python/text file with line numbers and its revision. Optional \
                       line_start/line_end select a 1-based inclusive range. Prefer the host's \
                       native read tool when available."
    )]
    fn py_read(&self, Parameters(args): Parameters<PyReadArgs>) -> Result<CallToolResult, Error> {
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
    }

    #[tool(
        description = "Write a Python/text file. Omit `revision` to create a new file (fails if \
                       it exists); pass the current `revision` to overwrite. Returns the new \
                       revision. Prefer the host's native write tool when available."
    )]
    fn py_write(&self, Parameters(args): Parameters<PyWriteArgs>) -> Result<CallToolResult, Error> {
        let new_revision = fs::with_file_lock(&args.path, || {
            match &args.revision {
                None => {
                    if fs::read(&args.path).is_ok() {
                        return Err(Error::AlreadyExists {
                            path: args.path.clone(),
                        });
                    }
                }
                Some(expected) => {
                    fs::read_verified(&args.path, expected)?;
                }
            }
            fs::write(&args.path, &args.content)
        })?;
        let verb = if args.revision.is_none() {
            "created"
        } else {
            "wrote"
        };
        Ok(mutation_result(
            args.format,
            format!("{verb} {}", args.path.display()),
            &args.path,
            &new_revision,
            None,
            json!({}),
        ))
    }

    #[tool(
        description = "Edit a Python/text file: replace an exact old_string with new_string \
                       (like an editor's edit tool). old_string must be unique unless \
                       replace_all is set. Requires the current revision; returns a unified \
                       diff and the new revision. Prefer the host's native edit tool when \
                       available."
    )]
    fn py_edit(&self, Parameters(args): Parameters<PyEditArgs>) -> Result<CallToolResult, Error> {
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
            Some(format!("```diff\n{diff}```")),
            json!({"diff": diff}),
        ))
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
        (None, None) => (1, total),
        (Some(s), Some(e)) => (s, e),
        (Some(s), None) => (s, total),
        (None, Some(e)) => (1, e),
    };
    if total == 0 {
        return Ok(((0, 0), Vec::new()));
    }
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
        out.push_str(&format!("{:>5} | {}\n", range.0 + offset, line));
    }
    if shown < lines.len() {
        out.push_str(&format!(
            "... {} more lines (use line_start/line_end)\n",
            lines.len() - shown
        ));
    }
    out.push_str("```\n");
    out
}
