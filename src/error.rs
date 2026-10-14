//! Domain error taxonomy shared by every tpmcp operation.
//!
//! Errors are rendered to the LLM as tool-level error results
//! (`isError = true`) with a machine-readable `structured_content` payload,
//! never as opaque JSON-RPC protocol errors, so the model always sees an
//! actionable message and can recover (re-read after a conflict, fix a bad
//! line range, and so on).

use std::path::PathBuf;

use rmcp::handler::server::tool::IntoCallToolResult;
use rmcp::model::{CallToolResponse, CallToolResult, ContentBlock, ErrorData};
use serde_json::{Map, Value};
use thiserror::Error as ThisError;

/// Everything that can go wrong inside a tpmcp tool call.
#[derive(Debug, ThisError)]
pub enum Error {
    /// The target path does not exist.
    #[error("file not found: {}", path.display())]
    NotFound { path: PathBuf },

    /// A create-only tool was pointed at an existing file.
    #[error("file already exists: {}", path.display())]
    AlreadyExists { path: PathBuf },

    /// The caller-supplied revision no longer matches the file on disk.
    #[error(
        "revision conflict for {}: expected {expected}, file is now at {current}; \
         re-read the file and retry with its current revision",
        path.display()
    )]
    Conflict {
        path: PathBuf,
        expected: String,
        current: String,
    },

    /// The file is not valid UTF-8; tpmcp only edits text.
    #[error("file is not valid UTF-8 text: {}", path.display())]
    NotUtf8 { path: PathBuf },

    /// The file is larger than the supported limit.
    #[error("file exceeds the {max_mib} MiB limit: {}", path.display())]
    TooLarge { path: PathBuf, max_mib: u64 },

    /// The file could not be parsed (invalid JSON, invalid notebook, ...).
    #[error("cannot parse {}: {message}", path.display())]
    Parse { path: PathBuf, message: String },

    /// A tool argument is semantically invalid (bad range, unknown cell, ...).
    #[error("{message}")]
    InvalidInput { message: String },

    /// The analysis engine (ruff/ty) failed internally. Lint diagnostics are
    /// results, not errors; this covers crashes and configuration failures.
    #[error("analysis failed: {message}")]
    Analysis { message: String },

    /// Underlying filesystem failure.
    #[error("I/O error{}: {source}", path_suffix(path.as_ref()))]
    Io {
        path: Option<PathBuf>,
        source: std::io::Error,
    },
}

fn path_suffix(path: Option<&PathBuf>) -> String {
    match path {
        Some(path) => format!(" for {}", path.display()),
        None => String::new(),
    }
}

impl Error {
    /// Wrap an I/O error that occurred while operating on `path`.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: Some(path.into()),
            source,
        }
    }

    /// Build an [`Error::InvalidInput`] from any displayable message.
    pub fn invalid_input(message: impl std::fmt::Display) -> Self {
        Self::InvalidInput {
            message: message.to_string(),
        }
    }

    /// Build an [`Error::Parse`] for `path`.
    pub fn parse(path: impl Into<PathBuf>, message: impl std::fmt::Display) -> Self {
        Self::Parse {
            path: path.into(),
            message: message.to_string(),
        }
    }

    /// Build an [`Error::Analysis`] from any displayable message.
    pub fn analysis(message: impl std::fmt::Display) -> Self {
        Self::Analysis {
            message: message.to_string(),
        }
    }

    /// Stable machine-readable error code used in `structured_content`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "not_found",
            Self::AlreadyExists { .. } => "already_exists",
            Self::Conflict { .. } => "conflict",
            Self::NotUtf8 { .. } => "not_utf8",
            Self::TooLarge { .. } => "too_large",
            Self::Parse { .. } => "parse_error",
            Self::InvalidInput { .. } => "invalid_input",
            Self::Analysis { .. } => "analysis_error",
            Self::Io { .. } => "io_error",
        }
    }

    /// Machine-readable payload attached to the tool error result.
    fn structured(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("error".into(), Value::String(self.kind().to_string()));
        obj.insert("message".into(), Value::String(self.to_string()));
        match self {
            Self::Conflict {
                path,
                expected,
                current,
            } => {
                obj.insert("path".into(), Value::String(path.display().to_string()));
                obj.insert("expected_revision".into(), Value::String(expected.clone()));
                obj.insert("current_revision".into(), Value::String(current.clone()));
            }
            Self::NotFound { path }
            | Self::AlreadyExists { path }
            | Self::NotUtf8 { path }
            | Self::TooLarge { path, .. }
            | Self::Parse { path, .. } => {
                obj.insert("path".into(), Value::String(path.display().to_string()));
            }
            Self::Io { path, .. } => {
                if let Some(path) = path {
                    obj.insert("path".into(), Value::String(path.display().to_string()));
                }
            }
            Self::InvalidInput { .. } | Self::Analysis { .. } => {}
        }
        Value::Object(obj)
    }
}

/// Render every domain error as an LLM-visible tool error instead of a
/// JSON-RPC protocol error, which hosts usually display opaquely.
impl IntoCallToolResult for Error {
    fn into_call_tool_result(self) -> Result<CallToolResponse, ErrorData> {
        let mut result = CallToolResult::error(vec![ContentBlock::text(self.to_string())]);
        result.structured_content = Some(self.structured());
        Ok(result.into())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod tests {
    use super::*;

    #[test]
    fn conflict_error_carries_revisions() {
        let error = Error::Conflict {
            path: PathBuf::from("/tmp/a.ipynb"),
            expected: "aaaa".into(),
            current: "bbbb".into(),
        };
        let structured = error.structured();
        assert_eq!(structured["error"], "conflict");
        assert_eq!(structured["expected_revision"], "aaaa");
        assert_eq!(structured["current_revision"], "bbbb");
        assert!(error.to_string().contains("re-read"));
    }

    #[test]
    fn io_error_message_includes_path() {
        let error = Error::io(
            "/tmp/x.py",
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        );
        assert_eq!(error.kind(), "io_error");
        assert!(error.to_string().contains("/tmp/x.py"));
        assert!(error.to_string().contains("denied"));
    }
}
