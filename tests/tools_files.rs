//! End-to-end tests that drive the real `tpmcp` binary over the MCP
//! stdio protocol: the file and stream tools (`py_read`/`py_write`/
//! `py_edit`, `file_copy`/`file_move`/`file_delete`, and the source_file and
//! from_path stream paths). Sibling of tests/tools.rs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::revision_of;
use common::{server::Server, temp_dir};
use serde_json::json;

#[test]
fn py_file_tools_over_the_wire() {
    let dir = temp_dir("pytools");
    let path = dir.join("a.py");
    let mut server = Server::start(&["--file-tools"]);

    let (err, text, _) = server.call(
        "py_write",
        json!({"path": path, "content": "import os\nx = 1\n"}),
    );
    assert!(!err, "{text}");
    let rev = revision_of(&text);

    let (err, text, _) = server.call("py_read", json!({"path": path}));
    assert!(!err, "{text}");
    assert!(text.contains("import os") && text.contains(&format!("revision: {rev}")));

    let (err, text, _) = server.call(
        "py_edit",
        json!({"path": path, "old_string": "x = 1", "new_string": "x = 2", "revision": rev}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("+x = 2"));
    let rev2 = revision_of(&text);
    assert_ne!(rev, rev2);

    // stale revision rejected
    let (err, text, _) = server.call(
        "py_edit",
        json!({"path": path, "old_string": "x = 2", "new_string": "x = 3", "revision": rev}),
    );
    assert!(err && text.contains("revision conflict"), "{text}");
}

/// The revision/range/replace_all branches the happy-path trio test does not
/// touch: overwrite-with-revision (and its stale conflict), JSON + line-range
/// reads, and py_edit replace_all + non-uniqueness.
#[test]
fn py_file_tools_revision_ranges_and_replace_all() {
    let dir = temp_dir("pytools-branches");
    let path = dir.join("a.py");
    let mut server = Server::start(&["--file-tools"]);

    // py_write create -> revision.
    let (err, text, _) = server.call(
        "py_write",
        json!({"path": path, "content": "x = 1\ny = 2\nx = 1\n"}),
    );
    assert!(!err, "{text}");
    let rev = revision_of(&text);

    // py_read JSON: the full-file shape (revision, line_count, range, content).
    let (err, text, sc) = server.call("py_read", json!({"path": path, "format": "json"}));
    assert!(!err, "{text}");
    assert_eq!(sc["revision"].as_str().unwrap(), rev);
    assert_eq!(sc["line_count"], 3);
    assert_eq!(sc["range"], json!([1, 3]));
    assert_eq!(sc["content"], "x = 1\ny = 2\nx = 1");

    // py_read markdown line range renders only the selected line.
    let (err, text, _) = server.call(
        "py_read",
        json!({"path": path, "line_start": 2, "line_end": 2}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("    2 | y = 2"), "{text}");
    assert!(!text.contains("1 | x = 1"), "{text}");
    assert!(!text.contains("3 | x = 1"), "{text}");

    // py_read JSON line range reflects the slice.
    let (err, _, sc) = server.call(
        "py_read",
        json!({"path": path, "line_start": 1, "line_end": 2, "format": "json"}),
    );
    assert!(!err, "{sc}");
    assert_eq!(sc["range"], json!([1, 2]));
    assert_eq!(sc["content"], "x = 1\ny = 2");

    // py_write overwrite: a revision is required and returns a fresh one.
    let (err, text, _) = server.call(
        "py_write",
        json!({"path": path, "content": "a = 1\n", "revision": rev}),
    );
    assert!(!err, "{text}");
    let rev2 = revision_of(&text);
    assert_ne!(rev, rev2);

    // py_write overwrite with a stale revision is a conflict.
    let (err, text, sc) = server.call(
        "py_write",
        json!({"path": path, "content": "clobber\n", "revision": rev}),
    );
    assert!(err, "stale revision must be rejected");
    assert!(text.contains("revision conflict"), "{text}");
    assert_eq!(sc["error"], "conflict");

    // py_edit replace_all rewrites every occurrence.
    let (err, text, _) = server.call(
        "py_write",
        json!({"path": path, "content": "x = 1\nx = 1\nx = 1\n", "revision": rev2}),
    );
    assert!(!err, "{text}");
    let rev3 = revision_of(&text);
    let (err, text, _) = server.call(
        "py_edit",
        json!({"path": path, "old_string": "x = 1", "new_string": "x = 2",
               "replace_all": true, "revision": rev3}),
    );
    assert!(!err, "{text}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "x = 2\nx = 2\nx = 2\n"
    );

    // py_edit without replace_all on a non-unique match is a caller error.
    let rev4 = revision_of(&text);
    let (err, text, _) = server.call(
        "py_edit",
        json!({"path": path, "old_string": "x = 2", "new_string": "x = 3", "revision": rev4}),
    );
    assert!(err, "non-unique match must be rejected");
    assert!(text.contains("occurs 3 times in file"), "{text}");
}

/// An explicit line range on an empty file is a bounds error, not a silent
/// empty result; the implicit whole-file read stays an empty view.
#[test]
fn py_read_explicit_range_on_empty_file_is_a_bounds_error() {
    let dir = temp_dir("empty-range");
    let path = dir.join("empty.py");
    std::fs::write(&path, "").unwrap();
    let mut server = Server::start(&["--file-tools"]);

    let (err, text, _) = server.call("py_read", json!({"path": path}));
    assert!(!err, "{text}");
    assert!(text.contains("lines: 0"), "{text}");

    let (err, text, _) = server.call(
        "py_read",
        json!({"path": path, "line_start": 1, "line_end": 1}),
    );
    assert!(
        err,
        "an explicit range on an empty file must not silently succeed"
    );
    assert!(text.contains("out of bounds: file has 0 lines"), "{text}");
}

/// The path-reference "stream" capability: `source_file` (cell_insert,
/// cell_set_source) and `from_path` (py_write) read the payload from disk
/// instead of the inline arg, and reject being combined with it.
#[test]
fn stream_path_source_into_cells_and_files() {
    let dir = temp_dir("stream-source");
    let mut server = Server::start(&["--file-tools"]);

    // A sizeable payload written to disk by the host — the agent only ever
    // passes its path.
    let payload = "PAYLOAD_MARKER_".repeat(2000);
    let source_path = dir.join("blob.txt");
    std::fs::write(&source_path, &payload).unwrap();

    // Notebook with one code cell.
    let nb = dir.join("nb.ipynb");
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": nb, "cells": [{"cell_type": "code", "source": "x = 1\n"}]}),
    );
    assert!(!err, "{text}");
    let mut rev = revision_of(&text);

    // cell_insert from a path.
    let (err, text, _) = server.call(
        "cell_insert",
        json!({"path": nb, "cell_type": "code", "source_file": source_path, "revision": rev}),
    );
    assert!(!err, "{text}");
    rev = revision_of(&text);
    let (err, _, sc) = server.call("notebook_read", json!({"path": nb, "format": "json"}));
    assert!(!err, "{sc}");
    assert_eq!(sc["cells"][1]["source"], payload);

    // cell_set_source from a path overwrites cell 0 wholesale.
    let (err, text, _) = server.call(
        "cell_set_source",
        json!({"path": nb, "cell": 0, "source_file": source_path, "revision": rev}),
    );
    assert!(!err, "{text}");
    rev = revision_of(&text);
    let (err, _, sc) = server.call("notebook_read", json!({"path": nb, "format": "json"}));
    assert!(!err, "{sc}");
    assert_eq!(sc["cells"][0]["source"], payload);

    // py_write from a path.
    let dest = dir.join("out.py");
    let (err, text, _) = server.call("py_write", json!({"path": dest, "from_path": source_path}));
    assert!(!err, "{text}");
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), payload);

    // cell_insert with neither source nor source_file is an empty cell.
    let (err, text, _) = server.call(
        "cell_insert",
        json!({"path": nb, "cell_type": "code", "revision": rev}),
    );
    assert!(!err, "empty source is valid for cell_insert: {text}");

    // Mutually exclusive: both inline and path.
    let (err, text, sc) = server.call(
        "cell_set_source",
        json!({"path": nb, "cell": 0, "source": "inline", "source_file": source_path,
               "revision": rev}),
    );
    assert!(err && sc["error"] == "invalid_input", "{sc}");
    assert!(text.contains("mutually exclusive"), "{text}");

    let (err, text, _) = server.call(
        "cell_insert",
        json!({"path": nb, "cell_type": "code", "source": "inline",
               "source_file": source_path, "revision": rev}),
    );
    assert!(err && text.contains("mutually exclusive"), "{text}");

    let (err, text, _) = server.call(
        "py_write",
        json!({"path": dir.join("both.py"), "content": "inline", "from_path": source_path}),
    );
    assert!(err && text.contains("mutually exclusive"), "{text}");

    // Required: cell_set_source and py_write refuse neither-param calls.
    let (err, text, _) = server.call(
        "cell_set_source",
        json!({"path": nb, "cell": 0, "revision": rev}),
    );
    assert!(
        err && text.contains("provide either source or source_file"),
        "{text}"
    );

    let (err, text, _) = server.call("py_write", json!({"path": dir.join("neither.py")}));
    assert!(
        err && text.contains("provide either content or from_path"),
        "{text}"
    );
}

