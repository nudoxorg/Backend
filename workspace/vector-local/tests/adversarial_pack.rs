//! Adversarial pack/unpack tests: hardlink/device entries, zstd-bomb shaped
//! inputs, duplicate schema.json entries, Unicode filenames (09c §1.1,
//! 09-vector §20.3).

mod common;

use heart::ContentHash;
use vector_local::{PackError, pack_shard, unpack_shard};

// ─── Area 7: pack/unpack hardening ───────────────────────────────────────────

/// A tar entry with entry type `Link` (hardlink) must be rejected as
/// `UnsafeEntry`, identical to symlinks — no partial extraction.
///
/// Hard links can escape the destination just like symlinks if the target
/// already exists; we treat them as uniformly hostile.
#[test]
fn hardlink_entry_in_artifact_rejected_as_unsafe() {
    // Build a tar.zst containing one hardlink entry.
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Link);
    header.set_size(0);
    header.set_mode(0o644);
    // Link target: some file that might exist on the host.
    builder.append_link(&mut header, "innocent.txt", "/etc/passwd").unwrap();
    let tar_bytes = builder.into_inner().unwrap();
    let artifact = zstd::stream::encode_all(tar_bytes.as_slice(), 3).unwrap();
    let hash = ContentHash::of_bytes(&artifact);

    let root = tempfile::tempdir().unwrap();
    let dest = root.path().join("shard");
    let err = unpack_shard(&artifact, &hash, &dest).unwrap_err();
    assert!(
        matches!(err, PackError::UnsafeEntry(_)),
        "hardlink entry must be UnsafeEntry, got {err:?}"
    );
    assert!(!dest.exists(), "no partial extraction after hardlink rejection");
}

/// A tar entry with entry type `Fifo` (special file) must be rejected.
#[test]
fn fifo_entry_in_artifact_rejected_as_unsafe() {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Fifo);
    header.set_size(0);
    header.set_mode(0o644);
    builder.append_data(&mut header, "pipe.fifo", std::io::empty()).unwrap();
    let tar_bytes = builder.into_inner().unwrap();
    let artifact = zstd::stream::encode_all(tar_bytes.as_slice(), 3).unwrap();
    let hash = ContentHash::of_bytes(&artifact);

    let root = tempfile::tempdir().unwrap();
    let dest = root.path().join("shard");
    let err = unpack_shard(&artifact, &hash, &dest).unwrap_err();
    assert!(
        matches!(err, PackError::UnsafeEntry(_)),
        "fifo entry must be UnsafeEntry, got {err:?}"
    );
    assert!(!dest.exists());
}

/// A tar entry with entry type `Char` (character device) must be rejected.
#[test]
fn char_device_entry_in_artifact_rejected_as_unsafe() {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Char);
    header.set_size(0);
    header.set_mode(0o644);
    builder.append_data(&mut header, "dev.char", std::io::empty()).unwrap();
    let tar_bytes = builder.into_inner().unwrap();
    let artifact = zstd::stream::encode_all(tar_bytes.as_slice(), 3).unwrap();
    let hash = ContentHash::of_bytes(&artifact);

    let root = tempfile::tempdir().unwrap();
    let dest = root.path().join("shard");
    let err = unpack_shard(&artifact, &hash, &dest).unwrap_err();
    assert!(
        matches!(err, PackError::UnsafeEntry(_)),
        "char device entry must be UnsafeEntry, got {err:?}"
    );
    assert!(!dest.exists());
}

/// A tar entry with type `Block` (block device) must be rejected.
#[test]
fn block_device_entry_in_artifact_rejected_as_unsafe() {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Block);
    header.set_size(0);
    header.set_mode(0o644);
    builder.append_data(&mut header, "dev.block", std::io::empty()).unwrap();
    let tar_bytes = builder.into_inner().unwrap();
    let artifact = zstd::stream::encode_all(tar_bytes.as_slice(), 3).unwrap();
    let hash = ContentHash::of_bytes(&artifact);

    let root = tempfile::tempdir().unwrap();
    let dest = root.path().join("shard");
    let err = unpack_shard(&artifact, &hash, &dest).unwrap_err();
    assert!(
        matches!(err, PackError::UnsafeEntry(_)),
        "block device entry must be UnsafeEntry, got {err:?}"
    );
    assert!(!dest.exists());
}

