//! `TyDb` per-thread cache tests: prove that caching the salsa database
//! (a) re-reads fresh bytes after an edit (not a stale memo), (b) evicts and
//! rebuilds across many roots without corrupting results, (c) shares the warm
//! database between `ty_check` and `py_docs` on the same root, and (d) guards
//! the deferred drop of a deeply-nested database at eviction time.
//!
//! Every test runs its several library calls on one thread, which is where the
//! cache lives (`thread_local!`), so the second and later calls genuinely reuse
//! or evict the database rather than starting cold.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::temp_dir;
use tpmcp::docs::resolve_symbol;
use tpmcp::ty;

fn code(d: &tpmcp::ruff::Diagnostic) -> &str {
    d.code.as_deref().unwrap_or("")
}

fn codes(diagnostics: &[tpmcp::ruff::Diagnostic]) -> Vec<String> {
    diagnostics.iter().map(|d| code(d).to_string()).collect()
}

/// Write a clean assignment, check (populates the cached database), overwrite
/// with a type error, then re-check on the same root. The second check reuses
/// the cached database and must report the edit — proving `File::sync_path`
/// invalidates the memoized check rather than serving a stale memo.
#[test]
fn edit_then_recheck_reads_fresh_bytes() {
    let dir = temp_dir("ty-cache-edit");
    let path = dir.join("mod.py");
    std::fs::write(&path, "x: int = 1\n").unwrap();
    assert!(ty::check_path(&path).unwrap().is_empty());
    std::fs::write(&path, "x: int = \"oops\"\n").unwrap();
    let diagnostics = ty::check_path(&path).unwrap();
    assert_eq!(
        codes(&diagnostics),
        vec!["invalid-assignment".to_string()],
        "a cached-database check must read the file's fresh bytes"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// More distinct roots than the per-thread slot cap forces least-recently-used
/// eviction. Two passes — the first evicts as it fills, the second re-checks
/// every root — prove an evicted database is rebuilt correctly, never returned
/// stale or dropped mid-check.
#[test]
fn eviction_across_roots_stays_correct() {
    const ROOTS: usize = 5; // > the cache's per-thread slot cap
    let dir = temp_dir("ty-cache-evict");
    let mut roots = Vec::with_capacity(ROOTS);
    for i in 0..ROOTS {
        let root = dir.join(format!("root{i}"));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("mod.py");
        std::fs::write(&path, format!("x: int = \"val{i}\"\n")).unwrap();
        roots.push(path);
    }

    for _pass in 0..2 {
        for (i, path) in roots.iter().enumerate() {
            let diagnostics = ty::check_path(path).unwrap();
            assert_eq!(codes(&diagnostics), vec!["invalid-assignment"], "root {i}");
            assert!(
                diagnostics[0].message.contains(&format!("val{i}")),
                "root {i}: wrong diagnostic {:?}",
                diagnostics[0].message
            );
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A check warms the shared database for its root, then a docs lookup on the
/// same root resolves a first-party symbol through that warm database (the
/// semantic index the check built is reused, not rebuilt cold).
#[test]
fn docs_shares_warm_database_with_check() {
    let dir = temp_dir("ty-cache-docs");
    std::fs::write(
        dir.join("mymod.py"),
        "def add(a: int, b: int) -> int:\n    return a + b\n",
    )
    .unwrap();

    assert!(ty::check_path(&dir.join("mymod.py")).unwrap().is_empty());

    let symbol = resolve_symbol("mymod.add", Some(&dir)).unwrap();
    assert_eq!(symbol.resolved_name, "add");
    assert_eq!(symbol.kind, "function");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A deeply-nested file checked first leaves its database (holding the deep
/// parsed AST) in the cache; filling the cache with other roots evicts and
/// *drops* that database. The drop is recursive `Drop` glue that overflows the
/// small worker stack unless the cache re-applies `crate::stack::guarded` —
/// which this pins, using the same subprocess shape as the ruff/ty deep-nesting
/// tests (a stack overflow is a SIGSEGV, not a catchable panic).
///
/// The default is 50k terms: the drop glue is sized at 256 B/node
/// (`stack::DROP_FRAME_BOUND`), so ~50k terms is already far past the 2 MiB
/// worker stack, and it halves the time of the 100k-term walk test above.
#[test]
fn deep_check_then_eviction_does_not_crash() {
    const CHILD_ENV: &str = "TPMCP_TEST_DEEP_EVICT";
    const TERMS_ENV: &str = "TPMCP_TEST_DEEP_TERMS";
    if std::env::var(CHILD_ENV).is_ok() {
        let terms: usize = std::env::var(TERMS_ENV)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(50_000)
            .max(2);
        let dir = temp_dir("deep-evict");
        let deep = dir.join("deep.py");
        std::fs::write(&deep, format!("x = {}1\n", "1+".repeat(terms - 1))).unwrap();
        eprintln!("stage: deep check ({terms} terms)");
        let _ = ty::check_path(&deep).unwrap();
        eprintln!("stage: deep check done, forcing eviction");
        // Four other roots push the cache past its cap, evicting (and dropping)
        // the deep database as the least-recently-used slot.
        for i in 0..4 {
            let root = dir.join(format!("other{i}"));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("mod.py"), format!("y{i}: int = 1\n")).unwrap();
            let _ = ty::check_path(&root.join("mod.py")).unwrap();
        }
        eprintln!("stage: eviction done");
        let _ = std::fs::remove_dir_all(&dir);
        eprintln!("deep-evict child survived {terms} terms");
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let output = std::process::Command::new(exe)
        .arg("--exact")
        .arg("deep_check_then_eviction_does_not_crash")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .output()
        .unwrap_or_else(|error| panic!("spawn deep-evict child: {error}"));
    assert!(
        output.status.success(),
        "deep-evict child died (exit {:?}); stderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}
