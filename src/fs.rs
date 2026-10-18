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
pub fn read_verified(path: &Path, revision: &str) -> Result<Snapshot, Error> {
    let snapshot = read(path)?;
    if snapshot.revision != revision {
        return Err(Error::Conflict {
            path: path.to_path_buf(),
            expected: revision.to_string(),
            current: snapshot.revision,
        });
    }
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
    let tmp = temp_path(&target);
    let result = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        preserve_mode(&target, &tmp);
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, &target)
    })();
    match result {
        Ok(()) => Ok(revision_of(content.as_bytes())),
        Err(source) => {
            let _ = fs::remove_file(&tmp);
            Err(Error::io(path.to_path_buf(), source))
        }
    }
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

/// Dominant line ending of `text`: CRLF if it occurs anywhere, else LF.
pub fn detect_eol(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// Logical (1-based editor) lines of `text`, without line terminators.
/// Empty text has no lines. A trailing terminator ends the last line
/// rather than creating an empty one.
pub fn logical_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split_inclusive('\n')
        .map(|line| {
            line.strip_suffix('\n')
                .map(|stripped| stripped.strip_suffix('\r').unwrap_or(stripped))
                .unwrap_or(line)
        })
        .collect()
}

/// Join logical lines with `eol`, appending a trailing terminator when
/// `trailing_eol` is set. Trailing terminators on the input lines are
/// stripped first so callers can pass lines verbatim from an LLM.
pub fn join_logical(lines: &[&str], eol: &str, trailing_eol: bool) -> String {
    let stripped: Vec<&str> = lines
        .iter()
        .map(|line| {
            line.strip_suffix('\n')
                .map(|s| s.strip_suffix('\r').unwrap_or(s))
                .unwrap_or(line)
        })
        .collect();
    let mut joined = stripped.join(eol);
    if trailing_eol && !stripped.is_empty() {
        joined.push_str(eol);
    }
    joined
}

