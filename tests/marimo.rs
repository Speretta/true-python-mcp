//! marimo core tests: byte-exact round-trip on real marimo files, cell
//! views, mutations, detection, and column-0 string preservation. Markdown
//! cells live in tests/marimo_markdown.rs, graph validation in
//! tests/marimo_graph.rs. Fixture provenance: tests/data/marimo/NOTICE.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::{ground_truth_python, marimo_fixture, run_python, synthetic_marimo};
use tpmcp::notebook::{Format, Notebook, open};

/// Every marimo fixture: the hand-picked files plus the shipped tutorials.
fn corpus() -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for dir in ["tests/data/marimo", "tests/data/marimo/tutorials"] {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|ext| ext == "py") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// The core guarantee: an untouched marimo file round-trips byte-for-byte,
/// on every fixture in the corpus.
#[test]
fn roundtrip_is_byte_exact() {
    for path in corpus() {
        let src = std::fs::read_to_string(&path).unwrap();
        let name = path.display();
        let notebook = open(&src).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            notebook.serialize(),
            src,
            "byte-exact roundtrip failed for {name}"
        );
    }
}

/// Cross-check with real marimo: every shipped tutorial loads as a healthy
/// notebook. (The edge fixtures broken.py/empty.py explore our scanner and
/// are deliberately not "valid" for marimo, so they are not covered here.)
#[test]
fn tutorials_are_valid_marimo() {
    let Some(py) = ground_truth_python() else {
        return;
    };
    const STATUSES: &str = r#"
import sys
from pathlib import Path
from marimo._ast.load import get_notebook_status

for path in sorted(Path(sys.argv[1]).glob("*.py")):
    print(path.name, get_notebook_status(str(path)).status)
"#;
    let report = run_python(&py, STATUSES, &["tests/data/marimo/tutorials"]);
    let lines: Vec<&str> = report.lines().collect();
    assert_eq!(lines.len(), 10, "expected 10 tutorials, got:\n{report}");
    for line in &lines {
        let (name, status) = line.split_once(' ').unwrap();
        assert!(
            matches!(status, "valid" | "has_warnings"),
            "{name}: unexpected marimo status {status}"
        );
    }
}

#[test]
fn emoji_cells_and_label() {
    let notebook = open(&marimo_fixture("emoji.py")).unwrap();
    assert_eq!(notebook.format_label(), "marimo 0.19.7");
    let cells = notebook.cells();
    assert_eq!(cells.len(), 3);
    // `mo.md(constant)` cells are exposed as markdown with the extracted text.
    assert_eq!(cells[0].cell_type, "markdown");
    assert_eq!(
        cells[0].source_text(),
        "Use colon syntax as a shortcut for **emojis** in your markdown."
    );
    assert_eq!(cells[1].cell_type, "markdown");
    assert_eq!(cells[1].source_text(), ":rocket: :smile:");
    // The import cell's trailing `return (mo,)` is stripped from the source.
    assert_eq!(cells[2].source_text(), "import marimo as mo");
    assert!(cells[2].is_code());
}

#[test]
fn generate_cells_expose_dedented_bodies() {
    let notebook = open(&marimo_fixture("generate.py")).unwrap();
    let cells = notebook.cells();
    // cell "one": `import numpy as np` (return stripped)
    assert_eq!(cells[0].source_text(), "import numpy as np");
    // cell "two": two statements, no blank from the return
    assert_eq!(cells[1].source_text(), "x = 0\nxx = 1");
    // cell "four": comment + code preserved
    assert!(cells[3].source_text().contains("# comment"));
}

#[test]
fn editing_one_cell_preserves_the_others() {
    let src = marimo_fixture("generate.py");
    let mut notebook = open(&src).unwrap();
    notebook.set_source(1, "x = 42").unwrap();
    let serialized = notebook.serialize();

    assert!(serialized.contains("    x = 42\n"), "{serialized}");
    assert!(!serialized.contains("xx = 1"));
    // Untouched cells stay byte-identical.
    assert!(serialized.contains("import numpy as np"));
    assert!(serialized.contains("y = x + 1"));
    // The trailing return of the edited cell comes back in marimo's
    // canonical shape (the edit refreshes it, as marimo's save would).
    assert!(serialized.contains("return (x,)"));
    // Still a valid marimo file.
    assert!(open(&serialized).is_ok());
}

