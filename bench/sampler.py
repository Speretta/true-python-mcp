#!/usr/bin/env python3
"""Sample one container's memory and process tree across the bench phases.

Waits for the driver to report phase=ready, lets the stack quiesce, samples
the idle window, tells the driver to run its operations (control file),
waits for phase=ops-done, quiesces again, samples the post-operation window,
then tells the driver to exit.

Primary metric: container cgroup memory (`docker stats` MemUsage) - the
accounting docker itself enforces limits with. It counts every process in
the container exactly once, with no shared-page double counting.

Secondary: per-process RSS, walking the tree from the container's init pid
through the host /proc. The captured process list is written next to the
numbers so readers can audit exactly what was counted.

Usage:
  sampler.py --stack NAME --cidfile CF --status S --control C --out DIR
             [--fast]
"""

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

PAGE = os.sysconf("SC_PAGE_SIZE")


def sh(argv):
    return subprocess.run(argv, capture_output=True, text=True, timeout=120)


def container_pid(cid):
    r = sh(["docker", "inspect", "--format", "{{.State.Pid}}", cid])
    return int(r.stdout.strip())


def proc_tree(root):
    """Subtree of `root` as [{pid, ppid, rss_bytes, cmd}], via host /proc."""
    procs = {}
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        pid = int(entry)
        try:
            stat = Path(f"/proc/{pid}/stat").read_text()
            cmd = (
                Path(f"/proc/{pid}/cmdline")
                .read_bytes()
                .replace(b"\0", b" ")
                .decode("utf-8", "replace")
                .strip()
            )
        except OSError:
            continue
        rp = stat.rfind(")")
        fields = stat[rp + 2 :].split()
        # fields[0] = state (stat field 3); ppid = field 4; num_threads =
        # field 20; rss = field 24
        try:
            ppid = int(fields[1])
            threads = int(fields[17])
            rss = int(fields[21])
        except (IndexError, ValueError):
            continue
        procs[pid] = (ppid, rss, threads, cmd)
    out = []
    stack = [root]
    seen = set()
    while stack:
        p = stack.pop()
        if p in seen:
            continue
        seen.add(p)
        if p in procs:
            ppid, rss, threads, cmd = procs[p]
            out.append({"pid": p, "ppid": ppid, "rss_bytes": rss * PAGE,
                        "threads": threads, "cmd": cmd[:200]})
        for pid, (ppid, _, _, _) in procs.items():
            if ppid == p:
                stack.append(pid)
    return out


def stats_bytes(cid):
    r = sh(["docker", "stats", "--no-stream", "--format", "{{.MemUsage}}", cid])
    left = r.stdout.split("/")[0].strip()
    for suf, mul in (("GiB", 1024**3), ("MiB", 1024**2), ("KiB", 1024), ("B", 1)):
        if left.endswith(suf):
            return int(float(left[: -len(suf)].strip()) * mul)
    return None


def median(xs):
    s = sorted(xs)
    n = len(s)
    if n == 0:
        return None
    return s[n // 2] if n % 2 else (s[n // 2 - 1] + s[n // 2]) // 2


def sample_window(cid, samples, interval, raw):
    cg, rss_sums, trees = [], [], []
    for i in range(samples):
        try:
            b = stats_bytes(cid)
            tree = proc_tree(container_pid(cid))
        except (subprocess.TimeoutExpired, ValueError, subprocess.CalledProcessError):
            b, tree = None, []
        if b is not None:
            cg.append(b)
        rss_sums.append(sum(t["rss_bytes"] for t in tree))
        trees.append(tree)
        raw.append({"sample": i, "cgroup_bytes": b,
                    "rss_sum_bytes": rss_sums[-1], "procs": tree})
        if i < samples - 1:
            time.sleep(interval)
    mid = trees[len(trees) // 2] if trees else []
    summary = {
        "cgroup_median_bytes": median(cg),
        "rss_sum_median_bytes": median(rss_sums),
        "proc_count": len(mid),
        "threads_total": sum(t["threads"] for t in mid),
    }
    return summary, mid


def wait_phase(status_path, want, timeout, abort_on_error=True):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            lines = Path(status_path).read_text().splitlines()
        except OSError:
            lines = []
        for line in lines:
            if line.startswith("phase=error") and abort_on_error:
                return f"ERROR: {line}"
            if line == f"phase={want}":
                return None
        time.sleep(0.25)
    return f"TIMEOUT waiting for phase={want}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--stack", required=True)
    ap.add_argument("--cidfile", required=True)
    ap.add_argument("--status", required=True)
    ap.add_argument("--control", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--fast", action="store_true",
                    help="short quiesce windows and fewer samples (pilot runs)")
    args = ap.parse_args()

    if args.fast:
        idle_q, ops_q, samples, interval = 10, 5, 3, 1
        ready_timeout = 600
    else:
        idle_q, ops_q, samples, interval = 60, 30, 5, 2
        ready_timeout = 900

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    raw = []
    result = {"stack": args.stack, "mode": "fast" if args.fast else "normal"}

    def control(line):
        with open(args.control, "a") as f:
            f.write(line + "\n")

    def fail(msg):
        result["error"] = msg
        control("exit")
        (out / "summary.json").write_text(json.dumps(result, indent=2))
        print(f"[sampler] FAILED: {msg}", file=sys.stderr)
        return 1

    # Wait for the container id (docker writes it as soon as the container
    # starts, for both `docker run -d` and `docker run -i` children).
    deadline = time.time() + 120
    cid = ""
    while time.time() < deadline and not cid:
        try:
            cid = Path(args.cidfile).read_text().strip()
        except OSError:
            time.sleep(0.25)
    if not cid:
        return fail("no container id appeared")
    result["container_id"] = cid

    err = wait_phase(args.status, "ready", ready_timeout)
    if err:
        return fail(err)
    # Grab driver-reported facts from the status file.
    for line in Path(args.status).read_text().splitlines():
        if line.startswith("time_to_ready_ms="):
            result["time_to_ready_ms"] = int(line.split("=", 1)[1])
        if line.startswith("tools_count="):
            result["tools_count"] = int(line.split("=", 1)[1])

    time.sleep(idle_q)
    result["idle"], idle_procs = sample_window(cid, samples, interval, raw)
    (out / "procs_idle.txt").write_text(
        "PID PPID RSS_KB THREADS CMD\n"
        + "".join(f"{p['pid']} {p['ppid']} {p['rss_bytes'] // 1024} {p['threads']} {p['cmd']}\n"
                  for p in idle_procs)
    )

    control("run-ops")
    err = wait_phase(args.status, "ops-done", ready_timeout)
    if err:
        return fail(err)
    for line in Path(args.status).read_text().splitlines():
        if line.startswith("op="):
            result.setdefault("ops", []).append(line)

    time.sleep(ops_q)
    result["ops"], ops_procs = sample_window(cid, samples, interval, raw)
    (out / "procs_ops.txt").write_text(
        "PID PPID RSS_KB THREADS CMD\n"
        + "".join(f"{p['pid']} {p['ppid']} {p['rss_bytes'] // 1024} {p['threads']} {p['cmd']}\n"
                  for p in ops_procs)
    )

    control("exit")
    (out / "samples_raw.json").write_text(json.dumps(raw, indent=2))
    (out / "summary.json").write_text(json.dumps(result, indent=2))
    print(f"[sampler] {args.stack}: idle={result['idle']} ops={result['ops']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
