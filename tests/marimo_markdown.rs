//! marimo markdown-cell tests: mo.md extraction, re-wrap fidelity against
//! marimo's own serializer, set_type conversions, and live round-trips
//! through the real marimo package. Core round-trip/mutation tests live in
//! tests/marimo.rs, graph validation in tests/marimo_graph.rs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::{ground_truth_python, marimo_fixture, run_python, synthetic_marimo};
use tpmcp::notebook::{Notebook, open};

// Live marimo checks use the shared venv helper (tests/common/mod.rs):
// skips are announced on stderr, and TPMCP_REQUIRE_GROUND_TRUTH=1 turns a
// missing venv into a hard failure.

/// Parse with real marimo, print the load status and per-cell extracted
/// markdown (mirrors what the marimo editor would show).
const INSPECT: &str = r#"
import sys
from pathlib import Path
from marimo._ast.parse import parse_notebook
from marimo._ast.compiler import compile_cell
from marimo._ast.load import get_notebook_status

path = Path(sys.argv[1])
status = get_notebook_status(str(path))
print("status", status.status)
nb = parse_notebook(path.read_text(), str(path))
for cell in nb.cells:
    try:
        impl = compile_cell(cell.code, "x")
    except SyntaxError:
        print(type(cell).__name__, "| syntax-error |")
        continue
    kind = "md" if impl.markdown is not None else "code"
    print(type(cell).__name__, "|", kind, "|", repr(impl.markdown))
"#;