/// A "zstd-bomb shaped" artifact: a few MiB of zeros, which compresses
/// to a tiny artifact but decompresses to a large byte stream.
///
/// CONTRACT PINNED: the unpack path does NOT enforce a decompressed-size
/// limit — `zstd::stream::read::Decoder` decompresses faithfully on the fly.
/// For a few MiB of zeros, decompression completes in a bounded amount of
/// memory and time (it's not a true bomb — a true bomb would require
/// multi-GB decompression from a tiny stream, which this is not).
///
/// This test documents that unpack SUCCEEDS for a 4 MiB of zeros artifact
/// (the "bomb" is bounded by the input's entropy; zstd cannot amplify
/// arbitrarily without matching input size).  If a size cap is ever added,
/// this test will fail and must be updated.
#[test]
fn artifact_with_large_zero_payload_unpacks_bounded() {
    const ZERO_BYTES: usize = 4 * 1024 * 1024; // 4 MiB of zeros
    let zeros = vec![0u8; ZERO_BYTES];

    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(zeros.len() as u64);
    header.set_mode(0o644);
    builder
        .append_data(&mut header, "big_zero_payload.bin", zeros.as_slice())
        .unwrap();
    let tar_bytes = builder.into_inner().unwrap();
    let artifact = zstd::stream::encode_all(tar_bytes.as_slice(), 3).unwrap();
    let hash = ContentHash::of_bytes(&artifact);

    // Compression ratio: 4 MiB of zeros → should be very small.
    assert!(
        artifact.len() < 1024,
        "4 MiB of zeros should compress to under 1 KiB; got {} bytes (zstd effectiveness sanity)",
        artifact.len()
    );

    let root = tempfile::tempdir().unwrap();
    let dest = root.path().join("shard");

    // CONTRACT: unpack succeeds — no size cap is enforced.
    unpack_shard(&artifact, &hash, &dest)
        .expect("4 MiB zero payload artifact should unpack successfully (no size limit enforced)");
    assert!(dest.exists(), "destination must exist after successful unpack");
    let extracted = dest.join("big_zero_payload.bin");
    let metadata = std::fs::metadata(&extracted).unwrap();
    assert_eq!(
        metadata.len(),
        ZERO_BYTES as u64,
        "extracted file must be exactly {ZERO_BYTES} bytes"
    );
}

/// A tar with `schema.json` appearing TWICE (duplicate entries).
///
/// CONTRACT PINNED: the unpack path writes each entry to its target path in
/// order — the second `schema.json` silently overwrites the first.  This is
/// the behavior of the current implementation (no duplicate-detection).
/// Pin it here.  If dedup is added, this test must be updated.
#[test]
fn duplicate_schema_json_entries_last_write_wins() {
    let content_first = b"first schema content";
    let content_second = b"second schema content overwrites";

    let mut builder = tar::Builder::new(Vec::new());

    let mut h1 = tar::Header::new_gnu();
    h1.set_size(content_first.len() as u64);
    h1.set_mode(0o644);
    builder
        .append_data(&mut h1, "schema.json", content_first.as_slice())
        .unwrap();

    let mut h2 = tar::Header::new_gnu();
    h2.set_size(content_second.len() as u64);
    h2.set_mode(0o644);
    builder
        .append_data(&mut h2, "schema.json", content_second.as_slice())
        .unwrap();

    let tar_bytes = builder.into_inner().unwrap();
    let artifact = zstd::stream::encode_all(tar_bytes.as_slice(), 3).unwrap();
    let hash = ContentHash::of_bytes(&artifact);

    let root = tempfile::tempdir().unwrap();
    let dest = root.path().join("shard");

    // CONTRACT: succeeds; last write wins.
    unpack_shard(&artifact, &hash, &dest)
        .expect("duplicate schema.json must not be an error (last-write-wins)");
    let actual = std::fs::read(dest.join("schema.json")).unwrap();
    assert_eq!(
        actual, content_second,
        "duplicate schema.json: last entry must win (second content overwrites first)"
    );
}

