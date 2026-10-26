//! The `notebook_read` pipeline: the query and validation phases and the
//! view both response formats render from.

use rmcp::model::CallToolResult;
use serde_json::{Value, json};

use super::{DiagnosticsLevel, NotebookReadArgs};
use crate::error::Error;
use crate::fs;
use crate::notebook::{AnyNotebook, Finding, Notebook};
use crate::render::{self, Detail, Lint, Window, WithOutputs};
use crate::ruff::{self, SafeFixes};
use crate::tools::respond;

/// How many raw-text lines the fallback view renders by default, matching
/// `py_read`'s rendering cap.
const MAX_FALLBACK_LINES: usize = 2000;

/// The validation attached to a `notebook_read` response: the structural
/// findings plus the lint half.
pub(super) struct Validation {
    findings: Vec<Finding>,
    lint: Lint,
}

/// The query phase: `None` without a query, otherwise the paged match list.
/// A blank query is rejected here, before any validation work runs.
pub(super) fn search_matches(
    args: &NotebookReadArgs,
    notebook: &AnyNotebook,
) -> Result<Option<render::search::Search>, Error> {
    match &args.query {
        Some(query) if query.trim().is_empty() => {
            Err(Error::invalid_input("query must not be empty"))
        }
        Some(query) => Ok(Some(render::search::search(
            notebook.cells(),
            query,
            args.start,
            args.limit,
        ))),
        None => Ok(None),
    }
}

/// The validation phase, on the same snapshot the response renders (reads
/// never lock; the reported revision is the one these findings were computed
/// from). `content` moves into ruff for check+lint: no second read, no
/// clone. A ruff failure degrades to [`Lint::Unavailable`]: an optional
/// attachment must never cost the caller the notebook itself.
pub(super) fn run_validation(
    args: &NotebookReadArgs,
    notebook: &AnyNotebook,
    content: String,
    revision: &str,
) -> Option<Validation> {
    let lint = match args.diagnostics {
        DiagnosticsLevel::None => return None,
        DiagnosticsLevel::Check => Lint::NotRequested,
        DiagnosticsLevel::CheckLint => {
            let snapshot = fs::Snapshot {
                content,
                revision: revision.to_string(),
            };
            match ruff::check(&args.path, SafeFixes::ReportOnly, snapshot) {
                Ok(outcome) => Lint::Issues(outcome.diagnostics),
                Err(error) => Lint::Unavailable(error.to_string()),
            }
        }
    };
    Some(Validation {
        findings: notebook.check(),
        lint,
    })
}

/// Everything one `notebook_read` response renders from.
pub(super) struct ReadView<'a> {
    pub(super) path: &'a str,
    pub(super) revision: &'a str,
    pub(super) notebook: &'a AnyNotebook,
    pub(super) detail: Detail,
    pub(super) with_outputs: WithOutputs,
    pub(super) window: Window,
    pub(super) found: Option<&'a render::search::Search>,
    pub(super) validation: Option<&'a Validation>,
}

impl ReadView<'_> {
    pub(super) fn markdown(&self) -> String {
        let mut out = match self.found {
            Some(found) => {
                render::search::query_markdown(self.path, self.revision, self.notebook, found)
            }
            None => render::notebook_markdown(
                self.path,
                self.revision,
                self.notebook,
                self.detail,
                self.with_outputs,
                self.window,
            ),
        };
        if let Some(validation) = self.validation {
            out.push_str(&render::diagnostics_markdown(
                &validation.findings,
                &validation.lint,
            ));
        }
        out
    }

    pub(super) fn json(&self) -> Value {
        let mut value = match self.found {
            Some(found) => {
                render::search::query_json(self.path, self.revision, self.notebook, found)
            }
            None => render::notebook_json(
                self.path,
                self.revision,
                self.notebook,
                self.detail,
                self.with_outputs,
                self.window,
            ),
        };
        if let Some(validation) = self.validation {
            value["findings"] = json!(validation.findings);
            render::diagnostics::lint_json(&validation.lint, &mut value);
        }
        value
    }
}

/// Raw-text fallback for files that are not notebooks. `notebook_read` used
/// to refuse them outright, which left callers without file tools unable to
/// see a rendered `.md` export or the raw JSON of a corrupt `.ipynb`. The
/// text comes back as numbered lines under an explicit `fallback` marker —
/// never as cells, never with validation.
///
/// In fallback mode `start`/`limit` page through lines (there are no cells)
/// and `query` filters them with the same case-insensitive substring
/// semantics as the cell search; the window is capped like `py_read`'s
/// rendering.
pub(super) fn fallback(
    args: &NotebookReadArgs,
    snapshot: &fs::Snapshot,
    reason: &str,
) -> Result<CallToolResult, Error> {
    if let Some(query) = &args.query
        && query.trim().is_empty()
    {
        return Err(Error::invalid_input("query must not be empty"));
    }
    let lines: Vec<&str> = fs::logical_lines(&snapshot.content);
    let needle = args.query.as_ref().map(|query| query.to_lowercase());
    let matched: Vec<(usize, &str)> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| match &needle {
            Some(needle) => line.to_lowercase().contains(needle),
            None => true,
        })
        .map(|(index, line)| (index + 1, *line))
        .collect();
    let start = args.start.unwrap_or(0).min(matched.len());
    let end = args
        .limit
        .map_or(start + MAX_FALLBACK_LINES, |limit| start + limit)
        .min(matched.len());
    let shown = &matched[start..end];
    let truncated = end < matched.len();
    let first = shown.first().map_or(0, |(number, _)| *number);
    let last = shown.last().map_or(0, |(number, _)| *number);
    let note = "raw text, not notebook cells: detail/with_outputs and diagnostics do not \
                apply; start/limit page through lines and query filters them \
                (case-insensitive substring)"
        .to_string();
    Ok(respond(
        args.format,
        || {
            let mut out = format!(
                "notebook read: {}\nrevision: {}\nfallback: raw text, not a notebook ({reason})\nnote: {note}\n",
                args.path.display(),
                snapshot.revision
            );
            match &args.query {
                Some(query) => out.push_str(&format!(
                    "lines {first}-{last} of {} matching {query:?} ({} matches)\n",
                    lines.len(),
                    matched.len()
                )),
                None => out.push_str(&format!("lines {first}-{last} of {}\n", lines.len())),
            }
            if truncated {
                out.push_str("note: more lines exist; page with start/limit\n");
            }
            out.push('\n');
            let width = last.to_string().len().max(1);
            for (number, text) in shown {
                out.push_str(&format!("{number:width$}: {text}\n"));
            }
            out
        },
        || {
            json!({
                "ok": true,
                "fallback": true,
                "path": args.path.display().to_string(),
                "revision": snapshot.revision,
                "reason": reason,
                "note": note,
                "total_lines": lines.len(),
                "matched_lines": matched.len(),
                "query": args.query,
                "line_start": first,
                "line_end": last,
                "truncated": truncated,
                "lines": shown
                    .iter()
                    .map(|(number, text)| json!({"line": number, "text": text}))
                    .collect::<Vec<_>>(),
            })
        },
    ))
}
