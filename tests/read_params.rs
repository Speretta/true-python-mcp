//! Wire pins for the `notebook_read` parameters: outline, query and
//! diagnostics, query paging messages, UTF-8 truncation boundaries, marimo
//! diagnostics and the degraded lint path.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::{server::Server, temp_dir};
use serde_json::json;

/// G3: truncation lands mid-codepoint and must back off to a char boundary
/// in both the outline row (80 bytes) and the query match line (120 bytes);
/// the results are the exact `floor_char_boundary` prefixes.
#[test]
fn truncation_backs_off_to_char_boundaries() {
    let dir = temp_dir("readparams-utf8");
    let path = dir.join("utf8.py");
    let emoji_line = format!("s = '{}'", "🙂".repeat(100));
    let cjk_line = format!("t = '{}'", "漢".repeat(100));
    let mut server = Server::start(&[]);
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": path, "cells": [
            {"cell_type": "code", "source": format!("{emoji_line}\n")},
            {"cell_type": "code", "source": format!("{cjk_line}\n")}
        ]}),
    );
    assert!(!err, "{text}");

    let (err, text, outline) = server.call(
        "notebook_read",
        json!({"path": path, "detail": "outline", "format": "json"}),
    );
    assert!(!err, "{text}");
    let expected_emoji = &emoji_line[..emoji_line.floor_char_boundary(80)];
    let expected_cjk = &cjk_line[..cjk_line.floor_char_boundary(80)];
    assert_eq!(outline["cells"][0]["first_line"], expected_emoji);
    assert_eq!(outline["cells"][1]["first_line"], expected_cjk);
    // The prefixes end mid-run: the next codepoint would cross the cap.
    assert!(expected_emoji.len() + "🙂".len() > 80, "{expected_emoji}");
    assert!(expected_cjk.len() + "漢".len() > 80, "{expected_cjk}");

    let (err, text, found) = server.call(
        "notebook_search",
        json!({"path": path, "query": "t =", "format": "json"}),
    );
    assert!(!err, "{text}");
    let expected_line = &cjk_line[..cjk_line.floor_char_boundary(120)];
    assert_eq!(found["hits"][0]["matches"][0]["text"], expected_line);
    assert!(expected_line.len() + "漢".len() > 120, "{expected_line}");

    // The markdown views of the same boundaries render (valid UTF-8 by
    // construction in Rust; this pins that no path panics or garbles).
    let (err, text, _) = server.call("notebook_read", json!({"path": path, "detail": "outline"}));
    assert!(!err && text.contains(expected_emoji), "{text}");
    let (err, text, _) = server.call("notebook_search", json!({"path": path, "query": "s ="}));
    assert!(!err, "{text}");
    let emoji_match = &emoji_line[..emoji_line.floor_char_boundary(120)];
    assert!(text.contains(emoji_match), "{text}");
}

