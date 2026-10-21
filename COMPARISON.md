# How tpmcp compares to other Python notebook, analysis, and execution MCP servers

This document compares tpmcp with seven Python MCP servers across the five categories that touch Python code and notebooks: file-based notebook editing ([jbeno/cursor-notebook-mcp](https://github.com/jbeno/cursor-notebook-mcp)), live Jupyter pairing ([datalayer/jupyter-mcp-server](https://github.com/datalayer/jupyter-mcp-server), [block/mcp-jupyter](https://github.com/block/mcp-jupyter)), editor-embedded ([marimo's built-in MCP server](https://github.com/marimo-team/marimo/blob/main/docs/guides/editor_features/mcp.md): read-only, experimental, HTTP), code analysis ([Anselmoo/mcp-server-analyzer](https://github.com/Anselmoo/mcp-server-analyzer)), and code execution ([pydantic/mcp-run-python](https://github.com/pydantic/mcp-run-python), retired and archived upstream but measured and labeled as such, and [yzfly/mcp-python-interpreter](https://github.com/yzfly/mcp-python-interpreter)). For the docs capability (`py_docs`/`py_module_members`) it also surveys [context7](https://context7.com), a hosted documentation-retrieval service — a different category (remote doc-snippet search, not a notebook server).

We measured what no vendor publishes: memory use, process count, time to ready, disk footprint, and what happens when the file changes while the AI works. Everything ran on the same machine, with the same notebook and the same operations. We checked every project's README, docs site, and issue tracker and found no published memory or startup numbers for any of these stacks (searched 2026-10-15), so we measure them ourselves and show the full method.

The rules this document holds itself to: every version is pinned with `==`; everything is reproducible from [bench/](bench); the measured numbers below are injected by `bench/render.py` at measurement time, so this prose contains none of them by hand; every external claim carries its source URL and the date it was checked. Where a stack did not start as published, we say so and show the evidence. Where our harness had to deviate from a README, we disclose that too.

## The short version

- The field splits into five categories: file-based notebook editors, live-Jupyter pairing servers, an editor-embedded server, code analyzers, and code executors. Each answers a different need; the headline table below labels every stack with its category, and steps a category cannot serve are marked skipped with reasons, never faked.
- tpmcp is one static binary with no runtime to install. The other seven are Python stacks; datalayer and block also need a running JupyterLab server with a kernel next to the MCP process, and mcp-run-python also runs Deno hosting a Pyodide sandbox.
- Conflict test: the file is rewritten on disk between the AI's read and its write. tpmcp refuses the stale edit with a revision conflict. block/mcp-jupyter also refuses it, with a change-detection guard its README does not advertise; credit where it is due.
- cursor-notebook-mcp applies the stale edit over the external change: in our runs it was the only stack that lost the external edit. datalayer returns success while the file on disk keeps the external edit; its in-memory model and the disk silently diverge, a distinct failure mode.
- tpmcp edits are byte-exact. Change one line and the git diff shows one line; outputs, execution counts, and metadata stay untouched.
- ruff is compiled into the tpmcp binary and lints any Python file by path. mcp-server-analyzer offers ruff too, as a pip-bundled subprocess that takes code strings and never touches your files. That architecture contrast is why this benchmark includes it. None of the other stacks bundles a file linter (marimo's editor bundles Ruff internally, but that is an editor feature, not an MCP tool for your files).
- tpmcp also resolves Python symbols and module members locally: `py_docs` (`json.dumps` → signature, parameters, docstring) and `py_module_members` (`json` → the module surface), with the compiled-in ty and no interpreter. context7 does something different and complementary — a hosted `resolve-library-id` + `query-docs` that return doc snippets by query over the network. We measure tpmcp's lookups cold vs warm and label context7's as remote so the distinction stays honest.
- tpmcp handles Jupyter `.ipynb` and marimo `.py` offline, as plain files. marimo's own MCP server is experimental, HTTP-only, and read-only: it cannot edit notebooks over MCP at all.
- tpmcp never runs code, by design: no kernel, no background process, nothing to fight your editor. datalayer and block drive live Jupyter kernels; mcp-run-python executes inside Pyodide (WASM) in Deno; yzfly's interpreter runs submitted code as a host subprocess with no sandbox.
- Three projects (cursor-notebook-mcp, block/mcp-jupyter, mcp-python-interpreter) needed manual dependency pins before they would even start with today's resolver, and the official mcp-server-analyzer image errors on every analysis tool until it gets a writable workdir. Every such finding is dated, quoted, and reproducible via `bench/`; see [Startup findings](#startup-findings-all-disclosed-all-reproducible).

## Measured results

The numbers below were produced by `bench/measure.sh` on the pinned versions listed under Methodology; the environment block states when, where, and how. Every stack is shown in two states: **idle** (after the MCP handshake, before any notebook tool runs) and **after the fixed operation profile** (read, check, edit cell 4, insert an algorithm cell, execute where supported, delete the spare cell, re-read, lint, analyze, the docs lookups — `py_docs` cold and warm, `py_module_members` — and the two conflict scenarios where applicable). All values are medians, of the samples inside each window and across runs. The process lists and raw operation results under the table are part of the audit: they show exactly which processes were counted and what every tool call returned.

**Environment.** 24 measured runs, mode: normal; docker 28.0.4; kernel Linux 6.17.0-1022-azure; measured 2026-10-21. Every stack ran on the same machine, one at a time; host load average (1 min) during the runs: 0.00-0.41.

| Server | Category | Idle RAM (cgroup) | After the full profile (cgroup) | Idle RAM (RSS sum) | Processes idle/after | Ready in | Image size | Runs |
|---|---|---|---|---|---|---|---|---|
| [tpmcp (this project)](https://github.com/Speretta/true-python-mcp) | notebook editor (files) | 1.5 MiB | 2.0 MiB | 6.5 MiB | 1/1 | 0.1 s | 37 MB | 3 |
| [datalayer/jupyter-mcp-server](https://github.com/datalayer/jupyter-mcp-server) | live Jupyter pairing | 273.7 MiB | 337.3 MiB | 344.2 MiB | 3/4 | 5.6 s | 549 MB | 3 |
| [block/mcp-jupyter](https://github.com/block/mcp-jupyter) | live Jupyter pairing | 140.5 MiB | 196.5 MiB | 177.0 MiB | 2/3 | 3.6 s | 491 MB | 3 |
| [jbeno/cursor-notebook-mcp](https://github.com/jbeno/cursor-notebook-mcp) | notebook editor (files) | 62.1 MiB | 72.9 MiB | 82.1 MiB | 1/1 | 0.8 s | 238 MB | 3 |
| [marimo built-in MCP](https://github.com/marimo-team/marimo) | editor-embedded (read-only MCP) | 90.1 MiB | 90.2 MiB | 117.1 MiB | 2/2 | 2.2 s | 378 MB | 3 |
| [Anselmoo/mcp-server-analyzer](https://github.com/Anselmoo/mcp-server-analyzer) | code analysis (strings) | 71.0 MiB | 71.3 MiB | 81.7 MiB | 1/1 | 2.5 s | 246 MB | 3 |
| [pydantic/mcp-run-python (retired upstream)](https://github.com/pydantic/mcp-run-python) | code execution (WASM) | 79.0 MiB | 252.4 MiB | 122.6 MiB | 2/2 | 4.0 s | 295 MB | 3 |
| [yzfly/mcp-python-interpreter](https://github.com/yzfly/mcp-python-interpreter) | code execution (subprocess) | 44.8 MiB | 45.0 MiB | 59.6 MiB | 1/1 | 0.6 s | 205 MB | 3 |

Idle = after the MCP handshake and tools/list, then a quiesce window. After = after the full operation profile (read, check, edit, insert an algorithm cell, execute where supported, delete, re-read, lint, and both conflict scenarios where applicable), then a quiesce window. Cgroup memory is the headline metric: it counts every process in the container exactly once and excludes file-cache pages. The RSS sum double-counts pages shared between processes (for example libpython between a Jupyter server and its kernels), so treat it as the conservative upper view. Ready = container start plus handshake up to the answered tools/list. All values are medians across the runs in the last column.

### What the memory is made of

Per-process RSS from the median run of each stack (the same data as the audit lists below, grouped by role). The cgroup total is the fair sum; the RSS column shows which component carries the weight.

| Server | Component | Idle RSS | After profile RSS |
|---|---|---|---|
| [tpmcp (this project)](https://github.com/Speretta/true-python-mcp) | tpmcp static binary | 6.7 MiB | 14.0 MiB |
| | **cgroup total (headline)** | **1.5 MiB** | **2.0 MiB** |
| [datalayer/jupyter-mcp-server](https://github.com/datalayer/jupyter-mcp-server) | jupyter-mcp-server (Python) | 137.4 MiB | 151.6 MiB |
|  | JupyterLab server (Python) | 152.2 MiB | 167.6 MiB |
|  | ipykernel (Python) | 55.3 MiB | 108.2 MiB |
| | **cgroup total (headline)** | **273.7 MiB** | **337.3 MiB** |
| [block/mcp-jupyter](https://github.com/block/mcp-jupyter) | /usr/local/bin/python3.12 /usr/local/bin/mcp-jupyter | 85.5 MiB | 86.0 MiB |
|  | JupyterLab server (Python) | 90.8 MiB | 105.3 MiB |
|  | ipykernel (Python) | — | 55.6 MiB |
| | **cgroup total (headline)** | **140.5 MiB** | **196.5 MiB** |
| [jbeno/cursor-notebook-mcp](https://github.com/jbeno/cursor-notebook-mcp) | cursor-notebook-mcp (Python) | 82.1 MiB | 92.7 MiB |
| | **cgroup total (headline)** | **62.1 MiB** | **72.9 MiB** |
| [marimo built-in MCP](https://github.com/marimo-team/marimo) | marimo server (Python) | 104.8 MiB | 104.9 MiB |
|  | /usr/local/bin/python3.12 -c from multiprocessing.resource_t | 12.4 MiB | 12.4 MiB |
| | **cgroup total (headline)** | **90.1 MiB** | **90.2 MiB** |
| [Anselmoo/mcp-server-analyzer](https://github.com/Anselmoo/mcp-server-analyzer) | mcp-server-analyzer (Python) | 81.4 MiB | 81.5 MiB |
| | **cgroup total (headline)** | **71.0 MiB** | **71.3 MiB** |
| [pydantic/mcp-run-python (retired upstream)](https://github.com/pydantic/mcp-run-python) | mcp-run-python launcher (Python) | 63.5 MiB | 63.5 MiB |
|  | Deno + Pyodide (WASM sandbox) | 59.1 MiB | 241.7 MiB |
| | **cgroup total (headline)** | **79.0 MiB** | **252.4 MiB** |
| [yzfly/mcp-python-interpreter](https://github.com/yzfly/mcp-python-interpreter) | mcp-python-interpreter (Python) | 59.5 MiB | 59.6 MiB |
| | **cgroup total (headline)** | **44.8 MiB** | **45.0 MiB** |

### Operation latency (median per call)

| Operation | tpmcp (this project) | datalayer/jupyter-mcp-server | block/mcp-jupyter | jbeno/cursor-notebook-mcp | marimo built-in MCP | Anselmoo/mcp-server-analyzer | pydantic/mcp-run-python (retired upstream) | yzfly/mcp-python-interpreter |
|---|---|---|---|---|---|---|---|---|
| **ready** (start → tools/list) | 121 ms | 5.6 s | 3.6 s | 818 ms | 2.2 s | 2.5 s | 4.0 s | 618 ms |
| **read** | 1 ms | 9 ms | 1.5 s | 37 ms | 3 ms | — | — | — |
| **check** | 1 ms | — | — | 4 ms | — | — | — | — |
| **edit** | 1 ms | 13 ms | 3.0 s | 5 ms | — | — | — | — |
| **insert** | 1 ms | 39 ms | 3.0 s | 6 ms | — | — | — | — |
| **execute** | — | 1.0 s | 3.1 s | — | — | — | 2.0 s | 4 ms |
| **delete** | 1 ms | 15 ms | 3.0 s | 5 ms | — | — | — | — |
| **reread** | 1 ms | 9 ms | 1.5 s | 3 ms | — | — | — | — |
| **lint** | 4 ms | — | — | — | — | 18 ms | — | — |
| **analyze** | — | — | — | — | — | 211 ms | — | — |
| **conflict-0s** | 502 ms | 524 ms | 3.5 s | 510 ms | — | — | — | — |
| **conflict-10s** | 10.5 s | 10.5 s | 13.5 s | 10.5 s | — | — | — | — |

“—” = the stack has no such tool or it cannot run headless (the reason is recorded per run in the audit); “n/a” = the operation was not attempted. execute means “run the bubble-sort cell” for notebook stacks and “run the same source as a snippet” for execution-only servers. lint means ruff on the same marimo-format fixture: by file path for tpmcp, as a code string for mcp-server-analyzer (its only input shape).

### The file changes while the AI works

Scenario: the AI reads cell 4, the file is then changed on disk behind its back (simulating a VS Code save), and the AI's planned edit lands with pre-change knowledge. Two variants: the external change almost immediately (0 s) and after a 10 s “thinking” pause. The verdict is read back from the file on disk.

| Server | External change after 0 s | External change after 10 s | External edit survived? |
|---|---|---|---|
| [tpmcp (this project)](https://github.com/Speretta/true-python-mcp) | refused (revision guard) | refused (revision guard) | yes |
| [datalayer/jupyter-mcp-server](https://github.com/datalayer/jupyter-mcp-server) | applied; external edit survived | edit errored: Error executing tool edit_cell_source: old_string not found in cell source | yes |
| [block/mcp-jupyter](https://github.com/block/mcp-jupyter) | refused (change detection) | refused (change detection) | yes |
| [jbeno/cursor-notebook-mcp](https://github.com/jbeno/cursor-notebook-mcp) | applied over the external edit (external LOST) | applied over the external edit (external LOST) | NO - lost |
| [marimo built-in MCP](https://github.com/marimo-team/marimo) | skipped: read-only MCP surface | skipped | — |
| [Anselmoo/mcp-server-analyzer](https://github.com/Anselmoo/mcp-server-analyzer) | skipped: writes no files (code-string analysis only) | skipped | — |
| [pydantic/mcp-run-python (retired upstream)](https://github.com/pydantic/mcp-run-python) | skipped: execution-only server, writes no files | skipped | — |
| [yzfly/mcp-python-interpreter](https://github.com/yzfly/mcp-python-interpreter) | skipped: execution-only profile, writes no files | skipped | — |

**Operation outcomes per stack** (from the run status files):

- [tpmcp (this project)](https://github.com/Speretta/true-python-mcp): cgroup_median_bytes, rss_sum_median_bytes, proc_count, threads_total
- [datalayer/jupyter-mcp-server](https://github.com/datalayer/jupyter-mcp-server): cgroup_median_bytes, rss_sum_median_bytes, proc_count, threads_total
- [block/mcp-jupyter](https://github.com/block/mcp-jupyter): cgroup_median_bytes, rss_sum_median_bytes, proc_count, threads_total
- [jbeno/cursor-notebook-mcp](https://github.com/jbeno/cursor-notebook-mcp): cgroup_median_bytes, rss_sum_median_bytes, proc_count, threads_total
- [marimo built-in MCP](https://github.com/marimo-team/marimo): cgroup_median_bytes, rss_sum_median_bytes, proc_count, threads_total
- [Anselmoo/mcp-server-analyzer](https://github.com/Anselmoo/mcp-server-analyzer): cgroup_median_bytes, rss_sum_median_bytes, proc_count, threads_total
- [pydantic/mcp-run-python (retired upstream)](https://github.com/pydantic/mcp-run-python): cgroup_median_bytes, rss_sum_median_bytes, proc_count, threads_total
- [yzfly/mcp-python-interpreter](https://github.com/yzfly/mcp-python-interpreter): cgroup_median_bytes, rss_sum_median_bytes, proc_count, threads_total

**Process audit.** What was actually running in each container, captured from the host during the median run (idle window, then after the profile):

````text
# tpmcp (idle) - run 2
PID PPID RSS_KB THREADS CMD
6814 6790 6828 2 /tpmcp
````

````text
# tpmcp (after profile) - run 2
PID PPID RSS_KB THREADS CMD
6814 6790 14292 2 /tpmcp
````

````text
# datalayer (idle) - run 2
PID PPID RSS_KB THREADS CMD
8616 8590 140748 5 /usr/local/bin/python3.12 /usr/local/bin/jupyter-mcp-server
8642 8616 155812 3 /usr/local/bin/python3.12 /usr/local/bin/jupyter-lab --port 8888 --IdentityProvider.token MY_TOKEN --ip 127.0.0.1 --no-browser --allow-root --ServerApp.root_dir=/work
8652 8642 56604 13 /usr/local/bin/python3.12 -m ipykernel_launcher -f /root/.local/share/jupyter/runtime/kernel-bcecf6ca-317d-4b6e-90a9-1e0dbf5f1de0.json
````

````text
# datalayer (after profile) - run 2
PID PPID RSS_KB THREADS CMD
8616 8590 155212 8 /usr/local/bin/python3.12 /usr/local/bin/jupyter-mcp-server
8642 8616 171588 5 /usr/local/bin/python3.12 /usr/local/bin/jupyter-lab --port 8888 --IdentityProvider.token MY_TOKEN --ip 127.0.0.1 --no-browser --allow-root --ServerApp.root_dir=/work
8767 8642 54224 13 /usr/local/bin/python3.12 -m ipykernel_launcher -f /root/.local/share/jupyter/runtime/kernel-f59eb233-c80d-4e2a-af0c-a675fec64dc0.json
8652 8642 56604 13 /usr/local/bin/python3.12 -m ipykernel_launcher -f /root/.local/share/jupyter/runtime/kernel-bcecf6ca-317d-4b6e-90a9-1e0dbf5f1de0.json
````

````text
# block (idle) - run 2
PID PPID RSS_KB THREADS CMD
10069 10044 87552 3 /usr/local/bin/python3.12 /usr/local/bin/mcp-jupyter
10096 10069 93020 1 /usr/local/bin/python3.12 /usr/local/bin/jupyter-lab --port 8888 --IdentityProvider.token BLOCK --ip 127.0.0.1 --no-browser --allow-root --ServerApp.root_dir=/work
````

````text
# block (after profile) - run 2
PID PPID RSS_KB THREADS CMD
10069 10044 88064 5 /usr/local/bin/python3.12 /usr/local/bin/mcp-jupyter
10096 10069 107812 3 /usr/local/bin/python3.12 /usr/local/bin/jupyter-lab --port 8888 --IdentityProvider.token BLOCK --ip 127.0.0.1 --no-browser --allow-root --ServerApp.root_dir=/work
10196 10096 56956 13 /usr/local/bin/python3.12 -m ipykernel_launcher -f /work/.local/share/jupyter/runtime/kernel-75bee11a-1e76-40aa-80d8-63c628901703.json
````

````text
# cursor (idle) - run 1
PID PPID RSS_KB THREADS CMD
11262 11237 84020 3 /usr/local/bin/python3.12 /usr/local/bin/cursor-notebook-mcp --allow-root /work
````

````text
# cursor (after profile) - run 1
PID PPID RSS_KB THREADS CMD
11262 11237 94968 4 /usr/local/bin/python3.12 /usr/local/bin/cursor-notebook-mcp --allow-root /work
````

````text
# marimo (idle) - run 1
PID PPID RSS_KB THREADS CMD
12475 12447 107328 1 /usr/local/bin/python3.12 /usr/local/bin/marimo edit /work/bench.py --mcp --no-token --headless --port 2718 --host 0.0.0.0
12525 12475 12748 1 /usr/local/bin/python3.12 -c from multiprocessing.resource_tracker import main;main(6)
````

````text
# marimo (after profile) - run 1
PID PPID RSS_KB THREADS CMD
12475 12447 107456 1 /usr/local/bin/python3.12 /usr/local/bin/marimo edit /work/bench.py --mcp --no-token --headless --port 2718 --host 0.0.0.0
12525 12475 12748 1 /usr/local/bin/python3.12 -c from multiprocessing.resource_tracker import main;main(6)
````

````text
# analyzer (idle) - run 1
PID PPID RSS_KB THREADS CMD
13659 13634 83348 3 /app/.venv/bin/python3 /app/.venv/bin/mcp-server-analyzer
````

````text
# analyzer (after profile) - run 1
PID PPID RSS_KB THREADS CMD
13659 13634 83476 3 /app/.venv/bin/python3 /app/.venv/bin/mcp-server-analyzer
````

````text
# runpython (idle) - run 2
PID PPID RSS_KB THREADS CMD
16317 16293 65036 1 /usr/local/bin/python3.13 /usr/local/bin/mcp-run-python stdio
16381 16317 60504 7 deno run --allow-net --allow-read=./node_modules --node-modules-dir=auto src/main.ts stdio --return-mode=xml --deps=
````

````text
# runpython (after profile) - run 2
PID PPID RSS_KB THREADS CMD
16317 16293 65036 1 /usr/local/bin/python3.13 /usr/local/bin/mcp-run-python stdio
16381 16317 247520 7 deno run --allow-net --allow-read=./node_modules --node-modules-dir=auto src/main.ts stdio --return-mode=xml --deps=
````

````text
# yzfly (idle) - run 2
PID PPID RSS_KB THREADS CMD
17578 17551 60928 3 /usr/local/bin/python3.12 /usr/local/bin/mcp-python-interpreter --dir /work
````

````text
# yzfly (after profile) - run 2
PID PPID RSS_KB THREADS CMD
17578 17551 61056 3 /usr/local/bin/python3.12 /usr/local/bin/mcp-python-interpreter --dir /work
````


## Methodology

This section describes exactly what the harness in [bench/](bench) does. Nothing here is aspirational; every step maps to code you can read.

### The field, in five categories

- **Notebook editor via files**: tpmcp, [jbeno/cursor-notebook-mcp](https://github.com/jbeno/cursor-notebook-mcp): edit `.ipynb` on disk; no server, no kernel.
- **Live Jupyter pairing**: [datalayer/jupyter-mcp-server](https://github.com/datalayer/jupyter-mcp-server), [block/mcp-jupyter](https://github.com/block/mcp-jupyter): an MCP server next to a running JupyterLab; edits and execution go through the server and its kernels.
- **Editor-embedded**: [marimo's built-in MCP](https://github.com/marimo-team/marimo/blob/main/docs/guides/editor_features/mcp.md): the editor itself is the server; read-only, experimental, HTTP.
- **Code analysis**: [Anselmoo/mcp-server-analyzer](https://github.com/Anselmoo/mcp-server-analyzer): code strings in, findings out; never touches files.
- **Code execution**: [pydantic/mcp-run-python](https://github.com/pydantic/mcp-run-python) (retired upstream), [yzfly/mcp-python-interpreter](https://github.com/yzfly/mcp-python-interpreter): run submitted code; no notebook awareness.

A sixth category — **remote documentation retrieval** — is surveyed separately for the docs capability: [context7](https://context7.com) is a hosted MCP server that returns doc snippets for a library by query, not a notebook server, so it is compared only on the docs lookups (see [Feature comparison](#feature-comparison) and the Context7 block under Measured results).

### One minimal Docker image per stack

- **tpmcp** ([bench/tpmcp/Dockerfile](bench/tpmcp/Dockerfile)): `FROM scratch` plus the static musl binary, the same artifact published as a release asset and built in CI. No OS, no runtime. The image is the binary.
- **cursor** and **marimo** ([bench/cursor/Dockerfile](bench/cursor/Dockerfile), [bench/marimo/Dockerfile](bench/marimo/Dockerfile)): `FROM python:3.12-slim` plus their pinned pip installs.
- **datalayer** and **block** ([bench/datalayer/Dockerfile](bench/datalayer/Dockerfile), [bench/block/Dockerfile](bench/block/Dockerfile)): both build on the shared [bench/jlab-base.Dockerfile](bench/jlab-base.Dockerfile): `python:3.12-slim` plus the JupyterLab stack both projects require, so the two live-Jupyter stacks are compared on an identical base.
- **analyzer**: not built by us. The official published image `ghcr.io/anselmoo/mcp-server-analyzer:latest` (`python:3.13-slim` base, non-root user) is pulled, retagged `bench-analyzer:pin`, and the pulled digest (`image_id` in `meta.json`) is the real pin, because the upstream tag floats.
- **runpython** ([bench/runpython/Dockerfile](bench/runpython/Dockerfile)): `python:3.13-slim-bookworm` plus `mcp-run-python==0.0.22`; Deno is installed at build time (upstream pins no version, so the installed one is recorded in the image at `/deno-version.txt`), and the build pre-warms the Deno/Pyodide cache so measured runs never pay first-start download costs.
- **yzfly** ([bench/yzfly/Dockerfile](bench/yzfly/Dockerfile)): `python:3.12-slim` plus `mcp-python-interpreter==1.2.3` and the manual `mcp==1.30.0` pin it needs to start (see Startup findings).
- Entrypoints follow each project's own README: datalayer and block start `jupyter lab`, poll its `/api/status` endpoint until it answers, then `exec` the MCP server on stdio. marimo runs `marimo edit /work/bench.py --mcp --no-token --headless --port 2718 --host 0.0.0.0`. cursor runs `cursor-notebook-mcp --allow-root /work`. yzfly runs `mcp-python-interpreter --dir /work` (its jail flag). runpython runs `mcp-run-python stdio`. tpmcp's entrypoint is the binary itself. The analyzer image's own entrypoint is the server; the harness adds `-w /tmp -e RUFF_CACHE_DIR=/tmp/ruff-cache` to the `docker run` (see Startup findings).

### Identical fixture per format

- [bench/fixture/bench.ipynb](bench/fixture/bench.ipynb) has 21 cells (16 code, 5 markdown) with static text outputs, generated with the real `nbformat` library so it passes the validators the servers use. No server re-runs those outputs during the benchmark.
- [bench/fixture/bench.py](bench/fixture/bench.py) is the same notebook in marimo format, generated with marimo's own converter (its header carries `__generated_with = "0.25.1"`); a byte-identical roundtrip was verified before it was frozen as the fixture. It doubles as the lint target.
- Cells have fixed roles: **cell 4** is the greeting line every stack edits (and the target of the conflict scenarios); **cell 19** is a spare cell (`extra_4 = 4 * total`) that the delete step removes (the insert at index 5 shifts it to **index 20**). Every stack sees the same indices.
- `measure.sh` copies both fixtures fresh into `bench/work/<stack>/` for every run and mounts that directory at `/work`. The fixtures themselves are never modified, and no run sees another run's edits.

### The fixed operation profile

The driver ([bench/driver.py](bench/driver.py)) is a minimal headless MCP client: plain JSON-RPC (protocol version 2024-11-05) over stdio, or streamable HTTP for marimo. No browser, no IDE, nothing else in the tree. It performs the `initialize` + `tools/list` handshake, then runs the same semantic profile on every stack, in this order: **read → check → edit cell 4 → insert a new cell at index 5 → execute that cell → delete the spare cell at index 20 → re-read → lint → analyze → docs (`py_docs` cold, `py_docs` warm, `py_module_members`)**, then the two conflict scenarios below. The inserted cell is a self-contained bubble sort: a well-known algorithm with a fixed source, byte-identical for every stack, needing no prior notebook state.

How each category serves these semantics (the mapping is disclosed, not hidden):

- **execute**: the kernel stacks run the inserted *notebook cell* (datalayer `execute_cell` at index 5; block `execute_notebook_code` at position 5; block has no free-form code tool, it executes notebook cells on the live kernel). The execution-only servers (runpython, yzfly) run the *same source as a snippet* through `run_python_code`: a snippet never touches the notebook, while a cell execution runs inside it. That is the closest semantic match each category offers. tpmcp and cursor have no execute step (no kernel, by design); marimo's kernel belongs to the editor and is not callable over MCP.
- **lint**: tpmcp runs `ruff_check` **by file path** on the marimo-format fixture `bench.py`; analyzer runs `ruff-check` with the **same source as a code string**, its only input shape. Same bytes, two input models; the asymmetry (path vs string) is inherent to the two architectures and is disclosed here and in the latency table. Every other stack has no lint tool and is skipped with that reason.
- **analyze**: analyzer only. `analyze-code` chains ruff, ty, and vulture in three sequential subprocesses and returns one JSON document (`ruff_result`, `ty_result`, `vulture_result`, `summary` with a `code_quality_score`). tpmcp skips it ("check + lint already cover validation and diagnostics"); the rest skip with "no combined analyze tool".
- **check**: tpmcp `notebook_check`, cursor `notebook_validate`; marimo's `get_notebook_errors` is session-scoped and cannot run headless; datalayer and block have no validate/check tool.
- **docs**: tpmcp only. `py_docs` resolves a dotted symbol (here `json.dumps`) and `py_module_members` enumerates a module (here `json`), both locally with the compiled-in ty — no interpreter, no subprocess. It is measured three ways on purpose: `docs-cold` is the FIRST call in the fresh server process (empty discovery and docstring caches), `docs-warm` the identical repeat call (caches populated), and `docs-members` is the module enumeration (a distinct tool that shares the now-warm discovery cache). Every other stack has no docs/symbol-lookup tool and is skipped with that reason.
- Setup calls that a design requires run first and are recorded: datalayer `use_notebook`, block `setup_notebook`, marimo `get_active_notebooks`.

Per-stack profiles in the driver: **tpmcp**: `notebook_read`, `notebook_check`, `cell_edit` (old/new string + revision), `cell_insert`, `cell_delete`, re-read, `ruff_check`, then `py_docs` (cold, then warm) and `py_module_members`. **datalayer**: `use_notebook`, `read_notebook` (detailed), `edit_cell_source` (cell_index, old/new string), `insert_cell`, `execute_cell`, `delete_cell`, re-read. **block**: `setup_notebook`, `query_notebook` (view_source of cell 4), `modify_notebook_cells` (edit_code / add_code / delete), `execute_notebook_code`. **cursor**: `notebook_read`, `notebook_validate`, `notebook_edit_cell` (full-source replacement, the only edit shape it offers), `notebook_add_cell`, `notebook_delete_cell`, re-read. **marimo**: `get_active_notebooks`, then `get_lightweight_cell_map`; headless, sessions belong to connected editor clients, so `get_active_notebooks` reports zero notebooks and the session-scoped tools cannot run. We measure the stack exactly as it runs headless and mark those steps skipped rather than faking a session. **analyzer**: `ruff-check` and `analyze-code` only; it is not a notebook server. **runpython** and **yzfly**: `run_python_code` only, execution-only profiles.

Tool schemas differ per project and per version, so each step carries argument variants: the driver tries them in order and records **every request and every response verbatim** in `ops.json`, including failed variants and every skipped step with its reason. The ops dumps quoted in this document come from those files.

### Conflict scenarios: the file changes while the AI works

Mechanism (`run_conflict` in [bench/driver.py](bench/driver.py)): the AI reads cell 4 through the server; the file is then rewritten **on disk** behind its back (cell 4 becomes `greeting = "hello bench (external edit)"`, simulating a user save in VS Code); then the AI's pre-planned **stale** edit lands (what it decided at read time, before the external change); then the verdict is read back **from the file**, not from the server's claims. Two timing variants run per stack: the external change after **0 s** and after a **10 s "thinking pause"**. After each scenario the driver restores the baseline copy, so every stack faces both variants from the same state. For datalayer, which must run as root (below), the external write goes through `docker exec`: the same bytes on disk, another writer, disclosed.

What the pilot runs showed (the render-generated table under Measured results carries the live verdicts; the behavior and the verbatim texts are the finding):

- **tpmcp** refused, identically at 0 s and 10 s: the revision is a content hash, not a timestamp, so a pause changes nothing. The stale `cell_edit` answered (run1 `ops.json`):

  ```
  revision conflict for /work/bench.ipynb: expected 86b8968026948473, file is now at e1b8ba2ec4183d3e; re-read the file and retry with its current revision
  ```

- **block/mcp-jupyter** refused with a change-detection guard its README does not advertise. We credit it explicitly. Verbatim (run1 `ops.json`):

  ```
  Error executing tool modify_notebook_cells: Notebook has changed since you last saw it (66c66dad856263896ca803ea52a6e5e3e6d948421741cd6493636bdf52c8cdbc --> 8858c30661f9cc05574bef6616290b1213dc9ae783fa1bf81ea12139ad9499e1). Use your view_source tool to update your knowledge of the notebook contents. If you are trying to edit/delete a specific cell, you can likely start by viewing just that cell rather than the whole notebook.
  ```

- **cursor-notebook-mcp** applied the stale edit **over** the external change; the file afterwards held `greeting = "hello bench (stale)"` and the external edit was gone. The tool answered `Successfully edited cell 4 in /work/bench.ipynb`. It was the only stack that lost data in our runs.
- **datalayer** returned **success** (`Cell 4 edited successfully!` plus a diff showing `-greeting = "hello bench (edited)"` / `+greeting = "hello bench (stale)"`) while the file on disk kept `greeting = "hello bench (external edit)"`. The server applied the edit to its own RTC/in-memory model, so the AI's belief and the disk diverge: silent divergence instead of data loss. The divergence runs both ways: at the start of the 10 s scenario, after the harness had restored the file on disk, datalayer's read still returned its model's value `greeting = "hello bench (stale)"`.
- **marimo, analyzer, runpython, yzfly**: not applicable (read-only over MCP, or writing no files in this profile) and marked skipped with those reasons in every run.

### Container users, and why they differ

The conflict scenarios rewrite the notebook **from the host** (the "VS Code save"), so participating stacks must not leave root-owned files behind: `measure.sh` runs tpmcp, cursor, and block as the host user (`--user $(id -u):$(id -g)` with a writable `HOME=/work`). datalayer is the exception: its jupyter-collaboration/RTC stack misbehaves when the container runs as a non-root user (measured 2026-10-15: every mutating tool timed out around 20 s and cell addressing failed with `Cell index 4 out of range`), so the harness runs it as root, exactly as its own README does, and the driver performs datalayer's external conflict write through `docker exec` instead (same bytes on disk, another writer; disclosed above and in the code). analyzer runs with the writable-workdir flags from its Startup finding; marimo's HTTP port is mapped on `127.0.0.1` only.

### Metrics

- **Headline: container cgroup memory**, read via `docker stats --no-stream` (`MemUsage`) by [bench/sampler.py](bench/sampler.py). This is the accounting Docker itself enforces limits with: it counts every process in the container exactly once, with no shared-page double counting, and excludes file-cache pages.
- **Secondary: per-process RSS**, walking the process tree from the container's init PID through the host `/proc`. RSS counts pages shared between processes (for example libpython shared by a Jupyter server and its kernels) once per process, so this sum slightly overstates multi-process stacks; the cgroup headline has no such distortion. The captured process lists (`procs_idle.txt`, `procs_ops.txt`, with PID, PPID, RSS, and full command line) are published next to the numbers so readers can audit exactly what was counted.
- **Time to ready**: for stdio stacks, container launch (the `docker run` that starts the server) to the answered `tools/list`. For marimo (HTTP), the container is brought up first and its start time is recorded separately in `meta.json`; the ready time covers the MCP handshake itself. Where a server re-runs setup work at every start (runpython's Deno dependency step), that cost is included in the ready time, not hidden.
- **Disk footprint**: the uncompressed image size from `docker image inspect`.

### Sampling and quiesce windows

After the handshake the stack quiesces for 60 s (normal mode), then the sampler takes 5 samples 2 s apart and reports the median. The driver then runs the operation profile; the stack quiesces again for 30 s, and another 5 samples give the post-operation median. `--fast` shortens this to 10 s/5 s with 3 samples and exists for smoke runs on a busy machine only; published numbers use normal mode. `render.py` aggregates medians across runs per stack.

### How the sampler and the driver coordinate

The sampler and the driver are separate processes that never talk to each other directly. They exchange two small files per run: the driver writes a status file (`phase=ready`, `time_to_ready_ms=…`, `tools_count=…`, one `op=LABEL ok|error|skipped` line per step, `phase=ops-done`), and the sampler writes a control file (`run-ops`, `exit`). The sequence per run is always the same:

1. `measure.sh` prepares a fresh work directory, starts the container, and launches sampler and driver.
2. The driver completes the MCP handshake and reports `phase=ready`.
3. The sampler waits out the idle quiesce window and samples it.
4. The sampler signals `run-ops`; the driver runs the profile and both conflict scenarios, then reports `phase=ops-done`.
5. The sampler waits out the post-operation window, samples it, signals `exit`, and writes the run's files.
6. `measure.sh` records `meta.json` (image size, image id/digest, mode, UTC date, Docker version, host kernel, host load average, available host memory, exit codes) and force-removes the container; an orphan guard makes sure nothing named `bench-*` survives a run.

### What each run leaves behind

Every run directory under `bench/results/<stack>/run<N>/` contains the full audit trail:

- `summary.json` — the medians for both windows (cgroup and RSS), process counts, time to ready, tool count, one line per operation with its outcome.
- `meta.json` — image size in bytes, image id (for analyzer, the pulled GHCR digest — the real pin), run mode (fast/normal), UTC timestamp, Docker version, host kernel, host load average, available host memory, driver and sampler exit codes.
- `ops.json` — every MCP request and every response verbatim, including failed argument variants, skipped steps with their reasons, and the conflict scenarios with their verdicts and the file state they left behind.
- `procs_idle.txt` and `procs_ops.txt` — the process trees that were counted: PID, PPID, RSS, and the full command line of each process.
- `samples_raw.json` — every individual sample, not just the medians.
- `driver.log`, `sampler.log`, and the server's own stderr — the consoles of all three processes.

The injected section of this document is built only from these files. If a number appears in the table, the run directory behind it says exactly how it came about.

### What render.py injects

`bench/render.py` replaces the placeholder above with, in order: the **environment block** (run count, mode, Docker version, kernel, date span, and host load average from the `meta.json` files, which also record available host memory), the **headline table** (idle and post-profile cgroup memory, RSS sum, process counts, ready time, image size, runs per stack, each stack linked to its repo with its category), the **per-component memory table** (role-classified RSS from the captured process lists: which component carries the weight), the **latency matrix** (median per call for every profile operation, ready time included), the **conflict verdict table** (0 s / 10 s verdicts and whether the external edit survived), the **operation outcomes** per stack, the **Context7 (remote) latency block** (only when a context7 measurement is present), and the **raw process audit** (the captured process lists verbatim).

### Pinned versions

Resolved from PyPI on 2026-10-15; no `@latest` anywhere, except the analyzer image, whose pin is its recorded digest:

- shared JupyterLab base: `python:3.12-slim`, `jupyterlab==4.6.4`, `jupyter-collaboration==5.0.4`, `ipykernel==7.4.0`
- datalayer: `jupyter-mcp-server==2.2.4`, `jupyter-mcp-tools==0.1.8`
- block: `mcp-jupyter==2.0.2`, plus three manual downpins it needs to start at all: `jupyter-kernel-client==0.8.0`, `mcp==1.15.0`, `pydantic==2.11.7` (see Startup findings)
- cursor: `cursor-notebook-mcp==0.3.1`, plus the two manual pins it needs to start: `pydantic==2.11.10`, `pydantic-settings==2.16.0` (see Startup findings)
- marimo: `marimo[mcp]==0.25.1`
- analyzer: the official image `ghcr.io/anselmoo/mcp-server-analyzer:latest` — the tag floats, so the pulled digest recorded in `meta.json` is the pin (0.4.1 is the current release on [PyPI](https://pypi.org/project/mcp-server-analyzer/json) as of 2026-10-15)
- runpython: `mcp-run-python==0.0.22` (final release, 2025-12-11); Deno installed at build time, version recorded in the image at `/deno-version.txt`
- yzfly: `mcp-python-interpreter==1.2.3`, plus the manual `mcp==1.30.0` pin it needs to start (see Startup findings)
- tpmcp: the static musl binary from the release assets (or built in CI from the tagged source)

### Fairness disclosures

- **Loopback binding.** The datalayer and block READMEs start JupyterLab with `--ip 0.0.0.0`. We bind to `127.0.0.1` inside the container for safety. This is the only deviation from their documented launch, and it cannot affect memory. marimo needs `--host 0.0.0.0` *inside* the container for the Docker port mapping to reach it; the host side of that mapping is bound to `127.0.0.1`, so it stays loopback-only.
- **Same machine, same run.** All eight stacks are measured on one machine within a single benchmark run; `meta.json` records the Docker version, host kernel, host load, and UTC timestamp per run, and the environment block under the table comes from those files.
- **Warm caches.** Every wheel is installed at image build time; nothing is downloaded or resolved during a measured run. Two disclosed exceptions, both visible in the captured server stderr: runpython's Deno dependency-install step re-runs at every container start (from the build-time cache), and analyzer's FastMCP queries pypi.org for a version check at startup.
- **Fresh kernels, no user imports.** The fixture imports only `json` and `math`; the inserted cell is a dependency-free bubble sort. An idle-kernel number that included pandas would measure the user's data stack, not the server.
- **Each format gets its own fixture.** datalayer, block, and cursor receive `bench.ipynb`; marimo receives `bench.py` in its native format. marimo's read-only MCP surface means its profile cannot include an edit: we benchmark the read path and say so instead of silently routing writes through marimo-pair (which is not an MCP server). analyzer receives the marimo fixture's source as a code string, the same content tpmcp lints by path; runpython and yzfly receive the same bubble-sort source as a snippet.
- **A retired stack is measured as retired.** pydantic/mcp-run-python is archived upstream; we benchmark its final release because the category exists and users still install it, and every table labels it as retired.
- **Remote is measured as remote.** context7 is a hosted service: its `resolve-library-id`/`query-docs` latencies are network round-trips (TLS, upstream search), not in-process work, so they are reported in a separate, clearly-labeled "Context7 (remote)" block and never in the local latency matrix. tpmcp's `py_docs` is local and in-process; putting the two in the same cell would mislead.

## What each server needs to run

The table collects the structural facts: category, who maintains each project, under which license, what must already be running before the MCP server can do anything, how many processes the stack spawns, and which transport it speaks. The paragraphs after it describe each tool surface and I/O style, with quotes from our own run1 audits where they show the shape of a response.

| Server | Category | Maintainer | License | Runtime prerequisites | Processes it spawns | Transport |
|---|---|---|---|---|---|---|
| tpmcp (this project) | notebook editor (files) | Speretta ([repo](https://github.com/Speretta/true-python-mcp)) | MIT OR Apache-2.0 | none — one static binary | 1 (the binary) | stdio |
| [datalayer/jupyter-mcp-server](https://github.com/datalayer/jupyter-mcp-server) 2.2.4 | live Jupyter pairing | Datalayer (org, company-backed) | BSD-3-Clause | Python ≥3.10 + a running JupyterLab with `jupyter-collaboration`, `jupyter-mcp-tools`, `ipykernel`; uv ≥0.6.14 for the uvx path | jupyter-lab + MCP server + one ipykernel per kernel | stdio (also Docker image and a hosted endpoint) |
| [block/mcp-jupyter](https://github.com/block/mcp-jupyter) 2.0.2 | live Jupyter pairing | Block, Inc (Goose team) | Apache-2.0 | uv + a running JupyterLab (`jupyterlab`, `ipykernel`; `jupyter-collaboration` recommended) | jupyter-lab + MCP server + ipykernel when executing | stdio |
| [jbeno/cursor-notebook-mcp](https://github.com/jbeno/cursor-notebook-mcp) 0.3.1 | notebook editor (files) | jbeno (individual) | CC BY-NC-SA 4.0 (**non-commercial**) | Python ≥3.10, pip package; no Jupyter server, no kernel | 1 Python process | stdio (SSE/HTTP modes also exist) |
| [marimo](https://github.com/marimo-team/marimo) 0.25.1 built-in MCP | editor-embedded | Marimo (org) | Apache-2.0 | Python ≥3.10, `marimo[mcp]`, a running `marimo edit` session | marimo server (+ a kernel subprocess per attached editor session; headless, none attaches) | streamable HTTP (`/mcp/server`) |
| [Anselmoo/mcp-server-analyzer](https://github.com/Anselmoo/mcp-server-analyzer) 0.4.1 | code analysis (strings) | Anselmoo (individual) | MIT | Python ≥3.13 + FastMCP; ruff/ty/vulture come bundled as pip dependencies; Biome tools need Node.js 22+ (the official image ships without it) | 1 Python process (+ brief ruff/ty/vulture subprocesses during `analyze-code`) | stdio |
| [pydantic/mcp-run-python](https://github.com/pydantic/mcp-run-python) 0.0.22 (retired) | code execution (WASM) | Pydantic (org) | MIT | Python + Deno (upstream pins no Deno version); Pyodide fetched into the Deno env | python launcher + deno | stdio (streamable-HTTP modes also exist) |
| [yzfly/mcp-python-interpreter](https://github.com/yzfly/mcp-python-interpreter) 1.2.3 | code execution (subprocess) | yzfly (individual) | **NOASSERTION** (GitHub license field) | pip package; executes in the selected Python environment (in our container: the container's own Python) | 1 Python process | stdio |

Prerequisites and process shapes are from the projects' READMEs and docs as quoted in the paragraphs below, and from the process lists our runs captured. Adoption figures are GitHub stars and PyPI downloads as of 2026-10-15 (re-checked 2026-10-16 for the three new stacks, where noted).

**tpmcp** exposes 13 notebook tools (plus 3 plain-Python file tools with `--file-tools`) answering in markdown or JSON. Every read returns a revision code; every write must send it back, and a stale write is refused with the new revision. Edits return the diff of what changed, and ruff lint/format is compiled in. The full `cell_edit` response from our run1 audit (musl release build, 2026-10-15):

````text
edited cell 4
path: /work/bench.ipynb
revision: b7893d5f20ae8acc

```diff
-greeting = "hello bench"
+greeting = "hello bench (edited)"
```
````

**datalayer/jupyter-mcp-server** is the de-facto leader of the notebook niche: 1,303 stars and roughly 55,600 PyPI downloads per month as of 2026-10-15 ([GitHub API](https://github.com/datalayer/jupyter-mcp-server), [pypistats](https://pypistats.org/api/packages/jupyter-mcp-server/recent)). Its docs list 22 tools plus a prompt; the four sandbox tools need the optional `jupyter_mcp_sandboxes` extra, so a plain install exposes the 18 core tools (our captured `tools/list` in `ops.json` confirms exactly those). Reads are structured and paginated (`response_format: brief|detailed`, `start_index`, `limit` — [read_notebook docs](https://jupyter-mcp-server.datalayer.tech/mcp/tools/read-notebook/)), and `edit_cell_source` does literal find-and-replace and "Returns a diff of the changes made" ([edit_cell_source docs](https://jupyter-mcp-server.datalayer.tech/mcp/tools/edit-cell-source/)). From our run1 audit (2.2.4):

```
@@ -1 +1 @@
-greeting = "hello bench"
+greeting = "hello bench (edited)"
```

It executes code on a live kernel or in cloud sandboxes (Kaggle, Colab, Modal, Daytona, E2B, CoreWeave, Cloudflare per its README). There is no revision guard: concurrency safety comes from RTC (jupyter-collaboration/yjs CRDT). They merge concurrent writes; they do not reject stale ones. The tracker has open integrity issues in exactly that area: [#263 "RTC mode: executing a cell deletes execution_count from all code cells; saved notebook fails nbformat validation"](https://github.com/datalayer/jupyter-mcp-server/issues/263) and [#251 "notebooks cells duplication"](https://github.com/datalayer/jupyter-mcp-server/issues/251). In our conflict scenarios it reported a successful stale edit while the file on disk kept the external change (see [Conflict scenarios](#conflict-scenarios-the-file-changes-while-the-ai-works)). Their published Docker image is 123.16 MB compressed (`datalayer/jupyter-mcp-server:0.22.0`, linux/arm64, [Docker Hub layers](https://hub.docker.com/layers/datalayer/jupyter-mcp-server/0.22.0/images/sha256-67ae1103ffaa69d462188ccd2796f6415d11c9bb4c735b8bf7078c3e2125a058?tab=layers), as of 2026-10-15).

**block/mcp-jupyter** is the org-backed entry from Block, Inc's Goose team (46 stars, ~115 downloads/month as of 2026-10-15 — [GitHub](https://github.com/block/mcp-jupyter), [pypistats](https://pypistats.org/api/packages/mcp-jupyter/recent)). It deliberately consolidates everything into 4 tools: `query_notebook` (all reads), `modify_notebook_cells` (all cell edits), `execute_notebook_code` (runs notebook cells on the live kernel; no free-form code tool exists), and `setup_notebook`. It requires a running JupyterLab and keeps kernel state between calls (README: "the state of your variables is preserved by the JupyterLab Kernel"). The `execute_notebook_code` response from our run1 audit (2.0.2), executing the inserted bubble-sort cell on a fresh kernel:

```
  "execution_count": 1,
  "outputs": [
    {
      "output_type": "execute_result",
      "metadata": {},
      "data": {
        "text/plain": "[1, 1, 2, 3, 4, 5, 6, 9]"
      },
      "execution_count": 1
    }
  ],
  "status": "ok"
```

The README opens by promising "no API compatibility guarantees" and warns "Breaking changes may occur in any release." Last push 2025-10-17, about 12 months before this writing. Its conflict behavior was the positive surprise of this benchmark: an unadvertised change-detection guard refused our stale edit (quoted in [Conflict scenarios](#conflict-scenarios-the-file-changes-while-the-ai-works)).

**jbeno/cursor-notebook-mcp** is the closest architectural analog to tpmcp: file-based, one Python process, no kernel, no Jupyter server (161 stars, ~970 downloads/month, last push 2025-11-08, as of 2026-10-15 — [GitHub](https://github.com/jbeno/cursor-notebook-mcp), [pypistats](https://pypistats.org/api/packages/cursor-notebook-mcp/recent)). Its README lists ~30 tools (our captured `tools/list` shows 29). `notebook_read` "Reads an entire notebook and returns its structure as a dictionary" (README); our run1 capture confirms it returns the whole notebook as raw JSON (0.3.1, run1, truncated here; the full response is the entire file):

```
{"cells":[{"cell_type":"markdown","id":"08acb465","metadata":{},"source":"# Bench fixture\n\nA fixed notebook used by the tpmcp benchmark. ...
```

Edits are full-source replacements (`notebook_edit_cell` "Replaces the source content"); responses carry no diff, and nothing in the README advertises a revision or conflict guard: the safety model is directory allow-lists plus nbformat validation. In our conflict scenarios the stale edit applied over the external change and the external edit was lost. The license is CC BY-NC-SA 4.0: **non-commercial use only** ([PyPI metadata](https://pypi.org/project/cursor-notebook-mcp/json)). PyPI ships 0.3.1 while the README header claims 0.3.2.

**marimo's built-in MCP server** ships inside marimo itself (23,192 stars for the whole project, Apache-2.0, 0.25.1 on [PyPI](https://pypi.org/project/marimo/json) as of 2026-10-15; ~2.45M downloads/month for all of marimo, not MCP-specific — [pypistats](https://pypistats.org/api/packages/marimo/recent)). The docs mark it experimental ("MCP features are currently experimental and under active development" — [mcp.md](https://github.com/marimo-team/marimo/blob/main/docs/guides/editor_features/mcp.md)) and read-only: it exposes the ten inspection tools (`get_active_notebooks`, `get_lightweight_cell_map`, `get_cell_outputs`, `lint_notebook`, and friends; our captured `tools/list` shows exactly ten, [tools.md](https://github.com/marimo-team/marimo/blob/main/docs/guides/editor_features/tools.md)), while the editing tools are explicitly not exposed over MCP. It speaks HTTP, not stdio. The headless reality we measured: sessions belong to connected editor clients, so with no browser attached `get_active_notebooks` reports zero notebooks and the session-scoped cell tools cannot run (marimo 0.25.1, run1):

```
      "total_notebooks": 0,
      "active_connections": 0
```

We measured the stack as it runs headless and marked the cell-map step skipped. [marimo-pair](https://github.com/marimo-team/marimo-pair) (428 stars, created 2026-02-24, as of 2026-10-15) is the companion write path, but it is a bash/curl/jq Agent-Skill that drives a running session, not an MCP server, and not part of this comparison.

**Anselmoo/mcp-server-analyzer** is not a market leader: 11 stars ([GitHub API](https://api.github.com/repos/Anselmoo/mcp-server-analyzer), checked 2026-10-16), ~436 PyPI downloads/month ([pypistats](https://pypistats.org/api/packages/mcp-server-analyzer/recent), as of 2026-10-15), MIT license, one individual maintainer, 0.4.1 current, Python ≥3.13 + FastMCP. It is here as the architecture contrast for tpmcp's built-in ruff. Its 8 tools (`ruff-check`, `ruff-format`, `ruff-check-ci`, `ty-check`, `vulture-scan`, `biome-check`, `biome-format`, `analyze-code`; our captured `tools/list`) are **string-in, string-out**: every tool takes `code` as a string, writes it to a temp file, runs the analyzer as a subprocess, and deletes the file. It never touches user files, has no notebook awareness, and offers no editing. ruff, ty, and vulture are **declared pip dependencies**, bundled by pip/uvx/Docker: against a common misconception, ruff does not need to be installed separately. The genuinely external prerequisite is Biome (Node.js 22+); the official GHCR image ships without Node, so `biome-check`/`biome-format` degrade to tool errors there while the other 6 tools work. From our captured server stderr (run1):

```
WARNING:mcp_server_analyzer.server:biome not available: Biome is not available: neither 'biome' nor 'npx biome' found: [Errno 2] No such file or directory: 'npx'
```

`ruff-check` on the same fixture source tpmcp lints by path returns structured JSON (run1, truncated here):

```
{"issues":[{"line":23,"column":5,"end_line":23,"end_column":11,"rule":"PLR1711","message":"Useless `return` statement at end of function","severity":"info","fixable":true}, ...
```

`analyze-code` chains ruff, ty, and vulture in three sequential subprocesses and merges them into one document (`ruff_result`, `ty_result`, `vulture_result`, `summary` with a `code_quality_score`).

**pydantic/mcp-run-python** was the most-used dedicated Python-execution MCP server: ~18,000 PyPI downloads/month ([pypistats](https://pypistats.org/api/packages/mcp-run-python/recent), as of 2026-10-15), 194 stars, MIT, org-backed by Pydantic ([GitHub API](https://api.github.com/repos/pydantic/mcp-run-python), `archived: true`, checked 2026-10-16). It is **retired**: the repository is archived and 0.0.22 (2025-12-11) is the final release. The README's retirement notice, verbatim ([README](https://raw.githubusercontent.com/pydantic/mcp-run-python/main/README.md)):

> there's just no safe way to run Python within pyodide safely with reasonable latency … **Python code running in pyodide can run arbitrary javascript** meaning it can do whatever the javascript runtime running pydodie can do, including: … OOMing the machine by consuming all memory - deno has no good way limit memory usage

The successor is [pydantic/monty](https://github.com/pydantic/monty), a Rust sandbox (8,676 stars, [GitHub API](https://api.github.com/repos/pydantic/monty), checked 2026-10-16); it has **no MCP surface yet** (checked 2026-10-15). The architecture: one tool, `run_python_code{python_code}` (our `tools/list` confirms it is the only one), executed in Pyodide — CPython compiled to WASM — inside Deno. The idle tree is a Python launcher plus a Deno process (see the process audit under the measured table). Deno's version is not pinned upstream; our image installs it at build time and records it at `/deno-version.txt`. The Deno dependency install re-runs at every container start (our captured server stderr opens with `Installing dependencies []...`) and is included in the ready time: the real cost of this architecture, not a measurement error. The bubble sort came back as (run1):

```
<status>success</status>
<return_value>
[
  1,
  1,
  2,
  3,
  4,
  5,
  6,
  9
]
</return_value>
```

**yzfly/mcp-python-interpreter** executes code as a **host subprocess** in the selected environment; in our container, the container's own Python. There is no sandbox: submitted code runs with the server user's full rights. The README documents a `--dir` jail flag (we run `--dir /work`) and an `MCP_ALLOW_SYSTEM_ACCESS` env var; our captured startup banner shows the jail engaged (run1 server stderr):

```
MCP Python Interpreter starting in directory: /work
Using default Python interpreter: /usr/local/bin/python3.12
System-wide file access: DISABLED
```

Adoption: 104 stars ([GitHub API](https://api.github.com/repos/yzfly/mcp-python-interpreter), checked 2026-10-16), ~8,287 PyPI downloads/month ([pypistats](https://pypistats.org/api/packages/mcp-python-interpreter/recent), as of 2026-10-15), last push 2026-07-02. **License: the GitHub license field says NOASSERTION**: no standard license is declared. We flag this prominently: we run and measure the code; we redistribute nothing. Its 10 tools (our `tools/list`) cover environment listing (`list_python_environments`, `list_installed_packages`), package installation (`install_package`), execution (`run_python_code`, `run_python_file`), generic file I/O (`read_file`, `write_file`, `list_directory`), and sessions (`clear_session`, `list_sessions`). Notebooks it does not know. The same bubble-sort source came back as (run1):

```
Execution in session 'default' (inline mode):

--- Output ---
(No output)
```

The snippet prints nothing, and the response carries no return-value section in this mode.

**context7** ([upstash/context7](https://github.com/upstash/context7), 63,439★, MIT, pushed 2026-10-21) is surveyed here as a **distinct category** — remote documentation retrieval — not a notebook editor or executor. It exposes two MCP tools, `resolve-library-id` and `query-docs`, which search Context7's cloud documentation index and return doc **snippets** for a library/framework, cross-language. It is a **remote** MCP server: streamable HTTP at `https://mcp.context7.com/mcp`, authenticated with `Authorization: Bearer <API key>` (the same server also runs locally over stdio via `npx @upstash/context7-mcp`). This is **not** what tpmcp does: tpmcp's `py_docs`/`py_module_members` resolve a dotted Python symbol against a local environment, in-process, with no interpreter, no API key, and no network, while context7 returns documentation snippets matched by query, over the network, and needs an API key. The two are complementary, and this document keeps that difference explicit rather than pretending one substitutes for the other.

## Startup findings (all disclosed, all reproducible)

Bringing the seven comparison stacks up inside pinned Docker images surfaced warts in five of them; marimo 0.25.1 installed and started exactly as its docs describe. We report the findings factually, with dates and quotes; they are maintenance signals of unpinned dependency ranges and unpublished runtime assumptions, not verdicts on the projects. Everything below is reproducible by building (or pulling) the images in `bench/`.

**cursor-notebook-mcp 0.3.1 does not start as published on PyPI.** Its sdist imports `pydantic-settings` at startup without declaring it, and dies with `"Error importing package components"`. On top of that, its `fastmcp<2.11` pin breaks on current pydantic with `"cannot specify both default and default_factory"`. Two manual pins were needed before the server would start: `pydantic==2.11.10` and `pydantic-settings==2.16.0` (both quoted from [bench/cursor/Dockerfile](bench/cursor/Dockerfile), resolved 2026-10-15). Also: PyPI says 0.3.1, its README says 0.3.2.

**block/mcp-jupyter 2.0.2 needs three manual downpins to start at all** (from [bench/block/Dockerfile](bench/block/Dockerfile), 2026-10-15):

- `jupyter-kernel-client==0.8.0` — the package declares `>=0.8.0` with no upper bound, and the 1.x releases removed the `KernelClient` export it imports.
- `mcp==1.15.0` — it uses `mcp.server.fastmcp`, which the MCP SDK 2.x removed.
- `pydantic==2.11.7` — mcp 1.15 imports pydantic internals that pydantic 2.14 dropped.

A year-old pin set no longer resolves to a starting server with today's packages. The same aging showed in its schema during bring-up (2026-10-15): a `modify_notebook_cells` call with `operation: "edit_cell"` was rejected with `"Invalid operation: edit_cell. Must be one of: add_code, edit_code, add_markdown, edit_markdown, delete"`; the published profile sends `edit_code` directly.

**mcp-python-interpreter 1.2.3 is the third project that does not start with today's dependency resolver** (after cursor-notebook-mcp and block/mcp-jupyter): it imports `mcp.server.fastmcp`, removed in MCP SDK 2.x. We pin `mcp==1.30.0` — quoted from [bench/yzfly/Dockerfile](bench/yzfly/Dockerfile): "mcp-python-interpreter 1.2.3 imports mcp.server.fastmcp, which the mcp SDK 2.x removed, so it does not start with today's resolver. Pinning the newest 1.x line fixes it".

**mcp-server-analyzer's official image cannot run its own tools with its own defaults.** The image's default workdir `/app` is not writable by its own non-root user, so ruff's cache initialization fails (`Failed to initialize cache at /app/.ruff_cache: Permission denied`, observed at bring-up 2026-10-15) and **every** analysis tool errors until the container gets a writable workdir or `RUFF_CACHE_DIR`. The README's own Docker client configuration hits the same wall. Our harness runs it with `-w /tmp -e RUFF_CACHE_DIR=/tmp/ruff-cache` (quoted from the comment in [bench/measure.sh](bench/measure.sh)); with that, all six non-Biome tools work — and the two Biome tools still fail, because the image has no Node.js (stderr quoted above).

**datalayer 2.2.4: the `cell_id` path of `edit_cell_source` fails internally.** During bring-up on 2026-10-15, addressing the edit by `cell_id` produced an internal `"quote_from_bytes() expected bytes"` error; `cell_index` works. The observation is recorded in [bench/driver.py](bench/driver.py)'s profile comment, and the published `ops.json` shows the working `cell_index` attempt verbatim, including the diff quoted above.

**datalayer's RTC stack misbehaves as a non-root user.** Measured 2026-10-15: running the container as the host user, every mutating tool timed out around 20 s and cell addressing failed with `Cell index 4 out of range`. The harness therefore runs datalayer as root — the same way its own README runs it — while tpmcp, cursor, and block run as the host user; datalayer's external conflict write goes through `docker exec` (see [Container users](#container-users-and-why-they-differ)).

**datalayer idles with an ipykernel already running.** Their open issue [#425](https://github.com/datalayer/jupyter-mcp-server/issues/425) asks whether kernels get created without the user asking. In our container, the idle process list, captured after the handshake and before any notebook tool ran, already shows an `ipykernel_launcher` process next to `jupyter-mcp-server` and `jupyter-lab`. When the driver then called `use_notebook`, it connected to a *new* kernel (datalayer 2.2.4, run1 `ops.json`):

```
[INFO] Connected to kernel '77b9548e-4429-4164-871a-3dea084f38ff'.
[INFO] Successfully activate notebook 'bench'.
```

The post-operation process list shows both kernels. We report what the lists show; the process lists under the measured table let you check every PID and command line yourself.

**mcp-run-python re-runs its Deno dependency install at every container start.** The captured server stderr begins with `Installing dependencies []...` followed by dozens of `install: Initialize …` lines before the server announces `Running mcp-run-python via stdio...`. The wheels come from the build-time cache, but the step is part of every startup and is included in the ready time. We disclose it as the cost of the architecture, not as a defect of the project.

**tpmcp and its fixture needed no fixes.** The image is `FROM scratch` plus the binary; the server started, answered the handshake, and ran the full profile including both conflict scenarios with no pins, patches, or workarounds.

## Feature comparison

Facts from the projects' READMEs/docs and issue trackers, checked 2026-10-15 (the three new stacks re-checked 2026-10-16 where noted); context7 checked 2026-10-21. Push dates from the GitHub API. One linked stack name per row; the guard column states what the projects advertise *and* what our conflict runs observed. The "Docs / symbol lookup" column is the new capability this benchmark measures: only tpmcp resolves symbols locally, and context7 (the last row) is a remote doc-snippet search — a different category, listed so the contrast is explicit.

| Server | Category | Revision/conflict guard | Executes code | Lint/format | Docs / symbol lookup | `.ipynb` | marimo `.py` | plain `.py` | Offline file editing | Transport | License | Last push |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| [tpmcp (this project)](https://github.com/Speretta/true-python-mcp) | notebook editor (files) | yes — revision code on every read/write; stale writes refused (observed) | never — by design | yes — ruff compiled into the binary, any `.py`/`.pyi`/`.ipynb` | yes — `py_docs`/`py_module_members`, local static resolution (no interpreter, no API) | yes | yes | yes (`py_read`/`py_write`/`py_edit` with `--file-tools`) | yes — plain files, no server | stdio | MIT OR Apache-2.0 | active (this repo) |
| [datalayer/jupyter-mcp-server](https://github.com/datalayer/jupyter-mcp-server) | live Jupyter pairing | none — RTC merge; divergence observed; open issues [#263](https://github.com/datalayer/jupyter-mcp-server/issues/263), [#251](https://github.com/datalayer/jupyter-mcp-server/issues/251) | yes — live kernel + cloud sandboxes | none advertised | no | yes (through the running Jupyter Server) | no | no | no — everything goes through the running server | stdio (Docker image + hosted endpoint also offered) | BSD-3-Clause | active — v2.2.4 released 2026-10-11 ([releases](https://api.github.com/repos/datalayer/jupyter-mcp-server/releases/latest)) |
| [block/mcp-jupyter](https://github.com/block/mcp-jupyter) | live Jupyter pairing | change detection observed, not advertised — refused our stale edit | yes — notebook cells on the live kernel | none | no | yes (through the running JupyterLab) | no | no | no — needs the running JupyterLab | stdio | Apache-2.0 | 2025-10-17 |
| [jbeno/cursor-notebook-mcp](https://github.com/jbeno/cursor-notebook-mcp) | notebook editor (files) | none advertised — stale edit applied, external edit lost (observed) | no | none | no | yes — edits `.ipynb` on disk | no | no (can export to `.py` via nbconvert) | yes — edits files on disk | stdio (SSE/HTTP modes exist) | CC BY-NC-SA 4.0 (non-commercial) | 2025-11-08 |
| [marimo built-in MCP](https://github.com/marimo-team/marimo) | editor-embedded | n/a — read-only surface over MCP | kernel runs in the editor; not callable as an MCP tool | Ruff inside the editor; `lint_notebook` exposes marimo rules, not file lint/format | no | via `marimo convert` CLI, not via MCP | yes — its native format (read-only over MCP) | no | no — needs the running editor/server | streamable HTTP (experimental) | Apache-2.0 | active — 0.25.1 current on PyPI |
| [Anselmoo/mcp-server-analyzer](https://github.com/Anselmoo/mcp-server-analyzer) | code analysis (strings) | never writes files — nothing to guard | never | yes — ruff/ty/vulture bundled as pip deps; Biome tools need external Node.js 22+ | no | no — no notebook awareness | no | no — takes code strings, not files | no — never touches user files | stdio | MIT | 2026-08-30 (checked 2026-10-16) |
| [pydantic/mcp-run-python](https://github.com/pydantic/mcp-run-python) | code execution (WASM) | n/a — execution-only, writes no files | yes — Pyodide (CPython→WASM) inside Deno | no | no | no | no | no | no | stdio (streamable-HTTP modes exist) | MIT | archived — final release 0.0.22, 2025-12-11 |
| [yzfly/mcp-python-interpreter](https://github.com/yzfly/mcp-python-interpreter) | code execution (subprocess) | none advertised — file tools exist, no notebook awareness | yes — host subprocess in the selected environment, no sandbox | no | no | no (generic `read_file`/`write_file`, no notebook structure) | no | runs `.py` files; reads/writes them as generic files | generic file writes only | stdio | **NOASSERTION** (GitHub license field) | 2026-07-02 (checked 2026-10-16) |
| [context7](https://context7.com) | remote docs retrieval (HTTP) | n/a — read-only doc search | no | no | yes — `resolve-library-id` + `query-docs`: doc-snippet search over its cloud index, cross-language; NOT local symbol resolution | no | no | no | no — remote API, needs an API key | streamable HTTP (`https://mcp.context7.com/mcp`; stdio via npx `@upstash/context7-mcp`) | MIT | active — 63,439★, pushed 2026-10-21 |

## Projects we looked at and left out

Surveyed via the GitHub search API on 2026-10-15 (`q=jupyter+mcp`, `q=mcp-jupyter+in:name`, star-sorted). The official [modelcontextprotocol/servers](https://github.com/modelcontextprotocol/servers) repo contains no Python/Jupyter notebook server, so this whole niche is community- or org-maintained.

### Notebook-side exclusions

- [jjsantos01/jupyter-notebook-mcp](https://github.com/jjsantos01/jupyter-notebook-mcp) — 128 stars, MIT, but stale (last push 2025-04-02) and its README says it is "compatible ONLY with Jupyter Notebook version 6.x. It does NOT work with Jupyter Lab, Notebook v7.x, VS Code, Colab".
- [codewithcheese/jupyter-kernel-mcp](https://github.com/codewithcheese/jupyter-kernel-mcp) — 4 stars; kernel execution only, no notebook editing; ~253 downloads/month ([pypistats](https://pypistats.org/api/packages/jupyter-kernel-mcp/recent)).
- [ihrpr/mcp-server-jupyter](https://github.com/ihrpr/mcp-server-jupyter) — 34 stars, MIT, 6 tools, last push 2025-02; superseded in adoption.
- [jupyter-ai-contrib/jupyter-server-mcp](https://github.com/jupyter-ai-contrib/jupyter-server-mcp) — 31 stars, BSD-3, active (pushed 2026-10-12), but a different product category: a Jupyter Server extension that exposes user-registered Python functions as MCP tools, not a notebook editor.
- **executeautomation "mcp-jupyter" does not exist as a repo.** A GitHub search for Jupyter repos under that user returned zero results as of 2026-10-15 ([search API](https://api.github.com/search/repositories?q=jupyter+user:executeautomation)); the account's real MCP repos are mcp-playwright, mcp-database-server, and azure-devops-mcp ([profile](https://github.com/executeautomation)). Blog and video content about an "mcp-jupyter" from them is sometimes mistaken for a maintained server.
- Negligible adoption or pre-alpha, all checked 2026-10-15: [ChengJiale150/jupyter-mcp-server](https://github.com/ChengJiale150/jupyter-mcp-server) (9 stars), [Try3D/JupyterMCP](https://github.com/Try3D/JupyterMCP) (0 stars), [qtalen/jupyter-mcp-kernel](https://github.com/qtalen/jupyter-mcp-kernel) (0 stars, created 2026-06), [bettyguo/mcp-jupyter](https://github.com/bettyguo/mcp-jupyter) (self-declared pre-alpha, not on PyPI), [twn39/jupyter-code-executor-mcp-server](https://github.com/twn39/jupyter-code-executor-mcp-server) (execution-only).
- Rankings on glama.ai and mcp.so could not be verified (JS-rendered pages, no scrapeable ranking), so the selection above rests on GitHub stars and PyPI download stats only.

### Execution-category exclusions

- [e2b-dev/mcp-server](https://github.com/e2b-dev/mcp-server) — 393 stars, Apache-2.0 (as of 2026-10-15): deprecated upstream, and it executes in E2B's paid cloud sandbox (`E2B_API_KEY` + outbound network). A local memory benchmark would measure only the thin shim, not the execution, so we exclude it and say why.
- [elusznik/mcp-server-code-execution-mode](https://github.com/elusznik/mcp-server-code-execution-mode) — 340 stars, GPL-3.0 (as of 2026-10-15): a container-sandbox bridge implementing Anthropic's "code execution with MCP" pattern; needs rootless Podman/Docker-in-Docker and installs from git (no registry package): out of scope for a pinned single-container benchmark.
- [bazinga012/mcp_code_executor](https://github.com/bazinga012/mcp_code_executor) — 212 GitHub stars but ~47 npm downloads/month (as of 2026-10-15); needs Conda.

### Nominees that do not exist (verified 2026-10-15)

Listed so nobody cites them later:

- **Python-MCP/python-mcp** — [github.com/Python-MCP/python-mcp](https://github.com/Python-MCP/python-mcp) is a 404. The PyPI name [`python-mcp`](https://pypi.org/project/python-mcp/) is an unrelated Chinese prompt-management package.
- **deepankarm/mcp-python-repl** — [github.com/deepankarm/mcp-python-repl](https://github.com/deepankarm/mcp-python-repl) is a 404. The PyPI name [`mcp-python-repl`](https://pypi.org/project/mcp-python-repl/) belongs to a 1-star repo by another author ([aazizisoufiane/mcp-python-repl](https://github.com/aazizisoufiane/mcp-python-repl)).
- **An Open Interpreter MCP server** — does not exist. [Open Interpreter](https://github.com/OpenInterpreter/open-interpreter) is an MCP *client*; an MCP server is an open feature request ([issue #1623](https://github.com/OpenInterpreter/open-interpreter/issues/1623)).

### Naming collisions

The huge "notebook MCP" repos target Google NotebookLM, not Python notebooks: [jacob-bd/gemini-notebook-mcp-cli](https://github.com/jacob-bd/gemini-notebook-mcp-cli) (6,318 stars) and [PleasePrompto/notebooklm-mcp](https://github.com/PleasePrompto/notebooklm-mcp) (3,436 stars), both as of 2026-10-15. They regularly top "notebook MCP" searches and are not comparable to anything here.

### The Rust/Go sibling sweep

GitHub API sweeps on 2026-10-15 found no second project in "Rust + byte-exact notebook editing + built-in ruff": tpmcp is effectively alone in that combination. The nearest relatives, for the record: [VeryEvilHumna/mcp-server-jupyter-edit](https://github.com/VeryEvilHumna/mcp-server-jupyter-edit) (Rust, 5 stars, self-described "vibecoded", markdown round-trip rather than byte-exact edits, no conflict guard) and [KidiXDev/ipynb-mcp](https://github.com/KidiXDev/ipynb-mcp) (Go, 2 stars). Lint-side Rust cousins exist but are different tools: [Goldziher/poly](https://github.com/Goldziher/poly) (20 stars, polyglot repo linter) and [mustel-py/mustel](https://github.com/mustel-py/mustel) (19 stars). No 2026 newcomer (created ≥2026, more than 200 stars) exists in any of the five categories.

## Reproduce it

Everything needs only `docker` and `python3` on a Linux host, plus network at build time: pip for the Python stacks, the GHCR pull for analyzer, and the Deno installer plus Pyodide cache warm-up for runpython. Measured runs themselves resolve nothing (see the two disclosed startup exceptions under Fairness disclosures). First, get the tpmcp binary into place (it is gitignored): download the musl release asset (`tpmcp-<version>-x86_64-unknown-linux-musl.tar.gz` from [GitHub Releases](https://github.com/Speretta/true-python-mcp/releases)) and put the binary at `bench/tpmcp/tpmcp`, or build it:

```sh
rustup target add x86_64-unknown-linux-musl
sudo apt-get install musl-tools
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc \
  cargo build --release --target x86_64-unknown-linux-musl
cp target/x86_64-unknown-linux-musl/release/tpmcp bench/tpmcp/tpmcp
```

Then measure:

```sh
bash bench/measure.sh --fast      # smoke run: short quiesce windows, not publishable
bash bench/measure.sh --runs 3    # full run: normal-mode windows, 3 repetitions, medians
```

Useful flags: `--stack tpmcp` measures a single stack (repeatable), and `--no-build` reuses the images from a previous run instead of rebuilding them. Without `--stack`, all eight stacks run in the same order, back to back.

The remote context7 measurement is optional and is skipped automatically when the secret is absent: set `CONTEXT7_TCP_MCP_BENCH_KEY` (a context7 API key) in the environment and `measure.sh` will additionally call `resolve-library-id` + `query-docs` over `https://mcp.context7.com/mcp` and write the latencies to `bench/results/context7/`. In CI that key comes from the `CONTEXT7_TCP_MCP_BENCH_KEY` repository secret (see [.github/workflows/bench.yml](.github/workflows/bench.yml)). The key is never hardcoded or committed.

Each run writes `summary.json`, `meta.json`, `ops.json` (the verbatim MCP transcript), and the two process lists into `bench/results/<stack>/run<N>/`. `bench/render.py` regenerates the published document from this file plus the results: it replaces the placeholder in the Measured results section above with the live tables and the environment block:

```sh
python3 bench/render.py --render bench/results COMPARISON.md
```

In CI: GitHub Actions → **Bench** → *Run workflow* ([.github/workflows/bench.yml](.github/workflows/bench.yml), dispatchable with a runs count and a fast-mode flag). The workflow builds the static musl binary, measures every stack, regenerates `COMPARISON.md`, attaches the raw results as a run artifact and, on `main`, commits the refreshed document if the numbers changed.

Cleanup:

```sh
bash bench/clean.sh               # removes every bench-* image, the pulled GHCR analyzer image, and leftover containers
```

`bench/results/`, `bench/work/`, and the `bench/tpmcp/tpmcp` binary are gitignored; `COMPARISON.md` at the repo root is the published artifact.

## Prior art and thanks

The tree-walking RSS method follows [rust-mcp-servers](https://rmcp.lukaloehr.com/), which measures "RSS summed across the whole process tree, not just the top PID" for Rust rewrites of common MCP servers. The presentation follows the honesty template of the [armavita-meta-ads-mcp BENCHMARKS.md](https://github.com/EfrainTorres/armavita-meta-ads-mcp/blob/rust/docs/BENCHMARKS.md): state the machine, report medians, and say what was not measured. The [TM Dev Lab MCP benchmark](https://github.com/thiagomendes/benchmark-mcp-servers-v2) showed that cross-language MCP comparisons are worth doing carefully. None of these projects are competitors; all three shaped how this benchmark reports.
