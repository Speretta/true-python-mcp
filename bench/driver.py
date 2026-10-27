#!/usr/bin/env python3
"""Drive one MCP server stack through the benchmark's operation profile.

The driver speaks plain MCP (JSON-RPC over stdio, or streamable HTTP for
marimo) and coordinates with sampler.py through two small files:

  status file  (driver writes):  phase=ready / time_to_ready_ms=N /
                                 op=LABEL ok|error|skipped / phase=ops-done
  control file (sampler writes): run-ops / exit

Usage (stdio stack - the driver spawns the server):
  driver.py --stack tpmcp --status S --control C --ops-out O --fixture F \
      -- docker run -i --rm --cidfile CF -v ... IMAGE [ARGS...]

Usage (HTTP stack - container already running):
  driver.py --stack marimo --url http://127.0.0.1:PORT/mcp/server \
      --status S --control C --ops-out O --fixture F

--fixture is the run's WORK copy (bench/work/<stack>/bench.ipynb); the
driver edits it directly for the conflict scenarios and restores it from
its own baseline copy afterwards. The pristine fixture is never touched.

Operation profile v2 (same semantics on every stack, where tools exist):
  read -> check -> edit cell 4 -> insert a bubble-sort cell at index 5 ->
  execute that cell (kernel stacks) -> delete the spare cell -> re-read ->
  lint. Execution-only servers run the same bubble-sort source as a snippet.
Then the two conflict scenarios ("the file changes while the AI works"):
  read -> [0 s | 10 s] -> external edit of cell 4 on disk -> stale edit
  with pre-external knowledge -> verdict from the file -> restore.

Every request and response is saved verbatim to --ops-out (same honesty
rule the examples/ docs follow). Steps carry argument VARIANTS because
schemas differ per project and version: the driver tries them in order and
records every attempt, including failures.

nb-cli is the one CLI stack: there is no server to speak MCP to, so each
operation is one `nb` shell command (CliClient below). The adapter keeps a
single shell running inside the container and feeds it the commands, which
keeps the MCP plumbing (Coordinator, ops log, latency) intact and keeps the
measured per-call latency at the fork+exec cost of the `nb` process itself.
Per call it also captures the peak RSS of the `nb` process (VmHWM) and of
the call's whole process tree (that is where the kernel `nb execute` spawns
shows up), because a CLI has no resident process to weigh.
"""

import argparse
import json
import os
import random
import select
import shlex
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

PROTOCOL_VERSION = "2024-11-05"
NB = "/work/bench.ipynb"          # container-side paths
NB_PY = "/work/bench.py"
EDIT_OLD = '"hello bench"'
EDIT_NEW = '"hello bench (edited)"'
EDITED_SOURCE = 'greeting = "hello bench (edited)"'

# The inserted cell: a well-known algorithm, self-contained so a fresh
# kernel (or a WASM sandbox) can execute it without prior notebook state.
ALGO_SOURCE = '''def bubble_sort(items):
    n = len(items)
    for i in range(n):
        for j in range(n - 1 - i):
            if items[j] > items[j + 1]:
                items[j], items[j + 1] = items[j + 1], items[j]
    return items


bubble_sort([3, 1, 4, 1, 5, 9, 2, 6])'''

# Conflict scenario: the "user in VS Code" rewrites cell 4 on disk while
# the AI is between its read and its write. The stale edit then tries to
# apply what the AI planned BEFORE that external change.
EXTERNAL_SOURCE = 'greeting = "hello bench (external edit)"'
STALE_OLD = '"hello bench (edited)"'
STALE_NEW = '"hello bench (stale)"'
STALE_SOURCE = 'greeting = "hello bench (stale)"'

# Canonical operation order (render.py builds the latency matrix from it).
OP_ORDER = ["read", "check", "edit", "insert", "execute", "delete",
            "reread", "lint", "analyze", "docs-cold", "docs-warm",
            "docs-members", "conflict-0s", "conflict-10s"]


def now_ms():
    return int(time.time() * 1000)


class RpcError(Exception):
    pass


class StdioClient:
    kind = "stdio"

    def __init__(self, argv, stderr_log):
        self.next_id = 0
        self.err = open(stderr_log, "wb")
        self.start_ms = now_ms()
        self.proc = subprocess.Popen(
            argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.err
        )

    def _send(self, msg):
        self.proc.stdin.write((json.dumps(msg) + "\n").encode())
        self.proc.stdin.flush()

    def request(self, method, params=None, timeout=300):
        self.next_id += 1
        mid = self.next_id
        msg = {"jsonrpc": "2.0", "id": mid, "method": method}
        if params is not None:
            msg["params"] = params
        self._send(msg)
        deadline = time.time() + timeout
        while True:
            if time.time() > deadline:
                raise RpcError(f"timeout waiting for response to {method}")
            line = self.proc.stdout.readline()
            if not line:
                raise RpcError(f"server closed stdout while waiting for {method}")
            try:
                m = json.loads(line.decode("utf-8", "replace"))
            except ValueError:
                continue  # tolerate non-JSON noise on stdout
            if m.get("id") == mid and ("result" in m or "error" in m):
                return m
            # A server-initiated request must be answered or the server may
            # block; answer pings, refuse everything else.
            if "method" in m and "id" in m:
                reply = {"jsonrpc": "2.0", "id": m["id"]}
                if m["method"] == "ping":
                    reply["result"] = {}
                else:
                    reply["error"] = {"code": -32601, "message": "method not found"}
                self._send(reply)

    def notify(self, method, params=None):
        msg = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = params
        self._send(msg)

    def close(self):
        try:
            self.proc.terminate()
            self.proc.wait(timeout=10)
        except Exception:
            try:
                self.proc.kill()
            except Exception:
                pass
        try:
            self.err.close()
        except Exception:
            pass


