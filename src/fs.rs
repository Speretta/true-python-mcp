//! Filesystem primitives: content-addressed revisions, atomic writes and
//! per-path locking.
//!
//! Every mutating tool follows the same protocol:
//!
//! 1. Take the per-path lock ([`with_file_lock`]) so concurrent tpmcp calls
//!    to the same file are serialized — in-process (pipelined tool calls)
//!    and cross-process (separate tpmcp servers, e.g. two AI clients).
//! 2. [`read_verified`] the file against the caller-supplied revision.
//! 3. Mutate the in-memory snapshot.
//! 4. [`write`] it back atomically (temp file + rename) and return the new
//!    revision.
//!
//! Readers never need the lock: because writes land via `rename`, a reader
//! always observes either the old or the new file, never a partial one.

pub mod lock;
mod sweep;

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::error::Error;

pub use lock::with_file_lock;

/// Files larger than this are rejected to keep reads and diffs token-safe.
const MAX_FILE_SIZE: u64 = 32 * 1024 * 1024;

/// A file's UTF-8 content together with its revision hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub content: String,
    pub revision: String,
}

/// Compute the revision of a byte slice: first 16 hex chars of BLAKE3.
pub fn revision_of(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex()[..16].to_string()
}

/// Read a text file into a [`Snapshot`].
pub fn read(path: &Path) -> Result<Snapshot, Error> {
    let bytes = read_bytes(path)?;
    let content = String::from_utf8(bytes).map_err(|_| Error::NotUtf8 {
        path: path.to_path_buf(),
    })?;
    let revision = revision_of(content.as_bytes());
    Ok(Snapshot { content, revision })
}

/// Read a text file and verify it matches the caller-supplied `revision`.
///
/// On mismatch returns [`Error::Conflict`] carrying the current revision so
/// the caller can re-read and retry.
///
/// On success the file's stat fingerprint is recorded for the pre-rename
/// recheck in [`write`]. The fingerprint is taken BEFORE the bytes are
/// read: an external save landing during the read shows up as a revision
/// mismatch here, and one landing after the read shows up at write time.
pub fn read_verified(path: &Path, revision: &str) -> Result<Snapshot, Error> {
    let stat = stat_fingerprint(path);
    let snapshot = read(path)?;
    if snapshot.revision != revision {
        return Err(Error::Conflict {
            path: path.to_path_buf(),
            expected: revision.to_string(),
            current: snapshot.revision,
        });
    }
    lock::record_verified(
        &lock::lock_key(path),
        lock::VerifiedStamp {
            stat,
            revision: snapshot.revision.clone(),
        },
    );
    Ok(snapshot)
}

/// Atomically write `content` to `path` and return the revision of the
/// written content. The parent directory must exist; create paths use
/// [`create`] instead, so a typo'd path never fabricates directories.
///
/// The bytes are written to a sibling temp file, fsynced, then renamed over
/// the target, so observers never see a partially written file.
///
/// Writes follow symlinks, matching reads: `path` is resolved first and the
/// rename lands next to the RESOLVED target. Renaming over the symlink
/// itself would replace the link with a regular file while every read keeps
/// following the old target — after the first edit the user's real file
/// (dotfiles/sync setups) would silently stop receiving updates. Hardlinks
/// cannot be preserved by any rename-based atomic write (the edit lands on a
/// fresh inode); this matches how Jupyter/nbformat itself saves.
///
/// The target's permission bits are copied onto the temp file before the
/// rename (best-effort), so an edit preserves e.g. 0600. Ownership is NOT
/// preserved: the rename installs the temp file, owned by the writing user —
/// if the target had another owner (group-writable directory), the owner
/// changes. Inherent to atomic-rename writes without privileges.
pub fn write(path: &Path, content: &str) -> Result<String, Error> {
    let target = resolve_target(path);
    if let Some(parent) = target.parent() {
        sweep::sweep_stale_temps(parent);
    }
    let tmp = temp_path(&target);
    match write_via_temp(&tmp, &target, path, content) {
        Ok(()) => Ok(revision_of(content.as_bytes())),
        Err(error) => {
            let _ = fs::remove_file(&tmp);
            Err(error)
        }
    }
}

