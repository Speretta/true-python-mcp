#!/usr/bin/env python3
"""Turn bench/results/** into the published comparison.

--print-summary DIR   print a compact markdown table of all runs (stdout)
--render DIR OUT      generate OUT (COMPARISON.md) from the static prose in
                      bench/comparison_static.md: the placeholder line
                      <!-- MEASURED_TABLE --> is replaced with the measured
                      tables (headline, per-component memory, latency
                      matrix, conflict verdicts), the environment block and
                      the captured process lists (the audit).

Numbers are medians across runs; every value comes from files the sampler
and driver wrote during a real measurement - nothing here is estimated.
Project names in every generated table are links to their repositories.
"""

import argparse
import json
import sys
from pathlib import Path

STACK_ORDER = ["tpmcp", "datalayer", "block", "cursor", "nbcli", "marimo",
               "analyzer", "runpython", "yzfly"]

# label, repository URL, category - the clickable identity of each stack.
LINKS = {
    "tpmcp": ("tpmcp (this project)",
              "https://github.com/Speretta/true-python-mcp",
              "notebook editor (files)"),
    "datalayer": ("datalayer/jupyter-mcp-server",
                  "https://github.com/datalayer/jupyter-mcp-server",
                  "live Jupyter pairing"),
    "block": ("block/mcp-jupyter",
              "https://github.com/block/mcp-jupyter",
              "live Jupyter pairing"),
    "cursor": ("jbeno/cursor-notebook-mcp",
               "https://github.com/jbeno/cursor-notebook-mcp",
               "notebook editor (files)"),
    "nbcli": ("jupyter-ai-contrib/nb-cli",
              "https://github.com/jupyter-ai-contrib/nb-cli",
              "notebook CLI (no resident process)"),
    "marimo": ("marimo built-in MCP",
               "https://github.com/marimo-team/marimo",
               "editor-embedded (read-only MCP)"),
    "analyzer": ("Anselmoo/mcp-server-analyzer",
                 "https://github.com/Anselmoo/mcp-server-analyzer",
                 "code analysis (strings)"),
    "runpython": ("pydantic/mcp-run-python (retired upstream)",
                  "https://github.com/pydantic/mcp-run-python",
                  "code execution (WASM)"),
    "yzfly": ("yzfly/mcp-python-interpreter",
              "https://github.com/yzfly/mcp-python-interpreter",
              "code execution (subprocess)"),
}

# Stacks with no resident process: each operation is one shell command, so
# idle/after memory is the container baseline and the per-call cost lives
# in the peak-RSS columns instead. The label is printed with the memory
# values so a ~0 MiB row can never be misread as a memory win.
CLI_STACKS = {"nbcli"}

OP_ORDER = ["read", "check", "edit", "insert", "execute", "delete",
            "reread", "lint", "analyze", "docs-cold", "docs-warm",
            "docs-members", "conflict-0s", "conflict-10s"]

PLACEHOLDER = "<!-- MEASURED_TABLE -->"


def link(stack):
    label, url, _ = LINKS[stack]
    return f"[{label}]({url})"


def mib(x):
    if x is None:
        return "n/a"
    return f"{x / 1024 / 1024:.1f}"


def mib_cell(x):
    if x is None:
        return "n/a"
    return f"{mib(x)} MiB"


def ms_cell(ms):
    if ms is None:
        return "—"
    if ms < 1000:
        return f"{ms:.0f} ms"
    return f"{ms / 1000:.1f} s"