class HttpClient:
    """Minimal MCP streamable-HTTP client (urllib only)."""

    kind = "http"

    def __init__(self, url, headers=None):
        self.url = url
        self.headers = dict(headers) if headers else {}
        self.next_id = 0
        self.session = None
        self.start_ms = now_ms()

    def _post(self, msg, timeout=300):
        headers = {
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
        }
        headers.update(self.headers)
        if self.session:
            headers["Mcp-Session-Id"] = self.session
        req = urllib.request.Request(
            self.url, data=json.dumps(msg).encode(), headers=headers, method="POST"
        )
        try:
            resp = urllib.request.urlopen(req, timeout=timeout)
        except urllib.error.HTTPError as e:
            body = e.read().decode("utf-8", "replace")[:500]
            raise RpcError(f"HTTP {e.code} from {self.url}: {body}")
        sid = resp.headers.get("Mcp-Session-Id")
        if sid:
            self.session = sid
        body = resp.read().decode("utf-8", "replace")
        if "text/event-stream" in resp.headers.get("Content-Type", ""):
            found = None
            for line in body.splitlines():
                if not line.startswith("data:"):
                    continue
                payload = line[5:].strip()
                if not payload or payload == "[DONE]":
                    continue
                try:
                    m = json.loads(payload)
                except ValueError:
                    continue
                if (
                    isinstance(m, dict)
                    and m.get("id") == msg.get("id")
                    and ("result" in m or "error" in m)
                ):
                    found = m
            return found
        if body.strip():
            return json.loads(body)
        return None

    def request(self, method, params=None, timeout=300):
        self.next_id += 1
        msg = {"jsonrpc": "2.0", "id": self.next_id, "method": method}
        if params is not None:
            msg["params"] = params
        m = self._post(msg, timeout)
        if m is None:
            raise RpcError(f"no response body for {method}")
        return m

    def notify(self, method, params=None):
        msg = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = params
        try:
            self._post(msg)
        except RpcError:
            pass  # notifications may be answered with a bare 202

    def close(self):
        pass


def escape_cli_source(text):
    """Source text -> one argv token for `nb --source`.

    The adapter speaks one command line at a time to the container's shell,
    so real newlines would split the command. nb unescapes \\n, \\t and \\
    itself (its documented multi-line `--source` form), which round-trips
    the source byte-exactly. The SKILL.md-preferred `--source -` stdin form
    is one invocation either way.
    """
    return text.replace("\\", "\\\\").replace("\n", "\\n").replace("\t", "\\t")


class CliClient:
    """Adapter for CLI stacks (nb-cli): no server, one `nb` call per op.

    A single shell runs inside the container (the shell an agent's terminal
    would have); each operation is one command line sent to it and read back
    up to a unique marker line, so the timed latency is the fork+exec of the
    `nb` process itself - not `docker exec` or shell start-up overhead.

    request("tools/call", ...) maps a pseudo-tool name to an `nb` argv and
    returns a response synthesized in MCP shape (one ops-log format) whose
    text block is the command's real output. The `cli` block next to it is
    the honest CLI record: argv, exit code, wall time, and the per-call
    peak memory - VmHWM of the `nb` process, plus the peak summed RSS of
    the call's whole process tree (the kernel `nb execute` spawns is a
    child of `nb`, so it lands in the tree number and never in the
    binary's own).
    """

    kind = "cli"

    def __init__(self, argv, cidfile=None, stderr_log=None):
        self.err = open(stderr_log, "wb") if stderr_log else subprocess.DEVNULL
        self.proc = subprocess.Popen(
            argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.err
        )
        self.cidfile = cidfile
        self.start_ms = now_ms()
        self.token = f"{os.getpid():x}{random.randrange(1 << 24):06x}"
        self._root = None

    def _container_pid(self):
        if not self.cidfile:
            return None
        try:
            cid = Path(self.cidfile).read_text().strip()
        except OSError:
            return None
        if not cid:
            return None
        try:
            p = subprocess.run(
                ["docker", "inspect", "--format", "{{.State.Pid}}", cid],
                capture_output=True, text=True, timeout=30)
            return int(p.stdout.strip())
        except (ValueError, subprocess.SubprocessError):
            return None

    def _watch_root(self):
        """Host pid to poll under: the container's init (commands sent to
        the shell land there) or, with no container, the spawned shell."""
        if self._root is None:
            root = self._container_pid()
            if root:
                self._root = root
                return root
            return self.proc.pid
        return self._root

    @staticmethod
    def _children(pid):
        kids = []
        try:
            tids = os.listdir(f"/proc/{pid}/task")
        except OSError:
            return kids
        for tid in tids:
            try:
                txt = open(f"/proc/{pid}/task/{tid}/children").read()
            except OSError:
                continue
            kids.extend(int(x) for x in txt.split())
        return kids

    @staticmethod
    def _proc_info(pid):
        """(ppid, argv0, rss_bytes, vmhwm_bytes) from /proc, or None if gone."""
        try:
            status = open(f"/proc/{pid}/status").read()
            argv0 = (open(f"/proc/{pid}/cmdline", "rb").read()
                     .split(b"\0", 1)[0].decode("utf-8", "replace"))
        except OSError:
            return None
        ppid, rss, hwm = 0, 0, 0
        for line in status.splitlines():
            if line.startswith("PPid:"):
                ppid = int(line.split()[1])
            elif line.startswith("VmRSS:"):
                rss = int(line.split()[1]) * 1024
            elif line.startswith("VmHWM:"):
                hwm = int(line.split()[1]) * 1024
        return ppid, argv0, rss, hwm

    def _tree(self, root):
        """[(pid, ppid, argv0, rss, hwm)] of root and its descendants."""
        out = []
        stack = [root]
        seen = set()
        while stack:
            pid = stack.pop()
            if pid in seen:
                continue
            seen.add(pid)
            info = self._proc_info(pid)
            if info is None:
                continue
            out.append((pid,) + info)
            stack.extend(self._children(pid))
        return out

    def _poll_peaks(self, acc):
        # One walk per poll, one consistent snapshot: find the `nb` process,
        # then sum RSS over it and its descendants (that is where the kernel
        # `nb execute` spawns shows up).
        nodes = self._tree(self._watch_root())
        nb = [n for n in nodes if os.path.basename(n[2]) == "nb"]
        if not nb:
            return
        acc["nb_seen"] = True
        members = {nb[0][0]}
        changed = True
        while changed:
            changed = False
            for n in nodes:
                if n[1] in members and n[0] not in members:
                    members.add(n[0])
                    changed = True
        # VmHWM is a high-water mark: one read during the call is already
        # the peak of the process up to that point.
        acc["nb"] = max(acc.get("nb", 0), nb[0][4])
        acc["tree"] = max(acc.get("tree", 0),
                          sum(n[3] for n in nodes if n[0] in members))

    def request(self, method, params=None, timeout=300):
        if method != "tools/call":
            raise RpcError(f"the CLI adapter has no {method}")
        name = (params or {}).get("name")
        try:
            argv = CLI_TOOLS[name]((params or {}).get("arguments") or {})
        except KeyError:
            raise RpcError(f"unknown CLI pseudo-tool {name}")
        return self.run_command(argv, timeout)

    def run_command(self, argv, timeout=300):
        """Run one `nb` command in the shell; return an MCP-shaped response."""
        marker = f"__nbcli_done_{self.token}"
        cmd = " ".join(shlex.quote(t) for t in argv)
        self._watch_root()  # resolve before timing, once per call
        acc = {}
        t0 = now_ms()
        self.proc.stdin.write(f'{cmd}; echo "{marker} rc=$?"\n'.encode())
        self.proc.stdin.flush()
        out, rc, deadline = [], None, time.time() + timeout
        buf = b""
        stdout_fd = self.proc.stdout.fileno()
        while True:
            if time.time() > deadline:
                raise RpcError(f"timeout waiting for {' '.join(argv)}")
            self._poll_peaks(acc)
            ready, _, _ = select.select([stdout_fd], [], [], 0.0005)
            if not ready:
                continue
            chunk = os.read(stdout_fd, 65536)
            if not chunk:
                raise RpcError(f"shell closed stdout during {' '.join(argv)}")
            buf += chunk
            while b"\n" in buf:
                raw, buf = buf.split(b"\n", 1)
                text = raw.decode("utf-8", "replace")
                if marker in text:
                    tail = text.split(marker, 1)[1]
                    digits = tail.split("rc=", 1)[1].split()[0] if "rc=" in tail else ""
                    rc = int(digits) if digits.isdigit() else None
                    ms = now_ms() - t0
                    ok = rc == 0
                    return {
                        "result": {
                            "content": [{"type": "text", "text": "".join(out)}],
                            "isError": not ok,
                        },
                        "cli": {
                            "argv": argv,
                            "rc": rc,
                            "ms": ms,
                            "peak_nb_rss_bytes": acc.get("nb") if acc.get("nb_seen") else None,
                            "peak_tree_rss_bytes": acc.get("tree") if acc.get("nb_seen") else None,
                        },
                    }
                out.append(text + "\n")

    def close(self):
        try:
            self.proc.stdin.write(b"exit\n")
            self.proc.stdin.flush()
            self.proc.stdin.close()
            self.proc.wait(timeout=10)
        except Exception:
            try:
                self.proc.kill()
            except Exception:
                pass
        try:
            self.err.close()
        except Exception:
            pass