/// Canonicalization shares the drift checker's name sets: a name bound by a
/// read-only `@app.function`/`@app.class_definition` block is a toplevel name,
/// so a cell that references it never takes it as a signature parameter after
/// an edit (the pre-shared-helper canonicalizer dropped this filter).
#[test]
fn canonicalize_excludes_toplevel_names_from_signatures() {
    let src = concat!(
        "import marimo\n\n__generated_with = \"0.25.1\"\napp = marimo.App()\n\n\n",
        "@app.function\ndef helper():\n    return 1\n\n\n",
        "@app.cell\ndef _():\n    y = 1\n    return\n\n\n",
        "if __name__ == \"__main__\":\n    app.run()\n",
    );
    let mut notebook = open(src).unwrap();
    notebook.set_source(1, "y = helper()").unwrap();
    let serialized = notebook.serialize();
    assert!(serialized.contains("y = helper()"), "{serialized}");
    assert!(
        !serialized.contains("def _(helper):"),
        "toplevel names are not signature parameters: {serialized}"
    );
    assert!(serialized.contains("def _():"), "{serialized}");
    // The rewritten file carries no stale drift.
    let findings = notebook.check();
    assert!(findings.is_empty(), "{findings:?}");
}

/// Canonicalizing one edited cell ripples into the returns of the other cells
/// it affects: dropping cell 0's reference to `z` empties cell 1's `return
/// (z,)`, because `z` is no longer referenced by any cell. Both cells are
/// edited (the shape every built notebook has), so the rewrite reaches them.
#[test]
fn cross_cell_ripple_updates_the_defining_cells_return() {
    let mut notebook = tpmcp::notebook::create(Format::Marimo);
    notebook.insert(0, "code", "print(z)\n").unwrap();
    notebook.insert(1, "code", "z = 42\n").unwrap();
    // Cell 0 references `z`, so the cell that defines it returns it.
    assert!(notebook.serialize().contains("return (z,)"));

    // Editing cell 0 to stop referencing `z` empties cell 1's return.
    notebook.set_source(0, "print('hi')\n").unwrap();
    let serialized = notebook.serialize();
    assert!(
        !serialized.contains("return (z,)"),
        "z is no longer referenced: {serialized}"
    );
    // The rewrite left no stale drift behind.
    let findings = notebook.check();
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn insert_and_delete_code_cell() {
    let src = marimo_fixture("generate.py");
    let mut notebook = open(&src).unwrap();
    let before = notebook.cells().len();

    notebook.insert(before, "code", "z = 99").unwrap();
    let inserted = notebook.serialize();
    assert!(inserted.contains("@app.cell"));
    assert!(inserted.contains("    z = 99"));
    assert_eq!(notebook.cells().len(), before + 1);
    assert!(open(&inserted).is_ok());

    notebook.delete(before).unwrap();
    assert_eq!(notebook.cells().len(), before);
    assert!(!notebook.serialize().contains("z = 99"));
}

#[test]
fn move_cell_is_valid() {
    let src = marimo_fixture("generate.py");
    let mut notebook = open(&src).unwrap();
    notebook.move_cell(0, 2).unwrap();
    let serialized = notebook.serialize();
    assert!(open(&serialized).is_ok());
    // The moved cell's content is still present exactly once.
    assert_eq!(serialized.matches("import numpy as np").count(), 1);
}

#[test]
fn read_only_and_unsupported_operations_error_clearly() {
    let mut notebook = open(&marimo_fixture("toplevel.py")).unwrap();
    // toplevel.py has a setup block, an @app.function and an @app.class_definition.
    let function_index = notebook
        .cells()
        .iter()
        .position(|cell| {
            cell.extra
                .get("marimo")
                .and_then(|value| value.get("kind"))
                .and_then(|kind| kind.as_str())
                == Some("function")
        })
        .expect("a function cell exists");
    let error = notebook.set_source(function_index, "x = 1").unwrap_err();
    assert!(error.to_string().contains("not supported"), "{error}");

    assert!(
        notebook
            .clear_outputs(None)
            .unwrap_err()
            .to_string()
            .contains("outputs")
    );
    // Read-only blocks cannot change type; app cells cannot become "raw".
    assert!(
        notebook
            .set_type(function_index, "markdown")
            .unwrap_err()
            .to_string()
            .contains("cannot be changed")
    );
    assert!(
        notebook
            .set_type(1, "raw")
            .unwrap_err()
            .to_string()
            .contains("cannot convert")
    );
    assert!(
        notebook
            .insert(0, "raw", "x")
            .unwrap_err()
            .to_string()
            .contains("do not support raw")
    );
}

#[test]
fn detection_rejects_non_marimo() {
    assert!(open("import os\nprint(1)\n").is_err());
    // Importing marimo as a library is not a notebook without an App.
    assert!(open("import marimo\nx = 1\n").is_err());
    // Jupyter still dispatches correctly.
    assert!(open(&std::fs::read_to_string("tests/data/nbformat_rich.ipynb").unwrap()).is_ok());
}

/// A file with a syntax error in one cell still opens: intact cells are
/// editable, the broken region is shown read-only, and the file round-trips
/// byte-for-byte until something is edited.
#[test]
fn broken_file_parses_partially() {
    let src = marimo_fixture("broken.py");
    let mut notebook = open(&src).expect("scanner opens a file with a syntax error");

    // Untouched round-trip is byte-exact even though the file has an error.
    assert_eq!(notebook.serialize(), src);

    let cells = notebook.cells();
    assert_eq!(cells.len(), 3, "two intact cells + one broken");
    assert_eq!(cells[0].source_text(), "x = 1");
    assert_eq!(cells[2].source_text(), "y = x + 1");
    // The broken region is visible so the LLM can see what to fix.
    assert!(cells[1].source_text().contains("this is not valid python"));

    // The broken cell is read-only with a helpful message.
    let error = notebook.set_source(1, "x = 2").unwrap_err();
    assert!(error.to_string().contains("syntax error"), "{error}");

    // Intact cells are still editable.
    notebook.set_source(0, "x = 100").unwrap();
    let serialized = notebook.serialize();
    assert!(serialized.contains("    x = 100\n"), "{serialized}");
    assert!(
        serialized.contains("this is not valid python"),
        "broken cell preserved"
    );
}

/// A dangling column-0 decorator before the FIRST cell belongs to that cell
/// block, exactly like one between two blocks. Without the rejoin the leading
/// literal region ends on an unparseable bare decorator, its header markers go
/// unrecorded, and the file is rejected outright (the nightly property run
/// shrank to this on 2026-10-26).
#[test]
fn dangling_decorator_before_the_first_cell_still_opens() {
    let src = "import marimo\n\n__generated_with = \"0.25.1\"\napp = marimo.App()\n\n\n@areturn\n\n\n@app.cell\ndef _():\n    x = 1\n    return (x,)\n\n\n@app.cell\ndef _(:\n    y = 2\n\n\nif __name__ == \"__main__\":\n    app.run()\n";
    let notebook = open(src).expect("the dangling decorator joins the first cell block");
    assert_eq!(
        notebook.serialize(),
        src,
        "untouched round-trip is byte-exact"
    );
    let cells = notebook.cells();
    assert_eq!(cells.len(), 2, "one intact cell + one broken");
    assert_eq!(cells[0].source_text(), "x = 1");
}

#[test]
fn broken_file_matches_marimo_status() {
    // Sanity: the fixture really is a marimo file with an error, and our
    // scanner treats the intact cells as valid code cells.
    let notebook = open(&marimo_fixture("broken.py")).unwrap();
    assert_eq!(notebook.format_label(), "marimo 0.25.1");
}

/// The scanner and the structured parser must agree about intact cells: a
/// region holding extra top-level statements next to the cell block (a
/// column-0 `return`, as left behind by a bad merge or a stray dedent —
/// grammatically legal at module level, so the file opens structured) keeps
/// its cell body-only when an UNRELATED edit makes the file untokenizable
/// and reopens it through the region scanner. Mechanism test for the
/// proptest seed in tests/property.proptest-regressions.
#[test]
fn scan_mode_keeps_intact_cells_with_trailing_statements() {
    let src = concat!(
        "import marimo\n\n__generated_with = \"0.25.1\"\napp = marimo.App()\n\n\n",
        "@app.cell(hide_code=True)\ndef _(mo):\n    mo.accordion({\"Tip\": \"x\"})\n",
        "return\n\n\n",
        "@app.cell\ndef _():\n    y = 1\n    return\n\n\n",
        "if __name__ == \"__main__\":\n    app.run()\n",
    );
    // While the file parses as a whole, the structured parser handles it: the
    // def block is the cell, the stray column-0 return is a literal segment.
    let mut notebook = open(src).unwrap();
    assert_eq!(notebook.cells().len(), 2);
    assert_eq!(
        notebook.cells()[0].source_text(),
        "mo.accordion({\"Tip\": \"x\"})"
    );
    assert_eq!(notebook.serialize(), src);

    // Editing the OTHER cell to an unterminated string makes the file
    // untokenizable; reopening degrades to the region scanner.
    notebook.set_source(1, "\"").unwrap();
    let serialized = notebook.serialize();
    let reopened = open(&serialized).unwrap();
    let cells = reopened.cells();
    assert_eq!(cells.len(), 2, "cell count survives the degradation");
    assert_eq!(
        cells[0].source_text(),
        "mo.accordion({\"Tip\": \"x\"})",
        "the untouched cell keeps its body-only source in scan mode"
    );
    assert_ne!(cells[0].marimo_kind(), Some("broken"));
    // The degraded file is stable: re-serializing is the identity, and the
    // stray return survives as a literal segment.
    assert_eq!(reopened.serialize(), serialized);
    assert!(serialized.contains(")\nreturn\n"), "{serialized}");
}

/// The scanner must not split a decorator stack: a junk decorator line
/// (`@app.class_definit% ...`, grammatically a modulo expression) belongs to
/// the class definition that follows it, exactly as the structured parser
/// reads it. When an unrelated edit degrades the file into scan mode, the
/// merged region must yield the same cells — the intact one before it and
/// the class cell with its full block. Mechanism test for the second
/// proptest seed in tests/property.proptest-regressions.
#[test]
fn scan_mode_rejoins_decorator_stacks_split_across_regions() {
    let src = concat!(
        "import marimo\n\n__generated_with = \"0.25.1\"\napp = marimo.App()\n\n\n",
        "@app.cell\ndef _():\n    note = 1\n    return (note,)\n\n\n",
        "@app.class_definit%    ...\n\n\n",
        "@app.class_definition\nclass Helper:\n    pass\n\n\n",
        "@app.cell\ndef _(note):\n    print(note)\n    return\n\n\n",
        "if __name__ == \"__main__\":\n    app.run()\n",
    );
    // The whole file parses (the junk decorator is a grammatical expression,
    // stacked on the class), so this opens through the structured parser.
    let mut notebook = open(src).unwrap();
    assert_eq!(notebook.cells().len(), 3);
    assert_eq!(notebook.cells()[0].source_text(), "note = 1");
    assert_eq!(notebook.cells()[1].marimo_kind(), Some("class"));
    let class_before = notebook.cells()[1].source_text().into_owned();
    assert!(
        class_before.starts_with("@app.class_definit%"),
        "the class block starts at its first decorator: {class_before}"
    );
    assert_eq!(notebook.serialize(), src);

    // Breaking the LAST cell (unterminated string) degrades the file into
    // scan mode; the cells before it must survive unchanged.
    notebook.set_source(2, "\"").unwrap();
    let serialized = notebook.serialize();
    let reopened = open(&serialized).unwrap();
    let cells = reopened.cells();
    assert_eq!(cells.len(), 3, "cell count survives the degradation");
    assert_eq!(cells[0].source_text(), "note = 1");
    assert_ne!(cells[0].marimo_kind(), Some("broken"));
    assert_eq!(cells[1].marimo_kind(), Some("class"));
    assert_eq!(cells[1].source_text(), class_before);
    assert_eq!(cells[2].marimo_kind(), Some("broken"));
    assert_eq!(reopened.serialize(), serialized);
}

/// The dangling-decorator shift must not depend on the FOLLOWING region
/// parsing: an edit that breaks one cell degrades the file into scan mode,
/// and a junk decorator line (`@app.c`, which classify sees first, shadowing
/// the real `@app.cell` behind it) must travel with the definition it
/// decorates — even when the broken cell is the one right before it. Without
/// the shift, the scanner sees the def with only its second decorator and a
/// phantom extra cell appears. Mechanism test for the third proptest seed in
/// tests/property.proptest-regressions.
#[test]
fn scan_mode_keeps_junk_decorators_with_the_decorated_def() {
    let src = concat!(
        "import marimo\n\n__generated_with = \"0.25.1\"\napp = marimo.App()\n\n\n",
        "@app.cell\ndef _():\n    x = 1\n    return (x,)\n\n\n",
        "@app.c#    return\n\n\n",
        "@app.cell\ndef _(x):\n    y = x + 1\n    return (y,)\n\n\n",
        "if __name__ == \"__main__\":\n    app.run()\n",
    );
    // Structured open: the second def carries decorators [@app.c, @app.cell];
    // classify takes the FIRST app.* decorator ("c"), so it is a literal
    // segment, not a cell — one cell total.
    let mut notebook = open(src).unwrap();
    assert_eq!(notebook.cells().len(), 1);
    assert_eq!(notebook.cells()[0].source_text(), "x = 1");
    assert_eq!(notebook.serialize(), src);

    // Breaking the only cell degrades the file into scan mode; the junk
    // decorator must not flip the literal def into a phantom cell.
    notebook.set_source(0, "\u{0}").unwrap();
    let serialized = notebook.serialize();
    let reopened = open(&serialized).unwrap();
    assert_eq!(
        reopened.cells().len(),
        1,
        "no phantom cell from the split decorator stack"
    );
    assert_eq!(reopened.cells()[0].marimo_kind(), Some("broken"));
    assert_eq!(reopened.serialize(), serialized);
}

/// Python `repr()` of a string, limited to what the fixture sources contain
/// (single-quoted form; they contain no `'`, no `\` and no control chars
/// beyond newlines).
fn python_repr(text: &str) -> String {
    assert!(
        !text.contains('\'') && !text.contains('\\'),
        "python_repr only covers quote/backslash-free text"
    );
    format!("'{}'", text.replace('\n', "\\n"))
}

/// Editing a cell whose body contains column-0 triple-quoted string lines
/// must not re-indent the string CONTENT on write-back: that silently changes
/// the string's value, and the tool's diff (computed on the dedented logical
/// source) hides it. Ground truth: marimo 0.25.1's own loader (`fixed_dedent`)
/// leaves under-indented string interiors untouched, and our written file
/// must load in marimo with exactly the intended change.
#[test]
fn marimo_edit_preserves_column_zero_string_lines() {
    let src = marimo_fixture("col0_string.py");
    let mut notebook = open(&src).unwrap();

    // Parse exposes the string interiors exactly as marimo's loader sees
    // them: col-0 content stays col-0, a uniformly indented block dedents.
    let cell0_before = "sql = \"\"\"\nSELECT 1\n\"\"\"\nprint(sql)";
    let cell1 = "note = \"\"\"\n    kept\n\"\"\"\nprint(note)";
    assert_eq!(notebook.cells()[0].source_text(), cell0_before);
    assert_eq!(notebook.cells()[1].source_text(), cell1);

    // Edit one token inside the column-0 string.
    let edited = notebook.cells()[0]
        .source_text()
        .replacen("SELECT 1", "SELECT 2", 1);
    notebook.set_source(0, &edited).unwrap();

    // Re-setting cell 1's source to itself must not move a single byte: the
    // regenerated block replays the recorded per-line indent origins.
    let unchanged = notebook.cells()[1].source_text().into_owned();
    notebook.set_source(1, &unchanged).unwrap();
    let serialized = notebook.serialize();

    // The whole file changed in exactly the one intended line.
    assert_eq!(
        src.lines().count(),
        serialized.lines().count(),
        "{serialized}"
    );
    let changed: Vec<(&str, &str)> = src
        .lines()
        .zip(serialized.lines())
        .filter(|(before, after)| before != after)
        .collect();
    assert_eq!(changed, vec![("SELECT 1", "SELECT 2")], "{serialized}");
    assert!(open(&serialized).is_ok());

    // Ground truth: marimo 0.25.1 loads both files; its exposed cell codes
    // match ours before and after the edit, and only the intended edit shows.
    let Some(py) = ground_truth_python() else {
        return;
    };
    const CODES: &str = r#"
import sys
from pathlib import Path
from marimo._ast.load import get_notebook_status
from marimo._ast.parse import parse_notebook

path = Path(sys.argv[1])
print("status", get_notebook_status(str(path)).status)
nb = parse_notebook(path.read_text(), str(path))
for cell in nb.cells:
    print(repr(cell.code))
"#;
    let dir = std::env::temp_dir().join(format!("tpmcp-col0-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let original_path = dir.join("original.py");
    let edited_path = dir.join("edited.py");
    std::fs::write(&original_path, &src).unwrap();
    std::fs::write(&edited_path, &serialized).unwrap();

    let marimo_codes = |path: &std::path::Path| -> Vec<String> {
        let report = run_python(&py, CODES, &[path.to_str().unwrap()]);
        let mut lines: Vec<String> = report.lines().map(str::to_string).collect();
        assert_eq!(lines.remove(0), "status valid", "{report}");
        lines
    };
    assert_eq!(
        marimo_codes(&original_path),
        vec![python_repr(cell0_before), python_repr(cell1)]
    );
    assert_eq!(
        marimo_codes(&edited_path),
        vec![
            python_repr(&notebook.cells()[0].source_text()),
            python_repr(cell1)
        ]
    );
}

/// The PEP 723 header tpmcp emits (single-line `dependencies`) round-trips
/// byte-exact through parse→serialize, and editing a cell leaves the header
/// untouched.
#[test]
fn pep723_header_roundtrips_and_survives_edits() {
    let header =
        "# /// script\n# requires-python = \">=3.12\"\n# dependencies = [\"marimo\"]\n# ///\n\n";
    let src = format!("{header}{}", synthetic_marimo(&["x = 1"]));

    let mut notebook = open(&src).unwrap();
    assert_eq!(notebook.serialize(), src, "byte-exact round-trip");

    notebook.set_source(0, "x = 42").unwrap();
    let serialized = notebook.serialize();
    assert!(
        serialized.starts_with(header),
        "the PEP 723 header survives an edit: {serialized}"
    );
    assert!(serialized.contains("    x = 42\n"), "{serialized}");
    assert!(!serialized.contains("x = 1"));
    assert!(open(&serialized).is_ok());
}

/// A file with a PEP 723 header and a broken cell degrades into the scanner;
/// the header stays a verbatim literal and the file round-trips byte-exact.
#[test]
fn pep723_header_survives_scan_mode() {
    let header =
        "# /// script\n# requires-python = \">=3.12\"\n# dependencies = [\"marimo\"]\n# ///\n\n";
    let src = format!("{header}{}", marimo_fixture("broken.py"));

    let mut notebook = open(&src).expect("scanner opens a file with a syntax error");
    assert_eq!(
        notebook.serialize(),
        src,
        "byte-exact round-trip in scan mode"
    );

    // The intact cell is editable while the header and broken cell stay put.
    notebook.set_source(0, "x = 100").unwrap();
    let serialized = notebook.serialize();
    assert!(
        serialized.starts_with(header),
        "the PEP 723 header survives a scan-mode edit: {serialized}"
    );
    assert!(serialized.contains("    x = 100\n"), "{serialized}");
    assert!(
        serialized.contains("this is not valid python"),
        "broken cell preserved"
    );
}
