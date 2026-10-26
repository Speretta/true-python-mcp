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
"""

import argparse
import json
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
        step("delete", "cell_delete", [lambda c: {
            "path": NB, "cell": 20, "revision": c["revision"]}],
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
    ap.add_argument("--dump-tools", help="write tools/list result here and exit")
    ap.add_argument("argv", nargs="*", help="stdio: command that runs the server")
    args = ap.parse_args()

    coord = Coordinator(args.status, args.control)
    ops_log = []
    ctx = {}

    # The marimo-format fixture doubles as the lint target; analyzer-style
    # servers receive its source as a code string.
    py_path = Path(args.fixture).parent / "bench.py"
    if py_path.is_file():
        ctx["py_source"] = py_path.read_text()

    if args.url:
        client = HttpClient(args.url)
    else:
        if not args.argv:
            print("stdio stack needs a server command after --", file=sys.stderr)
            return 2
        client = StdioClient(args.argv, args.ops_out + ".server-stderr.log")

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
    coord.status(f"tools_count={len(tool_names)}")
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
        try:
            ok = run_step(client, st, ctx, ops_log)
        except RpcError as e:
            ops_log.append({"label": st["label"], "ok": False,
                            "attempts": [{"transport_error": str(e)}]})
            ok = False
        if ok is None:
            results.append((st["label"], "skipped", 0))
            coord.status(f"op={st['label']} skipped")
        else:
            results.append((st["label"], ok, now_ms() - t0))
            coord.status(f"op={st['label']} {'ok' if ok else 'error'}")

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
            try:
                verdict = run_conflict(client, args.stack, ctx, coord, ops_log,
                                       args.fixture, baseline, delay, label,
                                       args.cidfile)
                results.append((label, True, now_ms() - t0))
                print(f"[driver] {label}: {verdict}")
            except (RpcError, OSError, ValueError, KeyError) as e:
                ops_log.append({"label": label, "ok": False,
                                "attempts": [{"transport_error": str(e)}]})
                results.append((label, False, now_ms() - t0))
                coord.status(f"op={label} error")
    else:
        reason = CONFLICT_SKIP.get(args.stack, "read-only over MCP")
        for label in ("conflict-0s", "conflict-10s"):
            ops_log.append({"label": label, "skipped": reason})
            results.append((label, "skipped", 0))
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

    with open(args.ops_out, "w") as f:
        json.dump({
            "stack": args.stack,
            "transport": client.kind,
            "time_to_ready_ms": ready_ms,
            "tools": tool_names,
            "op_results": [{"label": l, "ok": o, "ms": m} for l, o, m in results],
            "log": ops_log,
        }, f, indent=2)

    coord.status("phase=ops-done")
    coord.wait_control("exit", timeout=300)
    client.close()
    print(f"[driver] done: {results}")
    return 0 if all(ok is not False for _, ok, _ in results) else 1


if __name__ == "__main__":
    sys.exit(main())