def result_text(resp):
    r = resp.get("result") or {}
    blocks = r.get("content") or []
    return "".join(
        b.get("text", "")
        for b in blocks
        if isinstance(b, dict) and b.get("type") == "text"
    )


def is_error(resp):
    if resp is None:
        return True
    if "error" in resp:
        return True
    return bool((resp.get("result") or {}).get("isError"))


def find_revision(text):
    """tpmcp markdown carries `revision: <16 hex>` (possibly mid-line)."""
    marker = "revision: "
    i = text.find(marker)
    while i != -1:
        frag = text[i + len(marker) : i + len(marker) + 16]
        if len(frag) == 16 and all(c in "0123456789abcdef" for c in frag):
            return frag
        i = text.find(marker, i + 1)
    return None


def grab_revision(ctx, resp):
    rev = find_revision(result_text(resp))
    if rev:
        ctx["revision"] = rev


def _find_key(obj, key):
    if isinstance(obj, dict):
        if key in obj and isinstance(obj[key], (str, int)):
            return str(obj[key])
        for v in obj.values():
            r = _find_key(v, key)
            if r:
                return r
    elif isinstance(obj, list):
        for v in obj:
            r = _find_key(v, key)
            if r:
                return r
    return None


def grab_session(ctx, resp):
    """marimo: fish the session id out of get_active_notebooks."""
    text = result_text(resp)
    sid = None
    try:
        sid = _find_key(json.loads(text), "session_id")
    except ValueError:
        pass
    if not sid:
        marker = '"session_id"'
        i = text.find(marker)
        if i != -1:
            rest = text[i + len(marker) :]
            j = rest.find('"')
            k = rest.find('"', j + 1)
            if j != -1 and k != -1:
                sid = rest[j + 1 : k]
    if sid:
        ctx["session_id"] = sid


def step(label, tool, variants, after=None, skip=None):
    s = {"label": label, "tool": tool, "variants": variants}
    if after:
        s["after"] = after
    if skip:
        s["skip"] = skip
    return s


NO_CHECK = "no validate/check tool"
NO_LINT = "no lint tool"
NO_ANALYZE = "no combined analyze tool"
NO_DOCS = "no docs/symbol-lookup tool"


def _docs_skips():
    """The three docs-profile rows for stacks without a docs tool.

    tpmcp fills these in with real calls (profile_tpmcp); every other stack
    lacks a symbol/docs tool, so the steps are recorded as explicit skips and
    render.py shows "—" (with the reason in the audit) instead of "n/a".
    """
    return [
        step("docs-cold", None, [], skip=NO_DOCS),
        step("docs-warm", None, [], skip=NO_DOCS),
        step("docs-members", None, [], skip=NO_DOCS),
    ]

# --------------------------------------------------------------------------
# Operation profiles v2. Index math: the fixture has 21 cells; cell 4 is
# the greeting (edit target), cell 19 is the spare (extra_4). The insert
# at index 5 shifts the spare to 20, and the delete removes it there, so
# every stack sees the same indices.
# --------------------------------------------------------------------------

def profile_tpmcp(ctx):
    return [
        step("read", "notebook_read", [{"path": NB}], after=grab_revision),
        step("check", "notebook_check", [{"path": NB}], after=grab_revision),
        step("edit", "cell_edit", [lambda c: {
            "path": NB, "cell": 4, "old_string": EDIT_OLD,
            "new_string": EDIT_NEW, "revision": c["revision"]}],
            after=grab_revision),
        step("insert", "cell_insert", [lambda c: {
            "path": NB, "index": 5, "cell_type": "code",
            "source": ALGO_SOURCE, "revision": c["revision"]}],
            after=grab_revision),
        step("execute", None, [], skip="never executes code, by design"),
        step("delete", "cell_delete", [
            # The tool-surface redesign renamed the target arg to
            # `cells: [...]` (delete takes several cells); the old
            # single-`cell` form is kept as a variant so pre-redesign
            # binaries still measure instead of erroring.
            lambda c: {"path": NB, "cells": [20], "revision": c["revision"]},
            lambda c: {"path": NB, "cell": 20, "revision": c["revision"]}],
            after=grab_revision),
        step("reread", "notebook_read", [{"path": NB}], after=grab_revision),
        step("lint", "ruff_check", [{"path": NB_PY}]),
        step("analyze", None, [],
             skip="check + lint already cover validation and diagnostics"),
        # Docs capability: py_docs (cold then warm) + py_module_members.
        # "cold" is the FIRST call in this fresh server process, so the
        # discovery and docstring caches are empty; "warm" is the identical
        # repeat, when those caches are populated. members is a distinct
        # tool that shares the (now warm) discovery cache but enumerates the
        # module itself. All three are read-only, no interpreter involved.
        step("docs-cold", "py_docs", [{"symbol": "json.dumps"}]),
        step("docs-warm", "py_docs", [{"symbol": "json.dumps"}]),
        step("docs-members", "py_module_members", [{"module": "json"}]),
    ]


