//! Unit tests for the scope engine, including a ground-truth cross-check
//! against marimo's own compile_cell when the verification venv exists.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
use super::{analyze, is_local};

/// See tests/common/mod.rs — duplicated here because src unit tests cannot
/// import from tests/.
fn ground_truth_python() -> Option<String> {
    let python = std::env::var("TPMCP_GROUND_TRUTH_PYTHON")
        .unwrap_or_else(|_| "/tmp/opencode/nbenv/bin/python".to_string());
    if std::path::Path::new(&python).exists() {
        return Some(python);
    }
    if std::env::var("TPMCP_REQUIRE_GROUND_TRUTH").as_deref() == Ok("1") {
        panic!("TPMCP_REQUIRE_GROUND_TRUTH=1 but {python} is missing");
    }
    eprintln!("note: skipping live ground-truth checks ({python} absent)");
    None
}

fn sets(code: &str) -> (Vec<String>, Vec<String>, Vec<String>, bool) {
    let info = analyze(code).expect("parses");
    let sorted = |set: &std::collections::HashSet<String>| {
        let mut items: Vec<String> = set.iter().cloned().collect();
        items.sort();
        items
    };
    (
        sorted(&info.defs),
        sorted(&info.refs),
        sorted(&info.deleted_refs),
        info.is_sql,
    )
}

fn sets_of(code: &str) -> (Vec<String>, Vec<String>) {
    let (defs, refs, _, _) = sets(code);
    (defs, refs)
}

#[test]
fn local_names() {
    assert!(is_local("_x") && is_local("__") && !is_local("__x__") && !is_local("x"));
}

#[test]
fn assignment_and_self_reference() {
    assert_eq!(sets_of("x = 1"), (vec!["x".to_string()], vec![]));
    // `x = x` registers the ref before the def (module level: no eviction).
    assert_eq!(
        sets_of("x = x"),
        (vec!["x".to_string()], vec!["x".to_string()])
    );
}

#[test]
fn closure_capture_is_evicted() {
    // x is captured inside f before being defined at the top level.
    assert_eq!(
        sets_of("def f():\n    print(x)\nx = 0"),
        (
            vec!["f".to_string(), "x".to_string()],
            vec!["print".to_string()]
        )
    );
}

#[test]
fn locals_never_leak() {
    assert_eq!(
        sets_of("_local = 1\ny = _local\n_undef_used = _missing"),
        (vec!["y".to_string()], vec![])
    );
}

#[test]
fn comprehension_scope() {
    assert_eq!(
        sets_of("[i for i in range(n)]"),
        (vec![], vec!["n".to_string(), "range".to_string()])
    );
    // Walrus in a comprehension binds in the enclosing (module) block.
    assert_eq!(
        sets_of("[y := i for i in range(3)]"),
        (vec!["y".to_string()], vec!["range".to_string()])
    );
}

#[test]
fn global_keyword_binds_at_top_level() {
    assert_eq!(
        sets_of("def f():\n    global g\n    g = 1"),
        (vec!["f".to_string(), "g".to_string()], vec![])
    );
}

#[test]
fn except_handler_is_obscured() {
    assert_eq!(
        sets_of("try:\n    pass\nexcept ValueError as e:\n    print(e)"),
        (vec![], vec!["ValueError".to_string(), "print".to_string()])
    );
}

#[test]
fn import_forms() {
    assert_eq!(
        sets_of("import a.b\nimport c as d\nfrom m import n\nfrom m2 import n2 as n3"),
        (
            vec![
                "a".to_string(),
                "d".to_string(),
                "n".to_string(),
                "n3".to_string()
            ],
            vec![]
        )
    );
}

#[test]
fn class_and_methods() {
    // Methods live in the class block: not cell defs. Class-body
    // assignments are not defs either; their refs leak out.
    assert_eq!(
        sets_of("class C:\n    def method(self):\n        return helper\n    attr = base"),
        (
            vec!["C".to_string()],
            vec!["base".to_string(), "helper".to_string()]
        )
    );
}

#[test]
fn nested_functions_and_params() {
    assert_eq!(
        sets_of("def outer(a):\n    def inner(b):\n        return a + b + free\n    return inner"),
        (vec!["outer".to_string()], vec!["free".to_string()])
    );
}

#[test]
fn lambda_params_are_scoped() {
    assert_eq!(
        sets_of("f = lambda p: p + q"),
        (vec!["f".to_string()], vec!["q".to_string()])
    );
}

