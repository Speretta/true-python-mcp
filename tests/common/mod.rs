//! Shared helpers for the integration tests.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
// Included by several test binaries; not every binary uses every helper.
#![allow(dead_code)]

pub mod server;

use std::path::PathBuf;

/// Unique temp dir for one test tag: pid + a hash of the tag, cleared and
/// recreated on every call.
pub fn temp_dir(tag: &str) -> PathBuf {
    let unique = blake3::hash(tag.as_bytes()).to_hex()[..8].to_string();
    let dir =
        std::env::temp_dir().join(format!("tpmcp-test-{}-{tag}-{unique}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Read one marimo fixture file verbatim (`tests/data/marimo/`).
pub fn marimo_fixture(name: &str) -> String {
    std::fs::read_to_string(format!("tests/data/marimo/{name}"))
        .unwrap_or_else(|error| panic!("cannot read {name}: {error}"))
}

/// Build a synthetic marimo file from cell bodies (indented 4, `return`
/// appended), for tests that need a notebook shape without a fixture file.
pub fn synthetic_marimo(bodies: &[&str]) -> String {
    let mut serialized =
        String::from("import marimo\n\n__generated_with = \"0.25.1\"\napp = marimo.App()\n");
    for body in bodies {
        serialized.push_str("\n\n@app.cell\ndef _():\n");
        for line in body.split('\n') {
            if line.is_empty() {
                serialized.push('\n');
            } else {
                serialized.push_str(&format!("    {line}\n"));
            }
        }
        serialized.push_str("    return");
    }
    serialized.push_str("\n\n\nif __name__ == \"__main__\":\n    app.run()\n");
    serialized
}

/// Default python of the ground-truth venv (real marimo + nbformat), overridable
/// with `TPMCP_GROUND_TRUTH_PYTHON`. Recreate with:
/// `uv venv /tmp/opencode/nbenv &&
///  uv pip install --python /tmp/opencode/nbenv/bin/python nbformat marimo`
const DEFAULT_NBENV_PY: &str = "/tmp/opencode/nbenv/bin/python";

/// The venv python, or `None` after announcing the skip on stderr. With
/// `TPMCP_REQUIRE_GROUND_TRUTH=1` a missing venv fails instead, so CI can
/// never silently lose the live checks.
pub fn ground_truth_python() -> Option<String> {
    let python =
        std::env::var("TPMCP_GROUND_TRUTH_PYTHON").unwrap_or_else(|_| DEFAULT_NBENV_PY.to_string());
    if std::path::Path::new(&python).exists() {
        return Some(python);
    }
    if std::env::var("TPMCP_REQUIRE_GROUND_TRUTH").as_deref() == Ok("1") {
        panic!("TPMCP_REQUIRE_GROUND_TRUTH=1 but {python} is missing (see README, Development)");
    }
    eprintln!("note: skipping live ground-truth checks ({python} absent)");
    None
}

/// Run a Python script with one argument against the venv interpreter.
pub fn run_python(py: &str, script: &str, args: &[&str]) -> String {
    let output = std::process::Command::new(py)
        .arg("-c")
        .arg(script)
        .args(args)
        .output()
        .expect("spawn venv python");
    assert!(
        output.status.success(),
        "python check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Python stdout is text mode: on Windows it translates \n to \r\n. tpmcp
    // always emits LF, so normalize once here rather than at every comparison
    // site (byte-exactness asserts against this output otherwise fail on
    // Windows).
    String::from_utf8(output.stdout)
        .expect("utf-8 output")
        .replace("\r\n", "\n")
}