/// EC-5: an empty query PAGE (start past the last match, or limit=0) says so
/// instead of claiming no cells match, and keeps the paging steering line.
#[test]
fn empty_query_pages_stay_honest() {
    let dir = temp_dir("readparams-paging");
    let path = dir.join("page.ipynb");
    let mut server = Server::start(&[]);
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": path, "cells": [
            {"cell_type": "code", "source": "import pandas as pd\n"},
            {"cell_type": "markdown", "source": "# Notes about pandas\n"},
            {"cell_type": "code", "source": "other = 1\n"}
        ]}),
    );
    assert!(!err, "{text}");

    // start beyond the match list
    let (err, text, _) = server.call(
        "notebook_search",
        json!({"path": path, "query": "pandas", "start": 99}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("2 cell(s)"), "{text}");
    assert!(text.contains("this page is empty"), "{text}");
    assert!(!text.contains("no cells match"), "{text}");

    // limit=0: empty page, and the steering line reports the full match list
    let (err, text, _) = server.call(
        "notebook_search",
        json!({"path": path, "query": "pandas", "limit": 0}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("this page is empty"), "{text}");
    assert!(text.contains("2 matching cell(s) in total"), "{text}");

    // a genuine no-match still says so
    let (err, text, _) = server.call(
        "notebook_search",
        json!({"path": path, "query": "zzz_missing"}),
    );
    assert!(!err && text.contains("no cells match"), "{text}");

    // window bounds clamp instead of overflowing
    let (err, text, window) = server.call(
        "notebook_read",
        json!({"path": path, "start": u64::MAX, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(window["range"], json!([3, 3]), "{window}");
    assert!(window["cells"].as_array().unwrap().is_empty(), "{window}");
    let (err, text, _) = server.call("notebook_read", json!({"path": path, "start": u64::MAX}));
    assert!(!err && text.contains("(no cells in range)"), "{text}");
    let (err, text, window) = server.call(
        "notebook_read",
        json!({"path": path, "limit": u64::MAX, "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(window["cells"].as_array().unwrap().len(), 3, "{window}");
}

/// G2: diagnostics=check+lint on marimo .py over the wire — graph findings
/// and file-level ruff lint in one response, on both an intact and a broken
/// marimo file (the broken one degrades, it does not fail the read).
#[test]
fn marimo_diagnostics_over_the_wire() {
    let mut server = Server::start(&[]);

    let (err, text, found) = server.call(
        "notebook_read",
        json!({"path": "tests/data/marimo/graph_cycle.py",
               "diagnostics": "check+lint", "format": "json"}),
    );
    assert!(!err, "{text}");
    let findings = found["findings"].as_array().unwrap();
    assert!(
        findings.iter().any(|finding| finding["code"] == "cycle"),
        "{found}"
    );
    assert!(
        found["lint"].is_array(),
        "the lint half rides along: {found}"
    );
    assert!(
        found["revision"].as_str().is_some(),
        "reads still chain: {found}"
    );

    let (err, text, found) = server.call(
        "notebook_read",
        json!({"path": "tests/data/marimo/broken.py",
               "diagnostics": "check+lint", "format": "json"}),
    );
    assert!(!err, "{text}");
    let findings = found["findings"].as_array().unwrap();
    assert!(
        findings
            .iter()
            .any(|finding| finding["code"] == "syntax-error"),
        "{found}"
    );
    // The broken cell is skipped by the per-cell lint (it has no valid body);
    // its syntax error is surfaced by the graph finding above, not by ruff.
    assert!(
        found["lint"].as_array().unwrap().is_empty(),
        "the broken cell has no lintable body: {found}"
    );

    // EC-8: the search view labels marimo block kinds like every other view.
    let (err, text, found) = server.call(
        "notebook_search",
        json!({"path": "tests/data/marimo/broken.py", "query": "def _", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(found["hits"][0]["kind"], "broken", "{found}");
    let (err, text, _) = server.call(
        "notebook_search",
        json!({"path": "tests/data/marimo/broken.py", "query": "def _"}),
    );
    assert!(!err && text.contains("(broken)"), "{text}");
    // ... and the outline JSON carries it as well.
    let (err, text, outline) = server.call(
        "notebook_read",
        json!({"path": "tests/data/marimo/broken.py", "detail": "outline", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(outline["cells"][1]["kind"], "broken", "{outline}");
}

/// EC-3: when ruff's parser is stricter than ours (schema_issues.ipynb),
/// check+lint degrades to check findings plus a lint-unavailable notice
/// instead of failing the read; a lintable file is unaffected.
#[test]
fn unavailable_lint_degrades_instead_of_failing_the_read() {
    let mut server = Server::start(&[]);

    let (err, text, _) = server.call(
        "notebook_read",
        json!({"path": "tests/data/schema_issues.ipynb",
               "diagnostics": "check+lint", "detail": "outline"}),
    );
    assert!(!err, "the read must survive ruff: {text}");
    assert!(
        text.contains("diagnostics: 9 finding(s), lint unavailable"),
        "{text}"
    );
    assert!(
        text.contains("- lint unavailable: analysis failed"),
        "{text}"
    );

    let (err, text, found) = server.call(
        "notebook_read",
        json!({"path": "tests/data/schema_issues.ipynb",
               "diagnostics": "check+lint", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(found["findings"].as_array().unwrap().len(), 9, "{found}");
    assert!(found["lint"].is_null(), "no lint half: {found}");
    assert!(found["lint_error"].as_str().is_some(), "{found}");

    // A lintable notebook still gets its issues, and no lint_error.
    let (err, text, found) = server.call(
        "notebook_read",
        json!({"path": "tests/data/lint_sample.ipynb",
               "diagnostics": "check+lint", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(found["lint"].as_array().unwrap().len(), 2, "{found}");
    assert!(found.get("lint_error").is_none(), "{found}");

    // The blank-query rejection happens BEFORE any search work.
    let (err, text, _) = server.call(
        "notebook_search",
        json!({"path": "tests/data/lint_sample.ipynb", "query": "  "}),
    );
    assert!(err && text.contains("query must not be empty"), "{text}");
}

/// The wave-3 read parameters over the wire: outline, query, diagnostics.
#[test]
fn read_parameters_outline_query_diagnostics() {
    let dir = temp_dir("readparams");
    let path = dir.join("nb.ipynb");
    let mut server = Server::start(&[]);
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": path, "cells": [
            {"cell_type": "code", "source": "import pandas as pd\nx = 1\n"},
            {"cell_type": "markdown", "source": "# Notes about pandas\n"},
            {"cell_type": "code", "source": "unused_import = os\n"}
        ]}),
    );
    assert!(!err, "{text}");

    // outline: one row per cell with first line and line count, no bodies
    let (err, text, sc) = server.call(
        "notebook_read",
        json!({"path": path, "detail": "outline", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["cells"].as_array().unwrap().len(), 3);
    assert!(
        sc["cells"][0]["source"].is_null(),
        "outline carries no source"
    );
    assert_eq!(sc["cells"][0]["line_count"], 2);
    assert_eq!(sc["cells"][0]["first_line"], "import pandas as pd");
    let (err, text, _) = server.call("notebook_read", json!({"path": path, "detail": "outline"}));
    assert!(!err, "{text}");
    assert!(
        text.contains("### [0] code · 2 lines · import pandas as pd"),
        "{text}"
    );
    assert!(!text.contains("x = 1"), "outline shows no bodies: {text}");

    // query: only matching cells and lines, searched case-insensitively
    let (err, text, sc) = server.call(
        "notebook_search",
        json!({"path": path, "query": "PANDAS", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["matched_cells"], 2);
    assert_eq!(sc["hits"][0]["index"], 0);
    assert_eq!(sc["hits"][0]["matches"][0]["at"], "source:1");
    assert_eq!(sc["hits"][1]["index"], 1, "markdown cells are searched too");
    let (err, text, _) = server.call("notebook_search", json!({"path": path, "query": "pandas"}));
    assert!(!err, "{text}");
    assert!(text.contains("2 cell(s), 2 line(s)"), "{text}");
    assert!(text.contains("source:1: import pandas as pd"), "{text}");
    let (err, text, _) = server.call(
        "notebook_search",
        json!({"path": path, "query": "zzz_missing"}),
    );
    assert!(!err && text.contains("no cells match"), "{text}");
    let (err, text, _) = server.call("notebook_search", json!({"path": path, "query": "  "}));
    assert!(err && text.contains("query must not be empty"), "{text}");

    // diagnostics=check: structural findings ride along, no lint section
    let (err, text, sc) = server.call(
        "notebook_read",
        json!({"path": path, "diagnostics": "check", "detail": "outline", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["findings"].as_array().unwrap().len(), 0, "{sc}");
    assert!(sc["lint"].is_null(), "check alone carries no lint: {sc}");

    // diagnostics=check+lint: ruff findings for the same snapshot
    let (err, text, sc) = server.call(
        "notebook_read",
        json!({"path": "tests/data/lint_sample.ipynb", "diagnostics": "check+lint", "format": "json"}),
    );
    assert!(!err, "{text}");
    let lint = sc["lint"].as_array().unwrap();
    assert_eq!(lint.len(), 2, "{sc}");
    assert_eq!(lint[0]["code"], "F401");
    assert!(sc["revision"].as_str().is_some(), "read still chains: {sc}");

    // markdown rendering of the findings section
    let (err, text, _) = server.call(
        "notebook_read",
        json!({"path": "tests/data/schema_issues.ipynb", "diagnostics": "check", "detail": "outline"}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("diagnostics: 9 finding(s)"), "{text}");
    assert!(text.contains("- error cell 1 `cell-id`:"), "{text}");
}
