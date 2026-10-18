//! Per-path locking: an in-process mutex plus a cross-process OS advisory
//! lock, both held across the whole verified-write flow.
//!
//! [`with_file_lock`] serializes every mutation of one file:
//!
//! 1. The in-process mutex (fast path) serializes pipelined tool calls of
//!    this server instance — rmcp spawns each request as an independent
//!    task, so two `cell_edit`s on one file can genuinely overlap.
//! 2. An OS advisory lock (`flock`/`LockFileEx` via the `fd-lock` crate, no
//!    unsafe here) serializes separate tpmcp processes — two AI clients
//!    each spawn their own server, and without this both could pass
//!    `read_verified` at revision A and the second rename would silently
//!    destroy the first writer's update.
//!
//! ## Why the OS lock is NOT on the notebook file itself
//!
//! Our atomic write renames a temp file over the target, and rename swaps
//! the inode behind the path. A `flock` on the notebook's own inode does not
//! exclude a competitor that opens the path *after* the rename: P1 locks
//! inode i1 and writes; P2 opens the path, now inode i2, locks i2 without
//! blocking, verifies and renames — both processes are past-verify at once
//! and one update is lost. The lock object must therefore have a *stable
//! name that is never renamed*. Two candidates:
//!
//! - A sidecar next to the notebook (`nb.ipynb.lock`): pollutes the user's
//!   project directory and git status, and cannot be created at all when
//!   the notebook's directory is read-only — both realistic for an editing
//!   tool.
//! - A per-canonical-path lock file under the user cache dir (**chosen**):
//!   `$XDG_CACHE_HOME|~/.cache` (Unix), `~/Library/Caches` (macOS),
//!   `%LOCALAPPDATA%` (Windows), `tpmcp/locks/<blake3(key)>.lock`. The
//!   project tree stays clean, locking works for read-only directories, and
//!   keying by canonical path makes every spelling of the same file share
//!   one lock — the same key the in-process mutex uses.
//!
//! Lock files are empty and are never deleted: unlinking a lock file
//! reintroduces an open-vs-unlink race between processes (P2 could lock a
//! fresh inode while P1 holds the unlinked old one). They cost one inode
//! per edited file in the cache dir.
//!
//! If the lock file cannot be opened or locked (no `$HOME`/`LOCALAPPDATA`,
//! exotic sandbox), we warn and continue with in-process locking only —
//! degrading to the previous behavior rather than failing every write.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use crate::error::Error;

/// Run `f` while holding both locks for `path` (see the module docs),
/// serializing tpmcp's mutations of the same file across concurrent tool
/// calls *and* concurrent tpmcp processes.
pub fn with_file_lock<T>(path: &Path, f: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    let key = lock_key(path);
    let mutex = mutex_for(&key);
    // A poisoned mutex only means a previous holder panicked; the atomic
    // write design keeps the file itself consistent, so keep going.
    let _in_process = mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut os_lock = open_lock_file(&key).map(fd_lock::RwLock::new);
    let _cross_process = os_lock.as_mut().and_then(write_lock);
    f()
}

/// Blocking write lock on the sidecar lock file. On failure we warn and
/// fall back to in-process locking rather than failing the user's write.
/// A blocking `flock` can in principle be interrupted (EINTR), but tpmcp
/// installs no signal handlers on its worker threads — a terminal Ctrl-C
/// kills the process by default disposition before the syscall returns —
/// so the reachable failures are environmental (missing cache dir,
/// sandboxing) and degrading with a loud warning is the useful behavior.
fn write_lock(lock: &mut fd_lock::RwLock<File>) -> Option<fd_lock::RwLockWriteGuard<'_, File>> {
    match lock.write() {
        Ok(guard) => Some(guard),
        Err(source) => {
            tracing::warn!(
                "cannot take the cross-process file lock, continuing with \
                 in-process locking only: {source}"
            );
            None
        }
    }
}

/// Identity used to key every lock (in-process mutex, OS lock file) of
/// `path`.
///
/// Keyed by canonical path so `./a.ipynb` and `/abs/a.ipynb` share one
/// lock. For a file that does not exist yet, canonicalizing the whole path
/// fails, so canonicalize the *parent* instead (this resolves symlinked
/// directories) and re-attach the file name — two spellings of the same
/// future file still share one lock. The `absolute` fallback only applies
/// when even the parent is missing; create paths then fabricate the
/// directory, and rename atomicity still guarantees a whole file.
pub fn lock_key(path: &Path) -> PathBuf {
    if let Ok(key) = std::fs::canonicalize(path) {
        return key;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
        && let Ok(parent) = std::fs::canonicalize(parent)
    {
        return parent.join(name);
    }
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

fn open_lock_file(key: &Path) -> Option<File> {
    let dir = lock_dir()?;
    // Hash the key so any path (length, separators, non-UTF-8) maps to a
    // safe fixed-size file name; blake3 makes cross-path collisions
    // irrelevant in practice.
    let digest = blake3::hash(key.as_os_str().as_encoded_bytes()).to_hex();
    let path = dir.join(format!("{}.lock", &digest[..32]));
    match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        // Never truncate: the file's content is irrelevant (it is only a
        // stable lock target), and concurrent processes may hold it open.
        .truncate(false)
        .open(&path)
    {
        Ok(file) => Some(file),
        Err(source) => {
            tracing::warn!("cannot open cross-process lock file {}: {source}", path.display());
            None
        }
    }
}

fn lock_dir() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = cache_dir()?.join("tpmcp").join("locks");
        match std::fs::create_dir_all(&dir) {
            Ok(()) => Some(dir),
            Err(source) => {
                tracing::warn!("cannot create lock dir {}: {source}", dir.display());
                None
            }
        }
    })
    .clone()
}

