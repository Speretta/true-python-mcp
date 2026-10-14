//! marimo format tests: byte-exact round-trip on real marimo files, cell
//! views, mutations, and detection. Fixtures are verbatim from the marimo
//! repo (examples + codegen golden files).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::{ground_truth_python, run_python};
use tpmcp::notebook::{Notebook, open};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("tests/data/marimo/{name}"))
        .unwrap_or_else(|error| panic!("cannot read {name}: {error}"))
}

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
        let nb = open(&src).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            nb.serialize(),
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
    let nb = open(&fixture("emoji.py")).unwrap();
    assert_eq!(nb.format_label(), "marimo 0.19.7");
    let cells = nb.cells();
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
    let nb = open(&fixture("generate.py")).unwrap();
    let cells = nb.cells();
    // cell "one": `import numpy as np` (return stripped)
    assert_eq!(cells[0].source_text(), "import numpy as np");
    // cell "two": two statements, no blank from the return
    assert_eq!(cells[1].source_text(), "x = 0\nxx = 1");
    // cell "four": comment + code preserved
    assert!(cells[3].source_text().contains("# comment"));
}

#[test]
fn editing_one_cell_preserves_the_others() {
    let src = fixture("generate.py");
    let mut nb = open(&src).unwrap();
    nb.set_source(1, "x = 42").unwrap();
    let out = nb.serialize();

    assert!(out.contains("    x = 42\n"), "{out}");
    assert!(!out.contains("xx = 1"));
    // Untouched cells stay byte-identical.
    assert!(out.contains("import numpy as np"));
    assert!(out.contains("y = x + 1"));
    // The trailing return of the edited cell is preserved (marimo regenerates
    // it on next save, so a stale return is fine).
    assert!(out.contains("return (x,)"));
    // Still a valid marimo file.
    assert!(open(&out).is_ok());
}

#[test]
fn insert_and_delete_code_cell() {
    let src = fixture("generate.py");
    let mut nb = open(&src).unwrap();
    let before = nb.cells().len();

    nb.insert(before, "code", "z = 99").unwrap();
    let inserted = nb.serialize();
    assert!(inserted.contains("@app.cell"));
    assert!(inserted.contains("    z = 99"));
    assert_eq!(nb.cells().len(), before + 1);
    assert!(open(&inserted).is_ok());

    nb.delete(before).unwrap();
    assert_eq!(nb.cells().len(), before);
    assert!(!nb.serialize().contains("z = 99"));
}

#[test]
fn move_cell_is_valid() {
    let src = fixture("generate.py");
    let mut nb = open(&src).unwrap();
    nb.move_cell(0, 2).unwrap();
    let out = nb.serialize();
    assert!(open(&out).is_ok());
    // The moved cell's content is still present exactly once.
    assert_eq!(out.matches("import numpy as np").count(), 1);
}

