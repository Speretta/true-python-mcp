//! Statically dispatched notebook document.
//!
//! `open` detects the format at runtime, so the tool layer needs one owned
//! type for "a notebook of some format". Instead of `Box<dyn Notebook>` this
//! is a closed enum with match delegation: no heap indirection, no vtable,
//! every call inlinable, and — the point of the closed set — adding a format
//! is compiler-enforced: a new variant turns every `match` here into a
//! compile error until it is handled. (The openness of `dyn` was illusory
//! anyway: `open`/`create`/`Format` hardcode the supported formats, so an
//! external impl could never be injected. A real plugin system would add an
//! `External(Box<dyn Notebook>)` variant plus a registration API.)

use super::{Cell, Finding, JupyterNotebook, MarimoNotebook, Notebook};

/// A parsed notebook of any supported format.
pub enum AnyNotebook {
    Jupyter(JupyterNotebook),
    Marimo(MarimoNotebook),
}

impl Notebook for AnyNotebook {
    fn format_label(&self) -> String {
        match self {
            Self::Jupyter(notebook) => notebook.format_label(),
            Self::Marimo(notebook) => notebook.format_label(),
        }
    }

    fn cells(&self) -> &[Cell] {
        match self {
            Self::Jupyter(notebook) => notebook.cells(),
            Self::Marimo(notebook) => notebook.cells(),
        }
    }

    fn set_source(&mut self, index: usize, text: &str) -> Result<(), String> {
        match self {
            Self::Jupyter(notebook) => notebook.set_source(index, text),
            Self::Marimo(notebook) => notebook.set_source(index, text),
        }
    }

    fn insert(&mut self, index: usize, cell_type: &str, source: &str) -> Result<(), String> {
        match self {
            Self::Jupyter(notebook) => notebook.insert(index, cell_type, source),
            Self::Marimo(notebook) => notebook.insert(index, cell_type, source),
        }
    }

    fn delete(&mut self, index: usize) -> Result<Cell, String> {
        match self {
            Self::Jupyter(notebook) => notebook.delete(index),
            Self::Marimo(notebook) => notebook.delete(index),
        }
    }

    fn move_cell(&mut self, from: usize, to: usize) -> Result<(), String> {
        match self {
            Self::Jupyter(notebook) => notebook.move_cell(from, to),
            Self::Marimo(notebook) => notebook.move_cell(from, to),
        }
    }

    fn set_type(&mut self, index: usize, cell_type: &str) -> Result<(), String> {
        match self {
            Self::Jupyter(notebook) => notebook.set_type(index, cell_type),
            Self::Marimo(notebook) => notebook.set_type(index, cell_type),
        }
    }

    fn clear_outputs(&mut self, index: Option<usize>) -> Result<Vec<usize>, String> {
        match self {
            Self::Jupyter(notebook) => notebook.clear_outputs(index),
            Self::Marimo(notebook) => notebook.clear_outputs(index),
        }
    }

    fn serialize(&self) -> String {
        match self {
            Self::Jupyter(notebook) => notebook.serialize(),
            Self::Marimo(notebook) => notebook.serialize(),
        }
    }

    fn check(&self) -> Vec<Finding> {
        match self {
            Self::Jupyter(notebook) => notebook.check(),
            Self::Marimo(notebook) => notebook.check(),
        }
    }
}
