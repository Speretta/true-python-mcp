//! Deterministic adversarial matrix: enumerated degenerate marimo files,
//! with NO randomness at all. It complements the property suite — which is
//! random and persists every counterexample as a regression seed — by
//! hand-shaping the exact classes that made the scanner fallback diverge
//! from the structured parser (fixed in a384326) and re-asserting the
//! byte-level contracts:
//!
//!   1. an edit never changes an unedited cell's source,
//!   2. serialization is stable (serialize twice is byte-identical),
//!   3. open / edit / serialize never panic.
//!
//! Three-layer discipline: this matrix enumerates the KNOWN shapes on every
//! CI push (deterministic), the property suite DISCOVERS unknown shapes in
//! the nightly 65536-case run, and every discovery is persisted as a seed
//! that then replays deterministically everywhere.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom

use tpmcp::notebook::{AnyNotebook, Notebook, open};

/// Every marimo fixture, including the deliberately degenerate ones.
const FIXTURES: &[&str] = &[
    "tests/data/marimo/broken.py",
    "tests/data/marimo/col0_string.py",
    "tests/data/marimo/emoji.py",
    "tests/data/marimo/empty.py",
    "tests/data/marimo/generate.py",
    "tests/data/marimo/graph_cycle.py",
    "tests/data/marimo/graph_dupedef.py",
    "tests/data/marimo/graph_setup_root.py",
    "tests/data/marimo/graph_stale.py",
    "tests/data/marimo/no_cells.py",
    "tests/data/marimo/toplevel.py",
    "tests/data/marimo/tutorials/dataflow.py",
    "tests/data/marimo/tutorials/external_dependencies.py",
    "tests/data/marimo/tutorials/fileformat.py",
    "tests/data/marimo/tutorials/for_jupyter_users.py",
    "tests/data/marimo/tutorials/intro.py",
    "tests/data/marimo/tutorials/layout.py",
    "tests/data/marimo/tutorials/markdown.py",
    "tests/data/marimo/tutorials/plots.py",
    "tests/data/marimo/tutorials/sql.py",
    "tests/data/marimo/tutorials/ui.py",
];

/// `col0_string.py` is the F6 class: column-0 `@app.cell` inside a multiline
/// string. The scanner mirrors marimo's own regex scanner there, so the
/// untouched-cell invariant does NOT hold by design (a documented boundary);
/// it is exercised for stability and no-panic only.
const F6_FIXTURE: &str = "tests/data/marimo/col0_string.py";

/// Edit bodies that target the three found seed triggers: a lone quote, a
/// quote with leading whitespace (regenerates to an IndentationError
/// against a 4-space `return`), a NUL, and plain leading whitespace.
const BODIES: &[&str] = &["\"", "\" a", "\0", " a"];

/// Deterministic file mutations — one per found class:
/// - `StripReturnIndent`: a column-0 `return` after a cell block (class 1).
/// - `SplitDecorator`: a blank line between a decorator and its `def`
///   (class 2, decorator stacks split across region boundaries).
/// - `JunkDecorator`: a junk `@app.c#` decorator before a NON-FIRST
///   `@app.cell` (class 3, first-`@app.*`-wins classification). It must go
///   before a cell after the header: a junk decorator in the header prefix
///   makes that whole region unparseable and `open()` legitimately rejects
///   the file — a shape marimo would not load either.
fn mutate(text: &str, m: Mutation) -> Option<String> {
    match m {
        Mutation::StripReturnIndent => strip_first_occurrence(text, "    return\n", "return\n"),
        Mutation::SplitDecorator => strip_first_occurrence(text, ")\ndef _", ")\n\ndef _"),
        Mutation::JunkDecorator => insert_before_nth(text, "@app.cell", 1, "@app.c#\n"),
    }
}

fn strip_first_occurrence(text: &str, needle: &str, replacement: &str) -> Option<String> {
    let pos = text.find(needle)?;
    let mut out = text.to_string();
    out.replace_range(pos..pos + needle.len(), replacement);
    Some(out)
}

fn insert_before_nth(text: &str, needle: &str, n: usize, insertion: &str) -> Option<String> {
    let pos = nth_occurrence(text, needle, n)?;
    let mut out = text.to_string();
    out.insert_str(pos, insertion);
    Some(out)
}

fn nth_occurrence(text: &str, needle: &str, n: usize) -> Option<usize> {
    let mut start = 0;
    let mut found = 0;
    for _ in 0..=n {
        let p = text[start..].find(needle)?;
        found = start + p;
        start = found + needle.len();
    }
    Some(found)
}

