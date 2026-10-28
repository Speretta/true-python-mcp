//! Adversarial property tests for the parse/serialize core, complementary
//! to the corpus-pinned frozen suites (roundtrip.rs, check.rs): structured
//! generators (arbitrary notebook JSON, mutations of real fixtures,
//! synthetic marimo scaffolds) plus raw bytes for the rejection paths.
//! Counterexamples shrink and persist under
//! `tests/property.proptest-regressions`.
//!
//! The seven properties are the model's byte-level contracts:
//! 1. Jupyter canonical form is idempotent: anything that parses serializes
//!    to a canonical form that re-parses and re-serializes identically.
//! 2. marimo serialization is the identity for every recognized file,
//!    including scanner-fallback files with broken cells.
//! 3. Editing a marimo file never corrupts it: a refused edit leaves the
//!    bytes untouched; an accepted edit yields a file that parses again
//!    with every unedited cell's source preserved.
//! 4. Editing a Jupyter notebook never corrupts it: the file parses again,
//!    the cell count survives, every unedited cell keeps its full JSON
//!    identity (source, outputs, execution_count, metadata, ids), and the
//!    edited cell round-trips exactly the source it was given.
//! 5. `check()` never panics on any recognized notebook.
//! 6. Format detection and parsing of arbitrary bytes and text never panic.
//! 7. `ty_check` never panics on arbitrary bounded bytes.
//!
//! The synthetic marimo alphabet deliberately omits the characters that
//! spell scaffolding keywords (`@`, `(`, letters beyond a/B), so generated
//! bodies stress escapes, EOLs and UTF-8 boundaries without being able to
//! forge new cell boundaries; realistic keyword content comes from the
//! fixture-mutation generator instead.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom

use proptest::prelude::*;
use proptest::test_runner::{FileFailurePersistence, TestCaseError, TestRunner};
use serde_json::{Map, Value, json};
use tpmcp::notebook::{AnyNotebook, JupyterNotebook, Notebook, open};

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

/// Cases per property: the five properties run in well under a second
/// total at this size, so the budget is generous.
const CASES: u32 = 256;

/// The cases config. Failing seeds persist to
/// `tests/property.proptest-regressions` (the nightly bot commits them), and
/// naming the resolution rule directly keeps the CI log honest: the default
/// `SourceParallel` reaches the same file only through its `lib.rs`/`main.rs`
/// fallback and warns on every run as if persistence had failed.
fn config(cases: u32) -> ProptestConfig {
    ProptestConfig {
        failure_persistence: Some(Box::new(FileFailurePersistence::WithSource(
            "proptest-regressions",
        ))),
        ..ProptestConfig::with_cases(cases)
    }
}

/// Characters that stress JSON escaping, EOL handling and UTF-8 boundaries.
const TRICKY_CHARS: [char; 15] = [
    'a', 'B', ' ', '\n', '\r', '\t', '"', '\\', '\u{0}', 'é', '😀', '\u{2028}', '%', '#', ':',
];

fn arb_text(max_chars: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(TRICKY_CHARS.to_vec()), 0..max_chars)
        .prop_map(|characters| characters.into_iter().collect())
}

/// Arbitrary f64s.
///
/// serde_json's `float_roundtrip` feature (Cargo.toml) makes the parser
/// read every spelling — zmij's, which matches Python's `repr` — back
/// exactly. Without it, ~30% of full-precision values parse 1 ULP off and
/// canonical output drifts one ULP per write, silently breaking the
/// normalize-once-then-stable contract. The parse→canonicalize idempotence
/// property below — together with the checked-in regression seed
/// `-8.087848827836297e-215` — fails immediately if the feature is ever
/// removed, so no generator filter is needed anymore.
fn arb_exact_float() -> impl Strategy<Value = f64> {
    any::<f64>()
}

