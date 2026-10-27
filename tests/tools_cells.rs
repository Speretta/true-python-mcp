//! End-to-end tests that drive the real `tpmcp` binary over the MCP stdio
//! protocol: cell metadata replacement. Sibling of tests/tools.rs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::{server::Server, temp_dir};
use serde_json::{Value, json};
use tpmcp::notebook::JupyterNotebook;

/// Pull the revision out of a markdown tool response.
fn revision_of(text: &str) -> String {
    for line in text.lines() {
        if let Some(revision) = line.strip_prefix("revision: ") {
            return revision.trim().to_string();
        }
    }
    panic!("no revision in output:\n{text}");
}

/// cell_set_metadata over the wire: full replace with the previous object
/// echoed back (markdown and JSON shapes), revision chaining and conflict
/// refusal, restore-from-echo, out-of-range index, and the marimo refusal.
#[test]
fn cell_set_metadata_over_the_wire() {
    let dir = temp_dir("setmeta");
    let path = dir.join("nb.ipynb");
    let mut server = Server::start(&[]);
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": path, "cells": [{"cell_type": "code", "source": "x = 1\n"}]}),
    );
    assert!(!err, "{text}");
    let mut revision = revision_of(&text);

    // Full replace of the empty metadata; the JSON shape echoes it back.
    let tags = json!({"tags": ["parameters"], "jupyter": {"source_hidden": true}});
    let (err, text, sc) = server.call(
        "cell_update",
        json!({"path": path, "cells": [0], "metadata": tags,
               "revision": revision, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["ok"], true);
    assert_eq!(sc["updated"][0]["index"], 0);
    assert_eq!(sc["updated"][0]["previous_metadata"], json!({}));
    revision = sc["revision"].as_str().unwrap().to_string();

    // The markdown shape echoes the previous (now non-empty) metadata as a
    // JSON block, and clears it back to {} — a restore fed by the echo.
    let (err, text, _) = server.call(
        "cell_update",
        json!({"path": path, "cells": [0], "metadata": {}, "revision": revision}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("previous metadata:"), "{text}");
    assert!(text.contains("\"tags\""), "{text}");
    assert!(text.contains("\"parameters\""), "{text}");
    revision = revision_of(&text);

    // On disk: the metadata is replaced, every other field is untouched,
    // and the file stays byte-exact canonical.
    let raw = std::fs::read_to_string(&path).unwrap();
    let parsed: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["cells"][0]["metadata"], json!({}));
    assert_eq!(parsed["cells"][0]["source"], json!(["x = 1\n"]));
    assert_eq!(parsed["cells"][0]["outputs"], json!([]));
    assert_eq!(parsed["cells"][0]["execution_count"], Value::Null);
    assert!(parsed["cells"][0]["id"].as_str().is_some(), "{raw}");
    let notebook = JupyterNotebook::parse(&raw).unwrap();
    assert_eq!(
        notebook.to_string_canonical(),
        raw,
        "file stays byte-exact canonical"
    );

    // A stale revision is refused with the structured conflict error.
    let (err, text, sc) = server.call(
        "cell_update",
        json!({"path": path, "cells": [0], "metadata": tags, "revision": "0000000000000000"}),
    );
    assert!(err, "stale revision must be rejected");
    assert!(text.contains("revision conflict"), "{text}");
    assert_eq!(sc["error"], "conflict");
    assert!(sc["current_revision"].as_str().is_some());

    // An out-of-range index is a tool error, not a write.
    let (err, text, _) = server.call(
        "cell_update",
        json!({"path": path, "cells": [7], "metadata": {}, "revision": revision}),
    );
    assert!(err && text.contains("out of bounds"), "{text}");

    // marimo is refused with an explanation naming where its cell
    // configuration actually lives; the file is not touched.
    let (err, text, _) = server.call(
        "notebook_read",
        json!({"path": "tests/data/marimo/emoji.py"}),
    );
    assert!(!err, "{text}");
    let marimo_revision = revision_of(&text);
    let (err, text, _) = server.call(
        "cell_update",
        json!({"path": "tests/data/marimo/emoji.py", "cells": [0], "metadata": {},
               "revision": marimo_revision}),
    );
    assert!(err, "{text}");
    assert!(
        text.contains("marimo cell configuration lives in code"),
        "{text}"
    );
    assert!(text.contains("Jupyter notebooks only"), "{text}");
}

/// cell_set_type raw <-> code: converting a raw cell to code keeps its
/// source verbatim and adds the schema-required empty outputs/execution_count;
/// converting back to raw removes them again. (The marimo code <-> markdown
/// branch is covered in tests/marimo_markdown.rs; this is the Jupyter raw
/// path.)
#[test]
fn cell_set_type_raw_to_code_and_back() {
    let dir = temp_dir("settype");
    let path = dir.join("nb.ipynb");
    let mut server = Server::start(&[]);
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": path, "cells": [{"cell_type": "raw", "source": "x = 1\n"}]}),
    );
    assert!(!err, "{text}");
    let revision = revision_of(&text);

    // raw -> code: the source becomes the code source, with empty
    // outputs/execution_count per nbformat.
    let (err, text, sc) = server.call(
        "cell_update",
        json!({"path": path, "cells": [0], "cell_type": "code",
               "revision": revision, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["updated"][0]["index"], 0);
    assert_eq!(sc["updated"][0]["cell_type"], "code");
    let revision = sc["revision"].as_str().unwrap().to_string();
    let raw = std::fs::read_to_string(&path).unwrap();
    let parsed: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["cells"][0]["cell_type"], "code");
    assert_eq!(parsed["cells"][0]["source"], json!(["x = 1\n"]));
    assert_eq!(parsed["cells"][0]["outputs"], json!([]));
    assert_eq!(parsed["cells"][0]["execution_count"], Value::Null);

    // code -> raw: the source stays, outputs and execution_count are removed.
    let (err, text, _) = server.call(
        "cell_update",
        json!({"path": path, "cells": [0], "cell_type": "raw", "revision": revision}),
    );
    assert!(!err, "{text}");
    let raw = std::fs::read_to_string(&path).unwrap();
    let parsed: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["cells"][0]["cell_type"], "raw");
    assert_eq!(parsed["cells"][0]["source"], json!(["x = 1\n"]));
    assert!(parsed["cells"][0].get("outputs").is_none(), "{raw}");
    assert!(parsed["cells"][0].get("execution_count").is_none(), "{raw}");
}

