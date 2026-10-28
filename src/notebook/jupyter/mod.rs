//! Jupyter notebooks (`.ipynb`, nbformat v4).
//!
//! Parsing keeps every non-`source` field as passthrough JSON (moved, never
//! cloned, out of the parsed document), and
//! [`JupyterNotebook::to_string_canonical`] writes the exact layout Jupyter
//! and VS Code produce (see [`serialize`]). Untouched cells round-trip
//! byte-for-byte on files already in that canonical layout, including their
//! original `source` shape; a file in another layout (CRLF structure,
//! two-space indent, exotic number spellings) normalizes once on the first
//! write and is stable afterwards; tests/canonical_forms.rs pins the
//! boundary. An edited cell is normalized to canonical lines, which is what
//! nbformat does on every save.

mod check;
mod engine;
mod outputs;
mod serialize;

pub(crate) use engine::{CellMap, concatenated_source};
pub(crate) use outputs::{MimePart, OutputView};

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use super::{
    Cell, Finding, Notebook, SourceText, check_index, check_insert_index, check_move_index,
};
use crate::error::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct JupyterNotebook {
    pub cells: Vec<Cell>,
    /// Every top-level field except `cells`, kept verbatim.
    pub extra: BTreeMap<String, Value>,
    /// Validated `nbformat_minor`; `extra` keeps the JSON value for
    /// serialization, so the two never diverge (extra is never mutated).
    pub minor: u64,
}

impl Default for JupyterNotebook {
    fn default() -> Self {
        Self::new()
    }
}

impl JupyterNotebook {
    /// An empty nbformat 4.5 notebook, byte-identical to
    /// `nbformat.v4.new_notebook()` output.
    pub fn new() -> Self {
        let mut extra = BTreeMap::new();
        extra.insert("metadata".into(), Value::Object(Map::new()));
        extra.insert("nbformat".into(), Value::from(4));
        extra.insert("nbformat_minor".into(), Value::from(5));
        Self {
            cells: Vec::new(),
            extra,
            minor: 5,
        }
    }

    pub fn nbformat_minor(&self) -> u64 {
        self.minor
    }

    pub fn parse(content: &str) -> Result<Self, String> {
        let root: Value =
            serde_json::from_str(content).map_err(|error| format!("invalid JSON: {error}"))?;
        let Value::Object(mut map) = root else {
            return Err("notebook must be a JSON object".to_string());
        };
        let version = version_field(&map, "nbformat")?;
        if version != 4 {
            return Err(format!(
                "unsupported nbformat version {version}: only v4 is supported"
            ));
        }
        let minor = version_field(&map, "nbformat_minor")?;
        // Consume the parsed document: the cells array moves out of the root
        // and every passthrough value moves into `extra`; zero clones.
        let raw_cells = match map.remove("cells") {
            Some(Value::Array(cells)) => cells,
            _ => return Err("missing \"cells\" array".to_string()),
        };
        let mut cells = Vec::with_capacity(raw_cells.len());
        for (index, raw) in raw_cells.into_iter().enumerate() {
            cells.push(parse_cell(index, raw)?);
        }
        let extra = passthrough(map);
        Ok(Self {
            cells,
            extra,
            minor,
        })
    }

    pub fn to_string_canonical(&self) -> String {
        serialize::to_canonical_string(self)
    }

    /// A cell shaped like `nbformat.v4.new_*_cell` output: code cells get
    /// `execution_count: null` and `outputs: []`; `id` only for minor >= 5.
    pub fn new_cell(cell_type: &str, source_text: &str, with_id: bool) -> Cell {
        let mut extra = BTreeMap::new();
        if cell_type == "code" {
            extra.insert("execution_count".into(), Value::Null);
            extra.insert("outputs".into(), Value::Array(Vec::new()));
        }
        extra.insert("metadata".into(), Value::Object(Map::new()));
        if with_id {
            extra.insert("id".into(), Value::String(new_cell_id()));
        }
        Cell {
            extra,
            ..Cell::from_text(cell_type, source_text)
        }
    }

    /// Replace one cell's `metadata` object, returning the previous value
    /// (an empty object when the cell had none). Every other field (`id`,
    /// `source`, `outputs`, `execution_count`) is untouched.
    pub fn set_metadata(
        &mut self,
        index: usize,
        metadata: Map<String, Value>,
    ) -> Result<Value, Error> {
        check_index(self.cells.len(), index)?;
        let previous = self.cells[index]
            .extra
            .insert("metadata".into(), Value::Object(metadata));
        Ok(previous.unwrap_or_else(|| Value::Object(Map::new())))
    }
}

impl Notebook for JupyterNotebook {
    fn format_label(&self) -> String {
        format!("nbformat 4.{}", self.nbformat_minor())
    }

    fn cells(&self) -> &[Cell] {
        &self.cells
    }

    fn set_source(&mut self, index: usize, text: &str) -> Result<(), Error> {
        check_index(self.cells.len(), index)?;
        self.cells[index].set_source_text(text);
        Ok(())
    }

    fn insert(&mut self, index: usize, cell_type: &str, source: &str) -> Result<(), Error> {
        check_insert_index(self.cells.len(), index)?;
        let with_id = self.nbformat_minor() >= 5;
        self.cells
            .insert(index, Self::new_cell(cell_type, source, with_id));
        Ok(())
    }

