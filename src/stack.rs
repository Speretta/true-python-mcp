//! Shared stack-growth guard for the third-party recursion we cannot
//! restructure: ruff guards its own parser with `stacker`, but the linter
//! walk, the formatter, and the recursive `Drop` glue of every parsed AST
//! run unguarded on the small worker stacks (tokio's default is 2 MiB). A
//! deeply nested yet valid Python file (a 100k-term `1+1+…` chain) parses
//! fine and then aborts the whole process (SIGSEGV, not a catchable panic)
//! during the walk, the format, or the drop.
//!
//! Sizing: every AST node consumes at least one source byte (a unary chain
//! is the densest nesting), so `source_len * frame_bound` bounds the worst
//! case; two measured bounds exist: a smaller one for recursive `Drop` glue
//! and a larger one for third-party walk/format recursion (debug-build
//! frames). The virtual reservation is capped; Linux overcommits the
//! mapping, so only pages the recursion actually touches cost memory.
//! Sources near the 32 MiB read cap can still exceed the cap in debug
//! builds, a residual, documented limit (release frames are ~2-4×
//! smaller).

/// Upper bound for one AST node's recursive `Drop` glue frame, generous for
/// debug builds (measured: marimo's 4M-term debug drop fit the 256 B/node
/// sizing).
const DROP_FRAME_BOUND: usize = 256;

/// Upper bound for one node of a third-party WALK or FORMAT recursion in a
/// debug build (ruff's linter visitor and formatter carry much fatter frames
/// than drop glue: 256 B and 2048 B per node were both measured to overflow
/// at 100k terms; 8192 B survived with margin). Production builds are
/// release; the bound is sized for the debug test environment, which is
/// the worst case. If a future ruff trains fatten the visitor frames
/// further, this bound is the knob to revisit (the deep-nesting test pins
/// it).
const WALK_FRAME_BOUND: usize = 8192;

/// Cap on the virtual reservation for one guarded segment. Linux overcommits
/// the mapping; only pages the recursion actually touches cost memory.
const STACK_CAP: usize = 4 * 1024 * 1024 * 1024;

fn grow_to<T>(frame_bound: usize, source_len: usize, work: impl FnOnce() -> T) -> T {
    let worst_case = frame_bound.saturating_mul(source_len).min(STACK_CAP);
    stacker::maybe_grow(worst_case, worst_case.max(1024 * 1024), work)
}

/// Run `work` (a recursive AST `Drop`) with a stack that provably contains
/// the worst case for `source_len` bytes of input: shallow inputs run inline
/// for free; when the remaining stack cannot contain the worst case, `work`
/// runs on a stacker segment sized from the source length.
pub fn guarded<T>(source_len: usize, work: impl FnOnce() -> T) -> T {
    grow_to(DROP_FRAME_BOUND, source_len, work)
}

/// [`guarded`], sized for third-party walk/format recursion (ruff's linter
/// visitor, formatter) whose per-node frames are fatter than drop glue.
pub fn guarded_walk<T>(source_len: usize, work: impl FnOnce() -> T) -> T {
    grow_to(WALK_FRAME_BOUND, source_len, work)
}
