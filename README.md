# tpmcp

[![CI](https://github.com/Speretta/true-python-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/Speretta/true-python-mcp/actions/workflows/ci.yml)

tpmcp is a small server that lets your AI coding assistant work with Python
notebooks. It is one Rust binary. It needs nothing else — no Python, no ruff,
no plugins.

It does two jobs:

1. **Edit notebooks without breaking them.** Jupyter (`.ipynb`) and marimo
   (`.py`) files keep their exact layout. If you change one line, the git diff
   shows one line.
2. **Check Python code.** Lint and format with ruff. ruff is built into the
   binary, so there is nothing to install.

tpmcp never runs your code. It only reads, edits, and checks.

## Install

Build from source with Rust 1.97 or newer; the finished binary itself needs
nothing:

```sh
git clone https://github.com/Speretta/true-python-mcp.git
cd true-python-mcp
cargo build --release
```

The binary is `target/release/tpmcp`. You can also run `cargo install --path .`
to copy it into `~/.cargo/bin`.

## Connect it to your AI tool

tpmcp talks MCP over stdio. Add this to the MCP config of your tool
(opencode, Claude Desktop, Claude Code, Cursor, VS Code):

```json
{
  "mcpServers": {
    "tpmcp": { "command": "/path/to/tpmcp" }
  }
}
```

By default tpmcp only offers notebook and ruff tools, because most AI tools
already have their own file tools. If your tool has no file tools (for example
plain Claude Desktop), add `"args": ["--file-tools"]` to also get `py_read`,
`py_write`, and `py_edit`.

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
| `ruff_check` | Finds problems in Python code. With `fix: true` it repairs the safe ones. |
| `ruff_format` | Formats Python code. With `check: true` it only shows what would change. |
| `py_read` / `py_write` / `py_edit` | Read, write, and edit plain Python files (only with `--file-tools`). |

Notebook outputs, execution counts, and metadata stay exactly as they are.
Only the cell you edit changes.

## Safe editing with revisions

A file can change while the AI is working on it. tpmcp stops that from
causing damage:

- Every read returns a short code called the **revision**.
- Every change must send that code back.
- If the file changed in between, the tool refuses the change and sends the
  new code. Then you read again and retry.

So nobody's work is overwritten by accident.

## What tpmcp does not do

- It does not run Python code. A kernel module may come later, as a separate
  part.
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

Make a release with `scripts/release.sh` (`--dry-run` shows what would
happen). The script picks the new version from the commits since the last
tag: small changes bump the patch number (0.1.0 → 0.1.1), a commit with a
`Bump: minor` footer bumps the minor number (0.1.1 → 0.2.0), and breaking
changes ask before becoming a major version.

Some tests compare against the real marimo and nbformat Python packages.
They need a small virtualenv:

```sh
uv venv /tmp/opencode/nbenv
uv pip install --python /tmp/opencode/nbenv/bin/python nbformat marimo
```

If it is missing, those tests say so and skip. CI runs them all. Use another
path with `TPMCP_GROUND_TRUTH_PYTHON`.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

The marimo test files under `tests/data/marimo/` come from the
[marimo project](https://github.com/marimo-team/marimo) (Apache-2.0).
See the `NOTICE` file in that folder.