def profile_datalayer(ctx):
    # Observed 2026-10-15 on 2.2.4: the cell_id addressing path of
    # edit_cell_source fails internally ("quote_from_bytes() expected
    # bytes"), cell_index works - documented in COMPARISON.md.
    return [
        step("use_notebook", "use_notebook",
             [{"notebook_name": "bench", "notebook_path": "bench.ipynb"}]),
        step("read", "read_notebook",
             [{"notebook_name": "bench", "response_format": "detailed"}]),
        step("check", None, [], skip=NO_CHECK),
        step("edit", "edit_cell_source",
             [{"notebook_name": "bench", "cell_index": 4,
               "old_string": EDIT_OLD, "new_string": EDIT_NEW}]),
        step("insert", "insert_cell",
             [{"notebook_name": "bench", "cell_index": 5,
               "cell_type": "code", "cell_source": ALGO_SOURCE}]),
        step("execute", "execute_cell",
             [{"notebook_name": "bench", "cell_index": 5},
              {"notebook_name": "bench", "index": 5}]),
        step("delete", "delete_cell",
             [{"notebook_name": "bench", "cell_indices": [20]}]),
        step("reread", "read_notebook",
             [{"notebook_name": "bench", "response_format": "detailed"}]),
        step("lint", None, [], skip=NO_LINT),
        step("analyze", None, [], skip=NO_ANALYZE),
    ] + _docs_skips()


def profile_block(ctx):
    # block consolidates everything into 4 tools; its execute step runs a
    # NOTEBOOK CELL on the live kernel (no free-form code tool exists).
    return [
        step("setup", "setup_notebook",
             [{"notebook_path": "bench.ipynb",
               "server_url": "http://localhost:8888"}]),
        step("read", "query_notebook",
             [{"notebook_path": "bench.ipynb", "query_type": "view_source",
               "position_index": 4}]),
        step("check", None, [], skip=NO_CHECK),
        step("edit", "modify_notebook_cells",
             [{"notebook_path": "bench.ipynb", "operation": "edit_code",
               "cell_content": EDITED_SOURCE, "position_index": 4,
               "execute": False}]),
        step("insert", "modify_notebook_cells",
             [{"notebook_path": "bench.ipynb", "operation": "add_code",
               "cell_content": ALGO_SOURCE, "position_index": 5,
               "execute": False}]),
        step("execute", "execute_notebook_code",
             [{"notebook_path": "bench.ipynb",
               "execution_type": "execute_cell", "position_index": 5}]),
        step("delete", "modify_notebook_cells",
             [{"notebook_path": "bench.ipynb", "operation": "delete",
               "position_index": 20}]),
        step("reread", "query_notebook",
             [{"notebook_path": "bench.ipynb", "query_type": "view_source",
               "position_index": 4}]),
        step("lint", None, [], skip=NO_LINT),
        step("analyze", None, [], skip=NO_ANALYZE),
    ] + _docs_skips()


def profile_cursor(ctx):
    return [
        step("read", "notebook_read", [{"notebook_path": NB}]),
        step("check", "notebook_validate", [{"notebook_path": NB}]),
        step("edit", "notebook_edit_cell",
             [{"notebook_path": NB, "cell_index": 4, "source": EDITED_SOURCE}]),
        step("insert", "notebook_add_cell",
             [{"notebook_path": NB, "cell_type": "code",
               "source": ALGO_SOURCE, "insert_after_index": 4}]),
        step("execute", None, [], skip="no kernel - file-only editor"),
        step("delete", "notebook_delete_cell",
             [{"notebook_path": NB, "cell_index": 20}]),
        step("reread", "notebook_read", [{"notebook_path": NB}]),
        step("lint", None, [], skip=NO_LINT),
        step("analyze", None, [], skip=NO_ANALYZE),
    ] + _docs_skips()


# nb-cli pseudo-tools: each maps one operation to one `nb` argv, run by
# CliClient in the container's shell. Cell update REPLACES the whole cell
# source (nb has no old_string/new_string fragment edit), and cell add
# inserts at an absolute index - same indices as every other stack.
CLI_TOOLS = {
    "nb-version": lambda a: ["nb", "--version"],
    "nb-read": lambda a: ["nb", "read", a["path"]],
    "nb-cell-update": lambda a: ["nb", "cell", "update", a["path"],
                                 "--cell-index", str(a["cell_index"]),
                                 "--source", escape_cli_source(a["source"])],
    "nb-cell-add": lambda a: ["nb", "cell", "add", a["path"],
                              "--type", a.get("cell_type", "code"),
                              "--insert-at", str(a["insert_at"]),
                              "--source", escape_cli_source(a["source"])],
    "nb-cell-add-batch": lambda a: ["nb", "cell", "add", a["path"],
                                    "--source", escape_cli_source(a["source"])],
    "nb-cell-delete": lambda a: ["nb", "cell", "delete", a["path"],
                                 "--cell-index", str(a["cell_index"])],
    "nb-execute": lambda a: ["nb", "execute", a["path"],
                             "--cell-index", str(a["cell_index"])],
}


def profile_nbcli(ctx):
    # jupyter-ai-contrib/nb-cli: one shell command per operation, no server
    # and no resident process. execute is MEASURED (nb-cli's headline
    # capability; the image ships ipykernel for its native kernel run) and
    # check/lint are honest skips: nb-cli has neither a validator nor a
    # linter. Same indices as every other stack (fixture: 21 cells).
    nb = ctx.get("nb_path", NB)
    return [
        step("read", "nb-read", [{"path": nb}]),
        step("check", None, [], skip="no validate/check tool (nb-cli has none)"),
        step("edit", "nb-cell-update",
             [{"path": nb, "cell_index": 4, "source": EDITED_SOURCE}]),
        step("insert", "nb-cell-add",
             [{"path": nb, "insert_at": 5, "cell_type": "code",
               "source": ALGO_SOURCE}]),
        step("execute", "nb-execute", [{"path": nb, "cell_index": 5}]),
        step("delete", "nb-cell-delete", [{"path": nb, "cell_index": 20}]),
        step("reread", "nb-read", [{"path": nb}]),
        step("lint", None, [], skip="no lint tool (nb-cli ships no linter)"),
        step("analyze", None, [], skip=NO_ANALYZE),
    ] + _docs_skips()


