//! Line-terminator stress for marimo files. Python ends lines at `\n`, `\r\n`,
//! and a bare `\r`, and every seam — cell boundaries, block capture, edit
//! write-back — must agree on where a line ends. Boundary scanning that looks
//! only for `\n` hides an `@app.cell` behind a bare CR and an edit eats the
//! neighbor cell; the nightly property run (65536 cases) shrank to exactly
//! that on 2026-10-25. These deterministic cases pin the class in plain
//! `nextest` so a push never needs the nightly to notice.
//!
//! A trailing `\r` inside a source line is by design: marimo bodies are split
//! on `\n` and the CR rides along with its line so CRLF write-back survives
//! (see `src/notebook/marimo/indent.rs`), so assertions below compare a cell's
//! source against its own pre-edit source rather than against LF literals.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom

use tpmcp::notebook::{AnyNotebook, MarimoNotebook, Notebook, open};

/// The input the nightly property run shrank to (2026-10-25): cell 1's body
/// ends at a bare `\r` and the next `@app.cell` follows it — a boundary that
/// `\n`-only line scanning never sees. Editing cell 1 dropped cell 2 whole.
const CR_HIDDEN_BOUNDARY: &str = "import marimo\n\n__generated_with = \"0.25.1\"\napp = marimo.App()\n\n\n@app.cell\ndef _():\n    x = 1\n    return (x,)\n\n\n@app.cell\ndef _():\n    this is not val\r@app.cell\ndef _(x):\n    y = x + 1\n    return (y,)\n\n\nif __name__ == \"__main__\":\n    app.run()\n";

/// A two-cell marimo file whose line endings are all `eol`.
fn two_cell_file(eol: &str) -> String {
    "import marimo\n\n__generated_with = \"0.25.1\"\napp = marimo.App()\n\n\n@app.cell\ndef _():\n    x = 1\n    return (x,)\n\n\n@app.cell\ndef _(y):\n    z = y + 1\n    return (z,)\n\n\nif __name__ == \"__main__\":\n    app.run()\n"
        .replace('\n', eol)
}

fn sources(notebook: &MarimoNotebook) -> Vec<String> {
    notebook
        .cells()
        .iter()
        .map(|cell| cell.source_text().into_owned())
        .collect()
}

/// The boundary behind a bare CR is still a cell boundary: the structured
/// parse splits at it because Python's lexer ends lines at `\r`.
#[test]
fn cr_hidden_boundary_is_a_cell_boundary() {
    let AnyNotebook::Marimo(notebook) = open(CR_HIDDEN_BOUNDARY).unwrap() else {
        panic!("the nightly seed must open as marimo");
    };
    let sources = sources(&notebook);
    assert_eq!(sources.len(), 3, "the cell behind the bare CR is a cell");
    assert_eq!(sources[2], "y = x + 1");
}

/// The nightly regression itself: an edit next to a CR-hidden boundary must
/// not eat the neighbor cell (2026-10-25: two cells after the edit, three
/// before).
#[test]
fn editing_next_to_a_cr_hidden_boundary_spares_the_neighbor() {
    let AnyNotebook::Marimo(mut notebook) = open(CR_HIDDEN_BOUNDARY).unwrap() else {
        panic!("the nightly seed must open as marimo");
    };
    let before = sources(&notebook);
    assert_eq!(before.len(), 3);
    notebook.set_source(1, " ").unwrap();
    let out = notebook.serialize();
    let AnyNotebook::Marimo(reopened) = open(&out).unwrap() else {
        panic!("an edited marimo file must still parse as marimo:\n{out}");
    };
    let after = sources(&reopened);
    assert_eq!(after.len(), 3, "cell count must survive an edit:\n{out}");
    assert_eq!(
        after[0], before[0],
        "cell 0 was not edited but changed:\n{out}"
    );
    assert_eq!(
        after[2], before[2],
        "cell 2 was not edited but changed:\n{out}"
    );
}

/// Byte-exact identity in all three terminators.
#[test]
fn every_line_ending_roundtrips_byte_exact() {
    for eol in ["\n", "\r\n", "\r"] {
        let text = two_cell_file(eol);
        let AnyNotebook::Marimo(notebook) = open(&text).unwrap() else {
            panic!("{eol:?}: must open as marimo");
        };
        assert_eq!(notebook.cells().len(), 2, "{eol:?}");
        assert_eq!(notebook.serialize(), text, "{eol:?}");
    }
}