/// Editing a markdown cell keeps the file valid for real marimo and the new
/// text re-extracts exactly; untouched cells stay byte-identical.
#[test]
fn markdown_edit_survives_real_marimo() {
    let src = marimo_fixture("emoji.py");
    let mut notebook = open(&src).unwrap();
    let new_text = "# Title\n\nNew **content** with :tada: and a blank line.\n";
    notebook.set_source(0, new_text).unwrap();
    let serialized = notebook.serialize();

    // The untouched markdown cell and the import cell are byte-identical.
    assert!(serialized.contains("    mo.md(r\"\"\"\n    :rocket: :smile:\n    \"\"\")"));
    assert!(serialized.contains("def _():\n    import marimo as mo\n\n    return (mo,)"));

    let Some(py) = ground_truth_python() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("tpmcp-md-edit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("emoji_edited.py");
    std::fs::write(&path, &serialized).unwrap();
    let report = run_python(&py, INSPECT, &[path.to_str().unwrap()]);
    assert!(report.contains("status valid"), "{report}");
    // marimo re-extracts exactly what we set (trailing newline is stripped).
    let debug = format!("{:?}", new_text.trim_end());
    let expected = format!("'{}'", &debug[1..debug.len() - 1]);
    assert!(report.contains(&expected), "{report}\nexpected {expected}");
}

/// Our re-wrap of an edited markdown cell is byte-identical to what marimo's
/// own serializer (`format_markdown` + body indentation) produces.
#[test]
fn markdown_rewrap_matches_marimo_serializer() {
    let Some(py) = ground_truth_python() else {
        return;
    };
    let src = marimo_fixture("emoji.py");
    let mut notebook = open(&src).unwrap();
    let new_text = "Edited **markdown** with \"quotes\" and `code`.";
    notebook.set_source(0, new_text).unwrap();
    let serialized = notebook.serialize();

    // Ground truth: regenerate cell 0's block through marimo's serializer.
    const REGEN: &str = r#"
import sys, textwrap
from pathlib import Path
from marimo._ast.parse import parse_notebook
from marimo._ast.compiler import compile_cell
from marimo._ast.codegen import format_markdown

path, new_markdown = sys.argv[1], sys.argv[2]
src = Path(path).read_text()
nb = parse_notebook(src, str(path))
impl = compile_cell(nb.cells[0].code, "0")
object.__setattr__(impl, "markdown", new_markdown)
body = format_markdown(impl)
# Original decorator + signature lines of cell 0, then marimo's to_functiondef
# markdown branch: header + indented body + bare return.
start = src.index("@app.cell")
lines = src[start:].split("\n")
cut = next(i for i, line in enumerate(lines) if "mo.md(" in line)
header = "\n".join(lines[:cut]) + "\n"
sys.stdout.write(header + textwrap.indent(body, "    ") + "\n    return")
"#;
    let dir = std::env::temp_dir().join(format!("tpmcp-md-regen-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("emoji_src.py");
    std::fs::write(&path, &src).unwrap();
    let expected_block = run_python(&py, REGEN, &[path.to_str().unwrap(), new_text]);

    let start = serialized.find("@app.cell").unwrap();
    let end = start + serialized[start..].find("\n\n\n").unwrap();
    let ours = serialized[start..end].to_string();
    assert_eq!(ours, expected_block);
}

/// `set_type` converts between code and markdown both ways, and the result
/// stays a valid marimo file with the expected cell shapes.
#[test]
fn set_type_converts_between_code_and_markdown() {
    let src = marimo_fixture("emoji.py");
    let mut notebook = open(&src).unwrap();

    // markdown -> code: the markdown text becomes the source, hide_code and
    // the wrapper go away.
    notebook.set_type(0, "code").unwrap();
    assert!(notebook.cells()[0].is_code());
    let serialized = notebook.serialize();
    assert!(!serialized.contains("hide_code"), "{serialized}");
    assert!(serialized.contains("def _(mo):"));

    // code -> markdown: the source becomes the markdown content, wrapped in
    // mo.md(r"""...""") with hide_code and a bare return.
    let mut notebook = open(&src).unwrap();
    notebook.set_source(2, "## Converted\n\nfrom code").unwrap();
    notebook.set_type(2, "markdown").unwrap();
    assert_eq!(notebook.cells()[2].cell_type, "markdown");
    let serialized = notebook.serialize();
    assert!(
        serialized.contains("@app.cell(hide_code=True)"),
        "{serialized}"
    );
    assert!(
        serialized.contains(
            "    mo.md(r\"\"\"\n    ## Converted\n\n    from code\n    \"\"\")\n    return"
        ),
        "{serialized}"
    );

    let Some(py) = ground_truth_python() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("tpmcp-md-type-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("converted.py");
    std::fs::write(&path, &serialized).unwrap();
    let report = run_python(&py, INSPECT, &[path.to_str().unwrap()]);
    assert!(report.contains("status valid"), "{report}");
    assert!(report.contains("'## Converted\\n\\nfrom code'"), "{report}");
}

/// Inserted markdown cells: `mo` becomes a signature parameter unless the
/// setup cell provides it; real marimo parses both shapes.
#[test]
fn insert_markdown_cell() {
    // emoji.py has no setup cell -> def _(mo)
    let mut notebook = open(&marimo_fixture("emoji.py")).unwrap();
    notebook.insert(1, "markdown", "Inserted **cell**").unwrap();
    let serialized = notebook.serialize();
    assert!(
        serialized.contains("@app.cell(hide_code=True)\ndef _(mo):"),
        "{serialized}"
    );
    assert_eq!(notebook.cells()[1].cell_type, "markdown");

    // toplevel.py's setup cell defines mo -> def _()
    let mut notebook = open(&marimo_fixture("toplevel.py")).unwrap();
    notebook
        .insert(1, "markdown", "# Setup provides mo")
        .unwrap();
    let out2 = notebook.serialize();
    assert!(
        out2.contains("@app.cell(hide_code=True)\ndef _():\n    mo.md(r\"\"\""),
        "{out2}"
    );

    let Some(py) = ground_truth_python() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("tpmcp-md-ins-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, content) in [("a.py", &serialized), ("b.py", &out2)] {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        let report = run_python(&py, INSPECT, &[path.to_str().unwrap()]);
        assert!(report.contains("status valid"), "{name}: {report}");
        assert!(report.contains("| md |"), "{name}: {report}");
    }
}

/// The detection matrix mirrors marimo's `_extract_markdown`: only a single
/// bare `mo.md(constant-string)` expression is a markdown cell.
#[test]
fn markdown_detection_matrix() {
    let cases: Vec<(&str, &str, Option<&str>)> = vec![
        (
            "plain triple",
            "mo.md(r\"\"\"\n    hello\n    \"\"\")",
            Some("hello"),
        ),
        ("single-line", "mo.md(\"hi\")", Some("hi")),
        ("single quotes", "mo.md('''t''')", Some("t")),
        (
            "f-string, no interpolation",
            "mo.md(f\"\"\"static\"\"\")",
            Some("static"),
        ),
        ("f-string interpolated", "mo.md(f\"{x}\")", None),
        ("assigned", "title = mo.md(\"x\")", None),
        ("not mo", "marimo.md(\"x\")", None),
        ("no args", "mo.md()", None),
        ("two args", "mo.md(\"a\", \"b\")", None),
        ("extra statement", "mo.md(\"x\")\ny = 1", None),
        ("keywords ok", "mo.md(\"x\", y=1)", Some("x")),
        ("implicit concat", "mo.md(\"a\" \"b\")", Some("ab")),
        ("mixed concat", "mo.md(\"a\" f\"b\")", None),
    ];
    for (label, body, expected) in cases {
        let src = synthetic_marimo(&[body]);
        let notebook = open(&src).unwrap_or_else(|error| panic!("{label}: {error}"));
        match expected {
            Some(text) => {
                assert_eq!(notebook.cells()[0].cell_type, "markdown", "{label}");
                assert_eq!(notebook.cells()[0].source_text(), text, "{label}");
            }
            None => assert_eq!(notebook.cells()[0].cell_type, "code", "{label}"),
        }
    }
}

/// Editing a markdown cell that had leading comments keeps them, and the
/// original quote tag is preserved (marimo's serializer keeps both).
#[test]
fn markdown_edit_keeps_comments_and_tag() {
    let src = synthetic_marimo(&["# a note\nmo.md('''content''')"]);
    let mut notebook = open(&src).unwrap();
    assert_eq!(notebook.cells()[0].cell_type, "markdown");
    notebook.set_source(0, "edited").unwrap();
    let serialized = notebook.serialize();
    assert!(serialized.contains("    # a note\n"), "{serialized}");
    // marimo always rewrites with """ quotes, keeping the tag ('' -> none).
    assert!(
        serialized.contains("    mo.md(\"\"\"\n    edited\n    \"\"\")"),
        "{serialized}"
    );

    // An r-tagged cell keeps its r prefix.
    let src = synthetic_marimo(&["mo.md(r\"\"\"raw \\text\"\"\")"]);
    let mut notebook = open(&src).unwrap();
    notebook.set_source(0, "new \\text").unwrap();
    let serialized = notebook.serialize();
    assert!(
        serialized.contains("mo.md(r\"\"\"\n    new \\text\n    \"\"\")"),
        "{serialized}"
    );

    let Some(py) = ground_truth_python() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("tpmcp-md-tag-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tagged.py");
    std::fs::write(&path, &serialized).unwrap();
    let report = run_python(&py, INSPECT, &[path.to_str().unwrap()]);
    assert!(report.contains("status valid"), "{report}");
    assert!(report.contains(r"'new \\text'"), "{report}");
}