/// Arbitrary JSON value trees (bounded depth) for metadata and passthrough
/// fields. Non-finite floats cannot exist in `Value` (they map to null).
fn arb_json_value(depth: u32) -> BoxedStrategy<Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        arb_exact_float().prop_map(Value::from),
        arb_text(12).prop_map(Value::String),
    ];
    if depth == 0 {
        return leaf.boxed();
    }
    let inner = depth - 1;
    prop_oneof![
        3 => leaf,
        1 => prop::collection::vec(arb_json_value(inner), 0..3).prop_map(Value::Array),
        1 => prop::collection::vec((arb_text(8), arb_json_value(inner)), 0..3)
            .prop_map(|entries| Value::Object(entries.into_iter().collect())),
    ]
    .boxed()
}

/// One Jupyter cell object: usually well-formed, sometimes adversarial
/// (unknown types, wrong field types, exotic execution counts).
fn arb_cell() -> impl Strategy<Value = Value> {
    let cell_type = prop_oneof![
        3 => Just(json!("code")),
        1 => Just(json!("markdown")),
        1 => Just(json!("raw")),
        1 => arb_text(6).prop_map(Value::String),
        1 => Just(json!(7)),
    ];
    let source = prop_oneof![
        3 => arb_text(60).prop_map(Value::String),
        2 => prop::collection::vec(arb_text(20).prop_map(Value::String), 0..4)
            .prop_map(Value::Array),
        1 => Just(Value::Null),
        1 => Just(json!(3.5)),
    ];
    let execution_count = prop_oneof![
        Just(Value::Null),
        (0..1000u64).prop_map(Value::from),
        Just(json!(3.0)),
        Just(json!(-1)),
        Just(json!(1.8446744073709552e19)),
        Just(json!("3")),
    ];
    (
        cell_type,
        source,
        execution_count,
        arb_json_value(2),
        prop::bool::ANY,
        prop::bool::ANY,
        arb_json_value(1),
    )
        .prop_map(
            |(cell_type, source, execution_count, metadata, with_id, with_outputs, surprise)| {
                let mut cell = Map::new();
                cell.insert("cell_type".into(), cell_type);
                cell.insert("source".into(), source);
                cell.insert("metadata".into(), metadata);
                cell.insert("execution_count".into(), execution_count);
                if with_id {
                    cell.insert("id".into(), json!("ab12cd34"));
                }
                if with_outputs {
                    cell.insert("outputs".into(), json!([]));
                }
                cell.insert("surprise".into(), surprise);
                Value::Object(cell)
            },
        )
}

/// A whole .ipynb document as text: usually structurally valid, sometimes
/// with version fields nbformat rejects.
fn arb_notebook_text() -> impl Strategy<Value = String> {
    let nbformat = prop_oneof![
        4 => Just(json!(4)),
        1 => Just(json!("4")),
        1 => Just(json!(4.0)),
        1 => Just(json!(5)),
    ];
    let minor = prop_oneof![
        2 => Just(json!(5)),
        1 => Just(json!(4)),
        1 => Just(json!(5.0)),
        1 => Just(Value::Null),
    ];
    (
        prop::collection::vec(arb_cell(), 0..5),
        arb_json_value(1),
        nbformat,
        minor,
    )
        .prop_map(|(cells, metadata, nbformat, minor)| {
            json!({
                "cells": cells,
                "metadata": metadata,
                "nbformat": nbformat,
                "nbformat_minor": minor,
            })
            .to_string()
        })
}

/// Real fixture text with a random insertion replacing a random span at
/// char-boundary-safe offsets: realistic keyword content, adversarial shape.
fn arb_fixture_mutation() -> impl Strategy<Value = String> {
    let fixtures = vec![
        include_str!("data/vscode_sample.ipynb"),
        include_str!("data/nbformat_rich.ipynb"),
        include_str!("data/noncanonical.ipynb"),
        include_str!("data/exotic_numbers.ipynb"),
        include_str!("data/schema_issues.ipynb"),
        include_str!("data/crlf_structured.ipynb"),
        include_str!("data/marimo/emoji.py"),
        include_str!("data/marimo/col0_string.py"),
        include_str!("data/marimo/broken.py"),
        include_str!("data/marimo/toplevel.py"),
        include_str!("data/marimo/tutorials/intro.py"),
        include_str!("data/marimo/tutorials/dataflow.py"),
    ];
    (
        prop::sample::select(fixtures),
        arb_text(12),
        any::<usize>(),
        0..64usize,
    )
        .prop_map(|(fixture, insertion, at, span)| {
            let start = fixture.floor_char_boundary(at % (fixture.len() + 1));
            let end = fixture.floor_char_boundary((start + span).min(fixture.len()));
            format!("{}{}{}", &fixture[..start], insertion, &fixture[end..])
        })
}