/// An edit regenerates one block and leaves the rest alone, in all three
/// terminators.
#[test]
fn every_line_ending_survives_an_edit() {
    for eol in ["\n", "\r\n", "\r"] {
        let text = two_cell_file(eol);
        let AnyNotebook::Marimo(mut notebook) = open(&text).unwrap() else {
            panic!("{eol:?}: must open as marimo");
        };
        let before = sources(&notebook);
        notebook.set_source(0, "x = 2").unwrap();
        let out = notebook.serialize();
        let AnyNotebook::Marimo(reopened) = open(&out).unwrap() else {
            panic!("{eol:?}: an edited marimo file must still parse as marimo:\n{out}");
        };
        let after = sources(&reopened);
        assert_eq!(after.len(), 2, "{eol:?}:\n{out}");
        assert_eq!(after[0], "x = 2", "{eol:?}:\n{out}");
        assert_eq!(
            after[1], before[1],
            "{eol:?}: cell 1 was not edited but changed:\n{out}"
        );
    }
}

/// The scanner fallback must split at a CR-hidden boundary too: its regions
/// are what any file falls back to once some cell is broken.
#[test]
fn the_scanner_splits_at_a_cr_hidden_boundary() {
    // The first cell's def is syntactically broken, so the whole-file parse
    // fails and the line scanner runs.
    let text = "import marimo\n\n__generated_with = \"0.25.1\"\napp = marimo.App()\n\n\n@app.cell\ndef _(:\n    x = 1\r@app.cell\ndef _(y):\n    z = y + 1\n    return (z,)\n\n\nif __name__ == \"__main__\":\n    app.run()\n";
    let AnyNotebook::Marimo(notebook) = open(text).unwrap() else {
        panic!("the scanner must open broken files as marimo");
    };
    let before = sources(&notebook);
    assert_eq!(
        before.len(),
        2,
        "the cell behind the bare CR survives the scanner"
    );
    assert_eq!(before[1], "z = y + 1");
}

/// An edited block always carries marimo's `return` line. A captured block
/// without one plus a blank body is a bodyless `def _():` — an
/// IndentationError marimo cannot open. marimo regenerates the return on its
/// own save and emits it even for a cell with no defs (codegen.py replaces the
/// `(...)` placeholder with nothing).
#[test]
fn a_blank_edit_writes_maroimos_return_line() {
    // The captured cell has no trailing `return`.
    let text = "import marimo\n\n__generated_with = \"0.25.1\"\napp = marimo.App()\n\n\n@app.cell\ndef _():\n    x = 1\n\n\nif __name__ == \"__main__\":\n    app.run()\n";
    let AnyNotebook::Marimo(mut notebook) = open(text).unwrap() else {
        panic!("must open as marimo");
    };
    assert_eq!(notebook.cells().len(), 1);
    notebook.set_source(0, " ").unwrap();
    let out = notebook.serialize();
    assert!(
        out.lines().any(|line| line.trim() == "return"),
        "the edited block must carry marimo's bare return line:\n{out}"
    );
    let AnyNotebook::Marimo(reopened) = open(&out).unwrap() else {
        panic!("an edited marimo file must still parse as marimo:\n{out}");
    };
    let after = sources(&reopened);
    assert_eq!(after.len(), 1, "{out}");
    // A whitespace-only body is not code: the blank line the block writes is
    // spacing before marimo's return line, so the source reopens empty — the
    // same shape the synthetic fixtures use for an empty cell body.
    assert_eq!(
        after[0], "",
        "the blank edit must stay an editable, empty cell:\n{out}"
    );
}

/// A statement-less setup body is an IndentationError. marimo appends `pass`
/// to a comment-only body (codegen.py `has_only_comments`) and drops an empty
/// setup section outright; tpmcp must keep the cell, so the body becomes
/// marimo's own `pass` shape.
#[test]
fn a_statementless_setup_edit_writes_pass() {
    let text = "import marimo\n\n__generated_with = \"0.25.1\"\napp = marimo.App()\n\n\nwith app.setup:\n    x = 1\n\n\nif __name__ == \"__main__\":\n    app.run()\n";
    let AnyNotebook::Marimo(mut notebook) = open(text).unwrap() else {
        panic!("must open as marimo");
    };
    notebook.set_source(0, "").unwrap();
    let out = notebook.serialize();
    let AnyNotebook::Marimo(reopened) = open(&out).unwrap() else {
        panic!("an edited marimo file must still parse as marimo:\n{out}");
    };
    let after = sources(&reopened);
    assert_eq!(after.len(), 1, "{out}");
    assert_eq!(after[0], "pass", "marimo's pass fallback:\n{out}");
}
