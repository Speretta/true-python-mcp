//! Sticky worker threads for the ty engine.
//!
//! The database cache ([`super::cache`]) is thread-local because a salsa
//! [`TyDb`] is `Send` but not `Sync` and must have exactly one owner. Running
//! engine calls on the runtime's blocking pool — where every call may land on
//! a different thread — therefore rebuilt the database per call and
//! duplicated the previous one per thread (measured on the bench profile: a
//! "warm" `py_docs` took ~30 ms and grew the process ~10 MiB, where a
//! same-thread hit takes ~2 ms and grows nothing; CI's own table showed the
//! same "warm is not warm" signature).
//!
//! Every `(src_root, python)` key is pinned to one worker thread by hash: its
//! database is built once and reused by every later call, never duplicated.
//! Distinct keys keep their own workers and still run in parallel, so pinning
//! costs no concurrency — same-key calls serialize, which is what a single
//! owned database requires anyway.
//!
//! The workers live for the process. A job that panics (only possible in
//! dependency code) does not kill its worker: the panic is caught, the reply
//! channel drops, and the caller sees an [`Error::Internal`]. Should a thread
//! ever fail to spawn, the job runs on the caller's thread instead — the
//! cache degrades to per-thread reuse, exactly the old behavior, never a
//! wrong answer.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Sender};

use super::cache::CacheKey;
use super::check::check_snapshot_inner;
use crate::docs::{DocModule, DocSymbol, resolve_module_members_inner, resolve_symbol_inner};
use crate::error::Error;
use crate::fs::Snapshot;
use crate::ruff::Diagnostic;

/// How many worker threads the engine may pin keys to. More workers mean more
/// parallelism across trees and more cache worlds; four comfortably covers
/// concurrent work on separate projects.
const WORKERS: usize = 4;

/// One engine job. The variants carry their arguments and a typed reply
/// channel rather than boxed closures: the job set is closed and the project
/// keeps its own code free of `dyn`.
enum Job {
    Check {
        path: PathBuf,
        snapshot: Snapshot,
        python: Option<PathBuf>,
        reply: Sender<Result<Vec<Diagnostic>, Error>>,
    },
    Docs {
        symbol: String,
        src_root: PathBuf,
        python: Option<PathBuf>,
        reply: Sender<Result<DocSymbol, Error>>,
    },
    Members {
        module: String,
        src_root: PathBuf,
        python: Option<PathBuf>,
        reply: Sender<Result<DocModule, Error>>,
    },
    /// Which thread serves a key (the stickiness test's probe).
    #[cfg(test)]
    Probe {
        reply: Sender<std::thread::ThreadId>,
    },
    /// A job that panics on purpose (the survival test's probe).
    #[cfg(test)]
    Panic { reply: Sender<()> },
}

/// Type-checks `path`'s already-read `snapshot` on the worker pinned to its
/// tree — the same work [`super::check_snapshot_with_python`] describes.
pub(crate) fn check(
    path: &Path,
    snapshot: &Snapshot,
    python: Option<&Path>,
) -> Result<Vec<Diagnostic>, Error> {
    let (reply, answer) = mpsc::channel();
    dispatch(
        &CacheKey::for_file(path, python),
        Job::Check {
            path: path.to_path_buf(),
            snapshot: snapshot.clone(),
            python: python.map(Path::to_path_buf),
            reply,
        },
    );
    match answer.recv() {
        Ok(result) => result,
        Err(_) => Err(Error::internal("the ty engine worker stopped mid-job")),
    }
}

/// Resolves `symbol` on the worker pinned to `(src_root, python)`.
pub(crate) fn docs(
    symbol: &str,
    src_root: Option<&Path>,
    python: Option<&Path>,
) -> Result<DocSymbol, Error> {
    let (reply, answer) = mpsc::channel();
    dispatch(
        &CacheKey::for_root(src_root, python),
        Job::Docs {
            symbol: symbol.to_string(),
            src_root: src_root.unwrap_or_else(|| Path::new(".")).to_path_buf(),
            python: python.map(Path::to_path_buf),
            reply,
        },
    );
    match answer.recv() {
        Ok(result) => result,
        Err(_) => Err(Error::internal("the ty engine worker stopped mid-job")),
    }
}

/// Lists `module`'s members on the worker pinned to `(src_root, python)`.
pub(crate) fn members(
    module: &str,
    src_root: Option<&Path>,
    python: Option<&Path>,
) -> Result<DocModule, Error> {
    let (reply, answer) = mpsc::channel();
    dispatch(
        &CacheKey::for_root(src_root, python),
        Job::Members {
            module: module.to_string(),
            src_root: src_root.unwrap_or_else(|| Path::new(".")).to_path_buf(),
            python: python.map(Path::to_path_buf),
            reply,
        },
    );
    match answer.recv() {
        Ok(result) => result,
        Err(_) => Err(Error::internal("the ty engine worker stopped mid-job")),
    }
}

