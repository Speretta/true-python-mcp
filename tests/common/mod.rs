//! Shared helpers for the integration tests.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom

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
    String::from_utf8(output.stdout).expect("utf-8 output")
}