#[derive(Clone, Copy)]
enum Mutation {
    StripReturnIndent,
    SplitDecorator,
    JunkDecorator,
}

const MUTATIONS: &[Mutation] = &[
    Mutation::StripReturnIndent,
    Mutation::SplitDecorator,
    Mutation::JunkDecorator,
];

/// Edit one cell to `body` and assert the untouched-cell invariant.
fn run_edit(text: &str, index: usize, body: &str) -> Result<(), String> {
    let AnyNotebook::Marimo(mut nb) = open(text).map_err(|e| format!("open: {e}"))? else {
        return Ok(()); // non-marimo open is out of this matrix's scope
    };
    let count = nb.cells().len();
    if count == 0 {
        return Ok(());
    }
    let index = index % count;
    let before = nb
        .cells()
        .iter()
        .map(|c| c.source_text().into_owned())
        .collect::<Vec<_>>();
    if nb.set_source(index, body).is_err() {
        // A refused edit (read-only block) must leave the bytes untouched.
        return if nb.serialize() == text {
            Ok(())
        } else {
            Err("a refused edit changed the file".to_string())
        };
    }
    let serialized = nb.serialize();
    let reopened = open(&serialized).map_err(|e| format!("reopen: {e}"))?;
    let AnyNotebook::Marimo(reopened) = reopened else {
        return Err("an edited marimo file reopened as non-marimo".to_string());
    };
    if reopened.cells().len() != count {
        return Err(format!("cell count {count} -> {}", reopened.cells().len()));
    }
    let after = reopened
        .cells()
        .iter()
        .map(|c| c.source_text().into_owned())
        .collect::<Vec<_>>();
    for (pos, (old, new)) in before.iter().zip(&after).enumerate() {
        if pos != index && old != new {
            return Err(format!(
                "cell {pos} changed:\n  before {old:?}\n  after  {new:?}"
            ));
        }
    }
    Ok(())
}

/// open -> serialize -> reopen -> serialize must be byte-identical.
fn run_stable(text: &str) -> Result<(), String> {
    let AnyNotebook::Marimo(nb) = open(text).map_err(|e| format!("open: {e}"))? else {
        return Ok(());
    };
    let first = nb.serialize();
    let reopened = open(&first).map_err(|e| format!("reopen: {e}"))?;
    let second = reopened.serialize();
    if first == second {
        Ok(())
    } else {
        Err("serialization is not stable".to_string())
    }
}

/// First / middle / last cell indices (deduplicated) for a notebook of
/// `count` cells.
fn positions(count: usize) -> Vec<usize> {
    let mut out = vec![0];
    if count >= 2 {
        out.push(count - 1);
    }
    if count >= 3 && !out.contains(&(count / 2)) {
        out.push(count / 2);
    }
    out
}

#[test]
fn matrix_untouched_cells_survive_degenerate_edits() {
    let mut cases = 0;
    for path in FIXTURES {
        let text = std::fs::read_to_string(path).unwrap();

        // Open-only stability on the pristine fixture.
        run_stable(&text).unwrap_or_else(|e| panic!("{path}: {e}"));
        cases += 1;

        // Edits on the pristine fixture at first/middle/last cells.
        for body in BODIES {
            for index in positions(match open(&text) {
                Ok(AnyNotebook::Marimo(nb)) => nb.cells().len(),
                _ => 0,
            }) {
                run_edit(&text, index, body)
                    .unwrap_or_else(|e| panic!("{path} (edit {index} {body:?}): {e}"));
                cases += 1;
            }
        }

        // The F6 fixture is stability-only (documented divergence); its
        // untouched-cell invariant is deliberately not asserted.
        if *path == F6_FIXTURE {
            continue;
        }

        // Each deterministic mutation × edit, first/middle/last.
        for mutation in MUTATIONS {
            let Some(mutated) = mutate(&text, *mutation) else {
                continue; // shape absent in this fixture
            };
            run_stable(&mutated).unwrap_or_else(|e| panic!("{path} (mutation): {e}"));
            cases += 1;
            for body in BODIES {
                for index in positions(match open(&mutated) {
                    Ok(AnyNotebook::Marimo(nb)) => nb.cells().len(),
                    _ => 0,
                }) {
                    run_edit(&mutated, index, body)
                        .unwrap_or_else(|e| panic!("{path} (mutation {index} {body:?}): {e}"));
                    cases += 1;
                }
            }
        }
    }
    // A canary so a future refactor cannot silently empty the matrix.
    assert!(
        cases > 500,
        "matrix shrank to {cases} cases — something emptied it"
    );
}
