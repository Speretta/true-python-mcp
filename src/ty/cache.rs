//! Thread-local, `Mutex`-free, `HashMap`-free cache for the salsa [`TyDb`].
//!
//! [`TyDb`] is `Send` but not `Sync`: salsa's `Storage` owns a per-thread
//! `ZalsaLocal`, and — decisively — cloning a handle arms salsa's
//! `cancel_others()` deadlock. `Storage::cancel_others` (reached from every
//! input-field write, e.g. `File::sync_path` / `File::sync_virtual_path`) sets
//! a cancellation flag and blocks until every *other* live handle has dropped
//! (the clone count falls back to 1). A second live handle — a clone of the
//! database — therefore wedges the worker thread forever on the next sync. The
//! database must have exactly one owner and must never be cloned or shared;
//! this cache is that owner.
//!
//! The cache is a `thread_local! { RefCell<Vec<DbSlot>> }`: a bounded vector
//! (≤ [`SLOTS_PER_THREAD`]) scanned linearly, so lookups need no `HashMap` and
//! cannot contend or poison across workers. A slot keeps its database behind
//! `Option` so a hit *moves* it out for the caller's `work` and back in
//! afterwards — moved, never cloned, so the clone count stays 1.
//!
//! Deferred drop guard: a deeply-nested parsed AST's recursive `Drop` glue
//! overflows the small worker stack (see [`crate::stack`]), but with a cache
//! the database is no longer dropped at the end of a check — it drops at
//! eviction or thread teardown. Each slot remembers the largest source length
//! it has served and re-applies [`crate::stack::guarded`] around that drop.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use crate::error::Error;

use super::db::TyDb;

/// Databases kept alive per worker before evicting the least-recently-used.
/// Four covers "check + docs + members" against one tree.
const SLOTS_PER_THREAD: usize = 4;

/// Identifies one database: the first-party search root plus the optional
/// explicit `python` override that pinned environment discovery. The in-memory
/// virtual source (a notebook's concatenated cells, a docs synthetic module) is
/// deliberately *not* part of the key — it is swapped in via
/// [`super::db::TyDb::serve_virtual`] before every query — so one database per
/// (root, python) serves every query against that tree.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CacheKey {
    pub(crate) src_root: PathBuf,
    pub(crate) python: Option<PathBuf>,
}

impl CacheKey {
    pub(crate) fn new(src_root: &Path, python: Option<&Path>) -> Self {
        Self {
            src_root: src_root.to_path_buf(),
            python: python.map(Path::to_path_buf),
        }
    }
}

/// One cached database. `db` is `None` only while a `with_db` call has it
/// checked out — or after a panicking checkout dropped it, which the next
/// call rebuilds — and that keeps the single-owner invariant structural.
struct DbSlot {
    key: CacheKey,
    db: Option<TyDb>,
    last_used: u64,
    /// Largest source length the slot has ever served; sizes the deferred drop
    /// guard (see [`crate::stack::guarded`]).
    max_source_len: usize,
}

/// A database checked out of its slot. The type exists for its `Drop`: whether
/// the checkout ends in [`Self::return_to_slot`] or in a panic inside `work`,
/// the database is dropped inside a [`crate::stack::guarded`] closure.
/// `guarded` grows the stack only for the duration of its closure, so a plain
/// local's drop glue — which runs in the unwinding frame, back on the small
/// worker stack — is exactly the overflow the guard exists to prevent (a deep
/// parsed AST drops recursively). The database must therefore be *moved into*
/// the guarded closure, like `evict_lru` and `Drop for ThreadCache` do.
struct CheckedOut {
    /// `None` only after [`Self::return_to_slot`] or while `Drop` runs.
    db: Option<TyDb>,
    /// Largest source this database has served, sizing the deferred drop.
    max_source_len: usize,
}

impl CheckedOut {
    fn new(db: TyDb, max_source_len: usize) -> Self {
        Self {
            db: Some(db),
            max_source_len,
        }
    }

    /// Hands the database back to its slot and ends the guard's coverage.
    fn return_to_slot(mut self) -> Option<TyDb> {
        self.db.take()
    }
}

impl Drop for CheckedOut {
    fn drop(&mut self) {
        if let Some(db) = self.db.take() {
            crate::stack::guarded(self.max_source_len, move || drop(db));
        }
    }
}

#[derive(Default)]
struct ThreadCache {
    slots: Vec<DbSlot>,
    clock: u64,
}

