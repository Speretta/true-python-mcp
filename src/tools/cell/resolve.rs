//! Cell addressing shared by the cell tools.
//!
//! A call targets a cell by a 0-based index (negative counts from the end),
//! or by a stable reference that survives reordering: the cell's `id` in a
//! Jupyter 4.5+ file, or its name (`def name():`) in a marimo file. The JSON
//! typing keeps the two worlds apart — a number is always an index, a string
//! is always an id or name — so `3` and `"3"` can never be confused.
//!
//! A file that carries neither ids (Jupyter < 4.5) nor names (unnamed marimo
//! cells) answers with an error naming that fact rather than guessing.

use serde::Deserialize;

use crate::error::Error;
use crate::notebook::Cell;

/// Which cell a call targets: an index or a stable id/name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
#[schemars(untagged)]
pub enum CellRef {
    /// A 0-based index; negative counts from the end (-1 is the last cell).
    Index(isize),
    /// The cell's stable reference: a Jupyter id or a marimo cell name.
    Name(String),
}

/// Resolves a batch of references to sorted, de-duplicated cell indexes:
/// every reference resolves against `cells` as they are now, then the list
/// is sorted and a repeated index is refused. `cell_delete` and
/// `cell_update` share this contract; it lives here with the rest of cell
/// addressing.
pub(crate) fn resolve_indices(cells: &[Cell], references: &[CellRef]) -> Result<Vec<usize>, Error> {
    let mut indices = Vec::with_capacity(references.len());
    for reference in references {
        indices.push(resolve_index(cells, reference)?);
    }
    indices.sort_unstable();
    if let Some(pair) = indices.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(Error::invalid_input(format!(
            "cell {} is listed twice",
            pair[0]
        )));
    }
    Ok(indices)
}

/// Resolves `reference` to a cell index in `cells`. Errors name what was
/// looked for and what the file can support: an unknown id/name, an ambiguous
/// one (two cells sharing it), a file whose cells carry no ids or names at
/// all, or an out-of-range index.
pub(crate) fn resolve_index(cells: &[Cell], reference: &CellRef) -> Result<usize, Error> {
    match reference {
        CellRef::Index(index) => {
            let resolved = if *index < 0 {
                cells.len() as isize + index
            } else {
                *index
            };
            if resolved < 0 || resolved as usize >= cells.len() {
                return Err(Error::invalid_input(format!(
                    "cell index {index} out of bounds: the notebook has {} cell(s)",
                    cells.len()
                )));
            }
            Ok(resolved as usize)
        }
        CellRef::Name(name) => {
            let matches: Vec<usize> = cells
                .iter()
                .enumerate()
                .filter(|(_, cell)| cell.id() == Some(name) || cell.marimo_name() == Some(name))
                .map(|(index, _)| index)
                .collect();
            match matches.as_slice() {
                [only] => Ok(*only),
                [] => Err(Error::invalid_input(format!(
                    "no cell with id or name {name:?}; {}",
                    if cells
                        .iter()
                        .any(|cell| { cell.id().is_some() || cell.marimo_name().is_some() })
                    {
                        "use one of the ids/names this notebook has, or the index"
                    } else {
                        "this notebook's cells carry no ids or names — use the index"
                    }
                ))),
                many => {
                    let indices: Vec<String> = many.iter().map(|index| index.to_string()).collect();
                    Err(Error::invalid_input(format!(
                        "{name:?} is ambiguous: {} cells match (indices {}); use the index",
                        many.len(),
                        indices.join(", ")
                    )))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom

    use super::*;

    fn cell(id: Option<&str>, name: Option<&str>) -> Cell {
        let mut cell = Cell::from_text("code", "x = 1\n");
        if let Some(id) = id {
            cell.extra.insert("id".into(), id.into());
        }
        if let Some(name) = name {
            cell.extra.insert(
                "marimo".into(),
                serde_json::json!({"kind": "cell", "name": name}),
            );
        }
        cell
    }

    #[test]
    fn indices_count_from_both_ends() {
        let cells = vec![cell(None, None), cell(None, None), cell(None, None)];
        assert_eq!(resolve_index(&cells, &CellRef::Index(0)).unwrap(), 0);
        assert_eq!(resolve_index(&cells, &CellRef::Index(2)).unwrap(), 2);
        assert_eq!(resolve_index(&cells, &CellRef::Index(-1)).unwrap(), 2);
        assert_eq!(resolve_index(&cells, &CellRef::Index(-3)).unwrap(), 0);
        assert!(resolve_index(&cells, &CellRef::Index(3)).is_err());
        assert!(resolve_index(&cells, &CellRef::Index(-4)).is_err());
    }

    #[test]
    fn ids_and_names_resolve() {
        let cells = vec![
            cell(Some("abc123"), None),
            cell(None, Some("train_model")),
            cell(None, None),
        ];
        assert_eq!(
            resolve_index(&cells, &CellRef::Name("abc123".into())).unwrap(),
            0
        );
        assert_eq!(
            resolve_index(&cells, &CellRef::Name("train_model".into())).unwrap(),
            1
        );
    }

    #[test]
    fn batches_resolve_sorted_and_refuse_repeats() {
        let cells = vec![
            cell(Some("abc123"), None),
            cell(None, Some("train_model")),
            cell(None, None),
        ];
        // Resolution order does not matter: the answer is sorted.
        let indices =
            resolve_indices(&cells, &[CellRef::Index(2), CellRef::Name("abc123".into())]).unwrap();
        assert_eq!(indices, vec![0, 2]);
        // An empty batch is the caller's decision (cell_delete refuses it
        // with its own message); the resolver itself has nothing to say.
        assert_eq!(resolve_indices(&cells, &[]).unwrap(), Vec::<usize>::new());
        // Two references naming the same cell are refused, with the index.
        let error = resolve_indices(
            &cells,
            &[CellRef::Index(1), CellRef::Name("train_model".into())],
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("cell 1 is listed twice"),
            "{error}"
        );
    }

    #[test]
    fn unknown_and_ambiguous_references_are_named_errors() {
        // Two cells sharing one id (a corrupt file) must not guess.
        let cells = vec![cell(Some("dup"), None), cell(Some("dup"), None)];
        let error = resolve_index(&cells, &CellRef::Name("nope".into())).unwrap_err();
        assert!(
            error.to_string().contains("no cell with id or name"),
            "{error}"
        );
        let error = resolve_index(&cells, &CellRef::Name("dup".into())).unwrap_err();
        assert!(error.to_string().contains("ambiguous"), "{error}");

        let bare = vec![cell(None, None)];
        let error = resolve_index(&bare, &CellRef::Name("x".into())).unwrap_err();
        assert!(
            error.to_string().contains("carry no ids or names"),
            "{error}"
        );
    }
}
