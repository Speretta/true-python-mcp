# tpmcp

[![CI](https://github.com/Speretta/true-python-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/Speretta/true-python-mcp/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Release](https://img.shields.io/github/v/tag/Speretta/true-python-mcp)](https://github.com/Speretta/true-python-mcp/tags)

*An MCP server for Python: safe notebook editing and built-in ruff
lint/format, for notebooks and plain `.py` files alike. One Rust binary,
nothing else to install.*

It does two jobs:

1. **Edit notebooks without breaking them.** Jupyter (`.ipynb`) and marimo
   (`.py`) files keep their exact layout. Change one line and the git diff
   shows one line. Notebooks are hard for AI tools: a `.ipynb` file is one
   big JSON document, and many notebook MCP servers run a hidden Jupyter
   kernel that can fight with the editor you have open, like VS Code. tpmcp
   starts no kernel and runs no code. It treats the notebook as a plain
   file and changes only the bytes you asked it to change.
2. **Lint and format any Python file.** ruff is compiled into the binary, so
   `ruff_check` and `ruff_format` work on every `.py`, `.pyi`, and `.ipynb`
   in your project, not just notebooks. Nothing to install, nothing to
   configure.

## At a glance

- One binary; no runtime dependencies, no kernel, no background process
- Jupyter `.ipynb` and marimo `.py`, detected automatically
- Marimo files tpmcp creates load on marimo 0.2.0 and newer (measured through 0.25.2)
- Byte-exact edits: your diff shows only what you changed
- Revision guard: stale writes are refused, not silently applied
- ruff lint and format for any `.py`, not just notebooks
- 16 tools, plus 3 for plain Python files with `--file-tools`

## Memory footprint

| Server | Idle RAM | After the full profile | Processes idle/after | Ready in | Image size |
|---|---|---|---|---|---|
| [tpmcp (this project)](https://github.com/Speretta/true-python-mcp) | 1.6 MiB | 15.0 MiB | 1/1 | 0.1 s | 38 MB |
| [datalayer/jupyter-mcp-server](https://github.com/datalayer/jupyter-mcp-server) | 270.3 MiB | 334.4 MiB | 3/4 | 5.6 s | 549 MB |
| [block/mcp-jupyter](https://github.com/block/mcp-jupyter) | 141.6 MiB | 198.1 MiB | 2/3 | 3.5 s | 492 MB |
| [jbeno/cursor-notebook-mcp](https://github.com/jbeno/cursor-notebook-mcp) | 62.1 MiB | 73.0 MiB | 1/1 | 0.8 s | 239 MB |
| [marimo built-in MCP](https://github.com/marimo-team/marimo) | 90.2 MiB | 90.7 MiB | 2/2 | 2.2 s | 379 MB |
| [Anselmoo/mcp-server-analyzer](https://github.com/Anselmoo/mcp-server-analyzer) | 70.8 MiB | 70.5 MiB | 1/1 | 2.5 s | 246 MB |
| [pydantic/mcp-run-python (retired upstream)](https://github.com/pydantic/mcp-run-python) | 78.7 MiB | 238.6 MiB | 2/2 | 3.7 s | 296 MB |
| [yzfly/mcp-python-interpreter](https://github.com/yzfly/mcp-python-interpreter) | 44.8 MiB | 44.8 MiB | 1/1 | 0.6 s | 206 MB |

Measured 2026-10-26 by the [Bench workflow](.github/workflows/bench.yml) (Linux 6.17.0-1022-azure, docker 28.0.4): median of 3-3 runs per stack (normal mode), each stack in its own minimal Docker container on the same machine. Full method, per-component memory, latency and conflict tables: [COMPARISON.md](COMPARISON.md).

_Idle RAM_ and _After the full profile_ are container cgroup totals, not process RSS — the cgroup figure excludes file-backed pages, so `ps` reports higher. tpmcp's profile additionally runs its docs lookups (`py_docs`, `py_module_members`), which the other stacks record as skips; the next refresh splits the memory column at those steps so the shared profile stays directly comparable. Both columns are listed in [COMPARISON.md](COMPARISON.md).

## See it work

Your AI fixes a typo in one cell. The git diff shows one line:

```diff
-    print("hello wrold")
+    print("hello world")
```

Nothing else changes. Outputs, cell order, and metadata stay exactly as they
are. Without tpmcp, the same edit often rewrites the whole file.

Full walkthroughs live in [examples/](examples/): every tool call and every
answer, captured from the real server.

## Quickstart

**1. Build it.** You need Rust 1.97 or newer; the finished binary needs
nothing:

```sh
git clone https://github.com/Speretta/true-python-mcp.git
cd true-python-mcp
cargo build --release
```

(`cargo install --path .` also works: it copies the binary into
`~/.cargo/bin`.)

The binary is the product. The `tpmcp` library API behind it is internal
and may change in any release.

**2. Connect your AI tool.** Add this to its MCP config (for example
`opencode.json`, `claude_desktop_config.json`, or `.cursor/mcp.json`):

```json
{
  "mcpServers": {
    "tpmcp": { "command": "/path/to/target/release/tpmcp" }
  }
}
```

If your tool has no file tools of its own (like plain Claude Desktop), add
`"args": ["--file-tools"]` to also get `py_read`, `py_write`, `py_edit`, and
`stream_copy`.

**3. Try it.** Ask your AI: *"Read the cells of demo.ipynb, then fix the typo
in cell 2."* In a plain Python project: *"Lint main.py with ruff and apply the
safe fixes."*

## Tools

Every tool answers in markdown (default) or JSON (`"format": "json"`).

| Tool | What it does |
| --- | --- |
| `notebook_read` | Shows the cells of a notebook. Can list them as an outline, search their text, and attach validation and lint findings. |
| `notebook_create` | Creates a new notebook (`.ipynb` or marimo `.py`), or converts an existing one (`from_path`; `.md` exports to markdown). |
| `cell_edit` | Replaces a piece of text inside one cell. |
| `cell_set_source` | Replaces a whole cell, or a line range in it. |
| `cell_insert` | Adds a new cell. |
| `cell_delete` | Deletes a cell. |
| `cell_move` | Moves a cell to another position. |
| `cell_set_type` | Switches a cell between code and markdown. |
| `cell_set_metadata` | Replaces one Jupyter cell's metadata (papermill tags, slideshow settings); the previous value comes back in the answer. |
| `cell_clear_outputs` | Empties the outputs of Jupyter cells. |
| `notebook_check` | Finds problems without changing anything: a broken notebook structure, merge conflict marks, or marimo graph errors (two cells defining the same name, a cycle). |
| `ruff_check` | Finds problems in any `.py`, `.pyi`, or `.ipynb` file (uses the nearest project ruff/pyproject config, falling back to the global user config). With `fix: true` it repairs the safe ones. |
| `ruff_format` | Formats any `.py` or `.ipynb` file. With `check: true` it only shows what would change. |
| `ty_check` | Type-checks any `.py`, `.pyi`, or `.ipynb` against annotations and imports (Astral's ty, compiled in; no interpreter, no subprocess). Pass `python` to check against a specific venv; otherwise auto-discovers it. Notebook diagnostics carry a 0-based `cell` index. Per-rule severities come from the project's `ty.toml` `[rules]` or `pyproject.toml` `[tool.ty.rules]` (`ignore`/`warn`/`error`); the answer names the config that applied and anything in it tpmcp did not honor. |
| `py_docs` | Resolve a library symbol (function/method/class/attribute) to its signature, return type, parameters and docstring; fully local, no interpreter, no external API; pass `python` to target a specific venv. |
| `py_module_members` | List a module's public members with one-line signatures and docstrings. |
| `py_read` / `py_write` / `py_edit` | Read, write, and edit plain Python files (only with `--file-tools`). `py_write` can take `from_path` instead of `content`. |
| `stream_copy` | Copy a file byte-for-byte through tpmcp by path, without round-tripping the content through the agent's context (only with `--file-tools`). Takes an optional `revision` to refuse clobbering anything but that exact `dest`; without one it overwrites unconditionally, like `cp`. |

Notebook outputs, execution counts, and metadata keep their values, and
only the cell you edit changes. One boundary to know: a file in Jupyter's
standard layout round-trips byte-for-byte. A file in an unusual layout
(Windows line endings, two-space indent, exotic number spellings)
normalizes once on the first tpmcp write; later writes are byte-identical.

## Safe editing with revisions

A file can change while the AI is working on it. tpmcp keeps that safe:

- Every read returns a short code called the **revision**.
- Every change must send that code back.
- If the file changed in between, the tool refuses the change and sends the
  new code. Then you read again and retry.

One opt-in exception: `stream_copy` is a copy primitive, so without a
`revision` it overwrites `dest` unconditionally (like `cp`) and is marked
destructive. With one, it follows the same refusal rule as every other
write.

Two tpmcp processes never overwrite each other: they take a cross-process
lock, and every write re-checks the file a moment before it lands. An
editor outside tpmcp takes no lock. A save from it that lands during the
final rename can still slip through; every longer race is detected and
refused.

## What tpmcp does not do

- It does not run Python code. It never starts a kernel or a background
  server, so there is nothing to fight with your editor.
- marimo `setup`, `function`, and `class_definition` blocks are read-only.
  You can see them, but not edit them.
- SQL cells in marimo are not checked for graph errors yet.
- It honors only the rules section of ty's configuration (`ty.toml`
  `[rules]`, `pyproject.toml` `[tool.ty.rules]`). Other sections
  (`environment`, `src`, ...) are named and ignored: the environment comes
  from `python` and the checked file's tree instead. The config is also
  discovered from the checked file's directory, where the `ty` CLI looks
  from its working directory.

## Development

```sh
cargo nextest run
cargo clippy --all-targets
cargo fmt
```

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/).
Turn the message check on once per clone:

```sh
git config core.hooksPath .githooks
```

Release process and ground-truth test setup: [CONTRIBUTING.md](CONTRIBUTING.md).
What is inside each release: [CHANGELOG.md](CHANGELOG.md).

## Continuous integration

Every push and pull request runs `.github/workflows/ci.yml` with three jobs:

- `check` runs fmt, clippy (`-D warnings`) and the full test suite against a
  live ground-truth venv (latest nbformat and marimo), then a release build.
- `windows` runs the same suite on Windows; two release targets ship there.
- `msrv` runs `cargo check` with the declared minimum Rust (1.97).

Three more workflows:

- `.github/workflows/release.yml` fires on a version tag; the test suite must
  pass before any binary is built or published.
- `.github/workflows/nightly.yml` stresses the property suite nightly (65536
  cases, debug and release) and commits any new counterexample seed.
- `.github/workflows/bench.yml` is dispatched manually to refresh the
  comparison measurements in this README.

The property tests are three-layered. A deterministic matrix
(`tests/adversarial.rs`) enumerates the known degenerate shapes. CI replays
the property tests with a date-derived seed. The nightly run discovers new
shapes at random; every find is persisted as a regression seed and replayed
deterministically everywhere.

The shared cargo cache lives in `.github/actions/`. A pre-push git hook
(`git config core.hooksPath .githooks`) blocks the push when a direct
dependency is outdated (`cargo outdated`, the manual ruff/ty train excepted)
or a new advisory appears (`cargo audit`, reviewed exceptions in
`.cargo/audit.toml`); bypass deliberately with `git push --no-verify`.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

The marimo test files under `tests/data/marimo/` come from the
[marimo project](https://github.com/marimo-team/marimo) (Apache-2.0).
See the `NOTICE` file in that folder.