/// EC-4 end to end: the cell-level `execution_count` follows nbformat's
/// integer-or-null policy, so notebook_check no longer calls a file valid
/// that nbformat rejects (float spellings, negatives) or that the next
/// tpmcp write would turn into an invalid file (a beyond-u64 integer is
/// re-spelled as a float by the serializer).
#[test]
fn execution_count_policy_over_the_wire() {
    fn notebook_with_count(count: &str) -> String {
        format!(
            r#"{{
 "cells": [
  {{
   "cell_type": "code",
   "execution_count": {count},
   "id": "c0000001",
   "metadata": {{}},
   "outputs": [],
   "source": ["x = 1\n"]
  }}
 ],
 "metadata": {{}},
 "nbformat": 4,
 "nbformat_minor": 5
}}"#
        )
    }

    let dir = temp_dir("execcount");
    let mut server = Server::start(&[]);
    for (name, count, valid) in [
        ("null", "null", true),
        ("integer", "3", true),
        ("u64_max", "18446744073709551615", true),
        ("float", "3.0", false),
        ("negative", "-1", false),
        ("beyond_u64", "18446744073709551616", false),
        ("string", "\"3\"", false),
    ] {
        let path = dir.join(format!("{name}.ipynb"));
        std::fs::write(&path, notebook_with_count(count)).unwrap();
        let (err, text, sc) =
            server.call("notebook_check", json!({"path": path, "format": "json"}));
        assert!(!err, "{text}");
        let codes: Vec<&str> = sc["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|finding| finding["code"].as_str().unwrap())
            .collect();
        if valid {
            assert!(codes.is_empty(), "{name}: {codes:?}");
        } else {
            assert!(codes.contains(&"execution-count"), "{name}: {codes:?}");
        }
    }

    // The EC-4 scenario: a beyond-u64 count survives an edit re-spelled as
    // a float, and the check reports the file as what it now is.
    let path = dir.join("edited.ipynb");
    std::fs::write(&path, notebook_with_count("18446744073709551616")).unwrap();
    let (err, text, _) = server.call("notebook_read", json!({"path": path}));
    assert!(!err, "{text}");
    let revision = revision_of(&text);
    let (err, text, _) = server.call(
        "cell_edit",
        json!({"path": path, "cell": 0, "old_string": "x = 1",
               "new_string": "x = 2", "revision": revision}),
    );
    assert!(!err, "{text}");
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        !raw.contains("18446744073709551616"),
        "the integer literal is gone after the write: {raw}"
    );
    let (err, _, sc) = server.call("notebook_check", json!({"path": path, "format": "json"}));
    assert!(!err);
    let codes: Vec<&str> = sc["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"execution-count"), "{codes:?}");
}