def median(xs):
    s = sorted(x for x in xs if x is not None)
    n = len(s)
    if n == 0:
        return None
    return s[n // 2] if n % 2 else (s[n // 2 - 1] + s[n // 2]) // 2


def classify(cmd):
    """Map a process command line to its role, for the component table."""
    if "/tpmcp" in cmd and "python" not in cmd:
        return "tpmcp static binary"
    if cmd.strip() in ("sh", "sh -s", "bash", "sleep", "sleep infinity"):
        return "container shell (no resident notebook process)"
    if cmd.startswith("nb ") or cmd.endswith("/nb") or "/nb " in cmd:
        return "nb binary (Rust, per call)"
    if "jupyter-mcp-server" in cmd:
        return "jupyter-mcp-server (Python)"
    if "jupyter-lab" in cmd:
        return "JupyterLab server (Python)"
    if "ipykernel" in cmd:
        return "ipykernel (Python)"
    if "cursor-notebook-mcp" in cmd:
        return "cursor-notebook-mcp (Python)"
    if "marimo" in cmd:
        if " edit " in cmd or cmd.endswith("marimo") or "--mcp" in cmd:
            return "marimo server (Python)"
        return "marimo session kernel (Python)"
    if "deno" in cmd:
        return "Deno + Pyodide (WASM sandbox)"
    if "mcp-run-python" in cmd or "mcp_run_python" in cmd:
        return "mcp-run-python launcher (Python)"
    if "mcp-python-interpreter" in cmd or "mcp_python_interpreter" in cmd:
        return "mcp-python-interpreter (Python)"
    if "mcp-server-analyzer" in cmd or "mcp_server_analyzer" in cmd:
        return "mcp-server-analyzer (Python)"
    return cmd.strip()[:60] or "(unknown)"


def load(dirpath):
    """stack -> list of (run_dir, merged summary+meta+ops dicts)."""
    out = {}
    root = Path(dirpath)
    if not root.is_dir():
        return out
    for stack_dir in sorted(root.iterdir()):
        if not stack_dir.is_dir():
            continue
        for run_dir in sorted(stack_dir.iterdir()):
            summary = run_dir / "summary.json"
            if not summary.is_file():
                continue
            data = json.loads(summary.read_text())
            meta = run_dir / "meta.json"
            if meta.is_file():
                data.update(json.loads(meta.read_text()))
            ops = run_dir / "ops.json"
            if ops.is_file():
                data["_ops"] = json.loads(ops.read_text())
            out.setdefault(stack_dir.name, []).append((run_dir, data))
    return out


def aggregate(runs):
    vals = [r for _, r in runs]
    ready = []
    for r in vals:
        t = r.get("time_to_ready_ms")
        if t is not None:
            ready.append(t + (r.get("container_start_ms") or 0))
    return {
        "runs": len(vals),
        "idle_cgroup": median([r.get("idle", {}).get("cgroup_median_bytes") for r in vals]),
        "profile_cgroup": median([r.get("profile", {}).get("cgroup_median_bytes") for r in vals]),
        "ops_cgroup": median([r.get("ops", {}).get("cgroup_median_bytes") for r in vals]),
        "idle_rss": median([r.get("idle", {}).get("rss_sum_median_bytes") for r in vals]),
        "profile_rss": median([r.get("profile", {}).get("rss_sum_median_bytes") for r in vals]),
        "ops_rss": median([r.get("ops", {}).get("rss_sum_median_bytes") for r in vals]),
        "procs_idle": median([r.get("idle", {}).get("proc_count") for r in vals]),
        "procs_profile": median([r.get("profile", {}).get("proc_count") for r in vals]),
        "procs_ops": median([r.get("ops", {}).get("proc_count") for r in vals]),
        "image_mb": median([r.get("image_size_bytes") for r in vals]),
        "ready_ms": median(ready),
        "mode": vals[0].get("mode", "?") if vals else "?",
    }


def op_latencies(runs, label):
    """Median ms of one op label across runs; None if never run ok."""
    vals = []
    for _, r in runs:
        ops = r.get("_ops") or {}
        for entry in ops.get("op_results", []):
            if entry.get("label") == label and entry.get("ok") is True:
                vals.append(entry.get("ms"))
    return median(vals)


def op_skipped(runs, label):
    for _, r in runs:
        ops = r.get("_ops") or {}
        for entry in ops.get("log", []):
            if entry.get("label") == label and entry.get("skipped"):
                return entry["skipped"]
    return None


def conflict_verdicts(runs):
    """label -> verdict string from the most common run outcome."""
    out = {}
    for _, r in runs:
        ops = r.get("_ops") or {}
        for entry in ops.get("log", []):
            if str(entry.get("label", "")).startswith("conflict") \
                    and entry.get("verdict"):
                out.setdefault(entry["label"], []).append(
                    (entry["verdict"], entry.get("external_survived")))
    final = {}
    for label, seen in out.items():
        verdicts = {v for v, _ in seen}
        survived = {s for _, s in seen}
        final[label] = (sorted(verdicts)[0] if len(verdicts) == 1
                        else " / ".join(sorted(verdicts)),
                        survived == {True})
    return final


def median_run(runs, agg):
    target = agg["idle_cgroup"]
    best, best_diff = runs[0], None
    for run_dir, r in runs:
        v = r.get("idle", {}).get("cgroup_median_bytes")
        if v is None or target is None:
            continue
        diff = abs(v - target)
        if best_diff is None or diff < best_diff:
            best, best_diff = (run_dir, r), diff
    return best


def read_procs(run_dir, phase):
    p = run_dir / f"procs_{phase}.txt"
    if not p.is_file():
        return []
    rows = []
    for line in p.read_text().splitlines()[1:]:
        # PID PPID RSS_KB THREADS CMD (cmd may contain spaces)
        parts = line.split(maxsplit=4)
        if len(parts) < 5:
            continue
        try:
            rows.append({"pid": int(parts[0]), "ppid": int(parts[1]),
                         "rss_kb": int(parts[2]), "cmd": parts[4]})
        except ValueError:
            continue
    return rows


def print_summary(dirpath):
    results = load(dirpath)
    if not results:
        print("no results found")
        return
    print("| stack | runs | idle cgroup MiB | post-op cgroup MiB | procs idle/ops "
          "| ready s | image MB |")
    print("|---|---|---|---|---|---|---|")
    for stack in STACK_ORDER:
        if stack not in results:
            continue
        a = aggregate(results[stack])
        ready = f"{a['ready_ms'] / 1000:.1f}" if a["ready_ms"] else "n/a"
        img = f"{a['image_mb'] / 1024 / 1024:.0f}" if a["image_mb"] else "n/a"
        print(f"| {stack} | {a['runs']} | {mib(a['idle_cgroup'])} "
              f"| {mib(a['ops_cgroup'])} | {a['procs_idle']}/{a['procs_ops']} "
              f"| {ready} | {img} |")
    for stack, runs in results.items():
        for _, r in runs:
            if r.get("error"):
                print(f"NOTE: {stack} run{r.get('run', '?')}: {r['error']}",
                      file=sys.stderr)


def load_context7(dirpath):
    """Aggregate the remote context7 runs into one dict, or None when the
    context7 measurement is absent.

    context7_measure.py writes `results/context7/runN/context7.json` with no
    summary.json, so load() skips it and those files never join the memory
    tables; they are read directly here. Per-call latencies are medianed
    across runs and labeled REMOTE (a network round-trip, unlike every local
    stack).
    """
    root = Path(dirpath) / "context7"
    if not root.is_dir():
        return None
    files = sorted(root.glob("run*/context7.json"))
    if not files:
        return None
    resolve_cold, resolve_ms, query_cold, query_ms = [], [], [], []
    endpoint = mode = date = None
    resolve_args = query_args = None
    for f in files:
        d = json.loads(f.read_text())
        endpoint = d.get("endpoint") or endpoint
        mode = d.get("mode") or mode
        date = d.get("date_utc") or date
        r = d.get("resolve_library_id") or {}
        q = d.get("query_docs") or {}
        resolve_args = r.get("args") or resolve_args
        query_args = q.get("args") or query_args
        if r.get("cold_ms") is not None:
            resolve_cold.append(r["cold_ms"])
        resolve_ms.extend(r.get("latencies_ms") or [])
        if q.get("cold_ms") is not None:
            query_cold.append(q["cold_ms"])
        query_ms.extend(q.get("latencies_ms") or [])
    return {
        "runs": len(files),
        "endpoint": endpoint,
        "mode": mode,
        "date": date,
        "resolve_args": resolve_args or {},
        "query_args": query_args or {},
        "resolve_median": median(resolve_ms),
        "resolve_cold": median(resolve_cold),
        "query_median": median(query_ms),
        "query_cold": median(query_cold),
    }


def op_latencies(runs, label):
    """Median ms of one op label across runs; None if never run ok."""
    vals = []
    for _, r in runs:
        ops = r.get("_ops") or {}
        for entry in ops.get("op_results", []):
            if entry.get("label") == label and entry.get("ok") is True:
                vals.append(entry.get("ms"))
    return median(vals)


def op_skipped(runs, label):
    for _, r in runs:
        ops = r.get("_ops") or {}
        for entry in ops.get("log", []):
            if entry.get("label") == label and entry.get("skipped"):
                return entry["skipped"]
    return None


def conflict_verdicts(runs):
    """label -> verdict string from the most common run outcome."""
    out = {}
    for _, r in runs:
        ops = r.get("_ops") or {}
        for entry in ops.get("log", []):
            if str(entry.get("label", "")).startswith("conflict") \
                    and entry.get("verdict"):
                out.setdefault(entry["label"], []).append(
                    (entry["verdict"], entry.get("external_survived")))
    final = {}
    for label, seen in out.items():
        verdicts = {v for v, _ in seen}
        survived = {s for _, s in seen}
        final[label] = (sorted(verdicts)[0] if len(verdicts) == 1
                        else " / ".join(sorted(verdicts)),
                        survived == {True})
    return final


def median_run(runs, agg):
    target = agg["idle_cgroup"]
    best, best_diff = runs[0], None
    for run_dir, r in runs:
        v = r.get("idle", {}).get("cgroup_median_bytes")
        if v is None or target is None:
            continue
        diff = abs(v - target)
        if best_diff is None or diff < best_diff:
            best, best_diff = (run_dir, r), diff
    return best


def read_procs(run_dir, phase):
    p = run_dir / f"procs_{phase}.txt"
    if not p.is_file():
        return []
    rows = []
    for line in p.read_text().splitlines()[1:]:
        # PID PPID RSS_KB THREADS CMD (cmd may contain spaces)
        parts = line.split(maxsplit=4)
        if len(parts) < 5:
            continue
        try:
            rows.append({"pid": int(parts[0]), "ppid": int(parts[1]),
                         "rss_kb": int(parts[2]), "cmd": parts[4]})
        except ValueError:
            continue
    return rows


def print_summary(dirpath):
    results = load(dirpath)
    if not results:
        print("no results found")
        return
    print("| stack | runs | idle cgroup MiB | post-op cgroup MiB | procs idle/ops "
          "| ready s | image MB |")
    print("|---|---|---|---|---|---|---|")
    for stack in STACK_ORDER:
        if stack not in results:
            continue
        a = aggregate(results[stack])
        ready = f"{a['ready_ms'] / 1000:.1f}" if a["ready_ms"] else "n/a"
        img = f"{a['image_mb'] / 1024 / 1024:.0f}" if a["image_mb"] else "n/a"
        print(f"| {stack} | {a['runs']} | {mib(a['idle_cgroup'])} "
              f"| {mib(a['ops_cgroup'])} | {a['procs_idle']}/{a['procs_ops']} "
              f"| {ready} | {img} |")
    for stack, runs in results.items():
        for _, r in runs:
            if r.get("error"):
                print(f"NOTE: {stack} run{r.get('run', '?')}: {r['error']}",
                      file=sys.stderr)


def load_context7(dirpath):
    """Aggregate the remote context7 runs into one dict, or None when the
    context7 measurement is absent.

    context7_measure.py writes `results/context7/runN/context7.json` with no
    summary.json, so load() skips it and those files never join the memory
    tables; they are read directly here. Per-call latencies are medianed
    across runs and labeled REMOTE (a network round-trip, unlike every local
    stack).
    """
    root = Path(dirpath) / "context7"
    if not root.is_dir():
        return None
    files = sorted(root.glob("run*/context7.json"))
    if not files:
        return None
    resolve_cold, resolve_ms, query_cold, query_ms = [], [], [], []
    endpoint = mode = date = None
    resolve_args = query_args = None
    for f in files:
        d = json.loads(f.read_text())
        endpoint = d.get("endpoint") or endpoint
        mode = d.get("mode") or mode
        date = d.get("date_utc") or date
        r = d.get("resolve_library_id") or {}
        q = d.get("query_docs") or {}
        resolve_args = r.get("args") or resolve_args
        query_args = q.get("args") or query_args
        if r.get("cold_ms") is not None:
            resolve_cold.append(r["cold_ms"])
        resolve_ms.extend(r.get("latencies_ms") or [])
        if q.get("cold_ms") is not None:
            query_cold.append(q["cold_ms"])
        query_ms.extend(q.get("latencies_ms") or [])
    return {
        "runs": len(files),
        "endpoint": endpoint,
        "mode": mode,
        "date": date,
        "resolve_args": resolve_args or {},
        "query_args": query_args or {},
        "resolve_median": median(resolve_ms),
        "resolve_cold": median(resolve_cold),
        "query_median": median(query_ms),
        "query_cold": median(query_cold),
    }


def op_peaks(runs):
    """Per-op per-call peak RSS (CLI stacks) -> (nb binary, with kernel).

    Values are medians of the per-run peaks the driver captured from
    /proc/<pid>/status while each `nb` invocation ran; MCP stacks have no
    per-call peaks and simply contribute nothing.
    """
    nb, tree = {}, {}
    for _, r in runs:
        for e in (r.get("_ops") or {}).get("op_results", []):
            if e.get("peak_nb_rss_bytes") is not None:
                nb.setdefault(e["label"], []).append(e["peak_nb_rss_bytes"])
            if e.get("peak_tree_rss_bytes") is not None:
                tree.setdefault(e["label"], []).append(e["peak_tree_rss_bytes"])
    return ({k: median(v) for k, v in nb.items()},
            {k: median(v) for k, v in tree.items()})


def op_calls(runs, label):
    """Median number of calls (tool calls / shell invocations) per op."""
    vals = []
    for _, r in runs:
        for e in (r.get("_ops") or {}).get("op_results", []):
            if e.get("label") == label and e.get("calls") is not None:
                vals.append(e["calls"])
    return median(vals)


def pair_latency(cold, warm):
    """`<cold> cold, <warm> warm`, or "not measured" when neither value exists.

    A missing measurement is never estimated: it renders as the literal
    "not measured" so the table cannot pass a guess off as a number.
    """
    if cold is None and warm is None:
        return "not measured"
    parts = []
    if cold is not None:
        parts.append(f"{ms_cell(cold)} cold")
    if warm is not None:
        parts.append(f"{ms_cell(warm)} warm")
    return ", ".join(parts)


def docs_matrix_lines(results, ctx7):
    """The checkmark capability matrix behind both docs tables: short rows,
    short cells, and a latency row whose numbers are generated from the runs
    (never typed). `—` means not measured.

    The rows answer "which of these can this approach do", so the cells are
    mostly ✅/❌ and the prose that used to fill the old seven-column table
    lives in the captions instead."""
    tpmcp = results.get("tpmcp", [])
    docs_cold = op_latencies(tpmcp, "docs-cold")
    members = op_latencies(tpmcp, "docs-members")
    local = "not measured"
    if docs_cold is not None and members is not None:
        local = f"{ms_cell(docs_cold)} / {ms_cell(members)}"
    remote = "—"
    if ctx7 is not None and ctx7["query_median"] is not None:
        remote = ms_cell(ctx7["query_median"])
        if ctx7["resolve_median"] is not None:
            remote += f" / {ms_cell(ctx7['resolve_median'])}"
    rows = [
        ("Exact signature (installed code)", "✅", "❌", "✅"),
        ("Docstring, params, return", "✅", "❌", "✅"),
        ("Whole module surface (1 call)", "✅", "❌", "❌"),
        ("Docs for a library you don't have", "❌", "✅", "❌"),
        ("Web usage/setup examples", "❌", "✅", "❌"),
        ("Runtime-defined names (`andes.rl`)", "❌", "❌", "✅"),
        ("Offline", "✅", "❌", "✅"),
        ("**Latency (median)**", f"**{local}**", f"**{remote}**", "—"),
    ]
    lines = ["| | tpmcp docs | context7 | read the source |",
             "|---|:---:|:---:|:---:|"]
    lines.extend("| " + " | ".join(row) + " |" for row in rows)
    return lines


def docs_comparison_lines(results, ctx7):
    """The docs comparison matrix for COMPARISON.md, with the full caption.
    Every number is rendered from data (the docs ops of the stack results,
    the context7 runs); cells without data say so."""
    lines = ["#### Docs lookups: local resolution vs remote doc search vs "
             "reading the source", ""]
    lines.extend(docs_matrix_lines(results, ctx7))
    lines.append("")
    lines.append(
        "Same question on both sides: `json.dumps`, module `json`. The "
        "latency row reads `py_docs` cold / `py_module_members` for tpmcp and "
        "`query-docs` / `resolve-library-id` for context7; `—` = not measured. "
        "context7 returns doc **snippets** from a community web index, not "
        "signatures; tpmcp resolves statically against your installed code, "
        "so runtime-defined names (`andes.rl`-style lazy loaders) come back "
        "as an honest `did not resolve` naming the failing segment, and only "
        "reading the source sees them.")
    return lines


def context7_block(dirpath, results):
    """Render the remote context7 latency block plus the docs comparison
    table, or "" when context7 was not measured.

    The comparison table is emitted here because this is where the remote
    half of the comparison lives; its local half comes from `results` (the
    docs ops of the tpmcp runs) so every latency in it is generated, never
    typed.
    """
    ctx7 = load_context7(dirpath)
    if ctx7 is None:
        return ""
    endpoint = ctx7["endpoint"]
    rl = ctx7["resolve_args"].get("libraryName", "Python")
    rq = ctx7["resolve_args"].get("query", "json.dumps")
    qq = ctx7["query_args"].get("query", "json.dumps")
    lines = []
    lines.append(
        "### Context7 (remote): doc-snippet search, not symbol resolution")
    lines.append("")
    lines.append(
        f"Context7 is a **remote** MCP server ([context7.com](https://context7.com), "
        f"streamable HTTP at `{endpoint}`, `Authorization: Bearer <key>`). It is a "
        "different category from every local stack above. `resolve-library-id` and "
        "`query-docs` search Context7's cloud documentation index and return doc "
        "**snippets**; they do not resolve symbols against a local environment. "
        "tpmcp's `py_docs`/`py_module_members` in the latency table above are local "
        "and in-process: no interpreter, no API key, no network. The two are not "
        "directly comparable, so the rows below are labeled REMOTE. Upstream "
        "recommends an API key for higher rate limits (its README).")
    lines.append("")
    lines.append("| Tool (REMOTE, network round-trip) | Median | First call |")
    lines.append("|---|---|---|")
    lines.append(f"| `resolve-library-id` (libraryName `{rl}`, query `{rq}`) "
                 f"| {ms_cell(ctx7['resolve_median'])} "
                 f"| {ms_cell(ctx7['resolve_cold'])} |")
    lines.append(f"| `query-docs` (query `{qq}`) "
                 f"| {ms_cell(ctx7['query_median'])} "
                 f"| {ms_cell(ctx7['query_cold'])} |")
    lines.append("")
    lines.append(
        f"Measured {(ctx7['date'] or '?')[:10]} ({ctx7['mode']} mode), median of "
        f"{ctx7['runs']} run(s) x the per-call samples. The median covers repeat "
        "calls on an established session; \"first call\" is the first call in the "
        "session, so it can include the server-side cold path and connection "
        "reuse. Every call is a network round-trip.")
    lines.append("")
    lines.extend(docs_comparison_lines(results, ctx7))
    lines.append("")
    return "\n".join(lines)


def measured_section(results, ctx7=""):
    lines = []
    present = [s for s in STACK_ORDER if s in results]

    # ---- environment block ----
    modes, dockers, kernels, dates, loads = set(), set(), set(), [], []
    total_runs = 0
    for runs in results.values():
        total_runs += len(runs)
        for _, r in runs:
            modes.add(r.get("mode", "?"))
            dockers.add(r.get("docker_version", "?"))
            kernels.add(r.get("host_kernel", "?"))
            if r.get("date_utc"):
                dates.append(r["date_utc"])
            if r.get("loadavg_1m") is not None:
                loads.append(r["loadavg_1m"])
    span = (f"{min(dates)[:10]}" if dates else "?") + \
           (f" to {max(dates)[:10]}" if dates and min(dates)[:10] != max(dates)[:10] else "")
    lines.append(f"**Environment.** {total_runs} measured runs, mode: "
                 f"{', '.join(sorted(modes))}; docker {', '.join(sorted(dockers))}; "
                 f"kernel {', '.join(sorted(kernels))}; measured {span}. "
                 f"Every stack ran on the same machine, one at a time"
                 + (f"; host load average (1 min) during the runs: "
                    f"{min(loads):.2f}-{max(loads):.2f}." if loads else "."))
    lines.append("")

    # ---- headline table ----
    lines.append("| Server | Category | Idle RAM (cgroup) | After the full "
                 "profile (cgroup) | After + docs (cgroup) | Idle RAM (RSS "
                 "sum) | Processes idle/after | Ready in | Image size | "
                 "Per-call peak RSS (nb / with kernel) | Runs |")
    lines.append("|---|---|---|---|---|---|---|---|---|---|---|")
    cli_present = False
    for stack in present:
        a = aggregate(results[stack])
        ready = f"{a['ready_ms'] / 1000:.1f} s" if a["ready_ms"] else "n/a"
        img = f"{a['image_mb'] / 1024 / 1024:.0f} MB" if a["image_mb"] else "n/a"
        nb_by_op, tree_by_op = op_peaks(results[stack])
        nb_peak, tree_peak = median(list(nb_by_op.values())), \
            median(list(tree_by_op.values()))
        peaks = "—" if nb_peak is None and tree_peak is None else \
            f"{mib(nb_peak)} / {mib(tree_peak)} MiB"
        mem = mib_cell
        if stack in CLI_STACKS:
            cli_present = True
            note = " (no resident process)"
            mem = lambda v: mib_cell(v) + note
            ready += " (first spawn; no handshake)"
        lines.append(
            f"| {link(stack)} | {LINKS[stack][2]} | {mem(a['idle_cgroup'])} "
            f"| {mem(a['profile_cgroup'])} | {mem(a['ops_cgroup'])} "
            f"| {mem(a['idle_rss'])} "
            f"| {a['procs_idle']}/{a['procs_ops']} | {ready} | {img} "
            f"| {peaks} | {a['runs']} |")
    lines.append("")
    lines.append("Idle = after the MCP handshake and tools/list, then a "
                 "quiesce window. After = after the full operation profile "
                 "(read, check, edit, insert an algorithm cell, execute "
                 "where supported, delete, re-read, lint, and both conflict "
                 "scenarios where applicable), then a quiesce window. The "
                 "two After columns split it at the docs lookups: `py_docs` "
                 "(cold, then warm) and `py_module_members`. Only tpmcp has "
                 "them; every other stack records them as explicit skips and "
                 "its two columns land together. Cgroup memory is the "
                 "headline metric: it counts every "
                 "process in the container exactly once and excludes "
                 "file-cache pages. The RSS sum is what `ps` would add up: it "
                 "counts shared pages (libpython between a Jupyter server and "
                 "its kernels) once per process and includes file-backed pages "
                 "the cgroup leaves out, so it reads higher than the cost. "
                 "Ready = container start plus "
                 "handshake up to the answered tools/list. All values are "
                 "medians across the runs in the Runs column.")
    if cli_present:
        lines.append("")
        lines.append("The CLI row's memory values are labeled *no resident "
                     "process* on purpose: `nb` is one shell command per "
                     "operation, so idle and after-profile hold nothing but "
                     "the container's bare shell - the ~0 MiB row is not a "
                     "memory win. Its real per-call cost is the peak-RSS "
                     "column and the per-call table below; its Ready figure "
                     "is the first `nb` spawn (there is no handshake).")
    lines.append("")

    # ---- component breakdown ----
    lines.append("### What the memory is made of")
    lines.append("")
    lines.append("Per-process RSS from the median run of each stack (the "
                 "same data as the audit lists below, grouped by role). "
                 "The cgroup total is the fair sum; the RSS column shows "
                 "which component carries the weight.")
    lines.append("")
    lines.append("| Server | Component | Idle RSS | After profile RSS |")
    lines.append("|---|---|---|---|")
    for stack in present:
        agg = aggregate(results[stack])
        run_dir, r = median_run(results[stack], agg)
        idle = read_procs(run_dir, "idle")
        ops = read_procs(run_dir, "ops")

        def group(rows):
            g = {}
            for p in rows:
                role = classify(p["cmd"])
                g.setdefault(role, []).append(p["rss_kb"])
            return g

        gi, go = group(idle), group(ops)
        roles = list(gi)
        for role in go:
            if role not in roles:
                roles.append(role)
        if not roles:
            lines.append(f"| {link(stack)} | (no processes captured) | — | — |")
            continue
        for i, role in enumerate(roles):
            name = link(stack) if i == 0 else ""
            vi = f"{sum(gi.get(role, [])) / 1024:.1f} MiB" if role in gi else "—"
            vo = f"{sum(go.get(role, [])) / 1024:.1f} MiB" if role in go else "—"
            count_i = len(gi.get(role, []))
            suffix = f" ×{count_i}" if count_i > 1 else ""
            lines.append(f"| {name} | {role}{suffix} | {vi} | {vo} |")
        lines.append(f"| | **cgroup total (headline)** "
                     f"| **{mib(agg['idle_cgroup'])} MiB** "
                     f"| **{mib(agg['ops_cgroup'])} MiB** |")
    lines.append("")

    # ---- latency matrix ----
    lines.append("### Operation latency (median per call)")
    lines.append("")
    header = "| Operation | " + " | ".join(LINKS[s][0] for s in present) + " |"
    lines.append(header)
    lines.append("|---|" + "---|" * len(present))
    ready_row = ["**ready** (start → tools/list)"]
    for stack in present:
        a = aggregate(results[stack])
        ready_row.append(ms_cell(a["ready_ms"]))
    lines.append("| " + " | ".join(ready_row) + " |")
    for op in OP_ORDER:
        row = [f"**{op}**"]
        for stack in present:
            runs = results[stack]
            med = op_latencies(runs, op)
            if med is not None:
                row.append(ms_cell(med))
            else:
                skip = op_skipped(runs, op)
                row.append("—" if skip else "n/a")
        lines.append("| " + " | ".join(row) + " |")
    lines.append("")
    lines.append("“—” = the stack has no such tool or it cannot run "
                 "headless (the reason is recorded per run in the audit); "
                 "“n/a” = the operation was not attempted. execute means "
                 "“run the bubble-sort cell” for notebook stacks and “run "
                 "the same source as a snippet” for execution-only "
                 "servers. lint means ruff on the same marimo-format "
                 "fixture: by file path for tpmcp, as a code string for "
                 "mcp-server-analyzer (its only input shape). docs-cold is "
                 "the FIRST `py_docs` call in a fresh server process (empty "
                 "discovery and docstring caches); docs-warm is the "
                 "identical repeat call (caches populated); docs-members is "
                 "`py_module_members`, a distinct tool that shares the "
                 "already-warm discovery cache but enumerates the module "
                 "itself.")
    lines.append("")
    if ctx7:
        lines.append(ctx7)
        if not lines[-1].endswith("\n\n"):
            lines.append("")

    # ---- per-call peak RSS (CLI stacks) ----
    if cli_present:
        lines.append("### Per-call peak RSS: the CLI's real memory cost")
        lines.append("")
        lines.append("A CLI has no resident process, so the Idle/After "
                     "columns above hold no notebook process at all. What "
                     "each call actually costs is the peak RSS of the `nb` "
                     "process while it runs (its VmHWM, read from "
                     "/proc/pid/status during the call); *with kernel* is "
                     "the peak summed RSS of the call's whole process tree, "
                     "which is where the kernel `nb execute` spawns shows "
                     "up. Medians of the per-run peaks; calls per op counts "
                     "shell invocations (1 per operation unless the variant "
                     "retries).")
        lines.append("")
        lines.append("| Operation | nb binary peak | with kernel | calls "
                     "per op |")
        lines.append("|---|---|---|---|")
        for stack in present:
            if stack not in CLI_STACKS:
                continue
            nb_by_op, tree_by_op = op_peaks(results[stack])
            for op in OP_ORDER:
                if op not in nb_by_op and op not in tree_by_op:
                    continue
                calls = op_calls(results[stack], op)
                lines.append(
                    f"| **{op}** ({LINKS[stack][0]}) "
                    f"| {mib_cell(nb_by_op.get(op))} "
                    f"| {mib_cell(tree_by_op.get(op))} "
                    f"| {calls if calls is not None else '—'} |")
        lines.append("")


    # ---- conflict verdicts ----
    lines.append("### The file changes while the AI works")
    lines.append("")
    lines.append("Scenario: the AI reads cell 4, the file is then changed "
                 "on disk behind its back (simulating a VS Code save), and "
                 "the AI's planned edit lands with pre-change knowledge. "
                 "Two variants: the external change almost immediately "
                 "(0 s) and after a 10 s “thinking” pause. The verdict is "
                 "read back from the file on disk.")
    lines.append("")
    lines.append("| Server | External change after 0 s | External change "
                 "after 10 s | External edit survived? |")
    lines.append("|---|---|---|---|")
    any_conflict = False
    for stack in present:
        v = conflict_verdicts(results[stack])
        if not v:
            reason = op_skipped(results[stack], "conflict-0s")
            if reason:
                lines.append(f"| {link(stack)} | skipped: {reason} | "
                             f"skipped | — |")
            continue
        any_conflict = True
        c0 = v.get("conflict-0s", ("not run", None))[0]
        c10 = v.get("conflict-10s", ("not run", None))[0]
        survived = all(x[1] for x in v.values())
        lines.append(f"| {link(stack)} | {c0} | {c10} "
                     f"| {'yes' if survived else 'NO - lost'} |")
    if not any_conflict:
        lines.append("| (no stack ran the conflict scenarios in this "
                     "measurement) | | | |")
    lines.append("")

    # ---- op outcomes + skips ----
    lines.append("**Operation outcomes per stack** (from the run status "
                 "files):")
    lines.append("")
    for stack in present:
        _, r = results[stack][0]
        ops = r.get("op_status") or []
        rendered = ", ".join(
            o.split("=", 1)[1].replace(" ", ": ") if "=" in o else o
            for o in ops) or "none recorded"
        lines.append(f"- {link(stack)}: {rendered}")
    lines.append("")

    # ---- raw process audit ----
    lines.append("**Process audit.** What was actually running in each "
                 "container, captured from the host during the median run "
                 "(idle window, then after the profile):")
    lines.append("")
    for stack in present:
        agg = aggregate(results[stack])
        run_dir, r = median_run(results[stack], agg)
        for phase in ("idle", "ops"):
            p = run_dir / f"procs_{phase}.txt"
            if not p.is_file():
                continue
            body = p.read_text().rstrip("\n")
            label = "idle" if phase == "idle" else "after profile"
            lines.append("````text")
            lines.append(f"# {stack} ({label}) - run {r.get('run', '?')}")
            lines.append(body)
            lines.append("````")
            lines.append("")
    return "\n".join(lines)


def render(dirpath, outfile):
    static_path = Path(__file__).parent / "comparison_static.md"
    if not static_path.is_file():
        print(f"missing static prose: {static_path}", file=sys.stderr)
        return 1
    static = static_path.read_text()
    if PLACEHOLDER not in static:
        print(f"placeholder {PLACEHOLDER} not found in {static_path}",
              file=sys.stderr)
        return 1
    results = load(dirpath)
    if not results:
        print("no results found - run bench/measure.sh first", file=sys.stderr)
        return 1
    ctx7 = context7_block(dirpath, results)
    doc = static.replace(PLACEHOLDER, measured_section(results, ctx7))
    Path(outfile).write_text(doc)
    print(f"{outfile} generated from {sum(len(v) for v in results.values())} "
          f"runs across {len(results)} stacks")
    return 0


README_HEADING = "## Memory footprint"
DOCS_HEADING = "## Docs lookups"


def build_readme_snippet(results):
    """The README-sized table: heading-anchored, fully generated."""
    present = [s for s in STACK_ORDER if s in results]
    lines = ["| Server | Idle RAM | After profile + docs | Processes "
             "idle/after | Ready in | Image size |",
             "|---|---|---|---|---|---|"]
    for stack in present:
        a = aggregate(results[stack])
        ready = f"{a['ready_ms'] / 1000:.1f} s" if a["ready_ms"] else "n/a"
        img = f"{a['image_mb'] / 1024 / 1024:.0f} MB" if a["image_mb"] else "n/a"
        note = " (no resident process)" if stack in CLI_STACKS else ""
        lines.append(f"| {link(stack)} | {mib(a['idle_cgroup'])} MiB{note} "
                     f"| {mib(a['ops_cgroup'])} MiB{note} "
                     f"| {a['procs_idle']}/{a['procs_ops']} | {ready} | {img} |")

    dockers, kernels, dates, modes = set(), set(), [], set()
    run_counts = []
    for runs in results.values():
        run_counts.append(len(runs))
        for _, r in runs:
            dockers.add(r.get("docker_version", "?"))
            kernels.add(r.get("host_kernel", "?"))
            modes.add(r.get("mode", "?"))
            if r.get("date_utc"):
                dates.append(r["date_utc"])
    when = max(dates)[:10] if dates else "(unknown date)"
    lines.append("")
    lines.append(
        f"Measured {when} by the [Bench workflow](.github/workflows/bench.yml) "
        f"({', '.join(sorted(kernels))}, docker {', '.join(sorted(dockers))}): "
        f"median of {min(run_counts)}-{max(run_counts)} runs per stack "
        f"({', '.join(sorted(modes))} mode), each stack in its own minimal "
        f"Docker container on the same machine. Full method, per-component "
        f"memory, latency and conflict tables: [COMPARISON.md](COMPARISON.md).")
    lines.append("")
    lines.append(
        "_Idle RAM_ and _After profile + docs_ are container cgroup totals, "
        "not process RSS. The cgroup figure excludes file-backed pages, so "
        "`ps` reports higher. tpmcp's profile additionally runs its docs "
        "lookups (`py_docs`, `py_module_members`), which the other stacks "
        "record as skips; [COMPARISON.md](COMPARISON.md) splits its memory "
        "table at those steps so the shared profile stays directly "
        "comparable.")
    return "\n".join(lines)


def build_docs_snippet(results, dirpath):
    """The README docs matrix: heading-anchored like the memory section and
    generated from the same runs (the docs ops of the tpmcp runs, the
    context7 runs for the remote column)."""
    ctx7 = load_context7(dirpath)
    lines = ["Both docs tools resolve against your installed code and work "
             "offline. context7 is a remote doc-snippet search: a different "
             "category, useful for the opposite cases.", ""]
    lines.extend(docs_matrix_lines(results, ctx7))
    lines.append("")
    lines.append(
        "Generated by the [Bench workflow](.github/workflows/bench.yml) from "
        "the same runs as the memory table; `—` = not measured. Use context7 "
        "for usage examples and for libraries you do not have installed; use "
        "`py_docs` for the exact API of the code in front of you. Full method "
        "and limits: [COMPARISON.md](COMPARISON.md).")
    return "\n".join(lines)


def replace_section(text, heading, snippet):
    """Replace the body between `heading` and the next level-2 heading with
    `snippet`, or None when the heading is missing. The heading is the
    anchor: no HTML markers live in the README."""
    start = text.find(heading + "\n")
    if start == -1:
        return None
    body_start = start + len(heading) + 1
    nxt = text.find("\n## ", body_start)
    end = (nxt + 1) if nxt != -1 else len(text)
    return text[:body_start] + "\n" + snippet + "\n\n" + text[end:]


def update_readme(dirpath, readme_path):
    """Regenerate both generated README sections: the Memory-footprint table
    and the Docs lookups table. Missing headings are a loud failure."""
    results = load(dirpath)
    if not results:
        print("no results found - run bench/measure.sh first", file=sys.stderr)
        return 1
    path = Path(readme_path)
    text = path.read_text()
    new_text = text
    touched = []
    for heading, snippet in ((README_HEADING, build_readme_snippet(results)),
                             (DOCS_HEADING,
                              build_docs_snippet(results, dirpath))):
        replaced = replace_section(new_text, heading, snippet)
        if replaced is None:
            print(f"heading not found in {readme_path}: {heading!r}",
                  file=sys.stderr)
            return 1
        if replaced != new_text:
            touched.append(heading)
        new_text = replaced
    if not touched:
        print(f"{readme_path}: generated sections already up to date")
        return 0
    path.write_text(new_text)
    print(f"{readme_path}: sections updated: "
          + ", ".join(heading.removeprefix("## ") for heading in touched))
    return 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--print-summary", metavar="DIR")
    ap.add_argument("--render", nargs=2, metavar=("DIR", "OUT"))
    ap.add_argument("--update-readme", nargs=2, metavar=("DIR", "README"))
    args = ap.parse_args()
    if args.print_summary:
        print_summary(args.print_summary)
        return 0
    if args.render:
        return render(args.render[0], args.render[1])
    if args.update_readme:
        return update_readme(args.update_readme[0], args.update_readme[1])
    ap.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