def profile_marimo(ctx):
    # Read-only MCP surface; headless there are no sessions, so even the
    # read tools have nothing to attach to. Measured as it runs headless.
    return [
        step("read", "get_active_notebooks", [{"args": {}}], after=grab_session),
        step("cellmap", "get_lightweight_cell_map",
             [lambda c: {"args": {"session_id": c["session_id"]}}],
             skip="needs an editor session; headless there is none"),
        step("check", "get_notebook_errors", [{"args": {}}],
             skip="session-scoped; headless there is none"),
        step("edit", None, [], skip="read-only MCP surface"),
        step("insert", None, [], skip="read-only MCP surface"),
        step("execute", None, [], skip="read-only MCP surface"),
        step("delete", None, [], skip="read-only MCP surface"),
        step("reread", None, [], skip="read-only MCP surface"),
        step("lint", "lint_notebook", [{"args": {}}],
             skip="session-scoped; headless there is none"),
        step("analyze", None, [], skip=NO_ANALYZE),
    ] + _docs_skips()


def profile_analyzer(ctx):
    # mcp-server-analyzer: code strings in, structured findings out. It
    # never sees files or notebooks, so only the analysis steps apply.
    # The harness passes the marimo fixture's source as the code string -
    # the same content tpmcp lints by path (asymmetry disclosed in the doc).
    src = ctx.get("py_source", "")
    return [
        step("read", None, [], skip="not a notebook server (code strings only)"),
        step("check", None, [], skip="no notebook awareness"),
        step("edit", None, [], skip="writes no files"),
        step("insert", None, [], skip="writes no files"),
        step("execute", None, [], skip="never executes code"),
        step("delete", None, [], skip="writes no files"),
        step("reread", None, [], skip="not a notebook server"),
        step("lint", "ruff-check", [{"code": src}]),
        step("analyze", "analyze-code", [{"code": src}]),
    ] + _docs_skips()


def profile_runpython(ctx):
    # pydantic/mcp-run-python (retired upstream - disclosed): one tool,
    # run_python_code, executed in Pyodide inside Deno.
    return [
        step("read", None, [], skip="execution-only server"),
        step("check", None, [], skip="execution-only server"),
        step("edit", None, [], skip="execution-only server"),
        step("insert", None, [], skip="execution-only server"),
        step("execute", "run_python_code", [{"python_code": ALGO_SOURCE}]),
        step("delete", None, [], skip="execution-only server"),
        step("reread", None, [], skip="execution-only server"),
        step("lint", None, [], skip=NO_LINT),
        step("analyze", None, [], skip=NO_ANALYZE),
    ] + _docs_skips()


def profile_yzfly(ctx):
    # yzfly/mcp-python-interpreter: subprocess execution in the container's
    # own Python (the "selected environment" is the system one here).
    return [
        step("read", None, [], skip="execution/file server, no notebooks"),
        step("check", None, [], skip="execution/file server, no notebooks"),
        step("edit", None, [], skip="out of profile scope"),
        step("insert", None, [], skip="out of profile scope"),
        step("execute", "run_python_code",
             [{"code": ALGO_SOURCE}, {"python_code": ALGO_SOURCE}]),
        step("delete", None, [], skip="out of profile scope"),
        step("reread", None, [], skip="out of profile scope"),
        step("lint", None, [], skip=NO_LINT),
        step("analyze", None, [], skip=NO_ANALYZE),
    ] + _docs_skips()


PROFILES = {
    "tpmcp": profile_tpmcp,
    "datalayer": profile_datalayer,
    "block": profile_block,
    "cursor": profile_cursor,
    "nbcli": profile_nbcli,
    "marimo": profile_marimo,
    "analyzer": profile_analyzer,
    "runpython": profile_runpython,
    "yzfly": profile_yzfly,
}

# The stale edit each stack receives during the conflict scenarios: what
# the AI planned at read time, sent after the file changed on disk.
STALE_EDITS = {
    "tpmcp": lambda c: {"tool": "cell_edit", "arguments": {
        "path": NB, "cell": 4, "old_string": STALE_OLD,
        "new_string": STALE_NEW, "revision": c["revision"]}},
    "datalayer": lambda c: {"tool": "edit_cell_source", "arguments": {
        "notebook_name": "bench", "cell_index": 4,
        "old_string": STALE_OLD, "new_string": STALE_NEW}},
    "block": lambda c: {"tool": "modify_notebook_cells", "arguments": {
        "notebook_path": "bench.ipynb", "operation": "edit_code",
        "cell_content": STALE_SOURCE, "position_index": 4, "execute": False}},
    "cursor": lambda c: {"tool": "notebook_edit_cell", "arguments": {
        "notebook_path": NB, "cell_index": 4, "source": STALE_SOURCE}},
    # nb-cli has NO revision guard: the stale write is one `nb cell update`
    # with pre-external knowledge. The verdict is read from the file after.
    "nbcli": lambda c: {"tool": "nb-cell-update", "arguments": {
        "path": c.get("nb_path", NB), "cell_index": 4, "source": STALE_SOURCE}},
}

CONFLICT_READS = {
    "tpmcp": lambda c: {"tool": "notebook_read", "arguments": {"path": NB}},
    "datalayer": lambda c: {"tool": "read_notebook", "arguments": {
        "notebook_name": "bench", "response_format": "detailed"}},
    "block": lambda c: {"tool": "query_notebook", "arguments": {
        "notebook_path": "bench.ipynb", "query_type": "view_source",
        "position_index": 4}},
    "cursor": lambda c: {"tool": "notebook_read",
                         "arguments": {"notebook_path": NB}},
    "nbcli": lambda c: {"tool": "nb-read",
                        "arguments": {"path": c.get("nb_path", NB)}},
}

CONFLICT_SKIP = {
    "marimo": "read-only MCP surface",
    "analyzer": "writes no files (code-string analysis only)",
    "runpython": "execution-only server, writes no files",
    "yzfly": "execution-only profile, writes no files",
}


class Coordinator:
    def __init__(self, status_path, control_path):
        self.status_path = status_path
        self.control_path = control_path
        open(self.status_path, "w").close()
        open(self.control_path, "w").close()

    def status(self, line):
        print(f"[driver] {line}", flush=True)
        with open(self.status_path, "a") as f:
            f.write(line + "\n")

    def wait_control(self, want, timeout):
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                lines = open(self.control_path).read().splitlines()
            except OSError:
                lines = []
            if want in lines:
                return True
            time.sleep(0.25)
        return False