/// Introducing markdown into a marimo notebook that lacks a `mo` definition
/// scaffolds `import marimo as mo` and warns — the cell_insert/cell_set_type
/// twin of the create/convert scaffold (without it the file NameErrors at
/// runtime under real marimo).
#[test]
fn markdown_mutations_scaffold_mo_in_marimo() {
    let dir = temp_dir("mo-scaffold");
    let mut server = Server::start(&[]);

    let path = dir.join("app.py");
    let (err, text, sc) = server.call(
        "notebook_create",
        json!({"path": path, "cells": [{"cell_type": "code", "source": "x = 1\n"}], "format": "json"}),
    );
    assert!(!err, "{text}");
    let revision = sc["revision"].as_str().unwrap().to_string();

    let (err, text, sc) = server.call(
        "cell_insert",
        json!({"path": path, "revision": revision, "cell_type": "markdown",
               "source": "# Title\n", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert!(
        sc["warning"]
            .as_str()
            .unwrap()
            .contains("import marimo as mo"),
        "{sc}"
    );
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("import marimo as mo"), "{content}");

    // A second markdown insert must NOT scaffold again.
    let revision = sc["revision"].as_str().unwrap().to_string();
    let (err, text, sc) = server.call(
        "cell_insert",
        json!({"path": path, "revision": revision, "cell_type": "markdown",
               "source": "## More\n", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert!(sc["warning"].is_null(), "{sc}");

    // cell_set_type to markdown scaffolds the same way.
    let path2 = dir.join("app2.py");
    let (err, text, sc) = server.call(
        "notebook_create",
        json!({"path": path2, "cells": [{"cell_type": "code", "source": "y = 2\n"}], "format": "json"}),
    );
    assert!(!err, "{text}");
    let revision = sc["revision"].as_str().unwrap().to_string();
    let (err, text, sc) = server.call(
        "cell_update",
        json!({"path": path2, "revision": revision, "cells": [0],
               "cell_type": "markdown", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert!(
        sc["warning"]
            .as_str()
            .unwrap()
            .contains("import marimo as mo"),
        "{sc}"
    );
}

/// cell_move error paths and cell_clear_outputs shapes over the wire: an
/// out-of-range `from`/`to` is a caller error (no write), and clearing a
/// single cell reports its index in `cleared_cells`, as does clearing all of
/// a notebook whose cells already have no outputs.
#[test]
fn cell_move_errors_and_clear_outputs_shape() {
    let dir = temp_dir("cellmove");
    let path = dir.join("nb.ipynb");
    let mut server = Server::start(&[]);
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": path, "cells": [
            {"cell_type": "code", "source": "a = 1\n"},
            {"cell_type": "code", "source": "b = 2\n"},
            {"cell_type": "code", "source": "c = 3\n"}
        ]}),
    );
    assert!(!err, "{text}");
    let revision = revision_of(&text);

    // An out-of-range `from` is a caller error, not a write.
    let (err, text, _) = server.call(
        "cell_move",
        json!({"path": path, "from": 3, "to": 0, "revision": revision}),
    );
    assert!(err, "out-of-range from must be rejected");
    assert!(text.contains("cell index 3 out of bounds"), "{text}");

    // `to == len` (the destination is interpreted after removal) is out of
    // bounds too.
    let (err, text, _) = server.call(
        "cell_move",
        json!({"path": path, "from": 0, "to": 3, "revision": revision}),
    );
    assert!(err, "out-of-range to must be rejected");
    assert!(text.contains("destination index 3 out of bounds"), "{text}");

    // cell_clear_outputs single-cell path reports the cleared index in JSON.
    let (err, text, sc) = server.call(
        "cell_update",
        json!({"clear_outputs": true,"path": path, "cells": [1], "revision": revision, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["updated"][0]["index"], 1);
    assert_eq!(sc["updated"][0]["outputs_cleared"], true);
    let revision = sc["revision"].as_str().unwrap().to_string();

    // Clearing a notebook whose cells already have no outputs is a no-op that
    // still reports every code cell's index.
    let (err, text, sc) = server.call(
        "cell_update",
        json!({"clear_outputs": true,"path": path, "revision": revision, "format": "json"}),
    );
    assert!(!err, "{text}");
    let indices: Vec<Value> = sc["updated"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["index"].clone())
        .collect();
    assert_eq!(Value::Array(indices), json!([0, 1, 2]));
}

/// Cells are addressed by index (negative counts from the end), Jupyter id
/// or marimo name — a stable reference survives reordering, and a file
/// whose cells carry neither says so instead of guessing.
#[test]
fn cells_are_addressed_by_id_name_and_signed_index() {
    let dir = temp_dir("cell-refs");
    let path = dir.join("nb.ipynb");
    let mut server = Server::start(&[]);
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": path, "cells": [
            {"cell_type": "code", "source": "a = 1\n"},
            {"cell_type": "code", "source": "b = 2\n"},
            {"cell_type": "code", "source": "c = 3\n"},
        ]}),
    );
    assert!(!err, "{text}");
    let (err, text, read) = server.call("notebook_read", json!({"path": path, "format": "json"}));
    assert!(!err, "{text}");
    let id = read["cells"][2]["id"].as_str().unwrap().to_string();
    let mut revision = read["revision"].as_str().unwrap().to_string();

    // By id: convert the last cell to markdown.
    let (err, text, sc) = server.call(
        "cell_update",
        json!({"path": path, "cells": [id], "cell_type": "markdown",
               "revision": revision, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["updated"][0]["index"], 2);
    revision = sc["revision"].as_str().unwrap().to_string();

    // By negative index: -1 is the same cell, back to code.
    let (err, text, sc) = server.call(
        "cell_update",
        json!({"path": path, "cells": [-1], "cell_type": "code",
               "revision": revision, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["updated"][0]["index"], 2);
    revision = sc["revision"].as_str().unwrap().to_string();

    // An unknown name is an error naming what was looked for.
    let (err, text, _) = server.call(
        "cell_edit",
        json!({"path": path, "cell": "nope", "old_string": "a", "new_string": "a",
               "revision": revision}),
    );
    assert!(err && text.contains("no cell with id or name"), "{text}");

    // marimo: named cells resolve by name, like ids.
    let (err, text, read) = server.call(
        "notebook_read",
        json!({"path": "tests/data/marimo/toplevel.py", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(read["cells"][2]["name"], "addition", "{read}");
    let revision = read["revision"].as_str().unwrap().to_string();
    // A named, editable cell resolves by name (the read-only @app.function
    // blocks are named too — addressing finds them; editing still refuses).
    let (err, text, _) = server.call(
        "cell_update",
        json!({"path": "tests/data/marimo/toplevel.py",
               "cells": ["cell_with_ref_and_def"],
               "cell_type": "code", "revision": revision}),
    );
    assert!(!err, "{text}");
}

/// Batch insert and batch delete: one call adds several cells in order, one
/// call removes several (all references resolve before any removal, so the
/// caller's indexes mean the same notebook). Duplicates are refused.
#[test]
fn batches_insert_and_delete_several_cells_at_once() {
    let dir = temp_dir("cell-batch");
    let path = dir.join("nb.ipynb");
    let mut server = Server::start(&[]);
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": path, "cells": [
            {"cell_type": "code", "source": "keep = 1\n"},
            {"cell_type": "code", "source": "drop_a = 1\n"},
            {"cell_type": "code", "source": "drop_b = 2\n"},
        ]}),
    );
    assert!(!err, "{text}");
    let (err, text, read) = server.call("notebook_read", json!({"path": path, "format": "json"}));
    assert!(!err, "{text}");
    let mut revision = read["revision"].as_str().unwrap().to_string();

    // One call: a markdown header followed by its code cell.
    let (err, text, sc) = server.call(
        "cell_insert",
        json!({"path": path, "index": 0, "cells": [
            {"cell_type": "markdown", "source": "# Section\n"},
            {"cell_type": "code", "source": "section = True\n"},
        ], "revision": revision, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(
        Value::Array(sc["inserted"].as_array().unwrap().clone()),
        json!([
            {"index": 0, "cell_type": "markdown"}, {"index": 1, "cell_type": "code"}
        ])
    );
    revision = sc["revision"].as_str().unwrap().to_string();

    // One call: delete both drop cells together (original indexes).
    let (err, text, sc) = server.call(
        "cell_delete",
        json!({"path": path, "cells": [3, 4], "revision": revision, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["deleted"].as_array().unwrap().len(), 2);
    revision = sc["revision"].as_str().unwrap().to_string();

    // Listing a cell twice is an error, not a double delete.
    let (err, text, _) = server.call(
        "cell_delete",
        json!({"path": path, "cells": [1, 1], "revision": revision}),
    );
    assert!(err && text.contains("listed twice"), "{text}");
}

/// cell_update applies every requested property in one pass: type,
/// metadata and cleared outputs together, echoing what it replaced.
#[test]
fn cell_update_changes_properties_in_one_call() {
    let dir = temp_dir("cell-update");
    let path = dir.join("nb.ipynb");
    let mut server = Server::start(&[]);
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": path, "cells": [{"cell_type": "code", "source": "x = 1\n"}]}),
    );
    assert!(!err, "{text}");
    let (err, text, read) = server.call("notebook_read", json!({"path": path, "format": "json"}));
    assert!(!err, "{text}");
    let mut revision = read["revision"].as_str().unwrap().to_string();

    // All three properties in one call.
    let tags = json!({"tags": ["parameters"]});
    let (err, text, sc) = server.call(
        "cell_update",
        json!({"path": path, "cells": [0], "cell_type": "markdown", "metadata": tags,
               "clear_outputs": true, "revision": revision, "format": "json"}),
    );
    assert!(!err, "{text}");
    let entry = &sc["updated"][0];
    assert_eq!(entry["cell_type"], "markdown");
    assert_eq!(entry["previous_type"], "code");
    assert_eq!(entry["previous_metadata"], json!({}));
    assert_eq!(entry["outputs_cleared"], true);
    revision = sc["revision"].as_str().unwrap().to_string();

    // Type and metadata changes must name their cells; only clearing
    // outputs may target every code cell implicitly.
    let (err, text, _) = server.call(
        "cell_update",
        json!({"path": path, "cell_type": "code", "revision": revision}),
    );
    assert!(err && text.contains("cells is required"), "{text}");
    let (err, text, _) = server.call(
        "cell_update",
        json!({"path": path, "clear_outputs": true, "revision": revision}),
    );
    assert!(!err, "{text}");
}