    fn delete(&mut self, index: usize) -> Result<Cell, Error> {
        check_index(self.cells.len(), index)?;
        Ok(self.cells.remove(index))
    }

    fn move_cell(&mut self, from: usize, to: usize) -> Result<(), Error> {
        check_index(self.cells.len(), from)?;
        check_move_index(self.cells.len(), to)?;
        let cell = self.cells.remove(from);
        self.cells.insert(to, cell);
        Ok(())
    }

    fn set_type(&mut self, index: usize, cell_type: &str) -> Result<(), Error> {
        check_index(self.cells.len(), index)?;
        let cell = &mut self.cells[index];
        let was_code = cell.is_code();
        cell.cell_type = cell_type.to_string();
        match (was_code, cell.is_code()) {
            (false, true) => {
                cell.extra
                    .entry("outputs".to_string())
                    .or_insert_with(|| Value::Array(Vec::new()));
                cell.extra
                    .entry("execution_count".to_string())
                    .or_insert(Value::Null);
            }
            (true, false) => {
                cell.extra.remove("outputs");
                cell.extra.remove("execution_count");
            }
            _ => {}
        }
        Ok(())
    }

    fn clear_outputs(&mut self, index: Option<usize>) -> Result<Vec<usize>, Error> {
        match index {
            Some(index) => {
                check_index(self.cells.len(), index)?;
                if !self.cells[index].is_code() {
                    return Err(Error::invalid_input(format!(
                        "cell {index} is {}, only code cells have outputs",
                        self.cells[index].cell_type
                    )));
                }
                clear_cell(&mut self.cells[index]);
                Ok(vec![index])
            }
            None => {
                // One pass: clear each code cell and record its index.
                let mut cleared = Vec::new();
                for (index, cell) in self.cells.iter_mut().enumerate() {
                    if cell.is_code() {
                        clear_cell(cell);
                        cleared.push(index);
                    }
                }
                Ok(cleared)
            }
        }
    }

    fn serialize(&self) -> String {
        self.to_string_canonical()
    }

    fn check(&self) -> Vec<Finding> {
        check::check(self)
    }
}

fn clear_cell(cell: &mut Cell) {
    cell.extra
        .insert("outputs".into(), Value::Array(Vec::new()));
    cell.extra.insert("execution_count".into(), Value::Null);
}

/// Read a required version field: it must be present and a non-negative
/// integer. A JSON float like `4.0` is present but not an integer (nbformat
/// itself rejects it), so the error reports the value that was found instead
/// of claiming the field is missing.
fn version_field(map: &Map<String, Value>, key: &str) -> Result<u64, String> {
    let Some(value) = map.get(key) else {
        return Err(format!("missing integer field \"{key}\""));
    };
    value
        .as_u64()
        .ok_or_else(|| format!("field \"{key}\" must be a non-negative integer, got {value}"))
}

/// Consume one raw cell object: `cell_type`/`source` become typed fields and
/// every other entry moves verbatim into `extra`. The original `source` shape
/// (single string vs. array of lines) is preserved untouched.
fn parse_cell(index: usize, value: Value) -> Result<Cell, String> {
    let Value::Object(mut map) = value else {
        return Err(format!("cell {index}: must be a JSON object"));
    };
    let cell_type = match map.remove("cell_type") {
        Some(Value::String(cell_type)) => cell_type,
        _ => return Err(format!("cell {index}: missing string field \"cell_type\"")),
    };
    let source = match map.remove("source") {
        Some(Value::String(text)) => SourceText::Str(text),
        Some(Value::Array(items)) => {
            let mut lines = Vec::with_capacity(items.len());
            for item in items {
                let Value::String(line) = item else {
                    return Err(format!(
                        "cell {index}: \"source\" array must contain only strings"
                    ));
                };
                lines.push(line);
            }
            SourceText::from_lines(lines)
        }
        Some(_) => {
            return Err(format!(
                "cell {index}: \"source\" must be a string or an array of strings"
            ));
        }
        None => return Err(format!("cell {index}: missing \"source\" field")),
    };
    Ok(Cell {
        cell_type,
        source,
        extra: passthrough(map),
    })
}

/// Every remaining field of a consumed JSON object, moved verbatim into
/// `extra` (the passthrough contract); no value is cloned.
fn passthrough(map: Map<String, Value>) -> BTreeMap<String, Value> {
    map.into_iter().collect()
}

/// 8-hex-char id matching nbformat's cell ids.
fn new_cell_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    blake3::hash(format!("{nanos}-{unique}").as_bytes()).to_hex()[..8].to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod tests {
    use super::*;

    #[test]
    fn split_canonical_matches_nbformat() {
        assert_eq!(SourceText::split_canonical(""), Vec::<String>::new());
        assert_eq!(SourceText::split_canonical("a\nb"), ["a\n", "b"]);
        assert_eq!(SourceText::split_canonical("a\nb\n"), ["a\n", "b\n"]);
        assert_eq!(SourceText::split_canonical("a\r\nb"), ["a\r\n", "b"]);
        assert_eq!(SourceText::split_canonical("a"), ["a"]);
    }
}
