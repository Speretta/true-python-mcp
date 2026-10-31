# Changelog

Notable changes, newest first. `scripts/release.sh` adds a section for each
release.

## [0.3.2] - 2026-10-31

**Tool surface.** `cell_set_type`, `cell_set_metadata` and `cell_clear_outputs`
are merged into `cell_update` (type, metadata and cleared outputs in one call);
`notebook_search` is new (content search in sources and stored outputs, plus the
failed-cell lookup); `notebook_read` no longer takes `query` (it moved to
`notebook_search`). Cells are addressed by 0-based index (negative counts from
the end), Jupyter id or marimo cell name, and `cell_insert` / `cell_delete` take
several cells at once. Pre-1.0 surface change.

### Added

- render the workflow probe table
- redesign the tool surface around homogeneous chunks
- render the nb-cli rows and per-call peak RSS
- drive nb-cli through a CLI shell adapter
- make the docs comparison a checkmark matrix
- fall back to raw text when a file is not a notebook
- turn a from_path create into a move with source_revision
- add file_delete

### Fixed

- keep cells when a degraded file re-segments
- measure the post-redesign tool shapes
- show recorded op outcomes in the renderer

### Changed

- stop claiming the nightly bot commit is re-verified by CI
- refresh comparison measurements [skip ci]
- persist new property regression seeds
- drop the em-dashes from the CI comments
- build only the property target in the nightly release stress
- refresh the lock to rmcp 3.8.0
- deduplicate batch resolution
- split the stdlib layout out of the database assembly
- split the logical-line model into its own module
- move the display vocabulary behind the model
- split the conversion, ty and docs suites by theme
- split the wire suite by tool family
- fold edit fixups into the mutations, one hook left
- delegate refusals and format notes to the formats
- move conversion behind the notebook facade
- own the lint policy, leave ruff glue
- own the concatenated-source engine view
- decode nbformat outputs in the format module
- share finding constructors and bounds wording
- scope marimo edit stability to same-view re-parses
- de-duplicate the nb-cli coverage and fix its aged claims
- put nb-cli on the record
- persist new property regression seeds
- refresh comparison measurements [skip ci]
- pin ubuntu-26.04 and move actions/cache to v5
- fix the release race and the Windows cache tax

## [0.3.1] - 2026-10-26

### Added

- render the docs comparison table, de-AI generated prose
- teach the long-cell streaming workflow
- rename stream_copy to file_copy and add file_move
- teach the code-writing contract in instructions and tool nudges
- honor ty.toml [rules] and [tool.ty.rules]
- split the memory profile at the docs lookups
- expire idle databases after ten minutes

### Changed

- bring toml to its current release
- pin the lazy-submodule non-resolution
- pin engine work to sticky per-tree worker threads
- fuzz the Jupyter edit contract like marimo's
- refresh comparison measurements [skip ci]

## [0.3.0] - 2026-10-26

### Added

- give stream_copy an opt-in revision guard
- canonicalize cell returns/signatures after every edit
- support PEP 723 script headers in marimo files
- stream large cell/file content from a path
- flag markdown cells referencing mo with no definition

### Fixed

- say when a CPython source could not be read
- make the marimo filter instance-level where it can be
- refuse to format a marimo notebook instead of mangling it
- stop reporting the header's names as undefined in marimo cells
- rejoin a leading dangling decorator with its cell block
- guard the database drop on every checkout exit path
- keep a regenerated cell block valid Python
- read bare CR as a line terminator like Python does
- treat a failed source read as a cache miss
- guard the db drop on the panic-unwind path
- lint marimo cells individually to drop scaffold false positives

### Changed

- bring every dependency to its current release
- run cargo fmt over the lookup-state test
- Merge origin/main: the nightly's persisted regression seed
- state the candidate dedup as a choice, not a limit
- name the seed-persistence rule explicitly
- state the lifetime bound of the process caches
- own the unshared settings directly instead of through Arc
- state why the candidate dedup stays a scan
- build one line index per file, not per diagnostic
- check builtin names per name instead of building the list
- analyze cell bodies once per check
- persist new property regression seeds
- Merge branch 'feat/marimo-cell-lint'
- Merge branch 'feat/autonomous-return'
- Merge branch 'feat/stream-cell-source'
- Merge branch 'feat/pep723-header'

