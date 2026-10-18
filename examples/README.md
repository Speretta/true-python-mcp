# Examples

Real walkthroughs of tpmcp at work. Every request and every answer in these
docs is quoted verbatim from the released binary: nothing is invented,
trimmed, or cleaned up. Each doc shows one task, from the user's question to
the final git diff.

| Example | Level | What it shows |
| --- | --- | --- |
| [01-fix-a-typo.md](01-fix-a-typo.md) | simple | Read a Jupyter notebook, fix one typo with `cell_edit`. The diff is one line; the outputs stay untouched. |
| [02-lint-and-format.md](02-lint-and-format.md) | simple | ruff on a plain `.py` file: findings, safe fixes, format. |
| [03-marimo-markdown-cell.md](03-marimo-markdown-cell.md) | basic | A marimo `mo.md` cell shown as plain markdown, then edited with a byte-exact re-wrap. |
| [04-revision-conflict.md](04-revision-conflict.md) | basic | The file is edited elsewhere mid-session: tpmcp refuses the stale write, the AI re-reads and retries. |
| [05-find-and-fix-a-cycle.md](05-find-and-fix-a-cycle.md) | complex | `notebook_check` finds a dependency cycle in a marimo app; the AI fixes one cell and verifies the result. |
| [06-convert-a-notebook-to-marimo.md](06-convert-a-notebook-to-marimo.md) | complex | Convert a Jupyter notebook with a magic, a raw cell and plot outputs into a marimo app: every warning reported, broken cells repaired, plus a `.md` export. |
| [07-find-and-verify-in-a-big-notebook.md](07-find-and-verify-in-a-big-notebook.md) | basic | Outline and query navigation in a 40-cell notebook, a stale-revision conflict mid query→edit chain, and verification with diagnostics riding on a read. |

## The input files

The notebooks used in these walkthroughs live in [demo/](demo/). Running the
same flows edits them. To get the originals back:

```sh
git checkout examples/demo
```

## Replay them yourself

Build the binary (see the [Quickstart](../README.md#quickstart) in the main
README) and add it to any MCP host. Then send the same tool calls: each doc
shows them exactly as they go over the wire (JSON). The standard MCP
handshake (`initialize`) is the same for every server, so the docs omit it.