/// Unicode filenames (including non-ASCII CJK, emoji, combining characters)
/// round-trip through pack + unpack exactly.
#[test]
fn unicode_filenames_roundtrip_pack_unpack() {
    let src = tempfile::tempdir().unwrap();

    // Various Unicode filenames.
    let names = [
        "日本語ファイル.bin",
        "ñoño.dat",
        "emoji_🔥.vec",
        "ＡＢＣ全角.txt",
        "café_résumé.json",
        "中文文件名.bin",
        "한국어.dat",
    ];

    for name in &names {
        std::fs::write(src.path().join(name), format!("content of {name}").as_bytes()).unwrap();
    }

    let (artifact, hash) = pack_shard(src.path()).unwrap();

    let root = tempfile::tempdir().unwrap();
    let dest = root.path().join("unicode-shard");
    unpack_shard(&artifact, &hash, &dest).unwrap();

    for name in &names {
        let extracted_path = dest.join(name);
        assert!(
            extracted_path.exists(),
            "unicode filename {name:?} must survive pack+unpack"
        );
        let content = std::fs::read_to_string(&extracted_path).unwrap();
        assert_eq!(
            content,
            format!("content of {name}"),
            "content of unicode file {name:?} must be preserved"
        );
    }
}

/// Filename that is exactly `schema.json` once (the normal case) must survive
/// and be exactly preserved.
#[test]
fn single_schema_json_entry_exact_preserve() {
    let schema_content = br#"{"format_version":1,"model_id":"test","dim":768}"#;

    let mut builder = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_size(schema_content.len() as u64);
    h.set_mode(0o644);
    builder
        .append_data(&mut h, "schema.json", schema_content.as_slice())
        .unwrap();
    let tar_bytes = builder.into_inner().unwrap();
    let artifact = zstd::stream::encode_all(tar_bytes.as_slice(), 3).unwrap();
    let hash = ContentHash::of_bytes(&artifact);

    let root = tempfile::tempdir().unwrap();
    let dest = root.path().join("shard");
    unpack_shard(&artifact, &hash, &dest).unwrap();

    let actual = std::fs::read(dest.join("schema.json")).unwrap();
    assert_eq!(actual, schema_content, "schema.json content must be preserved exactly");
}

/// Empty artifact (no entries): unpack succeeds and destination is an empty directory.
#[test]
fn empty_tar_artifact_creates_empty_destination() {
    let builder = tar::Builder::new(Vec::new());
    let tar_bytes = builder.into_inner().unwrap();
    let artifact = zstd::stream::encode_all(tar_bytes.as_slice(), 3).unwrap();
    let hash = ContentHash::of_bytes(&artifact);

    let root = tempfile::tempdir().unwrap();
    let dest = root.path().join("empty-shard");
    unpack_shard(&artifact, &hash, &dest).unwrap();
    assert!(dest.is_dir(), "empty artifact must create an empty destination directory");
    assert_eq!(
        std::fs::read_dir(&dest).unwrap().count(),
        0,
        "empty artifact must produce an empty directory"
    );
}

/// A `CurDir` component (`.`) in a path is safe (allowed by the path validator).
/// Verify it unpacks without error and the file lands at the expected location.
#[test]
fn curdir_component_in_path_is_allowed() {
    let content = b"data";
    let mut builder = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_size(content.len() as u64);
    h.set_mode(0o644);
    // Path with `./` prefix — CurDir component, which is explicitly allowed.
    builder.append_data(&mut h, "./subdir/file.bin", content.as_slice()).unwrap();
    let tar_bytes = builder.into_inner().unwrap();
    let artifact = zstd::stream::encode_all(tar_bytes.as_slice(), 3).unwrap();
    let hash = ContentHash::of_bytes(&artifact);

    let root = tempfile::tempdir().unwrap();
    let dest = root.path().join("shard");
    unpack_shard(&artifact, &hash, &dest).unwrap();

    // The file should be present under the destination.
    let extracted = dest.join("subdir").join("file.bin");
    assert!(
        extracted.exists(),
        "./subdir/file.bin with CurDir component must be allowed and unpacked"
    );
    assert_eq!(std::fs::read(&extracted).unwrap(), content);
}