## [0.2.2] - 2026-10-21

### Added

- report a class's superclasses in py_docs
- enrich docstrings from the real CPython source
- add py_module_members, a read-only ty-backed module member listing
- deepen py_docs resolution, overloads, constructors and ranking
- expose py_docs, a ty-backed read-only symbol docs tool
- add a ty-backed symbol resolver for py_docs

### Fixed

- dedup candidates by definition identity and harden signature string parsing

### Changed

- note missing docstrings instead of a silent None
- document py_docs and py_module_members in the README
- rename fake_venv test helper to scaffold_venv
- split ty, docs and cell into submodules under 500 lines
- cover nested paths, overloads and constructors in py_docs
- cover py_docs resolution over the wire

## [0.2.1] - 2026-10-21

`ty_check` now resolves the project's virtual environment and ruff honors the
global user config: type-checking targets the discovered venv (VIRTUAL_ENV,
CONDA_PREFIX, a `.venv`/`venv` ancestor walk, or an explicit `python`
parameter) and its Python version; ruff falls back to the global user config
when no project config exists.

### Added

- target ty checks at the discovered environment's python version
- honor the ruff global user config with project precedence
- discover venv ancestors and add python param to ty_check

### Changed

- document the ty_check python param and ruff global config
- pin ty venv discovery and python param
- pin notebook_create notebook_format override
- pin cell_set_type raw-to-code conversion
- pin notebook_read with_outputs rendering
- cache the ty discovery result across checks
- persist nightly regression seeds even when the stress fails
- re-level example 07 and note the unexemplified file tools
- add a notebook cleanup and restructure walkthrough
- add a fresh notebook_create walkthrough
- add a ruff-on-a-notebook example walkthrough
- pin cell_move errors and cell_clear_outputs shapes
- pin ty_check on a marimo .py as plain Python
- cover apply_edit's branch cases
- pin ruff fix and format on a notebook over the wire
- pin the py_read/py_write/py_edit file tools over the wire
- pin cross-file type resolution in ty_check
- run cargo audit in CI
- wait for ci.yml before publishing a release

## [0.2.0] - 2026-10-20

The embedded `ty_check` type checker roughly doubles the binary (17.9 MiB →
36.9 MiB, +115%): Astral's ty and its semantic index are compiled in.

### Added

- map .ipynb diagnostics to cells
- add the ty_check tool for .py/.pyi files

### Fixed

- guard the ty database drop against deep-AST recursion
- reject non-python paths in ty_check
- refuse oversize sources in ty_check
- type-check notebooks in memory, not via a temp file
- parse the sampler THREADS column and clarify the cgroup headline
- normalize only the header version in the oracle
- make the pre-push gate fail-closed and match the house idiom
- normalize ground-truth CRLF to LF in run_python

### Changed

