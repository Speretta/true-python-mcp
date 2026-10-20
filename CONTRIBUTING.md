# Contributing

## Prerequisites

- Rust 1.97 or newer (`rustup` recommended)
- The [cargo-nextest](https://nexte.st) test runner (latest; plain
  `cargo test` also works):

  ```sh
  curl -LsSf https://get.nexte.st/latest/linux | tar zxf - -C "$HOME/.cargo/bin"
  ```

- Optional: a Python venv with `nbformat` and `marimo` for the live
  ground-truth tests (see below)

## Everyday commands

```sh
cargo nextest run             # all tests (or: cargo test)
cargo clippy --all-targets    # must stay warning-free
cargo fmt                     # code style
```

## Git hooks

`git config core.hooksPath .githooks` enables them once per clone:

- `commit-msg` rejects non-[Conventional Commits] commit subjects.
- `pre-push` blocks the push when a direct dependency is outdated
  (`cargo outdated`, the manual ruff/ty train excepted) or a new
  `cargo-audit` advisory appears; bypass deliberately with
  `git push --no-verify`. Both checkers must be installed
  (`cargo install cargo-outdated cargo-audit`).

Two rules are enforced by tooling:

- Production code must not panic. Clippy denies `unwrap`, `expect`, and
  `panic` outside tests; handle errors with the `Error` type in
  `src/error.rs`.
- Keep source files under ~500 lines. Split large modules the way
  `marimo/check.rs` + `marimo/check_tests.rs` do.

## House style

Naming and structure conventions the codebase follows (from the 2026-10
style audit). A change that violates one should say why.

1. **Bindings are domain nouns.** `notebook`, `cell`, `source`,
   `revision`, `findings`, `diagnostics`, `snapshot`; never
   `a`/`s`/`v`/`res`/`out`/`data`/`tmp`, and closure parameters are no
   exception (`|diagnostic|`, not `|d|`). Single letters only for char
   values and domain-less loop counters. (String-builder locals named
   `out` predate this rule and stay until touched.)
2. **Booleans read as predicates.** `seen_import`,
   `inserting_before_existing`, `is_past_the_end`: a name that could
   follow `if` without mental translation.
3. **Chains cap at ~3-4 adapters; closures stay expressions.** When a
   closure body needs a match, a loop, or a second statement, extract a
   named fn and pass it by reference (`.map(output_summary)`): the chain
   states the shape, the function states the rule.
4. **Loops and iterators compile to the same code; choose by clarity.**
   One pass doing two jobs is a `for` loop; "select the code cells'
   indexes" is `.filter().map().collect()`. Never hand-roll what one
   adapter names outright (`split_inclusive`, not a byte-index loop).
5. **Guard early, then stay flat.** let-else for extract-or-bail,
   let-chains for compound conditions, early returns instead of else
   pyramids; a body nested more than two blocks deep inside its outermost
   loop wants a helper.
6. **Every phase of an algorithm is a named function, even with one
   caller.** The top of a long operation should read like a table of
   contents (`check` → `build`/`multiple_definitions`/`cycles`/
   `setup_root`/`drift`). Comments that say "what this block does" are
   unnamed functions. Promote them.
7. **Errors are classified once, at the definition.** `Error` (thiserror)
   is the crate's language; `anyhow` lives only in `main.rs`. A layer
   that can classify, must: repeated `.map_err(...)` at call sites means
   the callee returns the wrong type. User-visible strings live in
   exactly one place.
8. **Repeated shapes become types.** A heterogeneous tuple is a struct
   (`LintPass`); parallel locals threaded through functions are a builder
   (`marimo/parse.rs`); a literal repeated at many sites is a constructor
   (`Cell::from_text`).
9. **No boolean-trap arguments across module boundaries.**
   `ruff::check(path, SafeFixes::Apply)` says what a `fix: bool` cannot.
   A fieldless enum costs the same as a bool. Intra-module booleans with
   named bindings at a single call site (`with_id`) are tolerable.
10. **Knowledge lives behind its owning module's facade.** `tools/`
    speaks only `notebook::{Notebook, AnyNotebook, Cell}`;
    format-specific keys of `Cell.extra` are read through typed accessors
    owned by the model (`Cell::marimo_kind`), never re-spelled in
    renderers. Readability rewrites must be codegen-neutral or cheaper.