/// Per-user cache directory. Deliberately no temp-dir fallback: a
/// world-writable shared directory would let another local user pre-create
/// the lock file and lock tpmcp out (denial of service). A missing
/// `$HOME`/`%LOCALAPPDATA%` degrades to in-process-only locking instead.
#[cfg(all(unix, not(target_os = "macos")))]
fn cache_dir() -> Option<PathBuf> {
    env_dir("XDG_CACHE_HOME").or_else(|| env_dir("HOME").map(|home| home.join(".cache")))
}

#[cfg(target_os = "macos")]
fn cache_dir() -> Option<PathBuf> {
    env_dir("HOME").map(|home| home.join("Library").join("Caches"))
}

#[cfg(windows)]
fn cache_dir() -> Option<PathBuf> {
    env_dir("LOCALAPPDATA").or_else(|| {
        env_dir("USERPROFILE").map(|home| home.join("AppData").join("Local"))
    })
}

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|value| value.is_absolute())
}

fn lock_table() -> &'static RwLock<HashMap<PathBuf, Arc<Mutex<()>>>> {
    static TABLE: OnceLock<RwLock<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    TABLE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn mutex_for(key: &Path) -> Arc<Mutex<()>> {
    // Poison recovery (same policy as with_file_lock): only map
    // get/entry/or_default and Arc clones run under the table lock, none of
    // which can leave the table inconsistent, so a poisoned table is still a
    // valid table and recovery is safe.
    if let Some(mutex) = lock_table()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(key)
    {
        return mutex.clone();
    }
    let mut table = lock_table()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    table.entry(key.to_path_buf()).or_default().clone()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let unique = blake3::hash(tag.as_bytes()).to_hex()[..8].to_string();
        let dir = std::env::temp_dir()
            .join(format!("tpmcp-test-{}-lock-{tag}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lock_key_unifies_spellings_of_one_file() {
        let dir = temp_dir("key");
        let file = dir.join("a.ipynb");
        std::fs::write(&file, b"x").unwrap();
        let canonical = std::fs::canonicalize(&dir).unwrap();
        // Existing file: canonical path, same as the parent-canonical form.
        assert_eq!(lock_key(&file), canonical.join("a.ipynb"));
        // Missing file: parent-canonical + file name, so two spellings of
        // the same future file share one lock (create races).
        let missing = canonical.join("future.ipynb");
        assert_eq!(lock_key(&missing), missing);
        let odd_spelling = canonical.join(".").join("future.ipynb");
        assert_eq!(lock_key(&odd_spelling), missing);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn file_lock_serializes_mutations() {
        let dir = temp_dir("serial");
        let path = dir.join("counter.txt");
        std::fs::write(&path, b"0").unwrap();

        let counter = Arc::new(Mutex::new(0));
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let (path, counter) = (&path, &counter);
                scope.spawn(move || {
                    for _ in 0..25 {
                        with_file_lock(path, || {
                            let value: i32 = std::fs::read_to_string(path)
                                .unwrap()
                                .trim()
                                .parse()
                                .unwrap();
                            *counter.lock().unwrap() += 1;
                            std::fs::write(path, (value + 1).to_string()).unwrap();
                            Ok(())
                        })
                        .unwrap();
                    }
                });
            }
        });

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "100");
        assert_eq!(*counter.lock().unwrap(), 100);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn os_lock_file_excludes_second_attempt_while_held() {
        // Two independent lock acquisitions on the same key (like two
        // processes) must exclude: while the first write guard lives, a
        // try_write on a second handle of the same lock file must fail.
        let dir = temp_dir("oslock");
        let key = dir.join("nb.ipynb");
        std::fs::write(&key, b"x").unwrap();
        let canonical = lock_key(&key);
        let first = open_lock_file(&canonical).expect("lock file opens");
        let second = open_lock_file(&canonical).expect("lock file reopens");
        let mut first = fd_lock::RwLock::new(first);
        let mut second = fd_lock::RwLock::new(second);
        let _guard = first.write().unwrap();
        assert!(second.try_write().is_err(), "flock must exclude");
        drop(_guard);
        assert!(second.try_write().is_ok(), "released lock is takeable");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