#[test]
fn delete_augassign_for_with() {
    let (_, refs, deleted, _) = sets("del undefined_name");
    assert_eq!(refs, vec!["undefined_name".to_string()]);
    assert_eq!(deleted, vec!["undefined_name".to_string()]);
    // AugAssign defines its target; for-targets bind.
    assert_eq!(
        sets_of("counter += 1"),
        (vec!["counter".to_string()], vec![])
    );
    assert_eq!(
        sets_of("for i in items:\n    total = i"),
        (
            vec!["i".to_string(), "total".to_string()],
            vec!["items".to_string()]
        )
    );
    assert_eq!(
        sets_of("with open(p) as fh:\n    data = fh.read()"),
        (
            vec!["data".to_string(), "fh".to_string()],
            vec!["open".to_string(), "p".to_string()]
        )
    );
}

#[test]
fn fstring_interpolations_are_refs() {
    assert_eq!(
        sets_of("text = mo.md(f'{value}')"),
        (
            vec!["text".to_string()],
            vec!["mo".to_string(), "value".to_string()]
        )
    );
}

#[test]
fn sql_call_detection() {
    let (_, _, _, sql) = sets("result = mo.sql('SELECT * FROM t')");
    assert!(sql);
    let (_, _, _, sql) = sets("x = duckdb.execute('SELECT 1')");
    assert!(sql);
    let (_, _, _, sql) = sets("x = other.sql('SELECT 1')");
    assert!(!sql);
}

#[test]
fn defaults_and_annotations_evaluate_outside() {
    assert_eq!(
        sets_of("def f(x=default, *, y: Annotation = other):\n    return x"),
        (
            vec!["f".to_string()],
            vec![
                "Annotation".to_string(),
                "default".to_string(),
                "other".to_string()
            ]
        )
    );
}

#[test]
fn type_params_do_not_leak_as_refs() {
    assert_eq!(
        sets_of("def f[T](x: T) -> T:\n    return x"),
        (vec!["f".to_string()], vec![])
    );
}

#[test]
fn match_patterns_bind() {
    let (defs, refs) = sets_of(
        "match command:\n    case {'k': v}:\n        pass\n    case [first, *rest]:\n        pass",
    );
    assert_eq!(
        defs,
        vec!["first".to_string(), "rest".to_string(), "v".to_string()]
    );
    assert_eq!(refs, vec!["command".to_string()]);
}

#[test]
fn async_def_is_scoped_like_def() {
    assert_eq!(
        sets_of("async def g():\n    await asyncio.sleep(1)"),
        (vec!["g".to_string()], vec!["asyncio".to_string()])
    );
}

/// Ground truth: marimo's own ScopedVisitor over the same snippets.
#[test]
fn matches_marimo_compile_cell() {
    let Some(py) = ground_truth_python() else {
        return;
    };
    const SCRIPT: &str = r#"
import json, sys
from marimo._ast.compiler import compile_cell
impl = compile_cell(sys.argv[1], "0")
print(json.dumps({
    "defs": sorted(impl.defs),
    "refs": sorted(impl.refs),
    "deleted": sorted(impl.deleted_refs),
    "sql": impl.language == "sql",
}))
"#;
    let samples = [
        "x = 1",
        "x = x",
        "def f():\n    print(x)\nx = 0",
        "_local = 1\ny = _local",
        "[i for i in range(n)]",
        "[y := i for i in range(3)]",
        "def f():\n    global g\n    g = 1",
        "try:\n    pass\nexcept ValueError as e:\n    print(e)",
        "import a.b\nimport c as d\nfrom m import n\nfrom m2 import n2 as n3",
        "class C:\n    def method(self):\n        return helper\n    attr = base",
        "del undefined_name",
        "counter += 1",
        "mo.sql('SELECT * FROM t')",
        "def outer(a):\n    def inner(b):\n        return a + b + free\n    return inner",
        "f = lambda p: p + q",
        "text = mo.md(f'{value}')",
        "match command:\n    case {'k': v}:\n        pass\n    case [first, *rest]:\n        pass",
        "def f(x=default):\n    return x",
        "async def g():\n    await asyncio.sleep(1)",
        "with open(p) as fh:\n    data = fh.read()",
        "for i in items:\n    total += i",
        "def f[T](x: T) -> T:\n    return x",
        "class D[T](Base[T]):\n    pass",
        "if flag:\n    defined_in_branch = 1",
        "print(f'{a}{b}')\n_ = '%s' % (c,)",
    ];
    for sample in samples {
        let output = std::process::Command::new(&py)
            .args(["-c", SCRIPT, sample])
            .output()
            .expect("spawn venv python");
        assert!(
            output.status.success(),
            "sample {sample:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
        let (defs, refs, deleted, sql) = sets(sample);
        let expected = |key: &str| -> Vec<String> {
            json[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(defs, expected("defs"), "defs for {sample:?}");
        assert_eq!(refs, expected("refs"), "refs for {sample:?}");
        assert_eq!(deleted, expected("deleted"), "deleted for {sample:?}");
        assert_eq!(sql, json["sql"].as_bool().unwrap(), "sql for {sample:?}");
    }
}