## Commit messages

Messages follow [Conventional Commits](https://www.conventionalcommits.org/):

```
type(scope): lowercase subject

types: feat fix refactor perf docs test build ci chore
```

Write self-contained messages: put the numbers, commands and results a
change rests on into the message itself. Local analysis files (`reports/`,
`PROGRESS.local.md`) are gitignored, so a message that only points at them
cannot be checked from a fresh clone.

Enable the check once per clone:

```sh
git config core.hooksPath .githooks
```

## Releases

```sh
scripts/release.sh --dry-run   # show what would happen
scripts/release.sh             # bump, changelog, commit, tag
```

The new version comes from the commits since the last tag:

| Commits since the last tag | Bump | Example |
| --- | --- | --- |
| cleanup, bug fixes, simple features | patch | 0.1.0 → 0.1.1 |
| a commit with a `Bump: minor` footer | minor | 0.1.1 → 0.2.0 |
| breaking change (`feat!:`, `BREAKING CHANGE:` footer, `Bump: major`) | major, asks first while below 1.0 | 0.2.0 → 1.0.0 |

The script updates `Cargo.toml` and `Cargo.lock`, adds a `CHANGELOG.md`
section, commits as `chore(release): vX.Y.Z`, and creates the tag. Push both
when done: `git push && git push --tags`.

## Upgrading the ruff train

All ten ruff crates are exact-pinned to one ruff release and move together
in a single commit: `ruff_linter = 0.X.Y` plus nine support crates at
`= 0.0.Z` (ruff ships this train weekly). The upgrade is manual and
deliberate; the pre-push `cargo outdated` gate ignores `ruff_*`/`ty_*` for
exactly this reason.

1. Bump every `ruff_*` pin in `Cargo.toml` in one commit, then run
   `cargo check` to re-lock and surface API breaks.
2. Breaks can only appear where ruff crates are imported. The linter,
   formatter, config and diagnostics-mapping imports live in `src/ruff.rs`
   and its `settings`/`diagnostics` submodules; the marimo parser and scope
   analysis (`src/notebook/marimo/`) additionally use `ruff_python_ast`,
   `ruff_python_parser`, `ruff_python_stdlib` and `ruff_text_size`. Any
   other file seeing a ruff type is a layering bug: fix the layering, not
   the pin.
3. Run the full suite with the ground-truth venv
   (`TPMCP_REQUIRE_GROUND_TRUTH=1`); `tests/analysis.rs` was captured
   against the real CLI, so install the matching one
   (`uv tool install ruff==0.X.Y`) whenever you need to compare behavior
   by hand.
4. Record the release binary size delta in the commit message; the fat-LTO
   x86_64 baseline is 17,711,816 bytes (measured 2026-10-18).

## Ground-truth tests

Some tests compare our results with the real `marimo` and `nbformat` Python
packages. They need a venv:

```sh
uv venv /tmp/opencode/nbenv
uv pip install --python /tmp/opencode/nbenv/bin/python nbformat marimo
```

- Missing venv: those tests print a note and skip.
- `TPMCP_REQUIRE_GROUND_TRUTH=1`: a missing venv fails the tests instead.
  CI sets this and installs its own venv, so the live checks always run there.
- `TPMCP_GROUND_TRUTH_PYTHON=/path/to/python`: use a different interpreter
  (CI uses `/tmp/nbenv/bin/python`).
- `TPMCP_LOCK_DIR=/absolute/path`: keep the cross-process lock files here
  instead of the user cache dir. CI sets this so runs are hermetic; setting
  it locally stops repeated suites from accumulating lock files in
  `~/.cache/tpmcp/locks`.

## Test data

- `tests/data/*.ipynb` — hand-made Jupyter fixtures.
- `tests/data/marimo/tutorials/` — files copied verbatim from the marimo
  project (Apache-2.0, see the `NOTICE` there). Do not edit them; add new
  files instead.
- `tests/data/marimo/*.py` — hand-written fixtures in the shape of marimo
  files (see the `NOTICE` for the provenance split).
