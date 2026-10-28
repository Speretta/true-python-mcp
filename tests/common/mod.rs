//! Shared helpers for the integration tests.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
// Included by several test binaries; not every binary uses every helper.
#![allow(dead_code)]

pub mod server;

use std::path::{Path, PathBuf};

/// Pull the revision out of a markdown tool response.
pub fn revision_of(text: &str) -> String {
    for line in text.lines() {
        if let Some(rev) = line.strip_prefix("revision: ") {
            return rev.trim().to_string();
        }
    }
    panic!("no revision in output:\n{text}");
}

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

/// Default `ty` binary of the ground-truth venv (the real ty CLI), overridable
/// with `TPMCP_GROUND_TRUTH_TY`. Recreate with:
/// `uv pip install --python /tmp/opencode/nbenv/bin/python ty==0.0.18`
const DEFAULT_TY: &str = "/tmp/opencode/nbenv/bin/ty";

/// The `ty` binary, or `None` after announcing the skip on stderr. With
/// `TPMCP_REQUIRE_GROUND_TRUTH=1` a missing binary fails instead, so CI can
/// never silently lose the live checks.
pub fn ground_truth_ty() -> Option<String> {
    let ty = std::env::var("TPMCP_GROUND_TRUTH_TY").unwrap_or_else(|_| DEFAULT_TY.to_string());
    if std::path::Path::new(&ty).exists() {
        return Some(ty);
    }
    if std::env::var("TPMCP_REQUIRE_GROUND_TRUTH").as_deref() == Ok("1") {
        panic!("TPMCP_REQUIRE_GROUND_TRUTH=1 but {ty} is missing (see README, Development)");
    }
    eprintln!("note: skipping live ty ground-truth checks ({ty} absent)");
    None
}

/// Run the real `ty` binary and return its stdout. Diagnostics print to stdout
/// and the process exits 1 on findings, so success is not asserted; callers
/// parse the diagnostic text (see tests/ty.rs `concise_codes`).
pub fn run_ty(ty: &str, args: &[&str]) -> String {
    let output = std::process::Command::new(ty)
        .args(args)
        .output()
        .expect("spawn ty binary");
    String::from_utf8(output.stdout).expect("utf-8 output")
}

/// [`run_ty`] with `cwd` as the working directory. `ty` discovers its
/// configuration (ty.toml / [tool.ty]) from its **cwd**, not from the checked
/// file, so a rule-config parity check must run the CLI from the fixture
/// directory.
pub fn run_ty_in(ty: &str, cwd: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new(ty)
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn ty binary");
    String::from_utf8(output.stdout).expect("utf-8 output")
}

// ---- virtual-environment scaffold ----

/// The on-disk site-packages path for a scaffolded venv, per platform: Windows
/// puts it at `Lib/site-packages` (no version component), POSIX at
/// `lib/pythonX.Y/site-packages`. ty's discovery scans for these layouts, so a
/// fixture that only exists in the POSIX shape breaks the Windows CI.
pub fn site_packages_dir(venv: &Path, version: &str) -> PathBuf {
    if cfg!(windows) {
        venv.join("Lib/site-packages")
    } else {
        venv.join(format!("lib/python{version}/site-packages"))
    }
}

/// Create a minimal on-disk scaffold for a virtual environment at
/// `root/<name>`: a `pyvenv.cfg` marker plus a `site-packages/<pkg>/__init__.py`
/// holding `body`, in the platform layout. ty only needs the on-disk shape — a
/// `pyvenv.cfg` and a resolvable `site-packages` — to discover the environment;
/// it never runs the (absent) interpreter. The `home` key is required by the
/// `pyvenv.cfg` parser but its target is never consulted because
/// `include-system-site-packages` is absent, so it just has to be an existing
/// directory.
pub fn scaffold_venv_pkg(root: &Path, name: &str, pkg: &str, body: &str) -> PathBuf {
    let venv = root.join(name);
    let site = site_packages_dir(&venv, "3.11");
    std::fs::create_dir_all(site.join(pkg)).unwrap();
    std::fs::write(site.join(pkg).join("__init__.py"), body).unwrap();
    std::fs::write(
        venv.join("pyvenv.cfg"),
        format!("home = {}\n", root.display()),
    )
    .unwrap();
    venv
}

/// [`scaffold_venv_pkg`] with a single-constant body — the original scaffold
/// shape `tests/ty.rs` relies on.
pub fn scaffold_venv(root: &Path, name: &str, pkg: &str) -> PathBuf {
    scaffold_venv_pkg(root, name, pkg, "SOME_CONSTANT = 1\n")
}