/// The write itself: temp file, fsync, external-editor recheck, rename.
/// `reported` is the caller's spelling of the path, used in errors.
fn write_via_temp(tmp: &Path, target: &Path, reported: &Path, content: &str) -> Result<(), Error> {
    let io = |source| Error::io(reported.to_path_buf(), source);
    let mut file = fs::File::create(tmp).map_err(io)?;
    preserve_mode(target, tmp);
    file.write_all(content.as_bytes()).map_err(io)?;
    file.sync_all().map_err(io)?;
    recheck_unchanged(target, reported)?;
    fs::rename(tmp, target).map_err(io)?;
    Ok(())
}

/// Final external-editor check immediately before the rename.
///
/// If this flow verified a revision earlier ([`read_verified`] recorded the
/// fingerprint) and the target's `(mtime, size)` changed since, an external
/// program — one that takes neither our lock nor part in the revision
/// protocol — saved mid-call. The stat is only a cheap filter: when it
/// differs we re-read and decide by content, so a bare `touch` or a
/// byte-identical save is not a conflict; anything else aborts with the
/// same [`Error::Conflict`] the revision guard produces.
///
/// This SHRINKS the in-call TOCTOU window from "parse + mutate + serialize"
/// (milliseconds) to "re-stat → rename" (microseconds); it does not close
/// it. A save landing between the re-stat and the rename is still lost, and
/// an identical-size save inside the filesystem's timestamp granularity is
/// undetectable. Create flows verified nothing and skip the check.
fn recheck_unchanged(target: &Path, reported: &Path) -> Result<(), Error> {
    let Some(stamp) = lock::take_stamp(&lock::lock_key(reported)) else {
        return Ok(()); // create path / unverified flow: nothing to recheck
    };
    let Some(verified) = stamp.stat else {
        return Ok(()); // fingerprint unavailable at verify time
    };
    if stat_fingerprint(target) == Some(verified) {
        return Ok(());
    }
    let current = read(target)?;
    if current.revision == stamp.revision {
        return Ok(()); // timestamp-only change: content is what we verified
    }
    Err(Error::Conflict {
        path: reported.to_path_buf(),
        expected: stamp.revision,
        current: current.revision,
    })
}

/// `(mtime, size)` fingerprint of `path`, or `None` when unavailable.
fn stat_fingerprint(path: &Path) -> Option<lock::FileStat> {
    let meta = fs::metadata(path).ok()?;
    Some(lock::FileStat(meta.modified().ok()?, meta.len()))
}

/// Like [`write`], but creates missing parent directories first. Only the
/// create-mode tools (`notebook_create`, `py_write` without a revision) may
/// call this; generic writes must not fabricate directories from a typo'd
/// path.
pub fn create(path: &Path, content: &str) -> Result<String, Error> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|source| Error::io(parent.to_path_buf(), source))?;
    }
    write(path, content)
}

/// Whether ANY filesystem entry exists at `path` — including directories,
/// unreadable files and symlinks (even dangling ones). One stat, no read:
/// create-mode tools must refuse to clobber entries that [`read`] cannot
/// see (non-UTF-8, oversized, permission-denied), and a read-based
/// existence test would both destroy them and pay O(bytes) for a yes/no
/// question.
pub fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// The real file behind `path`: canonicalize resolves every symlink, so an
/// edit through a symlinked path renames over the real target (see
/// [`write`]). Falls back to `path` itself when the file does not exist yet
/// (create paths): there is nothing to resolve, and the create tools refuse
/// to replace any existing entry, including symlinks.
fn resolve_target(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Copy the target's permission bits onto the temp file (best-effort: a
/// platform that cannot do this must not fail the write itself).
fn preserve_mode(target: &Path, tmp: &Path) {
    if let Ok(meta) = fs::metadata(target) {
        let _ = fs::set_permissions(tmp, meta.permissions());
    }
}

/// Majority line ending of `text`: CRLF when CRLF-terminated lines
/// outnumber LF-only-terminated ones, else LF (ties go to LF, the more
/// common default). The previous rule — "CRLF if it occurs anywhere" — let
/// a single stray CRLF flip a 1000-line LF cell to CRLF on the next
/// line-range edit.
pub fn detect_eol(text: &str) -> &'static str {
    let lf = text.split('\n').count() - 1; // every '\n', CRLF-terminated or not
    let crlf = text.matches("\r\n").count();
    if crlf > lf.saturating_sub(crlf) {
        "\r\n"
    } else {
        "\n"
    }
}