#[test]
fn read_only_and_unsupported_operations_error_clearly() {
    let mut nb = open(&fixture("toplevel.py")).unwrap();
    // toplevel.py has a setup block, an @app.function and an @app.class_definition.
    let function_index = nb
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
    let error = nb.set_source(function_index, "x = 1").unwrap_err();
    assert!(error.contains("not supported"), "{error}");

    assert!(nb.clear_outputs(None).unwrap_err().contains("outputs"));
    // Read-only blocks cannot change type; app cells cannot become "raw".
    assert!(
        nb.set_type(function_index, "markdown")
            .unwrap_err()
            .contains("cannot be changed")
    );
    assert!(
        nb.set_type(1, "raw")
            .unwrap_err()
            .contains("cannot convert")
    );
    assert!(
        nb.insert(0, "raw", "x")
            .unwrap_err()
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
    let src = fixture("broken.py");
    let mut nb = open(&src).expect("scanner opens a file with a syntax error");

    // Untouched round-trip is byte-exact even though the file has an error.
    assert_eq!(nb.serialize(), src);

    let cells = nb.cells();
    assert_eq!(cells.len(), 3, "two intact cells + one broken");
    assert_eq!(cells[0].source_text(), "x = 1");
    assert_eq!(cells[2].source_text(), "y = x + 1");
    // The broken region is visible so the LLM can see what to fix.
    assert!(cells[1].source_text().contains("this is not valid python"));

    // The broken cell is read-only with a helpful message.
    let error = nb.set_source(1, "x = 2").unwrap_err();
    assert!(error.contains("syntax error"), "{error}");

    // Intact cells are still editable.
    nb.set_source(0, "x = 100").unwrap();
    let out = nb.serialize();
    assert!(out.contains("    x = 100\n"), "{out}");
    assert!(
        out.contains("this is not valid python"),
        "broken cell preserved"
    );
}

#[test]
fn broken_file_matches_marimo_status() {
    // Sanity: the fixture really is a marimo file with an error, and our
    // scanner treats the intact cells as valid code cells.
    let nb = open(&fixture("broken.py")).unwrap();
    assert_eq!(nb.format_label(), "marimo 0.25.1");
}

// ---------------------------------------------------------------------------
// Markdown cells
// ---------------------------------------------------------------------------

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
    let src = fixture("emoji.py");
    let mut nb = open(&src).unwrap();
    let new_text = "# Title\n\nNew **content** with :tada: and a blank line.\n";
    nb.set_source(0, new_text).unwrap();
    let out = nb.serialize();

    // The untouched markdown cell and the import cell are byte-identical.
    assert!(out.contains("    mo.md(r\"\"\"\n    :rocket: :smile:\n    \"\"\")"));
    assert!(out.contains("def _():\n    import marimo as mo\n\n    return (mo,)"));

    let Some(py) = ground_truth_python() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("tpmcp-md-edit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("emoji_edited.py");
    std::fs::write(&path, &out).unwrap();
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
    let src = fixture("emoji.py");
    let mut nb = open(&src).unwrap();
    let new_text = "Edited **markdown** with \"quotes\" and `code`.";
    nb.set_source(0, new_text).unwrap();
    let out = nb.serialize();

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

    let start = out.find("@app.cell").unwrap();
    let end = start + out[start..].find("\n\n\n").unwrap();
    let ours = out[start..end].to_string();
    assert_eq!(ours, expected_block);
}

/// `set_type` converts between code and markdown both ways, and the result
/// stays a valid marimo file with the expected cell shapes.
#[test]
fn set_type_converts_between_code_and_markdown() {
    let src = fixture("emoji.py");
    let mut nb = open(&src).unwrap();

    // markdown -> code: the markdown text becomes the source, hide_code and
    // the wrapper go away.
    nb.set_type(0, "code").unwrap();
    assert!(nb.cells()[0].is_code());
    let out = nb.serialize();
    assert!(!out.contains("hide_code"), "{out}");
    assert!(out.contains("def _(mo):"));

    // code -> markdown: the source becomes the markdown content, wrapped in
    // mo.md(r"""...""") with hide_code and a bare return.
    let mut nb = open(&src).unwrap();
    nb.set_source(2, "## Converted\n\nfrom code").unwrap();
    nb.set_type(2, "markdown").unwrap();
    assert_eq!(nb.cells()[2].cell_type, "markdown");
    let out = nb.serialize();
    assert!(out.contains("@app.cell(hide_code=True)"), "{out}");
    assert!(
        out.contains(
            "    mo.md(r\"\"\"\n    ## Converted\n\n    from code\n    \"\"\")\n    return"
        ),
        "{out}"
    );

    let Some(py) = ground_truth_python() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("tpmcp-md-type-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("converted.py");
    std::fs::write(&path, &out).unwrap();
    let report = run_python(&py, INSPECT, &[path.to_str().unwrap()]);
    assert!(report.contains("status valid"), "{report}");
    assert!(report.contains("'## Converted\\n\\nfrom code'"), "{report}");
}

/// Inserted markdown cells: `mo` becomes a signature parameter unless the
/// setup cell provides it; real marimo parses both shapes.
#[test]
fn insert_markdown_cell() {
    // emoji.py has no setup cell -> def _(mo)
    let mut nb = open(&fixture("emoji.py")).unwrap();
    nb.insert(1, "markdown", "Inserted **cell**").unwrap();
    let out = nb.serialize();
    assert!(
        out.contains("@app.cell(hide_code=True)\ndef _(mo):"),
        "{out}"
    );
    assert_eq!(nb.cells()[1].cell_type, "markdown");

    // toplevel.py's setup cell defines mo -> def _()
    let mut nb = open(&fixture("toplevel.py")).unwrap();
    nb.insert(1, "markdown", "# Setup provides mo").unwrap();
    let out2 = nb.serialize();
    assert!(
        out2.contains("@app.cell(hide_code=True)\ndef _():\n    mo.md(r\"\"\""),
        "{out2}"
    );

    let Some(py) = ground_truth_python() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("tpmcp-md-ins-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, content) in [("a.py", &out), ("b.py", &out2)] {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        let report = run_python(&py, INSPECT, &[path.to_str().unwrap()]);
        assert!(report.contains("status valid"), "{name}: {report}");
        assert!(report.contains("| md |"), "{name}: {report}");
    }
}

/// Build a synthetic marimo file from cell bodies (indented 4, `return`
/// appended), for detection-matrix tests.
fn synthetic(bodies: &[&str]) -> String {
    let mut out =
        String::from("import marimo\n\n__generated_with = \"0.25.1\"\napp = marimo.App()\n");
    for body in bodies {
        out.push_str("\n\n@app.cell\ndef _():\n");
        for line in body.split('\n') {
            if line.is_empty() {
                out.push('\n');
            } else {
                out.push_str(&format!("    {line}\n"));
            }
        }
        out.push_str("    return");
    }
    out.push_str("\n\n\nif __name__ == \"__main__\":\n    app.run()\n");
    out
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
        let src = synthetic(&[body]);
        let nb = open(&src).unwrap_or_else(|error| panic!("{label}: {error}"));
        match expected {
            Some(text) => {
                assert_eq!(nb.cells()[0].cell_type, "markdown", "{label}");
                assert_eq!(nb.cells()[0].source_text(), text, "{label}");
            }
            None => assert_eq!(nb.cells()[0].cell_type, "code", "{label}"),
        }
    }
}

/// Editing a markdown cell that had leading comments keeps them, and the
/// original quote tag is preserved (marimo's serializer keeps both).
#[test]
fn markdown_edit_keeps_comments_and_tag() {
    let src = synthetic(&["# a note\nmo.md('''content''')"]);
    let mut nb = open(&src).unwrap();
    assert_eq!(nb.cells()[0].cell_type, "markdown");
    nb.set_source(0, "edited").unwrap();
    let out = nb.serialize();
    assert!(out.contains("    # a note\n"), "{out}");
    // marimo always rewrites with """ quotes, keeping the tag ('' -> none).
    assert!(
        out.contains("    mo.md(\"\"\"\n    edited\n    \"\"\")"),
        "{out}"
    );

    // An r-tagged cell keeps its r prefix.
    let src = synthetic(&["mo.md(r\"\"\"raw \\text\"\"\")"]);
    let mut nb = open(&src).unwrap();
    nb.set_source(0, "new \\text").unwrap();
    let out = nb.serialize();
    assert!(
        out.contains("mo.md(r\"\"\"\n    new \\text\n    \"\"\")"),
        "{out}"
    );

    let Some(py) = ground_truth_python() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("tpmcp-md-tag-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tagged.py");
    std::fs::write(&path, &out).unwrap();
    let report = run_python(&py, INSPECT, &[path.to_str().unwrap()]);
    assert!(report.contains("status valid"), "{report}");
    assert!(report.contains(r"'new \\text'"), "{report}");
}

// ---------------------------------------------------------------------------
// Graph validation (check)
// ---------------------------------------------------------------------------

/// Ground truth: marimo's own graph validator over the same fixtures.
const GRAPH_CHECK: &str = r#"
import sys
from pathlib import Path
from marimo._ast.parse import parse_notebook
from marimo._ast.compiler import compile_cell
from marimo._ast.names import SETUP_CELL_NAME
from marimo._runtime.dataflow import DirectedGraph
from marimo._lint.validate_graph import check_for_errors
from marimo._types.ids import CellId_t

path = Path(sys.argv[1])
nb = parse_notebook(path.read_text(), str(path))
graph = DirectedGraph()
for i, cell in enumerate(nb.cells):
    kind = type(cell).__name__
    if kind == "UnparsableCell":
        continue
    cid = SETUP_CELL_NAME if kind == "SetupCell" else str(i)
    try:
        impl = compile_cell(cell.code, cid)
    except SyntaxError:
        continue
    graph.register_cell(CellId_t(cid), impl)
for cid, errors in sorted(check_for_errors(graph).items()):
    for error in errors:
        print(cid, type(error).__name__)
"#;

fn codes(findings: &[tpmcp::notebook::Finding]) -> Vec<(&str, Option<usize>)> {
    findings
        .iter()
        .map(|finding| (finding.code.as_str(), finding.cell))
        .collect()
}

#[test]
fn check_flags_cycles() {
    let nb = open(&fixture("graph_cycle.py")).unwrap();
    let findings = nb.check();
    assert_eq!(codes(&findings), vec![("cycle", Some(0))], "{findings:?}");
    assert!(findings[0].message.contains("0, 1"), "{findings:?}");
    assert!(findings[0].message.contains('x'), "{findings:?}");
}

#[test]
fn check_flags_multiple_definitions() {
    let nb = open(&fixture("graph_dupedef.py")).unwrap();
    let findings = nb.check();
    assert_eq!(
        codes(&findings),
        vec![("multiple-definitions", Some(0))],
        "{findings:?}"
    );
    assert!(findings[0].message.contains("`config`"), "{findings:?}");
    assert!(findings[0].message.contains("0, 1"), "{findings:?}");
}

#[test]
fn check_flags_setup_root() {
    let nb = open(&fixture("graph_setup_root.py")).unwrap();
    let findings = nb.check();
    assert_eq!(
        codes(&findings),
        vec![("setup-root", Some(0))],
        "{findings:?}"
    );
    assert!(findings[0].message.contains("payload"), "{findings:?}");
}

#[test]
fn check_reports_stale_drift_as_info() {
    let nb = open(&fixture("graph_stale.py")).unwrap();
    let findings = nb.check();
    assert_eq!(
        codes(&findings),
        vec![
            ("stale-return", Some(0)),
            ("stale-return", Some(1)),
            ("stale-return", Some(2)),
            ("stale-return", Some(3)),
            ("stale-signature", Some(3)),
        ],
        "{findings:?}"
    );
    use tpmcp::notebook::Severity;
    assert!(findings.iter().all(|f| f.severity == Severity::Info));
}

#[test]
fn check_is_clean_on_valid_fixtures() {
    for name in [
        "emoji.py",
        "toplevel.py",
        "generate.py",
        "empty.py",
        "no_cells.py",
    ] {
        let nb = open(&fixture(name)).unwrap();
        assert_eq!(nb.check(), Vec::new(), "{name} should be clean");
    }
    // broken.py: the unparsable region is one syntax finding; the intact
    // cell after it is still analyzed (its stored return is stale drift).
    let nb = open(&fixture("broken.py")).unwrap();
    let findings = nb.check();
    assert_eq!(
        codes(&findings),
        vec![("syntax-error", Some(1)), ("stale-return", Some(2))],
        "{findings:?}"
    );
}

/// Cross-check the Error-severity findings against marimo's own graph
/// validator (real marimo, real DirectedGraph, real check_for_errors).
#[test]
fn check_matches_real_marimo_graph_errors() {
    let Some(py) = ground_truth_python() else {
        return;
    };
    let fixtures = [
        "graph_cycle.py",
        "graph_dupedef.py",
        "graph_setup_root.py",
        "graph_stale.py",
        "emoji.py",
        "toplevel.py",
        "generate.py",
    ];
    let dir = std::env::temp_dir().join(format!("tpmcp-graph-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for name in fixtures {
        let path = dir.join(name);
        std::fs::write(&path, fixture(name)).unwrap();
        let output = run_python(&py, GRAPH_CHECK, &[path.to_str().unwrap()]);
        let mut marimo_errors: Vec<(String, String)> = output
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                let (cid, kind) = line.split_once(' ').unwrap();
                (cid.to_string(), kind.to_string())
            })
            .collect();
        marimo_errors.sort();

        let nb = open(&fixture(name)).unwrap();
        let mut ours: Vec<(String, String)> = nb
            .check()
            .iter()
            .filter(|finding| finding.severity == tpmcp::notebook::Severity::Error)
            .flat_map(|finding| {
                let mapped = match finding.code.as_str() {
                    "cycle" => "CycleError",
                    "multiple-definitions" => "MultipleDefinitionError",
                    "setup-root" => "SetupRootError",
                    "syntax-error" => "SyntaxError",
                    other => other,
                };
                // Our finding lists every involved cell in the message; marimo
                // repeats one error per involved cell. Expand ours the same
                // way by matching message cell lists. The setup-root error is
                // reported on the setup cell, which marimo ids as "setup".
                if mapped == "SetupRootError" {
                    return vec![("setup".to_string(), mapped.to_string())];
                }
                let cells: Vec<String> = message_cells(&finding.message);
                if cells.is_empty() {
                    vec![(finding.cell.unwrap().to_string(), mapped.to_string())]
                } else {
                    cells.into_iter().map(|c| (c, mapped.to_string())).collect()
                }
            })
            .collect();
        ours.sort();
        assert_eq!(ours, marimo_errors, "{name}");
    }
}

/// Cell indexes mentioned in a finding message ("cells [0, 1]", "(0, 1)").
fn message_cells(message: &str) -> Vec<String> {
    let mut cells = Vec::new();
    for open in ['[', '('] {
        if let Some(start) = message.find(open)
            && let Some(end) = message[start..].find(if open == '[' { ']' } else { ')' })
        {
            let inner = &message[start + 1..start + end];
            if !inner.is_empty()
                && inner
                    .split(", ")
                    .all(|part| part.chars().all(|c| c.is_ascii_digit()))
            {
                cells.extend(inner.split(", ").map(str::to_string));
            }
        }
    }
    cells
}
