#!/usr/bin/env python3
"""Measure the remote context7 MCP server's doc-search latency.

context7 is a hosted MCP server (streamable HTTP) - a different category from
every local stack in this benchmark: `resolve-library-id` and `query-docs`
search Context7's cloud documentation index and return doc snippets, so every
call is a network round-trip. This script measures only those two tools'
latencies (there is no container and no memory to sample).

Auth: the API key is read from the CONTEXT7_TCP_MCP_BENCH_KEY environment
variable (never hardcoded, never committed) and sent as
`Authorization: Bearer <key>`. If the variable is absent the script exits 2,
which measure.sh treats as a non-fatal "skipped".

Usage:
  CONTEXT7_TCP_MCP_BENCH_KEY=... python3 context7_measure.py \
      --run 1 --out results/context7

Writes results/context7/run<N>/context7.json (latencies) plus ops.json (the
verbatim transcript) and meta.json. render.py reads these and emits the
"Context7 (remote)" latency block in COMPARISON.md.
"""

import argparse
import json
import os
import re
import shutil
import sys
from datetime import datetime, timezone
from pathlib import Path

from driver import HttpClient, RpcError, is_error, now_ms, result_text

ENDPOINT = "https://mcp.context7.com/mcp"
KEY_ENV = "CONTEXT7_TCP_MCP_BENCH_KEY"
DEFAULT_CALLS = 5  # per tool: the first call is "cold", the rest are repeats

# The same doc lookups tpmcp answers locally with py_docs/py_module_members,
# asked of context7 as a documentation-snippet search instead.
RESOLVE_ARGS = {"query": "json.dumps", "libraryName": "Python"}
QUERY_ARGS = {"query": "json.dumps", "language": "Python"}


def median(xs):
    s = sorted(xs)
    if not s:
        return None
    return s[len(s) // 2] if len(s) % 2 else (s[len(s) // 2 - 1] + s[len(s) // 2]) // 2


def measure(client, tool, args, calls, transcript):
    """Call `tool` `calls` times, recording each latency + response verbatim.

    Returns a summary dict (no response bodies - those live in the transcript);
    `cold_ms` is the first call, `latencies_ms` every call.
    """
    latencies, ok, last = [], True, None
    for i in range(calls):
        entry = {"tool": tool, "attempt": i, "arguments": args}
        try:
            t0 = now_ms()
            resp = client.request("tools/call", {"name": tool, "arguments": args})
            entry["ms"] = now_ms() - t0
            entry["response"] = resp
            latencies.append(entry["ms"])
            last = resp
            if is_error(resp):
                ok = False
        except RpcError as e:
            entry["transport_error"] = str(e)
            ok = False
            transcript.append(entry)
            break
        transcript.append(entry)
    return {"tool": tool, "args": args, "cold_ms": latencies[0] if latencies else None,
            "latencies_ms": latencies, "ok": ok, "response": last}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run", type=int, default=1)
    ap.add_argument("--out", required=True, help="results/context7 directory")
    ap.add_argument("--calls", type=int, default=DEFAULT_CALLS)
    ap.add_argument("--endpoint", default=ENDPOINT,
                    help="MCP streamable-HTTP URL (default: the hosted context7)")
    args = ap.parse_args()

    key = os.environ.get(KEY_ENV)
    if not key:
        print(f"{KEY_ENV} not set - skipping context7 measurement", file=sys.stderr)
        return 2  # non-fatal skip

    run_dir = Path(args.out) / f"run{args.run}"
    if run_dir.exists():
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True, exist_ok=True)

    transcript = []
    client = HttpClient(args.endpoint, headers={"Authorization": f"Bearer {key}"})
    try:
        resp = client.request("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "tpmcp-bench", "version": "0"},
        })
        transcript.append({"initialize": resp})
        if "error" in resp:
            raise RpcError(f"initialize failed: {resp['error']}")
        client.notify("notifications/initialized")
        tools = client.request("tools/list", {})
        transcript.append({"tools/list": tools})
        names = [t["name"] for t in (tools.get("result", {}).get("tools") or [])]

        res = measure(client, "resolve-library-id", dict(RESOLVE_ARGS),
                      args.calls, transcript)
        lib_id = None
        if res["ok"] and res["response"] is not None:
            m = re.search(r"Context7-compatible library ID:\s*(\S+)",
                          result_text(res["response"]))
            if m:
                lib_id = m.group(1)
        res["library_id"] = lib_id

        # query-docs needs the id chained from resolve-library-id; when the
        # search found nothing, fall back to a plausible id and let the call
        # record its own failure honestly instead of being skipped.
        qargs = dict(QUERY_ARGS)
        qargs["libraryId"] = lib_id or "/python/python"
        qres = measure(client, "query-docs", qargs, args.calls, transcript)
    except RpcError as e:
        print(f"context7 measurement failed: {e}", file=sys.stderr)
        client.close()
        return 1
    client.close()

    result = {
        "stack": "context7",
        "run": args.run,
        "endpoint": args.endpoint,
        "transport": "streamable HTTP (Authorization: Bearer <redacted>)",
        "tools": names,
        "date_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "mode": os.environ.get("BENCH_MODE", "normal"),
        "resolve_library_id": {
            k: res[k] for k in ("tool", "args", "cold_ms", "latencies_ms",
                                "ok", "library_id")
        },
        "query_docs": {
            k: qres[k] for k in ("tool", "args", "cold_ms", "latencies_ms", "ok")
        },
    }
    (run_dir / "context7.json").write_text(json.dumps(result, indent=2))
    (run_dir / "ops.json").write_text(
        json.dumps({"stack": "context7", "log": transcript}, indent=2))
    (run_dir / "meta.json").write_text(json.dumps({
        "stack": "context7", "run": args.run, "date_utc": result["date_utc"],
        "mode": result["mode"], "endpoint": args.endpoint,
        "note": "remote service; no container, no memory measurement",
    }, indent=2))
    print(f"context7 run{args.run}: resolve median={median(res['latencies_ms'])} ms "
          f"query median={median(qres['latencies_ms'])} ms")
    return 0


if __name__ == "__main__":
    sys.exit(main())
