# Changelog

Notable changes, newest first. `scripts/release.sh` adds a section for each
release.

## [0.1.4] - 2026-10-19

### Added

- add cell_set_metadata for Jupyter cell metadata
- convert notebooks with notebook_create from_path
- add outline, query and diagnostics to notebook_read

### Fixed

- keep scanner-fallback cells identical to the structured parse
- reject explicit line ranges on empty files
- scaffold mo when a mutation introduces marimo markdown
- parse JSON floats exactly so writes stop drifting them
- align the cell execution_count check with the nbformat policy
- keep cell separators when deleting broken marimo cells
- build marimo files byte-identical to marimo's own serializer
- cap the JSON outline first line like the markdown one
- run the two-process rename-gate test in debug builds only
- render the search, outline and export views honestly
- report unavailable lint instead of failing notebook_read
- report everything a conversion leaves behind
- define mo in created and converted marimo files with markdown cells

### Changed

- stress the property suite nightly at 65536 cases
- split the windows uv step and pin the ground-truth python
- sync the README with the shipped surface
- rebase before the bench bot pushes
- add the ruff train runbook and the message-evidence policy
- mark the library API unstable in the quickstart
- prepare the crate for crates.io
- split the ruff diagnostics mapping into a submodule
- add issue templates
- add dependabot for cargo and actions
- add a security policy
- test on windows, check the msrv, pin and cache
- gate the release binaries on the test suite
- pin byte-exact fixtures against EOL rewriting
- add proptest property harness for the parse/serialize core
- complete the tool annotation matrix with titles and four hints
- add examples 06 and 07 with their demo fixtures
- pin the notebook_read parameter edges on the wire
- sweep the conversion corpus and run the artifacts under real marimo
- complete the annotations and titles of the two notebook tools
- render the notebook Format enum compactly in schemas
- describe tool side effects and teach the new parameters
- simplify the prose across the markdown surface
- capture per-process thread counts in the sampler
- switch the release profile to fat LTO and one codegen unit
- note the mixed-EOL behavior of inserted marimo lines
- adopt the house style rules
- name diagnostic bindings as domain nouns
- split the marimo suite into core, markdown and graph files
- typed notebook errors, borrowed sources, lazy extras
- unify the wire harness and temp dirs in tests/common
- keep test lock files out of the real cache dir
- trim tokio features and drop the unused ruff_source_file dep
- switch to a current-thread runtime
- run every tool body on a blocking thread
- refresh comparison measurements [skip ci]

## [0.1.3] - 2026-10-18

### Fixed

- drop the early read-based existence check in notebook_create
- guard the ruff walk and format against deep-AST recursion
- re-check the target before rename and sweep stale temp files
- preserve per-line endings outside edited line ranges
- refuse to clobber unreadable files in the create paths
- write through symlinks and preserve the file mode
- hold a cross-process lock across the verified write path
- guard the marimo scope walk and AST drops against stack overflow
- preserve column-zero string lines when regenerating marimo cells
- accurate version-field errors, nbformat-aligned output checks
- fetch the arm64 musl toolchain from a pinned GitHub mirror

### Changed

- split fixture provenance in the marimo NOTICE
- state the byte-exact and overwrite guarantees precisely
- pin the float-spelling boundary of the canonical writer
- spell out single-letter bindings in the wire suite
- move the requested source instead of cloning it
- count cell source lines in a single byte scan
- split output rendering into a private submodule
- build rendered views without per-line temporaries
- name the window and output-summary concepts
- pin write-path robustness against clobbers and races
- build py_read markdown without per-line temporaries
- single-read ruff path and drop redundant deep clones
- resolve ruff settings once per config state
- apply rustfmt to the marimo module and its tests
- rename the scope analyzer's refs to ref_sites
- stop copying unedited marimo cell bytes twice per serialize
- name the markdown dedent margin helpers
- unify the marimo parse state in a Builder
- table-drive the marimo hide_code header surgery
- split the marimo check into a graph build and named finding passes
- apply rustfmt to the touched jupyter sources
- split the per-cell check body into named helpers
- pin the canonical-form boundary with CRLF and exotic-number fixtures
- clear all outputs in a single pass
- emit canonical ipynb bytes straight from the model
- consume the parsed jupyter document instead of cloning it
- point contributors at cargo nextest
- run the test suite with pinned cargo-nextest
- keep local analysis reports out of version control
- refresh comparison measurements [skip ci]
- add dispatchable bench workflow
- add 8-stack memory, latency and conflict benchmark

## [0.1.2] - 2026-10-15

### Changed

- add automatic binary release workflow

## [0.1.1] - 2026-10-15

### Added

- report revisions from read-only ruff calls

### Fixed

- honor the ground-truth python override in scope tests

### Changed

- replace regex parsing in git hook and release script with bash builtins
- add captured example walkthroughs
- present plain-Python support in README and package description
- restructure README with quickstart and demo, add CONTRIBUTING
- replace node-based setup actions with shell steps

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