impl ThreadCache {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// Evicts the least-recently-used slot, dropping its database under the
    /// same stack guard its sources required (see the module docs).
    fn evict_lru(&mut self) {
        let Some((index, _)) = self
            .slots
            .iter()
            .enumerate()
            .min_by_key(|(_, slot)| slot.last_used)
        else {
            return;
        };
        let evicted = self.slots.swap_remove(index);
        if let Some(db) = evicted.db {
            crate::stack::guarded(evicted.max_source_len, move || drop(db));
        }
    }
}

impl Drop for ThreadCache {
    fn drop(&mut self) {
        for slot in &mut self.slots {
            if let Some(db) = slot.db.take() {
                crate::stack::guarded(slot.max_source_len, move || drop(db));
            }
        }
    }
}

thread_local! {
    static CACHE: RefCell<ThreadCache> = RefCell::new(ThreadCache::default());
}

/// Runs `work` against a database cached for `key`, building it with `build` on
/// a miss. `source_len` is the byte length of the source about to be checked
/// (or resolved); it sizes the deferred drop guard.
///
/// `work` receives `&mut TyDb` so it can [`File::sync_path`] /
/// [`File::sync_virtual_path`] before querying (those take `&mut dyn Db`);
/// queries then borrow the same value immutably. The database is moved out of
/// and back into its slot — never cloned — so salsa's clone count stays 1 and
/// `cancel_others` never wedges.
///
/// [`File::sync_path`]: ruff_db::files::File::sync_path
/// [`File::sync_virtual_path`]: ruff_db::files::File::sync_virtual_path
pub(crate) fn with_db<R>(
    key: CacheKey,
    source_len: usize,
    build: impl FnOnce() -> Result<TyDb, Error>,
    work: impl FnOnce(&mut TyDb) -> Result<R, Error>,
) -> Result<R, Error> {
    CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        let now = cache.tick();

        let index = match cache.slots.iter().position(|slot| slot.key == key) {
            Some(index) => {
                cache.slots[index].last_used = now;
                index
            }
            None => {
                if cache.slots.len() >= SLOTS_PER_THREAD {
                    cache.evict_lru();
                }
                cache.slots.push(DbSlot {
                    key,
                    db: None,
                    last_used: now,
                    max_source_len: 0,
                });
                cache.slots.len() - 1
            }
        };

        // Move the database out (a miss builds a fresh one); on a build error
        // the slot stays empty and the error propagates.
        let db = match cache.slots[index].db.take() {
            Some(db) => db,
            None => build()?,
        };

        let max_source_len = cache.slots[index].max_source_len.max(source_len);
        cache.slots[index].max_source_len = max_source_len;

        // The guard drops the database under the deferred-drop guard on every
        // path, panic or not (see `CheckedOut`): wrapping `work` in
        // `stack::guarded` alone would NOT help, because the database's drop
        // glue does not run inside that closure.
        let mut checked_out = CheckedOut::new(db, max_source_len);
        let result = match checked_out.db.as_mut() {
            Some(db) => work(db),
            // Unreachable: `new` holds a database and only this match and
            // `Drop` take it out. An honest error, never a panic.
            None => Err(Error::internal("checked-out database is missing")),
        };
        cache.slots[index].db = checked_out.return_to_slot();
        result
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom

    use std::panic::AssertUnwindSafe;
    use std::path::Path;

    use super::*;

    /// A panicking `work` must leave no damage behind: the checked-out
    /// database drops under the deferred-drop guard (its `Drop` moves the
    /// database into `stack::guarded`'s closure — the only place the grown
    /// stack covers a drop), the slot stays empty, and the next call rebuilds
    /// and serves normally. This is precisely the exit path that wrapping
    /// `work` in `stack::guarded` does not cover.
    #[test]
    fn a_panicking_checkout_drops_guarded_and_the_cache_recovers() {
        let root = Path::new(".");
        let panicked = std::panic::catch_unwind(AssertUnwindSafe(|| {
            with_db(
                CacheKey::new(root, None),
                8,
                || TyDb::for_src_root(root, None),
                |_| -> Result<(), Error> { panic!("work panics on purpose") },
            )
        }));
        assert!(panicked.is_err(), "the panic must propagate");
        let served = with_db(
            CacheKey::new(root, None),
            8,
            || TyDb::for_src_root(root, None),
            |_| Ok(()),
        );
        assert!(served.is_ok(), "the cache must serve the key after a panic");
    }
}