/// A synthetic marimo scaffold with arbitrary cell bodies, optionally a
/// canonical markdown cell and a syntactically broken cell (which forces
/// the scanner-fallback path).
fn arb_synthetic_marimo() -> impl Strategy<Value = String> {
    (
        prop::collection::vec(arb_text(40), 1..4),
        prop::bool::ANY,
        prop::bool::ANY,
    )
        .prop_map(|(bodies, with_markdown, with_broken)| {
            let mut file =
                String::from("import marimo\n\n__generated_with = \"0.25.1\"\napp = marimo.App()\n");
            if with_markdown {
                file.push_str("\n\n\n@app.cell(hide_code=True)\ndef _(mo):\n    mo.md(r\"\"\"hello\"\"\")\n    return");
            }
            for body in &bodies {
                file.push_str("\n\n\n@app.cell\ndef _():\n");
                for line in body.split('\n') {
                    if line.is_empty() {
                        file.push('\n');
                    } else {
                        file.push_str("    ");
                        file.push_str(line);
                        file.push('\n');
                    }
                }
                file.push_str("    return");
            }
            if with_broken {
                file.push_str("\n\n\n@app.cell\ndef _(:\n    x = 1");
            }
            file.push_str("\n\n\nif __name__ == \"__main__\":\n    app.run()\n");
            file
        })
}

/// marimo-shaped input: synthetic scaffolds, verbatim fixtures, mutated
/// fixtures, and arbitrary text (usually unrecognized — the properties
/// filter on the marimo arm).
fn arb_marimo_text() -> impl Strategy<Value = String> {
    let fixtures = vec![
        include_str!("data/marimo/emoji.py"),
        include_str!("data/marimo/generate.py"),
        include_str!("data/marimo/empty.py"),
        include_str!("data/marimo/no_cells.py"),
        include_str!("data/marimo/graph_cycle.py"),
        include_str!("data/marimo/tutorials/markdown.py"),
        include_str!("data/marimo/tutorials/sql.py"),
    ];
    prop_oneof![
        3 => arb_synthetic_marimo(),
        2 => prop::sample::select(fixtures).prop_map(str::to_string),
        2 => arb_fixture_mutation(),
        1 => arb_text(300),
    ]
}

/// A notebook cell's full JSON identity: its type, its source text, and every
/// extra field (outputs, execution_count, metadata, id) — whole-cell equality
/// for the edit property. Nothing outside `cell_type`/`source` may move when
/// another cell is edited.
fn jupyter_fingerprint(
    notebook: &JupyterNotebook,
) -> Vec<(String, String, BTreeMap<String, Value>)> {
    notebook
        .cells()
        .iter()
        .map(|cell| {
            (
                cell.cell_type.clone(),
                cell.source.text().into_owned(),
                cell.extra.clone(),
            )
        })
        .collect()
}

/// Arbitrary raw input: lossy-decoded random bytes and adversarial text.
fn arb_raw_text() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::collection::vec(any::<u8>(), 0..2048)
            .prop_map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
        arb_text(2048),
    ]
}