/// Test-only: the thread pinned to `key`, to assert stickiness.
#[cfg(test)]
pub(crate) fn probe(key: &CacheKey) -> Option<std::thread::ThreadId> {
    let (reply, answer) = mpsc::channel();
    dispatch(key, Job::Probe { reply });
    answer.recv().ok()
}

/// Sends `job` to the worker pinned for `key`, or — when no worker thread can
/// be reached — runs it on this thread with identical semantics.
fn dispatch(key: &CacheKey, job: Job) {
    let job = match sender_for(key) {
        Some(sender) => match sender.send(job) {
            Ok(()) => return,
            // The worker died holding the job: send comes back with it.
            Err(mpsc::SendError(job)) => job,
        },
        None => job,
    };
    run_job(job);
}

/// The worker for `key`, spawned on first use. `None` when the OS refuses the
/// thread, in which case [`dispatch`] runs jobs inline.
fn sender_for(key: &CacheKey) -> Option<&'static Sender<Job>> {
    static SENDERS: [OnceLock<Option<Sender<Job>>>; WORKERS] = [const { OnceLock::new() }; WORKERS];
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    let slot = (hasher.finish() as usize) % WORKERS;
    SENDERS[slot].get_or_init(|| spawn_worker(slot)).as_ref()
}

/// Spawns one worker thread, or `None` when the OS refuses. The thread owns
/// its receiver for the life of the process.
fn spawn_worker(slot: usize) -> Option<Sender<Job>> {
    let (sender, receiver) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name(format!("ty-engine-{slot}"))
        .spawn(move || worker_loop(receiver));
    match spawned {
        Ok(_) => Some(sender),
        Err(_) => None,
    }
}

fn worker_loop(receiver: mpsc::Receiver<Job>) {
    while let Ok(job) = receiver.recv() {
        // A panic inside a job (only possible in dependency code) must not
        // kill the worker: catch it, drop the job's reply so the caller sees
        // an error, and keep the thread — and its cache — alive.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_job(job)));
    }
}

fn run_job(job: Job) {
    match job {
        Job::Check {
            path,
            snapshot,
            python,
            reply,
        } => {
            let _ = reply.send(check_snapshot_inner(&path, &snapshot, python.as_deref()));
        }
        Job::Docs {
            symbol,
            src_root,
            python,
            reply,
        } => {
            let _ = reply.send(resolve_symbol_inner(&symbol, &src_root, python.as_deref()));
        }
        Job::Members {
            module,
            src_root,
            python,
            reply,
        } => {
            let _ = reply.send(resolve_module_members_inner(
                &module,
                &src_root,
                python.as_deref(),
            ));
        }
        #[cfg(test)]
        Job::Probe { reply } => {
            let _ = reply.send(std::thread::current().id());
        }
        #[cfg(test)]
        Job::Panic { reply } => tests::panic_on_purpose(reply),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom

    use super::*;

    /// The body of [`Job::Panic`], kept in the test module so the panic idiom
    /// lives where the project allows it. The reply sender is alive when the
    /// panic fires: unwinding drops it, which is exactly what the caller's
    /// `recv` observes as an error.
    pub(super) fn panic_on_purpose(reply: Sender<()>) {
        let _kept_alive = reply;
        panic!("a test job panics on purpose");
    }

    /// A key's calls always land on one thread: its database is built once and
    /// reused, never duplicated across blocking-pool threads (the "warm is not
    /// warm" defect these workers exist to remove).
    #[test]
    fn one_key_is_served_by_one_thread() {
        let key = CacheKey::new(Path::new("/tmp/tpmcp-worker-stickiness"), None);
        let first = probe(&key).expect("the worker answers");
        let second = probe(&key).expect("the worker answers");
        assert_eq!(first, second, "the same key must pin to one worker");
    }

    /// A job that panics leaves the worker serving: the caller sees a dropped
    /// reply (an error at every entry point), and the next call on the same
    /// key still lands on the same, still-alive thread.
    #[test]
    fn a_panicking_job_does_not_kill_the_worker() {
        let key = CacheKey::new(Path::new("/tmp/tpmcp-worker-panic"), None);
        let before = probe(&key).expect("the worker answers");
        let (reply, answer) = mpsc::channel();
        dispatch(&key, Job::Panic { reply });
        assert!(
            answer.recv().is_err(),
            "a panicking job must drop its reply"
        );
        let after = probe(&key).expect("the worker must still answer");
        assert_eq!(before, after, "the worker must survive its jobs");
    }
}