def call(client, tool, args, ops_entry):
    """One tools/call, recorded verbatim; returns (ok, response)."""
    try:
        resp = client.request("tools/call", {"name": tool, "arguments": args})
    except RpcError as e:
        ops_entry["attempts"].append(
            {"tool": tool, "arguments": args, "transport_error": str(e)})
        return False, None
    ops_entry["attempts"].append(
        {"tool": tool, "arguments": args, "response": resp})
    return (not is_error(resp)), resp


def attempt_peaks(ops_entry):
    """(peak nb RSS, peak call-tree RSS) across a step's attempts, or Nones.

    Only CLI attempts carry the `cli` block (see CliClient); MCP stacks
    have a resident process instead of per-call peaks and return None.
    """
    nb, tree = [], []
    for a in ops_entry.get("attempts", []):
        cli = (a.get("response") or {}).get("cli") or {}
        if cli.get("peak_nb_rss_bytes") is not None:
            nb.append(cli["peak_nb_rss_bytes"])
        if cli.get("peak_tree_rss_bytes") is not None:
            tree.append(cli["peak_tree_rss_bytes"])
    return (max(nb) if nb else None, max(tree) if tree else None)


def run_step(client, st, ctx, ops_log):
    if st.get("skip"):
        ops_log.append({"label": st["label"], "skipped": st["skip"]})
        return None
    entry = {"label": st["label"], "ok": False, "attempts": []}
    ok = False
    resp = None
    for variant in st["variants"]:
        try:
            args = variant(ctx) if callable(variant) else variant
        except Exception as e:  # a chained context value never arrived
            entry["attempts"].append(
                {"tool": st["tool"], "variant_error": str(e)})
            continue
        ok, resp = call(client, st["tool"], args, entry)
        if ok:
            if st.get("after"):
                st["after"](ctx, resp)
            break
    entry["ok"] = ok
    ops_log.append(entry)
    return ok


def host_read(fixture, cidfile):
    """Read the work copy; fall back to `docker exec cat` when the server
    leaves root-owned files the host user cannot read."""
    try:
        with open(fixture) as f:
            return f.read()
    except PermissionError:
        if not cidfile:
            raise
        cid = Path(cidfile).read_text().strip()
        p = subprocess.run(["docker", "exec", cid, "cat", NB],
                           capture_output=True)
        if p.returncode != 0:
            raise OSError(f"docker exec cat failed: {p.stderr[:200]}")
        return p.stdout.decode("utf-8", "replace")


def host_write(fixture, text, cidfile):
    """Write the work copy; fall back to piping through `docker exec` when
    the file is root-owned (another writer, same bytes on disk)."""
    try:
        with open(fixture, "w") as f:
            f.write(text)
        return
    except PermissionError:
        if not cidfile:
            raise
        cid = Path(cidfile).read_text().strip()
        p = subprocess.run(["docker", "exec", "-i", cid, "sh", "-c",
                            f"cat > {NB}"], input=text.encode())
        if p.returncode != 0:
            raise OSError("docker exec write failed")


def set_cell4(fixture, source, cidfile):
    """Rewrite cell 4's source directly on disk (the 'VS Code save')."""
    nb = json.loads(host_read(fixture, cidfile))
    nb["cells"][4]["source"] = source
    host_write(fixture, json.dumps(nb, indent=1, ensure_ascii=False) + "\n",
               cidfile)


def cell4_source(fixture, cidfile):
    nb = json.loads(host_read(fixture, cidfile))
    src = nb["cells"][4]["source"]
    if isinstance(src, list):
        src = "".join(src)
    return src


def run_conflict(client, stack, ctx, coord, ops_log, fixture, baseline,
                 delay_s, label, cidfile):
    """One conflict scenario: read -> wait -> external disk edit -> stale
    edit -> verdict from the file -> restore the baseline."""
    entry = {"label": label, "delay_s": delay_s, "attempts": []}
    # 1. A fresh read: the AI looks at the file before planning its edit.
    read = CONFLICT_READS[stack](ctx)
    ok, resp = call(client, read["tool"], read["arguments"], entry)
    entry["read_ok"] = ok
    if ok and resp is not None and stack == "tpmcp":
        grab_revision(ctx, resp)
    entry["revision_used"] = ctx.get("revision")
    # 2. The AI "thinks" for delay_s; meanwhile the user saves the file.
    time.sleep(delay_s)
    set_cell4(fixture, EXTERNAL_SOURCE, cidfile)
    # 3. The stale edit lands with pre-external knowledge.
    stale = STALE_EDITS[stack](ctx)
    ok_stale, resp_stale = call(client, stale["tool"], stale["arguments"], entry)
    err_text = result_text(resp_stale) if resp_stale else ""
    low = err_text.lower()
    # 4. Verdict straight from the file on disk.
    time.sleep(0.5)  # let server-side saves settle
    after = cell4_source(fixture, cidfile)
    if (not ok_stale) and "revision" in low:
        verdict = "refused (revision guard)"
    elif (not ok_stale) and ("conflict" in low or "has changed" in low):
        verdict = "refused (change detection)"
    elif ok_stale and "external edit" in after:
        verdict = "applied; external edit survived"
    elif ok_stale and "stale" in after:
        verdict = "applied over the external edit (external LOST)"
    elif ok_stale:
        verdict = "applied; unexpected file state"
    else:
        verdict = f"edit errored: {err_text[:120]}"
    entry["verdict"] = verdict
    entry["external_survived"] = "external edit" in after
    entry["stale_applied"] = ok_stale
    entry["cell4_after"] = after
    ops_log.append(entry)
    coord.status(f"op={label} ok")
    # 5. Restore for the next scenario.
    host_write(fixture, host_read(baseline, None), cidfile)
    return verdict


# --------------------------------------------------------------------------
# Tool-call counting. The composite task below is the concrete form of
# nb-cli's claim ("one shell command replaces many tool calls"): add two
# cells, execute the second one, read the notebook back. It runs AFTER the
# sampler's last window (the measurement is over) on its own copies of the
# notebook, so no published number moves - only the call counts land.
# --------------------------------------------------------------------------

WF_CELL_1 = "first_wf = 1"
WF_CELL_2 = "second_wf = 2"
WF_SCENARIO = "add two cells + execute the second cell + read the notebook back"
WORKFLOW_STACKS = ("nbcli", "tpmcp")