proptest! {
    #![proptest_config(config(CASES))]

    /// Property 1: the canonical form is a fixpoint — canonical output
    /// always re-parses and re-serializes to identical bytes.
    #[test]
    fn jupyter_canonical_form_is_idempotent(
        text in prop_oneof![3 => arb_notebook_text(), 1 => arb_fixture_mutation()]
    ) {
        if let Ok(notebook) = JupyterNotebook::parse(&text) {
            let canonical = notebook.to_string_canonical();
            let reparsed = JupyterNotebook::parse(&canonical)
                .expect("canonical output must always re-parse");
            prop_assert_eq!(&reparsed.to_string_canonical(), &canonical);
        }
    }

    /// Property 2: a recognized marimo file round-trips byte-for-byte, and
    /// our own output is always recognized again.
    #[test]
    fn marimo_serialize_is_the_identity(text in arb_marimo_text()) {
        if let Ok(AnyNotebook::Marimo(notebook)) = open(&text) {
            let serialized = notebook.serialize();
            prop_assert_eq!(
                &serialized,
                &text,
                "recognized marimo input must round-trip byte-for-byte"
            );
            prop_assert!(
                matches!(open(&serialized), Ok(AnyNotebook::Marimo(_))),
                "serialized output must be recognized as marimo again"
            );
        }
    }

    /// Property 3: an edit either is refused with the bytes untouched, or
    /// yields a file that parses again with every unedited cell's source
    /// preserved. When the input needed the recovery scanner (a
    /// tokenizer-level syntax error), a regenerated block can make the file
    /// tokenize again and re-segment the view, exactly like marimo's own
    /// `scanner.py` (line-based fallback on tokenizer failure, tokenized
    /// boundaries otherwise); such a view flip is only required to be
    /// byte-stable, while a structured-view input must keep its cells.
    #[test]
    fn marimo_edits_never_corrupt_untouched_cells(
        (text, index, body) in (arb_marimo_text(), any::<usize>(), arb_text(60))
    ) {
        let Ok(AnyNotebook::Marimo(mut notebook)) = open(&text) else {
            return Ok(());
        };
        let recovery_view = notebook.is_recovery_view();
        let count = notebook.cells().len();
        if count == 0 {
            return Ok(());
        }
        let index = index % count;
        let before: Vec<String> = notebook
            .cells()
            .iter()
            .map(|cell| cell.source_text().into_owned())
            .collect();
        if notebook.set_source(index, &body).is_err() {
            // A refused edit (read-only block) must leave the file untouched.
            prop_assert_eq!(&notebook.serialize(), &text);
            return Ok(());
        }
        let serialized = notebook.serialize();
        let Ok(AnyNotebook::Marimo(reopened)) = open(&serialized) else {
            prop_assert!(
                false,
                "an edited marimo file must still parse as marimo:\n{}",
                serialized
            );
            return Ok(());
        };
        if reopened.cells().len() != count {
            prop_assert!(
                recovery_view,
                "cell count must survive an edit of a structured-view input"
            );
            prop_assert_eq!(
                &reopened.serialize(),
                &serialized,
                "a recovery-view re-segmentation must still be byte-stable"
            );
            return Ok(());
        }
        let after: Vec<String> = reopened
            .cells()
            .iter()
            .map(|cell| cell.source_text().into_owned())
            .collect();
        for (position, (old, new)) in before.iter().zip(&after).enumerate() {
            if position != index {
                prop_assert_eq!(new, old, "cell {} was not edited but changed", position);
            }
        }
    }

    /// Property 4: an edit to a Jupyter notebook either is refused or yields
    /// a file that parses again with the cell count intact, every unedited
    /// cell keeping its FULL JSON identity — source, outputs,
    /// execution_count, metadata, ids — and the edited cell round-tripping
    /// exactly the source it was given. The marimo twin (property 3) checks
    /// sources only; a notebook's cells carry much more to protect.
    #[test]
    fn jupyter_edits_never_corrupt_untouched_cells(
        (text, index, body) in (arb_notebook_text(), any::<usize>(), arb_text(60))
    ) {
        let Ok(mut notebook) = JupyterNotebook::parse(&text) else {
            return Ok(());
        };
        let count = notebook.cells().len();
        if count == 0 {
            return Ok(());
        }
        let index = index % count;
        let before = jupyter_fingerprint(&notebook);
        if notebook.set_source(index, &body).is_err() {
            // A refused edit must leave the notebook itself untouched.
            prop_assert_eq!(jupyter_fingerprint(&notebook), before, "a refused edit changed cells");
            return Ok(());
        }
        let serialized = notebook.serialize();
        let Ok(reopened) = JupyterNotebook::parse(&serialized) else {
            prop_assert!(
                false,
                "an edited .ipynb must still parse as a notebook:\n{serialized}"
            );
            return Ok(());
        };
        prop_assert_eq!(
            reopened.cells().len(),
            count,
            "cell count must survive an edit"
        );
        let after = jupyter_fingerprint(&reopened);
        for (position, (old, new)) in before.iter().zip(&after).enumerate() {
            if position != index {
                prop_assert_eq!(new, old, "cell {} was not edited but changed", position);
            }
        }
        prop_assert_eq!(
            &after[index].1,
            &body,
            "the edited cell must round-trip its new source"
        );
    }

    /// Property 5: validation turns problems into findings, never panics.
    #[test]
    fn check_never_panics(
        text in prop_oneof![1 => arb_notebook_text(), 2 => arb_marimo_text(), 1 => arb_fixture_mutation()]
    ) {
        if let Ok(notebook) = open(&text) {
            let _ = notebook.check();
        }
    }

    /// Property 6: format detection and parsing of arbitrary bytes and text
    /// always decide cleanly — Ok or Err, never a panic.
    #[test]
    fn open_never_panics(text in arb_raw_text()) {
        let _ = open(&text);
    }
}