- pin ty_check over the wire
- add a ty_check example walkthrough
- add proptest property harness for the ty checker
- pin ty_check against deep-AST recursion
- install the pinned ty binary for the ground-truth oracle
- pin ty_check fixtures against the real ty CLI
- capitalize the notebook tool titles
- name the type-check entry points and tighten visibility
- list ty_check in the tools table
- add the ty type-checker dependency train
- re-verify persisted nightly seeds on the bot commit
- gate releases on the Windows test suite too
- note that an empty marimo file reports has_errors
- point the ruff-train runbook at the hook, not dependabot
- bump the ruff train to 0.17.1 / 0.0.18
- bump rmcp to 3.6.0
- decouple the marimo version stamp from byte-exactness
- gate pushes locally instead of dependabot and CI
- bump tokio to 1.54.0
- compare convert output modulo host line endings
- fold the shared cargo cache into one composite action
- scan dependencies for known advisories with cargo-audit
- seed the property suite per UTC day
- enumerate a deterministic adversarial matrix
- track the latest nextest and float the ground-truth venv

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
- **Jupyter (.ipynb)**:
  - Canonical byte-exact serializer with the nbformat/VS Code layout:
    - 1-space indent;
    - byte-wise sorted keys at every level;
    - raw UTF-8, LF, single trailing newline;
    - inline empty containers;
    - untouched cells keep their original `source` shape, only edited cells normalize.
  - Verified byte-identical against real nbformat 5.11 output on 240+ notebooks from real writers.
  - Schema-subset validation, cross-checked against `nbformat.validate` on every fixture: our Error findings and its verdict always agree. The subset covers:
    - cell types;
    - ids, including duplicates and the nbformat ≥ 4.5 requirement;
    - metadata;
    - outputs per `output_type`, including mime bundles with the `application/*+json` exception;
    - `execution_count`;
    - unknown fields;
    - code-only fields on non-code cells;
    - git conflict markers.
- **marimo (.py)**:
  - Segment-based serializer: unedited cells replay their captured bytes, so parse→serialize is byte-identical to the original. This is strictly stronger than marimo's own save, which renormalizes.
  - AST parse via ruff's parser, with a syntax-error scanner fallback that keeps broken regions byte-preserved, visible, and read-only.
  - `mo.md(constant)` cells surface as markdown cells (extracted, dedented text) and re-wrap byte-identically to marimo's `format_markdown`:
    - prefix tag kept;
    - `"""` quotes;
    - brace doubling for f-strings;
    - `""` escaping.
  - Port of marimo's `ScopedVisitor`, verified against `compile_cell` on a 25-snippet matrix. The port covers:
    - block scopes;
    - closure-capture eviction;
    - cell-local `_` names;
    - `global`;
    - obscured except-handlers;
    - comprehension/walrus binding;
    - SQL-cell detection.
  - Reactive-graph validation, verified against marimo's `check_for_errors`:
    - multiple definitions;
    - cycles via Tarjan SCC;
    - setup-root violations;
    - syntax errors as Errors;
    - stale `return`/signature drift as Info.
  - All ground truth against marimo 0.25.1, including the shipped tutorial corpus.
- **In-process ruff 0.17.0**:
  - Lint (project config respected, safe fixes with `fix: true`) and format for `.py`/`.pyi`/`.ipynb`.
  - Diagnostics mapped to 0-based cell index and cell-relative lines.
  - All ruff linter/formatter imports isolated in `src/ruff.rs`, exact-pinned to one ruff tag (`ruff_linter = 0.17.0`, support crates `= 0.0.17`).
- **`notebook_check`**: unified read-only validation across both formats
  (findings carry severity, cell index, and code; requires no revision and
  returns the current one for chaining into mutations).
- **Quality gates**:
  - Panic-free production code: clippy `unwrap_used`, `expect_used`, and `panic` denied crate-wide. Invariants enforced structurally; for example, the scope analyzer's module block cannot be popped and Tarjan state lives in its work-stack frames.
  - Static dispatch through a closed `AnyNotebook` enum (no `Box<dyn Notebook>`).
  - Every source file under ~500 lines.
  - 88 tests, including live ground-truth cross-checks that announce skips when the venv is absent (`TPMCP_REQUIRE_GROUND_TRUTH=1` enforces).
- **Release tooling**: Conventional-Commits `commit-msg` hook and
  `scripts/release.sh` deriving the semver bump from commits since the last
  tag (patch by default, `Bump: minor` footer for complex features, breaking
  changes gated by confirmation below 1.0), updating Cargo metadata and this
  changelog, committing, and tagging.
- **CI** (GitHub Actions, ubuntu-latest): `fmt --check`,
  `clippy --all-targets -D warnings`, full test suite with a uv-managed
  marimo/nbformat ground-truth venv, and a release build.
