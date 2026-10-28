//! End-to-end tests that drive the real `tpmcp` binary over the MCP
//! stdio protocol: the `notebook_read` views and `notebook_search`.
//! Sibling of tests/tools.rs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::{server::Server, temp_dir};
use serde_json::json;

/// `with_outputs` renders cell outputs only when requested: `full` returns
/// the raw payload array (JSON) and the payload text (markdown), `summary`
/// collapses them to a one-line string, and `none` omits them entirely. A
/// read stays byte-safe: the on-disk notebook is never rewritten.
#[test]
fn notebook_read_with_outputs() {
    let dir = temp_dir("read-outputs");
    let path = dir.join("outputs.ipynb");
    let source = r##"{
 "cells": [
  {
   "cell_type": "code",
   "execution_count": 1,
   "id": "a1",
   "metadata": {},
   "outputs": [
    {"output_type": "stream", "name": "stdout", "text": ["hello from stdout\n"]},
    {"output_type": "execute_result", "execution_count": 1, "metadata": {},
     "data": {"text/plain": "42"}}
   ],
   "source": ["print('hello')\n"]
  }
 ],
 "metadata": {},
 "nbformat": 4,
 "nbformat_minor": 5
}"##;
    std::fs::write(&path, source).unwrap();
    let mut server = Server::start(&[]);

    // full JSON: the raw outputs array is present on the code cell.
    let (err, text, sc) = server.call(
        "notebook_read",
        json!({"path": path, "with_outputs": "full", "format": "json"}),
    );
    assert!(!err, "{text}");
    let outputs = sc["cells"][0]["outputs"].as_array().unwrap();
    assert_eq!(outputs.len(), 2, "{sc}");
    assert_eq!(outputs[0]["output_type"], "stream");
    assert_eq!(outputs[0]["name"], "stdout");
    assert_eq!(outputs[1]["output_type"], "execute_result");
    assert_eq!(outputs[1]["data"]["text/plain"], "42");

    // full markdown renders the payload text.
    let (err, text, _) = server.call(
        "notebook_read",
        json!({"path": path, "with_outputs": "full"}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("outputs(2):"), "{text}");
    assert!(text.contains("stream(stdout):"), "{text}");
    assert!(text.contains("hello from stdout"), "{text}");
    assert!(text.contains("42"), "{text}");

    // summary JSON collapses to a one-line string, not the raw array.
    let (err, text, sc) = server.call(
        "notebook_read",
        json!({"path": path, "with_outputs": "summary", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(
        sc["cells"][0]["outputs"],
        "stream(stdout), execute_result[text/plain]"
    );

    // none omits outputs entirely: no JSON "outputs" key, no markdown section.
    let (err, text, sc) = server.call(
        "notebook_read",
        json!({"path": path, "with_outputs": "none", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert!(sc["cells"][0].get("outputs").is_none(), "{sc}");
    let (err, text, _) = server.call(
        "notebook_read",
        json!({"path": path, "with_outputs": "none"}),
    );
    assert!(!err, "{text}");
    assert!(!text.contains("outputs("), "{text}");
    assert!(!text.contains("hello from stdout"), "{text}");

    // Reads never mutate: the on-disk notebook is byte-identical.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
}

/// marimo notebooks read through the same tool, and the outline labels
/// their special blocks.
#[test]
fn marimo_outline_labels_blocks() {
    let mut server = Server::start(&[]);
    let (err, text, _) = server.call(
        "notebook_read",
        json!({"path": "tests/data/marimo/graph_setup_root.py", "detail": "outline"}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("(setup)"), "{text}");
    assert!(text.contains("marimo"), "{text}");
}

/// A file that is not a notebook is not a read failure: it comes back as raw
/// numbered lines under an explicit fallback marker, with the revision and
/// an honest note about what does not apply. This is how a caller without
/// file tools verifies a rendered .md export.
#[test]
fn notebook_read_falls_back_to_raw_text() {
    let dir = temp_dir("read-fallback");
    let mut server = Server::start(&[]);
    let path = dir.join("notes.md");
    std::fs::write(&path, "# C1\nfirst line\nsecond line\n").unwrap();

    let (err, text, sc) = server.call("notebook_read", json!({"path": path, "format": "json"}));
    assert!(!err, "{text}");
    assert_eq!(sc["fallback"], true, "{sc}");
    assert_eq!(sc["revision"].as_str().unwrap().len(), 16, "{sc}");
    assert!(
        sc["reason"]
            .as_str()
            .unwrap()
            .contains("not a recognized notebook"),
        "{sc}"
    );
    assert_eq!(sc["total_lines"], 3, "{sc}");
    assert_eq!(sc["lines"][0]["text"], "# C1", "{sc}");
    assert_eq!(sc["lines"][0]["line"], 1, "{sc}");
    let note = sc["note"].as_str().unwrap();
    assert!(note.contains("not notebook cells"), "{note}");

    // Markdown carries the same disclosure and numbered lines.
    let (err, text, _) = server.call("notebook_read", json!({"path": path}));
    assert!(!err, "{text}");
    assert!(text.contains("fallback: raw text"), "{text}");
    assert!(text.contains("1: # C1"), "{text}");
}

/// In fallback mode query filters lines (case-insensitive substring, the
/// cell-search semantics) and start/limit page through the matches.
#[test]
fn notebook_read_fallback_query_pages_lines() {
    let dir = temp_dir("read-fallback-query");
    let mut server = Server::start(&[]);
    let path = dir.join("big.txt");
    let body: String = (1..=10)
        .map(|n| format!("line {n} needle {}\n", n % 2))
        .collect();
    std::fs::write(&path, body).unwrap();

    let (err, text, sc) = server.call(
        "notebook_search",
        json!({"path": path, "query": "NEEDLE 1", "limit": 2, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["matched_lines"], 5, "five lines carry 'needle 1': {sc}");
    assert_eq!(
        sc["lines"].as_array().unwrap().len(),
        2,
        "limit pages: {sc}"
    );
    assert_eq!(sc["truncated"], true, "{sc}");

    // A corrupt .ipynb falls back too, with the parse error as the reason.
    let broken = dir.join("broken.ipynb");
    std::fs::write(&broken, "{not json").unwrap();
    let (err, text, sc) = server.call("notebook_read", json!({"path": broken, "format": "json"}));
    assert!(!err, "{text}");
    assert_eq!(sc["fallback"], true, "{sc}");
    assert!(sc["reason"].as_str().unwrap().len() > 10, "{sc}");
}

/// A notebook whose stored outputs include an execution error: only_failed
/// answers "which cells dropped and why" (ename/evalue + capped traceback),
/// search_scope=output finds matches inside stored outputs while the source
/// scope does not see them, and marimo (which stores no outputs) says so
/// instead of pretending nothing failed.
#[test]
fn notebook_search_finds_failures_and_output_matches() {
    let dir = temp_dir("search-outputs");
    let path = dir.join("failed.ipynb");
    let fixture = r#"{
 "cells": [
  {"cell_type": "code", "execution_count": 1, "id": "err1", "metadata": {},
   "outputs": [{"output_type": "error", "ename": "ValueError",
                "evalue": "boom went the dynamite",
                "traceback": ["Traceback (most recent call last):",
                              "ValueError: boom went the dynamite"]}],
   "source": ["fail_here()\n"]},
  {"cell_type": "code", "execution_count": 2, "id": "out2", "metadata": {},
   "outputs": [{"output_type": "stream", "name": "stdout",
                "text": ["printed boom in output\n"]}],
   "source": ["print('clean')\n"]}
 ],
 "metadata": {}, "nbformat": 4, "nbformat_minor": 5
}"#;
    std::fs::write(&path, fixture).unwrap();
    let mut server = Server::start(&[]);

    // Which cells dropped, and why.
    let (err, text, sc) = server.call(
        "notebook_search",
        json!({"path": path, "only_failed": true, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["matched_cells"], 1, "{sc}");
    assert_eq!(sc["hits"][0]["id"], "err1");
    assert_eq!(sc["hits"][0]["error"]["ename"], "ValueError", "{sc}");
    assert_eq!(sc["hits"][0]["error"]["evalue"], "boom went the dynamite");
    assert!(
        !sc["hits"][0]["error"]["traceback"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // Outputs are searchable when the scope says so...
    let (err, text, sc) = server.call(
        "notebook_search",
        json!({"path": path, "query": "boom", "search_scope": "output", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["matched_cells"], 2, "the traceback and the stream: {sc}");
    // ...and invisible to a source-scope search.
    let (err, _, sc) = server.call(
        "notebook_search",
        json!({"path": path, "query": "boom", "format": "json"}),
    );
    assert!(!err);
    assert_eq!(sc["matched_cells"], 0, "{sc}");
    let (err, _, sc) = server.call(
        "notebook_search",
        json!({"path": path, "query": "fail_here", "format": "json"}),
    );
    assert!(!err);
    assert_eq!(sc["matched_cells"], 1, "{sc}");

    // marimo stores no outputs: the note says so instead of a clean lie.
    let (err, text, sc) = server.call(
        "notebook_search",
        json!({"path": "tests/data/marimo/toplevel.py", "only_failed": true,
               "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["matched_cells"], 0, "{sc}");
    let note = sc["note"].as_str().unwrap_or_default();
    assert!(note.contains("store no outputs"), "{sc}");
}

/// notebook_read caps rendered output payloads per output in full mode;
/// the truncation names the knob that widens it.
#[test]
fn notebook_read_output_limit_truncates_with_a_note() {
    let dir = temp_dir("output-limit");
    let path = dir.join("loud.ipynb");
    let long = "x".repeat(500);
    let fixture = format!(
        r#"{{
 "cells": [
  {{"cell_type": "code", "execution_count": 1, "id": "loud", "metadata": {{}},
   "outputs": [{{"output_type": "stream", "name": "stdout", "text": ["{long}\n"]}}],
   "source": ["print('loud')\n"]}}
 ],
 "metadata": {{}}, "nbformat": 4, "nbformat_minor": 5
}}"#
    );
    std::fs::write(&path, fixture).unwrap();
    let mut server = Server::start(&[]);

    let (err, text, _) = server.call(
        "notebook_read",
        json!({"path": path, "detail": "full", "with_outputs": "full", "output_limit": 64}),
    );
    assert!(!err, "{text}");
    assert!(
        text.contains("truncated (") && text.contains("bytes total; raise output_limit"),
        "{text}"
    );
    assert!(
        !text.contains(&"x".repeat(300)),
        "the payload must be capped: {text}"
    );
    let (err, text, _) = server.call(
        "notebook_read",
        json!({"path": path, "detail": "full", "with_outputs": "full", "output_limit": 4096}),
    );
    assert!(!err, "{text}");
    assert!(!text.contains("truncated"), "{text}");
}
