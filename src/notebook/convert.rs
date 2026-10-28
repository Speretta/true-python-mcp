//! Conversion between notebook formats and the markdown export.
//!
//! A conversion never drops content silently: every cell is carried over
//! and everything that cannot come along (a format-specific metadata
//! layer, stray text outside cells, magics, raw cells, outputs a target
//! does not store) is disclosed in per-cell and per-file warnings. The
//! per-format facts come from the format modules; this module owns the
//! pair semantics (what a source cell becomes in a given target) and the
//! shared final shaping of a freshly built file.

use super::{AnyNotebook, Format, Notebook};
use crate::error::Error;

/// The product of a conversion: the serialized target file, the number of
/// source cells it carries, and the warnings disclosing everything left
/// behind. `cell_count` reports the source cells; a scaffold cell tpmcp
/// adds (the `mo` import marimo markdown cells need) is disclosed in the
/// warnings instead.
pub struct Converted {
    pub content: String,
    pub cell_count: usize,
    pub warnings: Vec<String>,
}

impl AnyNotebook {
    /// Convert this notebook into a fresh one of format `target`, keeping
    /// every cell. Nothing is dropped silently: warnings name each class of
    /// content the target cannot carry.
    pub fn convert(&self, target: Format) -> Result<Converted, Error> {
        let mut notebook = super::create(target);
        let mut warnings = match (self, target) {
            (Self::Marimo(marimo), _) => marimo.conversion_warnings(),
            (Self::Jupyter(jupyter), Format::Marimo) => jupyter.metadata_warnings(),
            (Self::Jupyter(_), Format::Jupyter) => Vec::new(),
        };
        let mut outputs_dropped = false;
        for (index, cell) in self.cells().iter().enumerate() {
            let text = cell.source_text();
            let cell_type = match target {
                Format::Marimo => {
                    if cell.cell_type == "raw" {
                        warnings.push(format!(
                            "cell {index}: raw cells have no marimo equivalent; kept as a code cell"
                        ));
                    }
                    // Magics are a code-cell concept: markdown text starting
                    // with `!` or `%` (e.g. an image `![alt](...)`) is content.
                    if !cell.is_markdown()
                        && let Some(magic) = super::JupyterNotebook::first_magic(&text)
                    {
                        warnings.push(format!(
                            "cell {index}: Jupyter magic `{magic}` has no marimo equivalent; kept \
                             verbatim, marimo will report a syntax error"
                        ));
                    }
                    if !cell.outputs().is_empty() {
                        outputs_dropped = true;
                    }
                    if cell.is_markdown() {
                        "markdown"
                    } else {
                        "code"
                    }
                }
                Format::Jupyter => {
                    if let Some(kind) = cell.marimo_kind()
                        && kind != "cell"
                    {
                        warnings.push(format!(
                            "cell {index}: marimo {kind} block exported as a plain code cell"
                        ));
                    }
                    cell.cell_type.as_str()
                }
            };
            notebook.insert(notebook.cells().len(), cell_type, &text)?;
        }
        if outputs_dropped {
            warnings
                .push("outputs and execution counts are not stored in marimo files".to_string());
        }
        if let (Self::Jupyter(jupyter), AnyNotebook::Jupyter(target)) = (self, &mut notebook) {
            jupyter.copy_extras_into(target);
        }
        let cell_count = notebook.cells().len();
        warnings.extend(notebook.finalize_creation()?);
        Ok(Converted {
            content: notebook.serialize(),
            cell_count,
            warnings,
        })
    }

    /// Export this notebook as markdown: the markdown text plus the warnings
    /// disclosing what the export leaves behind (binary outputs, and the
    /// source format's stray content).
    pub fn export_markdown(&self) -> (String, Vec<String>) {
        let (content, mut warnings) = crate::render::markdown_export(self);
        if let Self::Marimo(marimo) = self {
            warnings.extend(marimo.conversion_warnings());
        }
        (content, warnings)
    }

    /// Final shaping of a freshly built notebook (creation or conversion):
    /// marimo files gain the `mo` definition markdown cells need at runtime
    /// (returning the warning that discloses the scaffold cell), and their
    /// generated cells get the return statements and signatures marimo's own
    /// serializer would write. A no-op for Jupyter targets.
    pub fn finalize_creation(&mut self) -> Result<Vec<String>, Error> {
        match self {
            Self::Jupyter(_) => Ok(Vec::new()),
            Self::Marimo(marimo) => {
                let warning = marimo.ensure_mo_definition()?;
                marimo.canonicalize_edited_cells();
                Ok(warning.into_iter().collect())
            }
        }
    }
}