/// A line without its terminator: one trailing `\n` and, before it, one
/// optional `\r` are stripped.
fn strip_terminator(line: &str) -> &str {
    line.strip_suffix('\n')
        .map(|stripped| stripped.strip_suffix('\r').unwrap_or(stripped))
        .unwrap_or(line)
}

/// Logical (1-based editor) lines of `text`, without line terminators.
/// Empty text has no lines. A trailing terminator ends the last line
/// rather than creating an empty one.
pub fn logical_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split_inclusive('\n').map(strip_terminator).collect()
}

/// Join logical lines with `eol`, appending a trailing terminator when
/// `trailing_eol` is set. Trailing terminators on the input lines are
/// stripped first so callers can pass lines verbatim from an LLM.
pub fn join_logical(lines: &[&str], eol: &str, trailing_eol: bool) -> String {
    let stripped: Vec<&str> = lines.iter().map(|line| strip_terminator(line)).collect();
    let mut joined = stripped.join(eol);
    if trailing_eol && !stripped.is_empty() {
        joined.push_str(eol);
    }
    joined
}

/// Replace the 1-based inclusive logical line range `start..=end` of
/// `text` with `new_lines` (empty = deletion).
///
/// Lines outside the range keep their original terminators byte-for-byte —
/// a stray CRLF among LF lines no longer flips the untouched lines.
/// Replacement lines are joined with the majority EOL of `text`
/// ([`detect_eol`]); terminators the caller's lines carry are stripped
/// first (LLMs often include them). When the range reaches the end of the
/// text, the original trailing-newline property applies to the last
/// replacement line; deleting the last line of a text without a trailing
/// newline leaves the previous line's terminator in place (byte
/// preservation wins over the trailing property).
pub fn replace_line_range(
    text: &str,
    start: usize,
    end: usize,
    new_lines: &[&str],
) -> Result<String, String> {
    let segments: Vec<&str> = if text.is_empty() {
        Vec::new()
    } else {
        text.split_inclusive('\n').collect()
    };
    if start < 1 || start > end || end > segments.len() {
        return Err(format!(
            "line range {start}..{end} out of bounds: text has {} lines",
            segments.len()
        ));
    }
    let eol = detect_eol(text);
    let trailing_eol = text.ends_with('\n');
    let suffix = &segments[end..];
    let mut out = String::with_capacity(text.len() + new_lines.len() * 16);
    out.push_str(&segments[..start - 1].concat());
    for (index, line) in new_lines.iter().enumerate() {
        out.push_str(strip_terminator(line));
        let is_last = index + 1 == new_lines.len();
        if !is_last || !suffix.is_empty() || trailing_eol {
            out.push_str(eol);
        }
    }
    out.push_str(&suffix.concat());
    Ok(out)
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, Error> {
    let meta = fs::metadata(path).map_err(|source| match source.kind() {
        std::io::ErrorKind::NotFound => Error::NotFound {
            path: path.to_path_buf(),
        },
        _ => Error::io(path.to_path_buf(), source),
    })?;
    if !meta.is_file() {
        return Err(Error::invalid_input(format!(
            "not a regular file: {}",
            path.display()
        )));
    }
    if meta.len() > MAX_FILE_SIZE {
        return Err(Error::TooLarge {
            path: path.to_path_buf(),
            max_mib: MAX_FILE_SIZE / (1024 * 1024),
        });
    }
    let bytes = fs::read(path).map_err(|source| Error::io(path.to_path_buf(), source))?;
    // The metadata check above is TOCTOU: recheck the actual byte count so a
    // file grown past the cap between stat and read is still rejected.
    if bytes.len() as u64 > MAX_FILE_SIZE {
        return Err(Error::TooLarge {
            path: path.to_path_buf(),
            max_mib: MAX_FILE_SIZE / (1024 * 1024),
        });
    }
    Ok(bytes)
}

/// Sibling temp file name: same directory (hence same filesystem), so the
/// final rename is atomic. Includes the pid so concurrent tpmcp processes
/// never share a temp name.
fn temp_path(path: &Path) -> PathBuf {
    let name = format!(
        ".{}.{}.tpmcp-tmp",
        std::process::id(),
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    path.with_file_name(name)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod tests;
