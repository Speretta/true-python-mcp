//! The wire harness: a running tpmcp server spoken to over JSON-RPC on
//! stdio, exactly as a host (opencode, Claude, VS Code) would.
//!
//! Requests can be sent WITHOUT waiting for the response: rmcp pipelines
//! every request as an independent task, which the concurrency tests in
//! tests/robustness.rs exercise deliberately. Responses that arrive before
//! they are asked for are parked until their id is requested.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

/// A running tpmcp server spoken to over JSON-RPC on stdio.
pub struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
    pending: Vec<Value>,
}

impl Server {
    pub fn start(extra_args: &[&str]) -> Self {
        Self::start_with_env(extra_args, &[])
    }

    pub fn start_with_env(extra_args: &[&str], extra_env: &[(&str, &str)]) -> Self {
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

    pub fn pid(&self) -> u32 {
        self.child.id()
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

    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params);
        self.recv(id)
    }

    /// Write a request and return its id WITHOUT waiting for the response.
    pub fn send(&mut self, method: &str, params: Value) -> i64 {
        self.next_id += 1;
        let id = self.next_id;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
        id
    }

    pub fn send_call(&mut self, name: &str, args: Value) -> i64 {
        self.send("tools/call", json!({"name": name, "arguments": args}))
    }

    /// Read responses until the one for `id` arrives; park the others.
    pub fn recv(&mut self, id: i64) -> Value {
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
    pub fn call(&mut self, name: &str, args: Value) -> (bool, String, Value) {
        let id = self.send_call(name, args);
        self.recv_call(id)
    }

    pub fn recv_call(&mut self, id: i64) -> (bool, String, Value) {
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

    pub fn notify(&mut self, method: &str, params: Value) {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    pub fn list_tools(&mut self) -> Vec<String> {
        let resp = self.request("tools/list", json!({}));
        let mut names: Vec<String> = resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
