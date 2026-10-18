//! Best-effort cleanup of temp files orphaned by hard-killed tpmcp
//! processes.
//!
//! `fs::write` creates `.{pid}.{name}.tpmcp-tmp` next to the target and
//! renames it milliseconds later; only a SIGKILL or power loss leaves one
//! behind. The first write into a directory sweeps it, once per process:
//! entries matching the temp pattern whose mtime is older than both this
//! process's reference time and a 60-second grace period are removed.
//!
//! The double bound keeps a LIVE temp of a concurrent tpmcp (lifetime
//! milliseconds) from ever being deleted in practice. In the pathological
//! case of a writer frozen mid-call for over a minute, deleting its
//! orphan-to-be only turns its doomed rename into a clean io_error; the
//! target file itself is never affected, because rename of a missing source
//! fails atomically. All errors are ignored — sweeping must never fail a
//! write.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

/// Temp-file suffix minted by `fs::temp_path`.
const TMP_SUFFIX: &str = ".tpmcp-tmp";

/// Minimum age before a temp file counts as orphaned.
const GRACE: Duration = Duration::from_secs(60);

/// Remove stale tpmcp temp files from `dir` (once per dir per process).
pub fn sweep_stale_temps(dir: &Path) {
    {
        // Poison recovery: the set only grows by insert, so a poisoned set
        // is still a valid set.
        let mut swept = swept_dirs()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !swept.insert(dir.to_path_buf()) {
            return;
        }
    }
    let reference = *reference_time();
    let Some(cutoff) = SystemTime::now().checked_sub(GRACE) else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !(name.starts_with('.') && name.ends_with(TMP_SUFFIX)) {
            continue;
        }
        // DirEntry::metadata does not follow symlinks; only plain files go.
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.file_type().is_file() {
            continue;
        }
        let Ok(mtime) = meta.modified() else {
            continue;
        };
        if mtime <= reference && mtime <= cutoff {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn swept_dirs() -> &'static Mutex<HashSet<PathBuf>> {
    static SWEPT: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    SWEPT.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Lazily captured "process start" stand-in: the moment of the first sweep,
/// never earlier than process start. Combined with `GRACE` this only ever
/// makes deletion more conservative.
fn reference_time() -> &'static SystemTime {
    static START: OnceLock<SystemTime> = OnceLock::new();
    START.get_or_init(SystemTime::now)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let unique = blake3::hash(tag.as_bytes()).to_hex()[..8].to_string();
        let dir = std::env::temp_dir().join(format!(
            "tpmcp-test-{}-sweep-{tag}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sweep_removes_only_aged_temp_files() {
        let dir = temp_dir("aged");
        let orphan = dir.join(".424242.old.ipynb.tpmcp-tmp");
        let fresh = dir.join(".424243.fresh.ipynb.tpmcp-tmp");
        let unrelated = dir.join("notes.txt");
        fs::write(&orphan, b"stale").unwrap();
        fs::write(&fresh, b"live").unwrap();
        fs::write(&unrelated, b"keep").unwrap();
        // Age the orphan past the grace period without extra deps.
        {
            let file = fs::OpenOptions::new().write(true).open(&orphan).unwrap();
            let times = std::fs::FileTimes::new().set_modified(SystemTime::now() - GRACE * 4);
            file.set_times(times).unwrap();
        }

        sweep_stale_temps(&dir);

        assert!(!orphan.exists(), "aged orphan goes");
        assert!(fresh.exists(), "fresh temp stays (may be live)");
        assert!(unrelated.exists(), "unrelated file stays");

        // Second sweep of the same dir is a no-op by design; a NEW aged
        // orphan therefore survives in this process. Unique dirs per test.
        fs::remove_dir_all(&dir).unwrap();
    }
}