/// One stable scratch dir for the ty property: `common::temp_dir` clears and
/// recreates on every call, which would churn one dir per proptest case.
fn ty_prop_dir() -> &'static Path {
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    DIR.get_or_init(|| common::temp_dir("ty-prop"))
}

proptest! {
    #![proptest_config(config(16))]

    /// Property 7: `ty_check` never panics on arbitrary bounded bytes — the
    /// glue-level containment invariant. ty's own rules are a dependency's
    /// surface and are deliberately NOT asserted; only that the process
    /// survives, on both the plain (.py) and notebook (.ipynb) read paths.
    #[test]
    fn ty_check_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        let text = String::from_utf8_lossy(&bytes).into_owned();
        for name in ["arb.py", "arb.ipynb"] {
            let path = ty_prop_dir().join(name);
            std::fs::write(&path, &text).unwrap();
            let _ = tpmcp::ty::check_path(&path); // Ok or Err — never a panic/abort
        }
    }
}

/// Guard against silent vacuity: the properties only mean something if the
/// generators actually reach both model arms. Synthetic marimo scaffolds
/// must always be recognized (structured parse or scanner fallback), and
/// a healthy share of generated .ipynb documents must parse. (Each check
/// gets a fresh TestRunner: a runner that has completed one run does not
/// execute cases for a second.)
#[test]
fn generators_are_effective() -> Result<(), TestCaseError> {
    TestRunner::new(config(64)).run(&arb_synthetic_marimo(), |text| {
        prop_assert!(
            matches!(open(&text), Ok(AnyNotebook::Marimo(_))),
            "a synthetic scaffold must be recognized as marimo:\n{}",
            text
        );
        Ok(())
    })?;
    let parsed = std::cell::Cell::new(0usize);
    TestRunner::new(ProptestConfig::with_cases(256)).run(&arb_notebook_text(), |text| {
        if JupyterNotebook::parse(&text).is_ok() {
            parsed.set(parsed.get() + 1);
        }
        Ok(())
    })?;
    // ~19% of generated documents parse by construction (adversarial
    // version fields and cell shapes); 256 cases put the expectation near
    // 48, so 16 is a flake-proof floor that still fails hard on a vacuous
    // generator (which yields exactly 0).
    assert!(
        parsed.get() > 16,
        "a healthy share of generated notebooks must parse, got {}",
        parsed.get()
    );
    Ok(())
}