/// Replace the 1-based inclusive logical line range `start..=end` of
/// `text` with `new_lines` (empty = deletion), preserving the dominant
/// EOL and the trailing-newline property of the original text.
pub fn replace_line_range(
    text: &str,
    start: usize,
    end: usize,
    new_lines: &[&str],
) -> Result<String, String> {
    let lines = logical_lines(text);
    if start < 1 || start > end || end > lines.len() {
        return Err(format!(
            "line range {start}..{end} out of bounds: text has {} lines",
            lines.len()
        ));
    }
    let eol = detect_eol(text);
    let trailing_eol = text.ends_with('\n');
    let mut rebuilt: Vec<&str> = Vec::with_capacity(lines.len());
    rebuilt.extend_from_slice(&lines[..start - 1]);
    rebuilt.extend_from_slice(new_lines);
    rebuilt.extend_from_slice(&lines[end..]);
    Ok(join_logical(&rebuilt, eol, trailing_eol))
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
    fs::read(path).map_err(|source| Error::io(path.to_path_buf(), source))
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
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let unique = blake3::hash(tag.as_bytes()).to_hex()[..8].to_string();
        let dir =
            std::env::temp_dir().join(format!("tpmcp-test-{}-{tag}-{unique}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_then_read_roundtrips() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("sub/dir/a.py");
        // create() fabricates the missing parents; plain write() must not.
        assert_eq!(write(&path, "print('hi')\n").unwrap_err().kind(), "io_error");
        let revision = create(&path, "print('hi')\n").unwrap();
        assert_eq!(revision, revision_of(b"print('hi')\n"));
        assert_eq!(revision.len(), 16);

        let snapshot = read(&path).unwrap();
        assert_eq!(snapshot.content, "print('hi')\n");
        assert_eq!(snapshot.revision, revision);

        // exists() sees entries read() cannot: files, dirs, dangling links.
        assert!(exists(&path));
        assert!(exists(&dir));
        assert!(!exists(&dir.join("nope.py")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn write_follows_symlink_and_keeps_the_link() {
        let dir = temp_dir("symlink");
        let target = dir.join("real.txt");
        write(&target, "first").unwrap();
        let link = dir.join("link.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        write(&link, "second").unwrap();

        // The link survives and the resolved target received the bytes.
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "second");
        assert_eq!(read(&link).unwrap().content, "second");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn write_preserves_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("mode");
        let path = dir.join("secret.py");
        write(&path, "x = 1\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        write(&path, "x = 2\n").unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "edit must not widen permissions");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_verified_detects_conflict() {
        let dir = temp_dir("conflict");
        let path = dir.join("a.txt");
        write(&path, "one").unwrap();
        let current = read(&path).unwrap().revision;

        assert!(read_verified(&path, &current).is_ok());
        let error = read_verified(&path, "stale0000000000").unwrap_err();
        match error {
            Error::Conflict { current, .. } => assert_eq!(current.len(), 16),
            other => panic!("expected conflict, got {other:?}"),
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_missing_file_is_not_found() {
        let dir = temp_dir("missing");
        let error = read(&dir.join("nope.py")).unwrap_err();
        assert_eq!(error.kind(), "not_found");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_rejects_non_utf8_and_directories() {
        let dir = temp_dir("binary");
        let path = dir.join("blob.bin");
        fs::write(&path, [0xff_u8, 0xfe, 0x00]).unwrap();
        assert_eq!(read(&path).unwrap_err().kind(), "not_utf8");
        assert_eq!(read(&dir).unwrap_err().kind(), "invalid_input");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_rejects_oversized_files() {
        let dir = temp_dir("large");
        let path = dir.join("big.py");
        // Sparse file: costs no disk blocks but reports a huge length.
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_FILE_SIZE + 1).unwrap();
        drop(file);
        assert_eq!(read(&path).unwrap_err().kind(), "too_large");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn logical_lines_handles_edges() {
        assert_eq!(logical_lines(""), Vec::<&str>::new());
        assert_eq!(logical_lines("a\nb\n"), ["a", "b"]);
        assert_eq!(logical_lines("a\nb"), ["a", "b"]);
        assert_eq!(logical_lines("a\n\nb\n"), ["a", "", "b"]);
        assert_eq!(logical_lines("a\r\nb\r\n"), ["a", "b"]);
        assert_eq!(detect_eol("a\r\nb"), "\r\n");
        assert_eq!(detect_eol("a\nb"), "\n");
    }

    #[test]
    fn replace_line_range_preserves_eol_and_trailing() {
        // Replace a middle line, trailing newline preserved.
        assert_eq!(
            replace_line_range("a\nb\nc\n", 2, 2, &["x"]).unwrap(),
            "a\nx\nc\n"
        );
        // Replace multiple lines with more lines.
        assert_eq!(
            replace_line_range("a\nb\nc\n", 1, 2, &["x", "y", "z"]).unwrap(),
            "x\ny\nz\nc\n"
        );
        // Deletion.
        assert_eq!(
            replace_line_range("a\nb\nc\n", 2, 2, &[]).unwrap(),
            "a\nc\n"
        );
        // No trailing newline in the original stays that way.
        assert_eq!(replace_line_range("a\nb", 2, 2, &["x"]).unwrap(), "a\nx");
        // CRLF is preserved.
        assert_eq!(
            replace_line_range("a\r\nb\r\n", 1, 1, &["x"]).unwrap(),
            "x\r\nb\r\n"
        );
        // Provided lines may carry their own terminators; they are stripped.
        assert_eq!(
            replace_line_range("a\nb\n", 2, 2, &["x\n", "y\r\n"]).unwrap(),
            "a\nx\ny\n"
        );
    }

    #[test]
    fn replace_line_range_rejects_bad_bounds() {
        assert!(replace_line_range("a\nb\n", 0, 1, &["x"]).is_err());
        assert!(replace_line_range("a\nb\n", 2, 1, &["x"]).is_err());
        assert!(replace_line_range("a\nb\n", 1, 3, &["x"]).is_err());
        assert!(replace_line_range("", 1, 1, &["x"]).is_err());
    }
}
