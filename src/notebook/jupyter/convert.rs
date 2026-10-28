//! Conversion of Jupyter notebooks: what a conversion leaves behind and
//! which cell facts it contributes.

use serde_json::Value;

use super::JupyterNotebook;

impl JupyterNotebook {
    /// The Jupyter metadata layers a target that stores none of it (marimo)
    /// cannot carry, warned once per layer when actually present. Direct
    /// `extra` reads: no typed accessors exist for metadata or attachments.
    pub(crate) fn metadata_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        let cells_carry_metadata = self.cells.iter().any(|cell| {
            carries_content(cell.extra.get("metadata"))
                || carries_content(cell.extra.get("attachments"))
        });
        if cells_carry_metadata {
            warnings.push(
                "cell metadata (tags, attachments, ...) is not stored in marimo files".to_string(),
            );
        }
        if carries_content(self.extra.get("metadata")) {
            warnings.push(
                "notebook metadata (kernelspec, language_info, ...) is not stored in marimo files"
                    .to_string(),
            );
        }
        warnings
    }

    /// Same-format Jupyter conversion is a rebuild, not a strip-down: carry
    /// the per-cell passthrough fields (outputs, execution counts, metadata,
    /// ids) and the notebook metadata across. Values move as already-parsed
    /// JSON and are never interpreted; the fresh target keeps its own
    /// nbformat 4.5 version fields.
    pub(crate) fn copy_extras_into(&self, target: &mut Self) {
        for (source_cell, target_cell) in self.cells.iter().zip(target.cells.iter_mut()) {
            for (key, value) in &source_cell.extra {
                if key != "cell_type" && key != "source" {
                    target_cell.extra.insert(key.clone(), value.clone());
                }
            }
        }
        if let Some(metadata) = self.extra.get("metadata") {
            target
                .extra
                .insert("metadata".to_string(), metadata.clone());
        }
    }

    /// The first Jupyter magic (`%`) or shell (`!`) line of a cell source.
    /// Magics are a code-cell concept: markdown text starting with `!` or
    /// `%` (e.g. an image `![alt](...)`) is content and must not match.
    pub(crate) fn first_magic(source: &str) -> Option<&str> {
        source.lines().find_map(|line| {
            let trimmed = line.trim_start();
            (trimmed.starts_with('%') || trimmed.starts_with('!'))
                .then_some(trimmed.split_whitespace().next().unwrap_or(trimmed))
        })
    }
}

/// Whether a passthrough field holds anything worth warning about: a
/// non-empty object, or an unexpected non-object shape (still dropped).
fn carries_content(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Object(map)) => !map.is_empty(),
        Some(_) => true,
        None => false,
    }
}
