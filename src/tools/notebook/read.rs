//! The `notebook_read` pipeline: the query and validation phases and the
//! view both response formats render from.

use serde_json::{Value, json};

use super::{DiagnosticsLevel, NotebookReadArgs};
use crate::error::Error;
use crate::fs;
use crate::notebook::{AnyNotebook, Finding, Notebook};
use crate::render::{self, Detail, Lint, Window, WithOutputs};
use crate::ruff::{self, SafeFixes};

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
