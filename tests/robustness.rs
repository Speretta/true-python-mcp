//! Write-path robustness suite: symlink handling, create-clobber refusal,
//! cross-process and pipelined concurrency, file-mode preservation and
//! mixed-EOL line-range edits. Pins the fixes for the audit findings
//! F1/F2/F3/F9/F10 (reports/06-correctness-audit.md, tests #1, #2, #3, #13,
//! #14, #16).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tpmcp::notebook::{Format, Notebook};

/// A running tpmcp server spoken to over JSON-RPC on stdio. Unlike the
/// tests/tools.rs harness, requests can be sent WITHOUT waiting for the
/// response: rmcp pipelines every request as an independent task, which the
/// concurrency tests exercise deliberately.
struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
    pending: Vec<Value>,
}

impl Server {
    fn start(extra_args: &[&str]) -> Self {
        Self::start_with_env(extra_args, &[])
    }

    fn start_with_env(extra_args: &[&str], extra_env: &[(&str, &str)]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tpmcp"));
        command
            .args(extra_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (key, value) in extra_env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn tpmcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut server = Self {
            child,
            stdin,
            stdout,
            next_id: 0,
            pending: Vec::new(),
        };
        server.initialize();
        server
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn initialize(&mut self) {
        let resp = self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "robustness", "version": "0"}
            }),
        );
        assert_eq!(resp["result"]["serverInfo"]["name"], "tpmcp");
        let msg = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params);
        self.recv(id)
    }

    /// Write a request and return its id WITHOUT waiting for the response.
    fn send(&mut self, method: &str, params: Value) -> i64 {
        self.next_id += 1;
        let id = self.next_id;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
        id
    }

    fn send_call(&mut self, name: &str, args: Value) -> i64 {
        self.send("tools/call", json!({"name": name, "arguments": args}))
    }

    /// Read responses until the one for `id` arrives; park the others.
    fn recv(&mut self, id: i64) -> Value {
        if let Some(position) = self.pending.iter().position(|r| r["id"] == json!(id)) {
            return self.pending.remove(position);
        }
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line).unwrap();
            assert!(!line.is_empty(), "server closed stdout before id {id}");
            let resp: Value =
                serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad JSON {line}: {e}"));
            if resp.get("id").is_none() {
                continue; // notification
            }
            if resp["id"] == json!(id) {
                return resp;
            }
            self.pending.push(resp);
        }
    }

    /// Call a tool and wait, returning (is_error, text, structured_content).
    fn call(&mut self, name: &str, args: Value) -> (bool, String, Value) {
        let id = self.send_call(name, args);
        self.recv_call(id)
    }

    fn recv_call(&mut self, id: i64) -> (bool, String, Value) {
        let resp = self.recv(id);
        assert!(
            resp.get("error").is_none(),
            "protocol error: {}",
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
    let dir = std::env::temp_dir().join(format!("tpmcp-rb-{}-{tag}-{unique}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Canonical Jupyter notebook bytes with one code cell per source.
fn fixture_notebook(sources: &[&str]) -> String {
    let mut notebook = tpmcp::notebook::create(Format::Jupyter);
    for (index, source) in sources.iter().enumerate() {
        notebook.insert(index, "code", source).unwrap();
    }
    notebook.serialize()
}

fn wait_for_file(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    path.exists()
}

/// Test #1 (F2): editing through a symlink must write THROUGH it — the link
/// survives and the resolved target receives the bytes. Before the fix the
/// rename replaced the symlink with a regular file and the user's real
/// notebook silently stopped receiving updates.
#[cfg(unix)]
#[test]
fn edit_through_symlink_keeps_link_and_updates_target() {
    let dir = temp_dir("symlink");
    let target = dir.join("real.ipynb");
    let link = dir.join("link.ipynb");
    std::fs::write(&target, fixture_notebook(&["value = 1\n"])).unwrap();
    if std::os::unix::fs::symlink(&target, &link).is_err() {
        eprintln!("note: skipping, platform refuses symlink creation");
        return;
    }

    let mut server = Server::start(&[]);
    // Reads follow the link...
    let (err, text, sc) = server.call("notebook_read", json!({"path": link, "format": "json"}));
    assert!(!err, "{text}");
    let revision = sc["revision"].as_str().unwrap();
    // ...so writes must too.
    let (err, text, _) = server.call(
        "cell_edit",
        json!({"path": link, "cell": 0, "old_string": "value = 1",
               "new_string": "value = 2", "revision": revision}),
    );
    assert!(!err, "{text}");

    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "edit must not replace the symlink with a regular file"
    );
    let real = std::fs::read_to_string(&target).unwrap();
    assert!(
        real.contains("value = 2"),
        "target must get the edit: {real}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Test #2 (F3): create-mode tools must refuse — with already_exists, not a
/// clobber — when the path holds an entry fs::read cannot see: non-UTF-8
/// bytes or a file beyond the 32 MiB cap.
#[test]
fn create_refuses_existing_unreadable_file() {
    let dir = temp_dir("clobber");
    let binary = dir.join("blob.py");
    std::fs::write(&binary, [0xff_u8, 0xfe, 0x62, 0x00]).unwrap();
    let huge = dir.join("huge.ipynb");
    {
        // Sparse file: no disk blocks, but a length past the cap.
        let file = std::fs::File::create(&huge).unwrap();
        file.set_len(33 * 1024 * 1024).unwrap();
    }

    let mut server = Server::start(&["--file-tools"]);
    let (err, text, sc) = server.call(
        "py_write",
        json!({"path": binary, "content": "print('clobbered')\n"}),
    );
    assert!(err, "py_write over non-UTF-8 must be refused: {text}");
    assert_eq!(sc["error"], "already_exists", "{sc}");

    let (err, text, sc) = server.call("notebook_create", json!({"path": huge}));
    assert!(
        err,
        "notebook_create over huge file must be refused: {text}"
    );
    assert_eq!(sc["error"], "already_exists", "{sc}");

    // Crossed tools, same policy.
    let (err, _, sc) = server.call("notebook_create", json!({"path": binary}));
    assert!(err && sc["error"] == "already_exists", "{sc}");
    let (err, _, sc) = server.call("py_write", json!({"path": huge, "content": "x"}));
    assert!(err && sc["error"] == "already_exists", "{sc}");

    // Both originals are untouched byte-for-byte.
    assert_eq!(std::fs::read(&binary).unwrap(), [0xff_u8, 0xfe, 0x62, 0x00]);
    assert_eq!(std::fs::metadata(&huge).unwrap().len(), 33 * 1024 * 1024);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Test #3 (F1): two tpmcp processes on one file. Sequential case: the
/// loser of a revision must get a conflict. HARD case: P1 pauses between
/// verify and rename (debug-only TPMCP_TEST_RENAME_GATE hook) while P2's
/// competing edit on the SAME revision is injected — without the
/// cross-process lock both would verify revision A and both renames would
/// land, silently destroying one update. With it, P2 blocks until P1
/// releases, then sees the new revision and conflicts.
#[test]
fn two_processes_conflict_is_detected() {
    let dir = temp_dir("twoproc");
    let path = dir.join("nb.ipynb");

    // Phase 1: sequential stale-revision refusal across processes.
    std::fs::write(&path, fixture_notebook(&["value = 1\n"])).unwrap();
    let revision = tpmcp::fs::read(&path).unwrap().revision;
    let mut p1 = Server::start(&[]);
    let mut p2 = Server::start(&[]);
    let (err, text, _) = p1.call(
        "cell_edit",
        json!({"path": path, "cell": 0, "old_string": "value = 1",
               "new_string": "value = 2", "revision": revision}),
    );
    assert!(!err, "P1 must land: {text}");
    let (err, text, sc) = p2.call(
        "cell_edit",
        json!({"path": path, "cell": 0, "old_string": "value = 2",
               "new_string": "value = 3", "revision": revision}),
    );
    assert!(err, "P2 must be refused: {text}");
    assert_eq!(sc["error"], "conflict", "{sc}");
    drop(p1);
    drop(p2);

    // Phase 2: the hard interleaving — both writers target revision A while
    // P1 is provably past verify and before its rename.
    std::fs::write(&path, fixture_notebook(&["value = 1\n"])).unwrap();
    let revision = tpmcp::fs::read(&path).unwrap().revision;
    let gate = dir.join("gate");
    std::fs::create_dir_all(&gate).unwrap();
    let mut p1 = Server::start_with_env(&[], &[("TPMCP_TEST_RENAME_GATE", gate.to_str().unwrap())]);
    let mut p2 = Server::start(&[]);

    let p1_call = p1.send_call(
        "cell_edit",
        json!({"path": path, "cell": 0, "old_string": "value = 1",
               "new_string": "value = 2", "revision": revision}),
    );
    let ready = gate.join(format!("ready-{}", p1.pid()));
    assert!(
        wait_for_file(&ready, Duration::from_secs(60)),
        "P1 never reached the rename gate"
    );
    // Injected while P1 holds the file mid-write: the on-disk revision is
    // still A (P1 has not renamed), so P2 would happily verify and rename
    // too if the cross-process lock let it through.
    let p2_call = p2.send_call(
        "cell_edit",
        json!({"path": path, "cell": 0, "old_string": "value = 1",
               "new_string": "value = 3", "revision": revision}),
    );
    // Give P2 time to reach the lock (generous for loaded machines; the
    // assertions below hold regardless of when P2 gets there).
    std::thread::sleep(Duration::from_millis(500));
    std::fs::write(gate.join("go"), b"").unwrap(); // release P1

    let (err1, text1, _) = p1.recv_call(p1_call);
    let (err2, text2, sc2) = p2.recv_call(p2_call);
    assert!(!err1, "P1 must land: {text1}");
    assert!(err2, "P2 must be refused: {text2}");
    assert_eq!(sc2["error"], "conflict", "{sc2}");
    // Exactly one edit landed: P1's.
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("value = 2"), "P1's edit must land: {raw}");
    assert!(!raw.contains("value = 3"), "P2's edit must not: {raw}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Test #13 (F9): the atomic rename must not widen a tightened file mode.
#[cfg(unix)]
#[test]
fn edit_preserves_file_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("mode");
    let path = dir.join("nb.ipynb");
    std::fs::write(&path, fixture_notebook(&["value = 1\n"])).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let mut server = Server::start(&[]);
    let (_, _, sc) = server.call("notebook_read", json!({"path": path, "format": "json"}));
    let revision = sc["revision"].as_str().unwrap();
    let (err, text, _) = server.call(
        "cell_edit",
        json!({"path": path, "cell": 0, "old_string": "value = 1",
               "new_string": "value = 2", "revision": revision}),
    );
    assert!(!err, "{text}");

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "edit must preserve the file mode");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Test #14 (F10): line-range edits keep untouched lines' terminators
/// byte-for-byte and join new lines with the MAJORITY EOL — a stray CRLF no
/// longer flips a whole LF cell (and vice versa).
#[test]
fn mixed_eol_line_range_edit_uses_majority_and_preserves_untouched() {
    // Direct API: LF majority (2 LF-only vs 1 CRLF).
    assert_eq!(
        tpmcp::fs::replace_line_range("a\r\nb\nc\n", 2, 2, &["x"]).unwrap(),
        "a\r\nx\nc\n"
    );
    // CRLF majority: new lines get CRLF, the LF line keeps its ending.
    assert_eq!(
        tpmcp::fs::replace_line_range("a\r\nb\r\nc\n", 2, 2, &["x"]).unwrap(),
        "a\r\nx\r\nc\n"
    );
    assert_eq!(tpmcp::fs::detect_eol("a\r\nb\nc\n"), "\n");
    assert_eq!(tpmcp::fs::detect_eol("a\r\nb\r\nc\n"), "\r\n");

    // End-to-end through cell_set_source.
    let dir = temp_dir("eol");
    let path = dir.join("nb.ipynb");
    std::fs::write(&path, fixture_notebook(&["a\r\nb\nc\n"])).unwrap();
    let mut server = Server::start(&[]);
    let (err, text, sc) = server.call("notebook_read", json!({"path": path, "format": "json"}));
    assert!(!err, "{text}");
    let revision = sc["revision"].as_str().unwrap();
    let (err, text, _) = server.call(
        "cell_set_source",
        json!({"path": path, "cell": 0, "source": "x",
               "start_line": 2, "end_line": 2, "revision": revision}),
    );
    assert!(!err, "{text}");

    let raw = std::fs::read_to_string(&path).unwrap();
    let notebook = tpmcp::notebook::open(&raw).unwrap();
    assert_eq!(
        notebook.cells()[0].source_text(),
        "a\r\nx\nc\n",
        "untouched lines keep their terminators; new line uses majority LF"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Test #16: two cell_edits on the SAME revision pipelined into one server
/// without waiting (rmcp spawns each request as an independent task). The
/// per-path lock serializes them: exactly one edit lands and the loser gets
/// a conflict — never two successes, never a corrupted file.
#[test]
fn concurrent_pipelined_calls_same_file_exactly_one_wins() {
    let dir = temp_dir("pipeline");
    let path = dir.join("nb.ipynb");
    std::fs::write(&path, fixture_notebook(&["value = 0\n"])).unwrap();
    let revision = tpmcp::fs::read(&path).unwrap().revision;
    let mut server = Server::start(&[]);

    let call1 = server.send_call(
        "cell_edit",
        json!({"path": path, "cell": 0, "old_string": "value = 0",
               "new_string": "value = 1", "revision": revision}),
    );
    let call2 = server.send_call(
        "cell_edit",
        json!({"path": path, "cell": 0, "old_string": "value = 0",
               "new_string": "value = 2", "revision": revision}),
    );
    let (err1, text1, sc1) = server.recv_call(call1);
    let (err2, text2, sc2) = server.recv_call(call2);

    assert_ne!(err1, err2, "exactly one edit must win: {text1} / {text2}");
    let loser = if err1 { &sc1 } else { &sc2 };
    assert_eq!(loser["error"], "conflict", "{loser}");
    let raw = std::fs::read_to_string(&path).unwrap();
    let winner = if err1 { "value = 2" } else { "value = 1" };
    assert!(raw.contains(winner), "winner's edit must be on disk: {raw}");
    std::fs::remove_dir_all(&dir).unwrap();
}