def run_workflow_probe(stack, client, ctx, host_dir):
    """Call counts for WF_SCENARIO, or None for stacks without a probe."""
    if stack not in WORKFLOW_STACKS:
        return None
    nb_dir = Path(ctx.get("nb_path", NB)).parent
    base_text = host_read(host_dir / "bench.ipynb", None)
    probe = {"scenario": WF_SCENARIO, "variants": []}

    def fresh(name):
        host = host_dir / f"wf_{name}.ipynb"
        with open(host, "w") as f:
            f.write(base_text)
        return str(nb_dir / f"wf_{name}.ipynb")

    def run_variant(name, file_note, steps):
        probe["variants"].append({
            "name": name, "note": file_note,
            "calls": sum(st.get("calls", 0) for st in steps),
            "total_ms": sum(st.get("ms", 0) for st in steps),
            "steps": steps})

    def do(label, tool, args):
        entry = {"label": label, "calls": 0, "attempts": []}
        t0 = now_ms()
        ok, resp = call(client, tool, args, entry)
        entry["ms"] = now_ms() - t0
        entry["ok"] = ok
        entry["calls"] = len(entry["attempts"])
        entry["peak_nb_rss_bytes"], entry["peak_tree_rss_bytes"] = \
            attempt_peaks(entry)
        return entry, resp

    if stack == "tpmcp":
        # tpmcp writes are revision-guarded, so the honest sequence starts
        # with a read to obtain the revision and ends with the read-back;
        # execute is a recorded skip (tpmcp never executes code). Two
        # variants, mirroring nb-cli's: one insert per cell, and both cells
        # in one batched cell_insert (the `cells: [...]` batch form - the
        # call count is the point of this probe).
        def tpmcp_variant(name, file_name, adds):
            f = fresh(file_name)
            steps = []
            entry, resp = do("read (get revision)", "notebook_read", {"path": f})
            steps.append(entry)
            rev = find_revision(result_text(resp)) if resp else None
            for label, tool, args in adds(f):
                if not rev:
                    steps.append({"label": label, "calls": 0, "ok": False,
                                  "note": "no revision - write not attempted"})
                    continue
                entry, resp = do(label, tool, dict(args, revision=rev))
                if resp is not None:
                    rev = find_revision(result_text(resp)) or rev
                steps.append(entry)
            steps.append({"label": "execute", "calls": 0,
                          "skipped": "never executes code, by design"})
            entry, _ = do("read back", "notebook_read", {"path": f})
            steps.append(entry)
            run_variant(name, f, steps)

        tpmcp_variant("tpmcp MCP tool calls, one insert per cell", "tpmcp",
                      lambda f: [
                          ("add cell 1", "cell_insert",
                           {"path": f, "index": 21, "cell_type": "code",
                            "source": WF_CELL_1}),
                          ("add cell 2", "cell_insert",
                           {"path": f, "index": 22, "cell_type": "code",
                            "source": WF_CELL_2})])
        tpmcp_variant("tpmcp MCP tool calls, both cells in one batched insert",
                      "tpmcp-batch",
                      lambda f: [
                          ("add 2 cells (batch)", "cell_insert",
                           {"path": f, "index": 21, "cells": [
                               {"cell_type": "code", "source": WF_CELL_1},
                               {"cell_type": "code", "source": WF_CELL_2}]})])
    else:
        # nb-cli, variant A: one `nb` command per cell (4 invocations).
        f = fresh("one-per-cell")
        steps = []
        for label, tool, args in (
            ("add cell 1", "nb-cell-add",
             {"path": f, "insert_at": 21, "cell_type": "code", "source": WF_CELL_1}),
            ("add cell 2", "nb-cell-add",
             {"path": f, "insert_at": 22, "cell_type": "code", "source": WF_CELL_2}),
            ("execute", "nb-execute", {"path": f, "cell_index": 22}),
            ("read back", "nb-read", {"path": f}),
        ):
            entry, _ = do(label, tool, args)
            steps.append(entry)
        run_variant("nb-cli, one command per cell", f, steps)
        # nb-cli, variant B: the two cells batched into ONE command via the
        # multi-cell sentinels its SKILL.md teaches (3 invocations).
        f = fresh("batch")
        steps = []
        for label, tool, args in (
            ("add 2 cells (batch)", "nb-cell-add-batch",
             {"path": f, "source": f"@@code\n{WF_CELL_1}\n@@code\n{WF_CELL_2}"}),
            ("execute", "nb-execute", {"path": f, "cell_index": 22}),
            ("read back", "nb-read", {"path": f}),
        ):
            entry, _ = do(label, tool, args)
            steps.append(entry)
        run_variant("nb-cli, both cells batched in one command", f, steps)
    return probe


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--stack", required=True, choices=sorted(PROFILES))
    ap.add_argument("--status", required=True)
    ap.add_argument("--control", required=True)
    ap.add_argument("--ops-out", required=True)
    ap.add_argument("--fixture", required=True,
                    help="host path of the run's work copy of bench.ipynb")
    ap.add_argument("--cidfile", help="container id file; used to write the "
                    "external change through docker exec when the server "
                    "leaves root-owned files (datalayer)")
    ap.add_argument("--url", help="streamable-HTTP endpoint (marimo)")
    ap.add_argument("--notebook-path",
                    help="CLI stacks: the notebook path as the CLI sees it "
                         "(default /work/bench.ipynb, the container mount)")
    ap.add_argument("--dump-tools", help="write tools/list result here and exit")
    ap.add_argument("argv", nargs="*", help="stdio: command that runs the server")
    args = ap.parse_args()

    coord = Coordinator(args.status, args.control)
    ops_log = []
    ctx = {}
    ctx["nb_path"] = args.notebook_path or NB

    # The marimo-format fixture doubles as the lint target; analyzer-style
    # servers receive its source as a code string.
    py_path = Path(args.fixture).parent / "bench.py"
    if py_path.is_file():
        ctx["py_source"] = py_path.read_text()

    ready_basis = "MCP handshake up to the answered tools/list"
    if args.stack == "nbcli":
        # CLI stack: no MCP handshake. The "server command" after -- is the
        # container's shell; ready is the FIRST `nb` spawn (there is no
        # resident process to hand-shake with).
        if not args.argv:
            print("nbcli needs the container shell command after --",
                  file=sys.stderr)
            return 2
        client = CliClient(args.argv, args.cidfile,
                           args.ops_out + ".server-stderr.log")
        t0 = now_ms()
        resp = client.request("tools/call", {"name": "nb-version",
                                             "arguments": {}})
        ready_ms = now_ms() - t0
        ops_log.append({"label": "ready-probe", "response": resp})
        if is_error(resp):
            coord.status(f"phase=error msg=nb --version failed: "
                         f"{result_text(resp)[:120]}")
            client.close()
            return 1
        tool_names = sorted(CLI_TOOLS)
        ops_log.append({"label": "cli-commands", "commands": tool_names,
                        "note": "CLI pseudo-tools, one `nb` invocation each; "
                                "nb-cli exposes no MCP tools"})
        ready_basis = "first nb spawn (no server handshake)"
        if args.dump_tools:
            with open(args.dump_tools, "w") as f:
                json.dump({"tools": [{"name": n} for n in tool_names]},
                          f, indent=2)
            print(f"[driver] tools dumped: {', '.join(tool_names)}")
            client.close()
            return 0
    elif args.url:
        client = HttpClient(args.url)
    else:
        if not args.argv:
            print("stdio stack needs a server command after --", file=sys.stderr)
            return 2
        client = StdioClient(args.argv, args.ops_out + ".server-stderr.log")

    if args.stack != "nbcli":
        try:
            resp = client.request("initialize", {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "tpmcp-bench", "version": "0"},
            })
            ops_log.append({"label": "initialize", "response": resp})
            if "error" in resp:
                raise RpcError(f"initialize failed: {resp['error']}")
            client.notify("notifications/initialized")
            tools = client.request("tools/list", {})
            ops_log.append({"label": "tools/list", "response": tools})
            if "error" in tools:
                raise RpcError(f"tools/list failed: {tools['error']}")
        except RpcError as e:
            coord.status(f"phase=error msg={e}")
            client.close()
            return 1

        ready_ms = now_ms() - client.start_ms
        tool_names = [t["name"] for t in (tools.get("result", {}).get("tools") or [])]
        if args.dump_tools:
            with open(args.dump_tools, "w") as f:
                json.dump(tools["result"], f, indent=2)
            print(f"[driver] tools dumped: {', '.join(tool_names)}")
            client.close()
            return 0

    coord.status(f"time_to_ready_ms={ready_ms}")
    if args.stack == "nbcli":
        # No MCP tools to count; the CLI's command surface is the analog.
        coord.status(f"cli_commands={len(tool_names)}")
    else:
        coord.status(f"tools_count={len(tool_names)}")
    coord.status(f"ready_basis={ready_basis}")
    coord.status("phase=ready")

    if not coord.wait_control("run-ops", timeout=900):
        coord.status("phase=error msg=run-ops signal never arrived")
        client.close()
        return 1

    # The steps split into the shared profile (every stack runs these) and
    # the docs lookups (tpmcp's py_docs/py_module_members; recorded as
    # explicit skips on stacks without a docs tool). They run as two phases
    # so the sampler can measure the shared profile's memory and the docs
    # cost in separate windows instead of one blended number.
    steps = PROFILES[args.stack](ctx)
    shared = [st for st in steps if not st["label"].startswith("docs-")]
    docs = [st for st in steps if st["label"].startswith("docs-")]

    results = []

    def run_one(st):
        t0 = now_ms()
        n_log = len(ops_log)
        try:
            ok = run_step(client, st, ctx, ops_log)
        except RpcError as e:
            ops_log.append({"label": st["label"], "ok": False,
                            "attempts": [{"transport_error": str(e)}]})
            ok = False
        entry = ops_log[n_log] if len(ops_log) > n_log else {}
        res = {"label": st["label"], "ms": now_ms() - t0,
               "calls": len(entry.get("attempts", []))}
        if ok is None:
            res["ok"] = "skipped"
            res["ms"] = 0
            coord.status(f"op={st['label']} skipped")
        else:
            res["ok"] = ok
            res["peak_nb_rss_bytes"], res["peak_tree_rss_bytes"] = \
                attempt_peaks(entry)
            coord.status(f"op={st['label']} {'ok' if ok else 'error'}")
        results.append(res)

    for st in shared:
        run_one(st)

    # Conflict scenarios: the file changes between the AI's read and its
    # write. Stacks that never write files get an honest skip instead.
    if args.stack in STALE_EDITS:
        baseline = args.fixture + ".baseline"
        with open(baseline, "w") as f:
            f.write(host_read(args.fixture, args.cidfile))
        for label, delay in (("conflict-0s", 0.0), ("conflict-10s", 10.0)):
            t0 = now_ms()
            n_log = len(ops_log)
            try:
                verdict = run_conflict(client, args.stack, ctx, coord, ops_log,
                                       args.fixture, baseline, delay, label,
                                       args.cidfile)
                entry = ops_log[n_log] if len(ops_log) > n_log else {}
                res = {"label": label, "ok": True, "ms": now_ms() - t0,
                       "calls": len(entry.get("attempts", []))}
                res["peak_nb_rss_bytes"], res["peak_tree_rss_bytes"] = \
                    attempt_peaks(entry)
                results.append(res)
                print(f"[driver] {label}: {verdict}")
            except (RpcError, OSError, ValueError, KeyError) as e:
                ops_log.append({"label": label, "ok": False,
                                "attempts": [{"transport_error": str(e)}]})
                results.append({"label": label, "ok": False,
                                "ms": now_ms() - t0, "calls": 0})
                coord.status(f"op={label} error")
    else:
        reason = CONFLICT_SKIP.get(args.stack, "read-only over MCP")
        for label in ("conflict-0s", "conflict-10s"):
            ops_log.append({"label": label, "skipped": reason})
            results.append({"label": label, "ok": "skipped", "ms": 0,
                            "calls": 0})
            coord.status(f"op={label} skipped")

    # The shared profile is complete (conflict scenarios included): let the
    # sampler take its window before the docs lookups run.
    coord.status("phase=profile-done")
    if not coord.wait_control("run-docs", timeout=900):
        coord.status("phase=error msg=run-docs signal never arrived")
        client.close()
        return 1

    for st in docs:
        run_one(st)

    def dump_ops(workflow=None):
        doc = {
            "stack": args.stack,
            "transport": client.kind,
            "time_to_ready_ms": ready_ms,
            "ready_basis": ready_basis,
            "tools": tool_names,
            "op_results": results,
            "log": ops_log,
        }
        if args.stack == "nbcli":
            doc["tools_note"] = ("CLI pseudo-tools, one `nb` shell invocation "
                                 "each; nb-cli exposes no MCP tools")
        if workflow is not None:
            doc["workflow"] = workflow
        with open(args.ops_out, "w") as f:
            json.dump(doc, f, indent=2)

    dump_ops()
    coord.status("phase=ops-done")
    coord.wait_control("exit", timeout=300)
    # Tool-call counting (see run_workflow_probe): deliberately AFTER the
    # sampler's last window so the published memory numbers cannot move.
    workflow = run_workflow_probe(args.stack, client, ctx,
                                  Path(args.fixture).parent)
    if workflow is not None:
        dump_ops(workflow)
    client.close()
    print(f"[driver] done: {[(r['label'], r['ok'], r['ms']) for r in results]}")
    return 0 if all(r["ok"] is not False for r in results) else 1


if __name__ == "__main__":
    sys.exit(main())