/// `file_copy` moves bytes source→dest through tpmcp (binary-safe, chunked,
/// size-capped) and returns the dest revision.
#[test]
fn file_copy_over_the_wire() {
    let dir = temp_dir("file-copy");
    let mut server = Server::start(&["--file-tools"]);

    // Binary source: file_copy moves raw bytes, not just UTF-8 text.
    let payload: Vec<u8> = (0_u8..=255).cycle().take(4096).collect();
    let source = dir.join("blob.bin");
    std::fs::write(&source, &payload).unwrap();

    let dest = dir.join("copy.bin");
    let (err, text, sc) = server.call(
        "file_copy",
        json!({"source": source, "dest": dest, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["bytes"], payload.len());
    assert_eq!(sc["revision"].as_str().unwrap().len(), 16);
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
    let current = sc["revision"].as_str().unwrap().to_string();

    // The revision guard: a stale `revision` refuses to clobber `dest`
    // (leaving it untouched), the current one copies through, and omitting
    // it overwrites unconditionally.
    let (err, _, sc) = server.call(
        "file_copy",
        json!({"source": source, "dest": dest, "revision": "0000000000000000", "format": "json"}),
    );
    assert!(err, "a stale revision must be refused: {sc}");
    assert_eq!(std::fs::read(&dest).unwrap(), payload, "dest untouched");
    let (err, text, _) = server.call(
        "file_copy",
        json!({"source": source, "dest": dest, "revision": current, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert!(source.exists(), "a copy keeps the source");

    // The markdown summary names both paths.
    let (err, text, _) = server.call(
        "file_copy",
        json!({"source": source, "dest": dir.join("copy2.bin")}),
    );
    assert!(!err, "{text}");
    assert!(
        text.contains("copied") && text.contains("blob.bin"),
        "{text}"
    );

    // The size cap is honored on the source.
    let huge = dir.join("huge.bin");
    {
        let file = std::fs::File::create(&huge).unwrap();
        file.set_len(33 * 1024 * 1024).unwrap();
    }
    let (err, _, sc) = server.call(
        "file_copy",
        json!({"source": huge, "dest": dir.join("huge-copy.bin")}),
    );
    assert!(err && sc["error"] == "too_large", "{sc}");
}

/// `file_move` is `file_copy` plus source removal: the bytes land at `dest`
/// and the source is gone. A refused move (stale revision) leaves both sides
/// untouched; a clean move reports `source_removed: true`.
#[test]
fn file_move_over_the_wire() {
    let dir = temp_dir("file-move");
    let mut server = Server::start(&["--file-tools"]);

    let payload: Vec<u8> = (0_u8..=255).cycle().take(4096).collect();
    let source = dir.join("blob.bin");
    std::fs::write(&source, &payload).unwrap();
    let dest = dir.join("moved.bin");

    // A stale revision refuses the move and removes nothing.
    std::fs::write(&dest, b"old bytes").unwrap();
    let (err, _, sc) = server.call(
        "file_move",
        json!({"source": source, "dest": dest, "revision": "0000000000000000", "format": "json"}),
    );
    assert!(err, "a stale revision must be refused: {sc}");
    assert!(source.exists(), "a refused move must keep the source");
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        b"old bytes",
        "dest untouched"
    );

    // The clean move: bytes at dest, source gone, note absent.
    let (err, text, sc) = server.call(
        "file_move",
        json!({"source": source, "dest": dest, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["bytes"], payload.len());
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
    assert!(!source.exists(), "a move removes the source");
    assert_eq!(sc["source_removed"], true, "{sc}");
    assert!(sc["note"].is_null(), "a clean move carries no note: {sc}");

    // The markdown summary says "moved" and names both paths.
    std::fs::write(&source, b"again").unwrap();
    let (err, text, _) = server.call(
        "file_move",
        json!({"source": source, "dest": dir.join("moved2.bin")}),
    );
    assert!(!err, "{text}");
    assert!(
        text.contains("moved") && text.contains("blob.bin"),
        "{text}"
    );
}

/// `file_delete` removes a file and reports the revision it removed. The
/// optional revision guard refuses to delete anything but that exact file.
#[test]
fn file_delete_over_the_wire() {
    let dir = temp_dir("file-delete");
    let mut server = Server::start(&["--file-tools"]);

    let path = dir.join("gone.bin");
    std::fs::write(&path, b"to be deleted").unwrap();

    // A stale revision refuses the delete and keeps the file.
    let (err, _, sc) = server.call(
        "file_delete",
        json!({"path": path, "revision": "0000000000000000", "format": "json"}),
    );
    assert!(err, "a stale revision must be refused: {sc}");
    assert!(path.exists(), "a refused delete must keep the file");

    // The clean delete: the file is gone and the answer names what it removed.
    let (err, text, sc) = server.call("file_delete", json!({"path": path, "format": "json"}));
    assert!(!err, "{text}");
    assert!(!path.exists());
    assert_eq!(sc["deleted"], true, "{sc}");
    assert_eq!(sc["revision"].as_str().unwrap().len(), 16, "{sc}");
    assert!(text.contains("deleted"), "{text}");

    // Deleting a missing file fails like `rm`.
    let (err, _, sc) = server.call("file_delete", json!({"path": path}));
    assert!(err && sc["error"] == "not_found", "{sc}");
}
