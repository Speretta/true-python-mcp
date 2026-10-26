//! Unit tests for the filesystem primitives (`crate::fs`).

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
    assert_eq!(
        write(&path, "print('hi')\n").unwrap_err().kind(),
        "io_error"
    );
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
fn copy_stream_roundtrips_bytes_and_revision() {
    let dir = temp_dir("stream");
    let source = dir.join("blob.bin");
    let dest = dir.join("copy.bin");
    // Binary payload: stream_copy moves raw bytes, not UTF-8 text.
    let payload: Vec<u8> = (0_u32..4096).flat_map(|n| n.to_be_bytes()).collect();
    fs::write(&source, &payload).unwrap();

    let (revision, bytes) = copy_stream(&source, &dest, None).unwrap();

    assert_eq!(bytes, payload.len() as u64);
    assert_eq!(revision, revision_of(&payload));
    assert_eq!(fs::read(&dest).unwrap(), payload);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn copy_stream_holds_the_revision_guard_when_given_one() {
    let dir = temp_dir("stream-guard");
    let source = dir.join("blob.bin");
    let dest = dir.join("copy.bin");
    fs::write(&source, b"new bytes").unwrap();
    fs::write(&dest, b"old bytes").unwrap();

    // A stale revision refuses the overwrite and leaves `dest` untouched —
    // the same conflict contract as `write`.
    let stale = revision_of(b"something else");
    assert_eq!(
        copy_stream(&source, &dest, Some(&stale))
            .unwrap_err()
            .kind(),
        "conflict"
    );
    assert_eq!(fs::read(&dest).unwrap(), b"old bytes");

    // The current revision copies through. Without a revision the copy is
    // unconditional (`cp` semantics) — covered by the roundtrip test above.
    let current = revision_of(b"old bytes");
    assert!(copy_stream(&source, &dest, Some(&current)).is_ok());
    assert_eq!(fs::read(&dest).unwrap(), b"new bytes");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn copy_stream_rejects_missing_non_regular_and_oversized_sources() {
    let dir = temp_dir("stream-errors");
    assert_eq!(
        copy_stream(&dir.join("nope.bin"), &dir.join("out.bin"), None)
            .unwrap_err()
            .kind(),
        "not_found"
    );
    assert_eq!(
        copy_stream(&dir, &dir.join("out.bin"), None)
            .unwrap_err()
            .kind(),
        "invalid_input"
    );
    let huge = dir.join("huge.bin");
    let file = fs::File::create(&huge).unwrap();
    file.set_len(MAX_FILE_SIZE + 1).unwrap();
    drop(file);
    assert_eq!(
        copy_stream(&huge, &dir.join("out.bin"), None)
            .unwrap_err()
            .kind(),
        "too_large"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn copy_stream_does_not_fabricate_missing_parents() {
    let dir = temp_dir("stream-parent");
    let source = dir.join("blob.bin");
    fs::write(&source, b"x").unwrap();
    let missing = dir.join("no").join("such").join("dir.bin");
    assert_eq!(
        copy_stream(&source, &missing, None).unwrap_err().kind(),
        "io_error"
    );
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

#[test]
fn replace_line_range_keeps_foreign_endings_outside_the_range() {
    // LF majority (2 LF-only vs 1 CRLF): new lines get LF and the
    // untouched lines keep their exact terminators.
    assert_eq!(
        replace_line_range("a\r\nb\nc\n", 2, 2, &["x"]).unwrap(),
        "a\r\nx\nc\n"
    );
    // CRLF majority: new lines get CRLF, untouched endings stay as-is.
    assert_eq!(
        replace_line_range("a\r\nb\r\nc\n", 2, 2, &["x"]).unwrap(),
        "a\r\nx\r\nc\n"
    );
    // detect_eol counts the majority; it no longer fires on "anywhere".
    assert_eq!(detect_eol("a\r\nb\nc\n"), "\n");
    assert_eq!(detect_eol("a\r\nb\r\nc\n"), "\r\n");
    assert_eq!(detect_eol("no eol at all"), "\n");
}

#[test]
fn write_conflicts_when_external_save_lands_after_verify() {
    let dir = temp_dir("recheck");
    let path = dir.join("a.txt");
    write(&path, "one").unwrap();
    let revision = read(&path).unwrap().revision;
    read_verified(&path, &revision).unwrap(); // records the fingerprint
    std::fs::write(&path, "external").unwrap(); // external editor saves

    let error = write(&path, "two").unwrap_err();
    match error {
        Error::Conflict {
            expected, current, ..
        } => {
            assert_eq!(expected, revision);
            assert_eq!(current, revision_of(b"external"));
        }
        other => panic!("expected conflict, got {other:?}"),
    }
    // The refused write renamed nothing: the external bytes survive.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "external");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn write_proceeds_when_external_save_keeps_content_identical() {
    let dir = temp_dir("recheck-same");
    let path = dir.join("a.txt");
    write(&path, "same").unwrap();
    let revision = read(&path).unwrap().revision;
    read_verified(&path, &revision).unwrap();
    // An external save of identical bytes changes the stat, not the
    // content: the stat filter fires, the content check clears it.
    std::fs::write(&path, "same").unwrap();

    assert_eq!(write(&path, "next").unwrap(), revision_of(b"next"));
    assert_eq!(read(&path).unwrap().content, "next");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn stamps_are_consumed_and_never_leak_across_flows() {
    let dir = temp_dir("stamps");
    let path = dir.join("a.txt");
    write(&path, "one").unwrap();

    // A verified write consumes its stamp: a second plain write without a
    // new read_verified does not recheck.
    let revision = read(&path).unwrap().revision;
    read_verified(&path, &revision).unwrap();
    write(&path, "two").unwrap();
    std::fs::write(&path, "external").unwrap();
    assert!(write(&path, "three").is_ok(), "no stamp, no recheck");

    // An aborted flow's fingerprint (verified, never written) is dropped at
    // the next lock acquisition instead of poisoning the new flow.
    let current = read(&path).unwrap().revision;
    read_verified(&path, &current).unwrap();
    std::fs::write(&path, "external2").unwrap(); // file moves on
    let result = with_file_lock(&path, || write(&path, "four"));
    assert!(result.is_ok(), "stale stamp must not leak into a new flow");
    assert_eq!(read(&path).unwrap().content, "four");
    fs::remove_dir_all(&dir).unwrap();
}
