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
- Byte-exact edits: your diff shows only what you changed
- Revision guard: stale writes are refused, not silently applied
- ruff lint and format for any `.py`, not just notebooks
- 12 tools, plus 3 for plain Python files with `--file-tools`

## Memory footprint

| Server | Idle RAM | After the full profile | Processes idle/after | Ready in | Image size |
|---|---|---|---|---|---|
| [tpmcp (this project)](https://github.com/Speretta/true-python-mcp) | 1.7 MiB | 2.4 MiB | 1/1 | 0.1 s | 22 MB |
| [datalayer/jupyter-mcp-server](https://github.com/datalayer/jupyter-mcp-server) | 272.9 MiB | 335.9 MiB | 3/4 | 7.5 s | 548 MB |
| [block/mcp-jupyter](https://github.com/block/mcp-jupyter) | 141.5 MiB | 196.2 MiB | 2/3 | 5.1 s | 491 MB |
| [jbeno/cursor-notebook-mcp](https://github.com/jbeno/cursor-notebook-mcp) | 62.0 MiB | 73.0 MiB | 1/1 | 1.1 s | 237 MB |
| [marimo built-in MCP](https://github.com/marimo-team/marimo) | 90.1 MiB | 89.9 MiB | 2/2 | 2.2 s | 378 MB |
| [Anselmoo/mcp-server-analyzer](https://github.com/Anselmoo/mcp-server-analyzer) | 70.6 MiB | 70.9 MiB | 1/1 | 3.1 s | 246 MB |
| [pydantic/mcp-run-python (retired upstream)](https://github.com/pydantic/mcp-run-python) | 78.5 MiB | 227.8 MiB | 2/2 | 4.4 s | 303 MB |
| [yzfly/mcp-python-interpreter](https://github.com/yzfly/mcp-python-interpreter) | 44.7 MiB | 45.2 MiB | 1/1 | 0.8 s | 205 MB |

Measured 2026-10-18 by the [Bench workflow](.github/workflows/bench.yml) (Linux 6.17.0-1022-azure, docker 28.0.4): median of 3-3 runs per stack (normal mode), each stack in its own minimal Docker container on the same machine. Full method, per-component memory, latency and conflict tables: [COMPARISON.md](COMPARISON.md).

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
`"args": ["--file-tools"]` to also get `py_read`, `py_write`, and `py_edit`.

**3. Try it.** Ask your AI: *"Read the cells of demo.ipynb, then fix the typo
in cell 2."* In a plain Python project: *"Lint main.py with ruff and apply the
safe fixes."*

## Tools

Every tool answers in markdown (default) or JSON (`"format": "json"`).

| Tool | What it does |
| --- | --- |
| `notebook_read` | Shows the cells of a notebook. |
| `notebook_create` | Creates a new notebook (`.ipynb` or marimo `.py`). |
| `cell_edit` | Replaces a piece of text inside one cell. |
| `cell_set_source` | Replaces a whole cell, or a line range in it. |
| `cell_insert` | Adds a new cell. |
| `cell_delete` | Deletes a cell. |
| `cell_move` | Moves a cell to another position. |
| `cell_set_type` | Switches a cell between code and markdown. |
| `cell_clear_outputs` | Empties the outputs of Jupyter cells. |
| `notebook_check` | Finds problems without changing anything: a broken notebook structure, merge conflict marks, or marimo graph errors (two cells defining the same name, a cycle). |
| `ruff_check` | Finds problems in any `.py`, `.pyi`, or `.ipynb` file. With `fix: true` it repairs the safe ones. |
| `ruff_format` | Formats any `.py` or `.ipynb` file. With `check: true` it only shows what would change. |
| `py_read` / `py_write` / `py_edit` | Read, write, and edit plain Python files (only with `--file-tools`). |

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

## Development

```sh
cargo test
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

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

The marimo test files under `tests/data/marimo/` come from the
[marimo project](https://github.com/marimo-team/marimo) (Apache-2.0).
See the `NOTICE` file in that folder.
