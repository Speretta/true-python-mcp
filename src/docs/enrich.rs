//! Docstring resolution for the docs resolver.
//!
//! The stub/source docstring wins; when a definition has none, the resolver
//! returns an honest human-readable note saying *why* and *what was looked at*
//! instead of a bare `None`. The note keeps the LLM from ever seeing a silent
//! gap in the docstring field.

use ruff_db::files::FilePath;

/// The docstring answer for one definition: the docstring itself, where it came
/// from, and — when there is none — an honest note explaining why.
pub(super) struct ResolvedDocstring {
    pub(super) docstring: Option<String>,
    /// `stub`, `source`, or `none`.
    pub(super) source: &'static str,
    pub(super) note: Option<String>,
}

/// Resolves a definition's docstring. `stub_docstring` is the docstring already
/// extracted from the resolved definition (which may be `None`); `definition_file`
/// names the file that definition lives in, reported in the note.
pub(super) fn resolve_docstring(
    definition_file: &FilePath,
    is_stub: bool,
    stub_docstring: Option<String>,
) -> ResolvedDocstring {
    if let Some(docstring) = stub_docstring {
        return ResolvedDocstring {
            docstring: Some(docstring),
            source: if is_stub { "stub" } else { "source" },
            note: None,
        };
    }

    ResolvedDocstring {
        docstring: None,
        source: "none",
        note: Some(if is_stub {
            format!(
                "no docstring: the stub has none (definition at `{}`)",
                definition_file.as_str()
            )
        } else {
            "no docstring: the definition has none (C extension or bare stub)".to_string()
        }),
    }
}
