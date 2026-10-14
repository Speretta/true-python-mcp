# Changelog

Notable changes, newest first. `scripts/release.sh` adds a section for each
release.

## [0.1.0] - 2026-10-15

First public release.

### Added

- **MCP server** over stdio (rmcp), single statically linked binary (~23 MB),
  no runtime dependencies. 12 tools by default; `--file-tools` adds
  `py_read`/`py_write`/`py_edit`. Every tool renders markdown (default) or
  JSON; domain errors surface as `isError` results with `structured_content`.
- **Revision-safe mutations**: blake3 content hash (first 16 hex) returned by
  every read and mutation, required by every write; a conflict returns
  `{error: "conflict", current_revision}` for read-and-retry. Per-path locks,
  atomic writes.
- **Jupyter (.ipynb)**: canonical byte-exact serializer (nbformat/VS Code
  layout: 1-space indent, byte-wise sorted keys at every level, raw UTF-8,
  LF, single trailing newline, inline empty containers; untouched cells keep
  their original `source` shape, only edited cells normalize). Verified
  byte-identical against real nbformat 5.11 output on 240+ notebooks from
  real writers. Schema-subset validation (cell types, ids including
  duplicates and the nbformat ≥ 4.5 requirement, metadata, outputs per
  `output_type` including mime bundles with the `application/*+json`
  exception, `execution_count`, unknown fields, code-only fields on non-code
  cells, git conflict markers) cross-checked against `nbformat.validate` on
  every fixture: our Error findings and its verdict always agree.
- **marimo (.py)**: segment-based serializer — unedited cells replay their
  captured bytes, so parse→serialize is byte-identical to the original
  (strictly stronger than marimo's own save, which renormalizes). AST parse
  via ruff's parser, with a syntax-error scanner fallback that keeps broken
  regions byte-preserved, visible, and read-only. `mo.md(constant)` cells
  surface as markdown cells (extracted, dedented text) and re-wrap
  byte-identically to marimo's `format_markdown` (prefix tag kept, `"""`
  quotes, brace doubling for f-strings, `""` escaping). Port of marimo's
  `ScopedVisitor` (block scopes, closure-capture eviction, cell-local `_`
  names, `global`, obscured except-handlers, comprehension/walrus binding,
  SQL-cell detection) verified against `compile_cell` on a 25-snippet matrix;
  reactive-graph validation (multiple definitions, cycles via Tarjan SCC,
  setup-root violations, syntax errors as Errors; stale `return`/signature
  drift as Info) verified against marimo's `check_for_errors`. All ground
  truth against marimo 0.25.1, including the shipped tutorial corpus.
- **In-process ruff 0.17.0**: lint (project config respected, safe fixes with
  `fix: true`) and format for `.py`/`.pyi`/`.ipynb`; diagnostics mapped to
  0-based cell index and cell-relative lines. All ruff linter/formatter
  imports isolated in `src/ruff.rs`, exact-pinned to one ruff tag
  (`ruff_linter = 0.17.0`, support crates `= 0.0.17`).
- **`notebook_check`**: unified read-only validation across both formats
  (findings carry severity, cell index, and code; requires no revision and
  returns the current one for chaining into mutations).
- **Quality gates**: panic-free production code (clippy `unwrap_used`,
  `expect_used`, `panic` denied crate-wide; invariants enforced structurally,
  e.g. the scope analyzer's module block cannot be popped and Tarjan state
  lives in its work-stack frames), static dispatch through a closed
  `AnyNotebook` enum (no `Box<dyn Notebook>`), every source file under
  ~500 lines, 88 tests including live ground-truth cross-checks that announce
  skips when the venv is absent (`TPMCP_REQUIRE_GROUND_TRUTH=1` enforces).
- **Release tooling**: Conventional-Commits `commit-msg` hook and
  `scripts/release.sh` deriving the semver bump from commits since the last
  tag (patch by default, `Bump: minor` footer for complex features, breaking
  changes gated by confirmation below 1.0), updating Cargo metadata and this
  changelog, committing, and tagging.
- **CI** (GitHub Actions, ubuntu-latest): `fmt --check`,
  `clippy --all-targets -D warnings`, full test suite with a uv-managed
  marimo/nbformat ground-truth venv, and a release build.
