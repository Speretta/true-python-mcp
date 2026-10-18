# Contributing

## Prerequisites

- Rust 1.97 or newer (`rustup` recommended)
- The [cargo-nextest](https://nexte.st) test runner (pinned; plain
  `cargo test` also works):

  ```sh
  curl -LsSf https://get.nexte.st/0.9.148/linux | tar zxf - -C "$HOME/.cargo/bin"
  ```

- Optional: a Python venv with `nbformat` and `marimo` for the live
  ground-truth tests (see below)

## Everyday commands

```sh
cargo nextest run             # all tests (or: cargo test)
cargo clippy --all-targets    # must stay warning-free
cargo fmt                     # code style
```

Two house rules:

- Production code must not panic. Clippy denies `unwrap`, `expect`, and
  `panic` outside tests; handle errors with the `Error` type in
  `src/error.rs`.
- Keep source files under ~500 lines. Split large modules the way
  `marimo/check.rs` + `marimo/check_tests.rs` do.

## Commit messages

Messages follow [Conventional Commits](https://www.conventionalcommits.org/):

```
type(scope): lowercase subject

types: feat fix refactor perf docs test build ci chore
```

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
