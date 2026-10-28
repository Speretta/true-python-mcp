//! The concatenated-source view of a Jupyter notebook for engine passes.
//!
//! A type checker or linter sees a notebook as one Python module: the code
//! cells' sources concatenated, with positions that map back to a cell and
//! a cell-relative row. This module owns that mapping for the format;
//! ruff's notebook reader is its implementation detail, so the cell
//! selection and magic handling match the lint pipeline exactly.

use ruff_notebook::{Notebook, NotebookIndex};
use ruff_source_file::{LineColumn, OneIndexed};

/// The concatenated code-cell source of a notebook plus the map from
/// concatenated-source positions back to its cells.
pub struct CellMap(NotebookIndex);

impl CellMap {
    /// The 0-based cell containing the given concatenated row, if any.
    pub fn cell_at(&self, row: usize) -> Option<usize> {
        self.0
            .cell(OneIndexed::new(row).unwrap_or(OneIndexed::MIN))
            .map(|cell| cell.get() - 1)
    }

    /// Translate a concatenated-source position to a cell-relative one.
    pub fn translate(&self, row: usize, column: usize) -> (usize, usize) {
        let translated = self.0.translate_line_column(&LineColumn {
            line: OneIndexed::new(row).unwrap_or(OneIndexed::MIN),
            column: OneIndexed::new(column).unwrap_or(OneIndexed::MIN),
        });
        (translated.line.get(), translated.column.get())
    }
}

/// Concatenate the notebook's code-cell source for a whole-file pass.
/// `content` is parsed by ruff's notebook reader, exactly as the lint
/// pipeline does, so a schema-violating file fails the same way.
pub fn concatenated_source(content: &str) -> Result<(String, CellMap), String> {
    let notebook = Notebook::from_source_code(content).map_err(|error| error.to_string())?;
    Ok((
        notebook.source_code().to_string(),
        CellMap(notebook.index().clone()),
    ))
}
