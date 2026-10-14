//! End-to-end tests that drive the real `tpmcp` binary over the MCP stdio
//! protocol, exactly as a host (opencode, Claude, VS Code) would.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

/// A running tpmcp server spoken to over JSON-RPC on stdio.
struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl Server {
    fn start(extra_args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_tpmcp"))
            .args(extra_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn tpmcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut server = Self {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        server.initialize();
        server
    }

    fn initialize(&mut self) {
        let resp = self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "integration", "version": "0"}
            }),
        );
        assert_eq!(resp["result"]["serverInfo"]["name"], "tpmcp");
        self.notify("notifications/initialized", Value::Null);
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let resp: Value =
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad JSON {line}: {e}"));
        assert_eq!(resp["id"], json!(id), "response id mismatch: {resp}");
        resp
    }

    fn notify(&mut self, method: &str, params: Value) {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn list_tools(&mut self) -> Vec<String> {
        let resp = self.request("tools/list", json!({}));
        let mut names: Vec<String> = resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        names.sort();
        names
    }

    /// Call a tool, returning (is_error, text, structured_content).
    fn call(&mut self, name: &str, args: Value) -> (bool, String, Value) {
        let resp = self.request("tools/call", json!({"name": name, "arguments": args}));
        assert!(
            resp.get("error").is_none(),
            "protocol error from {name}: {}",
            resp["error"]
        );
        let result = &resp["result"];
        let is_error = result["isError"].as_bool().unwrap_or(false);
        let text = result["content"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b["type"] == "text")
                    .map(|b| b["text"].as_str().unwrap_or(""))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        (is_error, text, result["structuredContent"].clone())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let unique = blake3::hash(tag.as_bytes()).to_hex()[..8].to_string();
    let dir = std::env::temp_dir().join(format!("tpmcp-it-{}-{tag}-{unique}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn revision_of(text: &str) -> String {
    for line in text.lines() {
        if let Some(rev) = line.strip_prefix("revision: ") {
            return rev.trim().to_string();
        }
    }
    panic!("no revision in output:\n{text}");
}

const DEFAULT_TOOLS: &[&str] = &[
    "cell_clear_outputs",
    "cell_delete",
    "cell_edit",
    "cell_insert",
    "cell_move",
    "cell_set_source",
    "cell_set_type",
    "notebook_check",
    "notebook_create",
    "notebook_read",
    "ruff_check",
    "ruff_format",
];

#[test]
fn default_tool_surface() {
    let mut server = Server::start(&[]);
    assert_eq!(server.list_tools(), DEFAULT_TOOLS);
}

#[test]
fn file_tools_flag_adds_py_tools() {
    let mut server = Server::start(&["--file-tools"]);
    let tools = server.list_tools();
    assert_eq!(tools.len(), DEFAULT_TOOLS.len() + 3);
    for name in ["py_read", "py_write", "py_edit"] {
        assert!(tools.contains(&name.to_string()), "missing {name}");
    }
}

/// Full notebook lifecycle over the wire, with revision chaining, byte-exact
/// output validation, and conflict rejection.
#[test]
fn notebook_lifecycle() {
    let dir = temp_dir("lifecycle");
    let path = dir.join("nb.ipynb");
    let mut server = Server::start(&[]);

    // create
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": path, "cells": [
            {"cell_type": "markdown", "source": "# Title\n"},
            {"cell_type": "code", "source": "import os\nprint('a')\n"}
        ]}),
    );
    assert!(!err, "{text}");

    // read -> revision; verify structure
    let (err, text, sc) = server.call("notebook_read", json!({"path": path, "format": "json"}));
    assert!(!err, "{text}");
    assert_eq!(sc["cell_count"], 2);
    let mut rev = sc["revision"].as_str().unwrap().to_string();

    // surgical edit of cell 1 (markdown preview carries the diff)
    let (err, text, _) = server.call(
        "cell_edit",
        json!({"path": path, "cell": 1, "old_string": "print('a')",
               "new_string": "print('b')", "revision": rev}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("print('b')"), "{text}");
    rev = revision_of(&text);

    // line-range set on cell 1 (replace line 1)
    let (err, text, _) = server.call(
        "cell_set_source",
        json!({"path": path, "cell": 1, "source": "import sys",
               "start_line": 1, "end_line": 1, "revision": rev}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("import sys"), "{text}");
    rev = revision_of(&text);

    // insert, move, set_type, clear_outputs, delete
    let (_, text, _) = server.call(
        "cell_insert",
        json!({"path": path, "cell_type": "code", "source": "y = 2\n", "revision": rev}),
    );
    rev = revision_of(&text);
    let (_, text, _) = server.call(
        "cell_move",
        json!({"path": path, "from": 2, "to": 0, "revision": rev}),
    );
    rev = revision_of(&text);
    let (_, text, _) = server.call(
        "cell_set_type",
        json!({"path": path, "cell": 1, "cell_type": "raw", "revision": rev}),
    );
    rev = revision_of(&text);
    let (_, text, _) = server.call("cell_clear_outputs", json!({"path": path, "revision": rev}));
    rev = revision_of(&text);
    let (_, text, _) = server.call(
        "cell_delete",
        json!({"path": path, "cell": 1, "revision": rev}),
    );
    let _ = revision_of(&text);

    // conflict: stale revision is rejected with a helpful error
    let (err, text, sc) = server.call(
        "cell_edit",
        json!({"path": path, "cell": 0, "old_string": "y", "new_string": "z",
               "revision": "0000000000000000"}),
    );
    assert!(err, "stale revision must be rejected");
    assert!(text.contains("revision conflict"), "{text}");
    assert_eq!(sc["error"], "conflict");
    assert!(sc["current_revision"].as_str().is_some());

    // on-disk file is valid, canonical JSON
    let raw = std::fs::read_to_string(&path).unwrap();
    let parsed: Value = serde_json::from_str(&raw).unwrap();
    assert!(raw.ends_with('\n') && !raw.ends_with("\n\n"));
    assert_eq!(parsed["nbformat"], 4);
    let nb = tpmcp::notebook::JupyterNotebook::parse(&raw).unwrap();
    assert_eq!(
        nb.to_string_canonical(),
        raw,
        "file stays byte-exact canonical"
    );
}

#[test]
fn ruff_tools_over_the_wire() {
    let mut server = Server::start(&[]);
    let (err, text, sc) = server.call(
        "ruff_check",
        json!({"path": "tests/data/lint_sample.py", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["diagnostic_count"], 3);
    let codes: Vec<&str> = sc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, vec!["F401", "F401", "F821"]);

    // notebook lint reports 0-based cells
    let (_, _, sc) = server.call(
        "ruff_check",
        json!({"path": "tests/data/lint_sample.ipynb", "format": "json"}),
    );
    let cells: Vec<u64> = sc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["cell"].as_u64().unwrap())
        .collect();
    assert_eq!(cells, vec![1, 2], "0-based cell indexes");
}

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

/// marimo notebooks work through the same tools: create by extension, read,
/// and surgically edit with byte-exact preservation of the rest.
#[test]
fn marimo_tools_over_the_wire() {
    let dir = temp_dir("marimo");
    let path = dir.join("app.py");
    let mut server = Server::start(&[]);

    // create a marimo notebook (extension .py -> marimo)
    let (err, text, _) = server.call(
        "notebook_create",
        json!({"path": path, "cells": [{"cell_type": "code", "source": "x = 1\n"}]}),
    );
    assert!(!err, "{text}");
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        raw.contains("import marimo") && raw.contains("@app.cell"),
        "{raw}"
    );
    assert!(raw.contains("app.run()"), "{raw}");

    // read it back
    let (err, text, sc) = server.call("notebook_read", json!({"path": path, "format": "json"}));
    assert!(!err, "{text}");
    assert!(sc["format"].as_str().unwrap().starts_with("marimo"), "{sc}");
    assert_eq!(sc["cell_count"], 1);
    let rev = sc["revision"].as_str().unwrap().to_string();

    // surgical edit preserves the marimo scaffolding
    let (err, text, _) = server.call(
        "cell_edit",
        json!({"path": path, "cell": 0, "old_string": "x = 1", "new_string": "x = 2", "revision": rev}),
    );
    assert!(!err, "{text}");
    let edited = std::fs::read_to_string(&path).unwrap();
    assert!(edited.contains("x = 2"), "{edited}");
    assert!(edited.contains("import marimo") && edited.contains("app.run()"));

    // an existing marimo fixture reads with the marimo label; markdown cells
    // show their extracted text
    let (err, text, _) = server.call(
        "notebook_read",
        json!({"path": "tests/data/marimo/emoji.py"}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("marimo"), "{text}");
    assert!(text.contains("markdown"), "{text}");
    assert!(text.contains("Use colon syntax"), "{text}");
}

/// notebook_check over the wire: JSON and markdown rendering, revision
/// chaining, marimo graph findings, and error propagation.
#[test]
fn notebook_check_over_the_wire() {
    let mut server = Server::start(&[]);

    // Jupyter schema issues (8 errors + 1 warning), with a revision that
    // matches notebook_read for chaining.
    let (err, _, read_sc) = server.call(
        "notebook_read",
        json!({"path": "tests/data/schema_issues.ipynb", "format": "json"}),
    );
    assert!(!err);
    let (err, text, sc) = server.call(
        "notebook_check",
        json!({"path": "tests/data/schema_issues.ipynb", "format": "json"}),
    );
    assert!(!err, "{text}");
    assert_eq!(sc["revision"], read_sc["revision"]);
    assert_eq!(sc["error_count"].as_u64(), Some(8), "{sc}");
    assert_eq!(sc["warning_count"].as_u64(), Some(1), "{sc}");
    assert_eq!(sc["info_count"].as_u64(), Some(0), "{sc}");
    let findings = sc["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 9, "{sc}");
    assert_eq!(findings[0]["code"], "cell-id");
    assert_eq!(findings[0]["cell"], 1);
    assert_eq!(findings[7]["code"], "conflict-marker");
    assert_eq!(findings[7]["severity"], "warning");

    // Markdown rendering
    let (err, text, _) = server.call(
        "notebook_check",
        json!({"path": "tests/data/schema_issues.ipynb"}),
    );
    assert!(!err);
    assert!(
        text.contains("9 finding(s): 8 error(s), 1 warning(s), 0 info"),
        "{text}"
    );
    assert!(text.contains("- error cell 1 `cell-id`:"), "{text}");
    assert!(
        text.contains("- warning cell 4 `conflict-marker`:"),
        "{text}"
    );

    // Clean notebook
    let (err, _, sc) = server.call(
        "notebook_check",
        json!({"path": "tests/data/vscode_sample.ipynb", "format": "json"}),
    );
    assert!(!err);
    assert_eq!(sc["findings"].as_array().unwrap().len(), 0, "{sc}");

    // marimo graph findings flow through the same tool
    let (err, _, sc) = server.call(
        "notebook_check",
        json!({"path": "tests/data/marimo/graph_cycle.py", "format": "json"}),
    );
    assert!(!err);
    assert_eq!(sc["error_count"].as_u64(), Some(1), "{sc}");
    assert_eq!(sc["findings"][0]["code"], "cycle");

    // A missing file is a tool error
    let (err, text, _) = server.call("notebook_check", json!({"path": "nope_missing.ipynb"}));
    assert!(err, "{text}");
}
