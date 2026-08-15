//! Adversarial integration tests for the NDPK v1 engine.
//!
//! These exercise the public crate surface end-to-end: determinism under
//! insertion-order shuffling, the path-traversal corpus, truncation at every
//! structural boundary, TOC corruption, range-read edge cases, a huge member
//! key, a zero-member pack, and a proptest members→pack→read round-trip.
//!
//! Invariant under test throughout: **adversarial input never panics** — every
//! failure is a typed [`PackError`].

use bytes::Bytes;
use index::pack::{
    MemberKey, ObjectPackBuilder, ObjectPackReader, PackError, RelativePath,
    SOURCE_CHUNK_SIZE_BYTES,
};
use proptest::prelude::*;
use smol_str::SmolStr;

/// Build a `MemberKey::Source` from a path string.
fn source_key(path: &str) -> MemberKey {
    MemberKey::Source { path: RelativePath(SmolStr::new(path)) }
}

/// Seal a builder to bytes, panicking (test-only) on error.
fn seal(builder: ObjectPackBuilder) -> Bytes {
    builder.seal_to_bytes().expect("seal must succeed").0
}

// ---------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------

#[test]
fn shuffled_insertion_order_is_byte_identical() {
    let members: Vec<(&str, &[u8])> = vec![
        ("z/last.rs", b"last"),
        ("a/first.rs", b"first"),
        ("m/middle.rs", b"middle"),
        ("a/also.rs", b"also"),
        ("nested/deep/file.rs", b"deep"),
    ];

    let forward = {
        let mut builder = ObjectPackBuilder::new();
        for (path, content) in &members {
            builder
                .add_member(source_key(path), Bytes::copy_from_slice(content))
                .expect("add");
        }
        builder.seal_to_bytes().expect("seal")
    };

    let reversed = {
        let mut builder = ObjectPackBuilder::new();
        for (path, content) in members.iter().rev() {
            builder
                .add_member(source_key(path), Bytes::copy_from_slice(content))
                .expect("add");
        }
        builder.seal_to_bytes().expect("seal")
    };

    assert_eq!(forward.1, reversed.1, "ids must match under shuffle");
    assert_eq!(forward.0, reversed.0, "bytes must be identical under shuffle");
}

#[test]
fn same_tree_built_twice_is_identical() {
    let build = || {
        let mut builder = ObjectPackBuilder::new();
        builder
            .add_member(source_key("src/lib.rs"), Bytes::from_static(b"pub fn a() {}"))
            .expect("add");
        builder
            .add_member(source_key("Cargo.toml"), Bytes::from_static(b"[package]"))
            .expect("add");
        builder.seal_to_bytes().expect("seal")
    };
    let (bytes_one, id_one) = build();
    let (bytes_two, id_two) = build();
    assert_eq!(id_one, id_two);
    assert_eq!(bytes_one, bytes_two);
}

// ---------------------------------------------------------------------------
// Path-traversal corpus
// ---------------------------------------------------------------------------

#[test]
fn path_traversal_corpus_is_rejected() {
    // (path, expected-to-be-an-absolute-path-error?)
    let absolute_cases = ["/abs", "/etc/passwd", "C:evil", "c:also"];
    let unsafe_cases = [
        "../x",
        "a/../b",
        "a/./b",
        "..",
        ".",
        "trailing/",
        "double//slash",
        "", // empty path -> single empty segment
        "back\\slash",
        "nul\0byte",
    ];

    for path in absolute_cases {
        let mut builder = ObjectPackBuilder::new();
        let err = builder
            .add_source_file(RelativePath(SmolStr::new(path)), Bytes::from_static(b"x"))
            .expect_err(&format!("path {path:?} must be rejected"));
        assert!(
            matches!(err, PackError::AbsolutePath { .. }),
            "path {path:?} expected AbsolutePath, got {err:?}"
        );
    }

    for path in unsafe_cases {
        let mut builder = ObjectPackBuilder::new();
        let err = builder
            .add_source_file(RelativePath(SmolStr::new(path)), Bytes::from_static(b"x"))
            .expect_err(&format!("path {path:?} must be rejected"));
        assert!(
            matches!(err, PackError::UnsafePathSegment { .. }),
            "path {path:?} expected UnsafePathSegment, got {err:?}"
        );
    }
}

#[test]
fn duplicate_member_is_rejected() {
    let mut builder = ObjectPackBuilder::new();
    builder
        .add_member(source_key("dup.rs"), Bytes::from_static(b"one"))
        .expect("first add");
    let err = builder
        .add_member(source_key("dup.rs"), Bytes::from_static(b"two"))
        .expect_err("duplicate must be rejected");
    assert!(matches!(err, PackError::DuplicateMember { .. }), "got {err:?}");
}

// ---------------------------------------------------------------------------
// Truncation at every structural boundary
// ---------------------------------------------------------------------------

#[test]
fn truncation_at_every_length_never_panics() {
    // A pack big enough to have a header, member frames, and a TOC.
    let mut builder = ObjectPackBuilder::new();
    builder
        .add_member(source_key("a.rs"), Bytes::from_static(b"some content here"))
        .expect("add");
    builder
        .add_member(source_key("b.rs"), Bytes::from_static(b"more content over here"))
        .expect("add");
    let full = seal(builder);

    // Truncate at every possible length from 0 to full length - 1. Each must
    // either open (only the full length should fully open) or return a typed
    // error — never panic.
    //
    // Doctrine §4: the measured region is the full truncation sweep — one reader
    // open (plus two member reads when it opens) per prefix length, so the work
    // scales with the sealed pack size. `disk_delta_bytes` is 0 here by
    // construction: the sweep is entirely in-memory over `Bytes`, so the signal
    // on this line is wall/RSS, and wall is only comparable on an idle host. The
    // reliable figure is `sealed_bytes`, asserted below.
    let sealed_bytes = full.len();
    let scratch = tempfile::tempdir().expect("tempdir");
    let (opened_ok, _cost) = heart::cost::measured(
        "object_pack/truncation_sweep",
        scratch.path(),
        || {
            let mut opened_ok = 0usize;
            for length in 0..full.len() {
                let truncated = full.slice(0..length);
                match ObjectPackReader::open_bytes(truncated) {
                    Ok(reader) => {
                        // If it opened, member reads must also never panic.
                        let _ = reader.get_member(&source_key("a.rs"));
                        let _ = reader.get_member(&source_key("b.rs"));
                        opened_ok += 1;
                    }
                    Err(error) => {
                        // Any typed error is acceptable; Display must work.
                        let _ = format!("{error}");
                    }
                }
            }
            opened_ok
        },
    );

    // Assert on content, not just on "it didn't panic": a strict prefix of a
    // well-formed pack must never be mistaken for a complete one, so of the
    // `sealed_bytes` prefixes swept (0..len, i.e. excluding the full pack) none
    // may open. A regression that made the header/TOC bounds check permissive
    // would show up here as a non-zero count.
    assert_eq!(
        opened_ok, 0,
        "no strict prefix of a {sealed_bytes}-byte pack may open as a valid pack"
    );
}

#[test]
fn bad_magic_is_typed() {
    let full = {
        let mut builder = ObjectPackBuilder::new();
        builder
            .add_member(source_key("x.rs"), Bytes::from_static(b"data"))
            .expect("add");
        seal(builder)
    };
    let mut bytes = full.to_vec();
    bytes[0] = b'X';
    let err = ObjectPackReader::open_bytes(Bytes::from(bytes)).expect_err("bad magic");
    assert!(matches!(err, PackError::BadMagic { .. }), "got {err:?}");
}

// ---------------------------------------------------------------------------
// TOC corruption
// ---------------------------------------------------------------------------

#[test]
fn corrupted_toc_byte_is_detected() {
    let (full, original_id) = {
        let mut builder = ObjectPackBuilder::new();
        builder
            .add_member(source_key("toc.rs"), Bytes::from_static(b"content for toc test"))
            .expect("add");
        builder.seal_to_bytes().expect("seal")
    };

    // The header records where the TOC begins; corrupt every TOC byte in turn.
    let header = index::pack::format::PackHeader::decode(&full).expect("decode header");
    let toc_start = header.toc_offset as usize;
    assert!(toc_start < full.len());

    for corrupt_at in toc_start..full.len() {
        let mut bytes = full.to_vec();
        bytes[corrupt_at] ^= 0xFF;

        match ObjectPackReader::open_bytes(Bytes::from(bytes)) {
            // Either the postcard decode / ordering check fails outright...
            Err(error) => {
                let _ = format!("{error}");
            }
            // ...or the flip produced a *different canonical* TOC (e.g. a bit
            // inside a stored hash or the policy). Content addressing is then
            // the guaranteed detector: the derived id must differ from the
            // announced one. Full verification must never panic, whatever it
            // concludes (a flipped member hash fails it; a flipped policy
            // field does not — the id catches that case).
            Ok(reader) => {
                assert_ne!(
                    reader.id(),
                    original_id,
                    "flipped TOC byte at {corrupt_at} must change the derived id"
                );
                if let Err(error) = reader.verify_all() {
                    let _ = format!("{error}");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Range reads
// ---------------------------------------------------------------------------

#[test]
fn range_read_edge_cases() {
    let chunk = SOURCE_CHUNK_SIZE_BYTES as usize;
    let content: Vec<u8> = (0u8..=255).cycle().take(chunk * 2 + 100).collect();

    let full = {
        let mut builder = ObjectPackBuilder::new();
        builder
            .add_member(source_key("big.bin"), Bytes::from(content.clone()))
            .expect("add");
        seal(builder)
    };
    let reader = ObjectPackReader::open_bytes(full).expect("open");
    let key = source_key("big.bin");
    let total = content.len() as u64;

    // Empty range.
    assert!(reader.get_member_range(&key, 0..0).expect("empty").is_empty());

    // Range exactly at a chunk boundary.
    let at_boundary = reader
        .get_member_range(&key, chunk as u64..(chunk as u64 + 10))
        .expect("boundary");
    assert_eq!(at_boundary.as_ref(), &content[chunk..chunk + 10]);

    // Range spanning all chunks.
    let whole = reader.get_member_range(&key, 0..total).expect("whole");
    assert_eq!(whole.as_ref(), &content[..]);

    // Range past end -> typed error.
    let err = reader
        .get_member_range(&key, 0..(total + 1))
        .expect_err("past end");
    assert!(matches!(err, PackError::RangeOutOfBounds { .. }), "got {err:?}");

    // start > end -> typed error.
    let start = 10_u64;
    let end = 5_u64;
    let err = reader
        .get_member_range(&key, start..end)
        .expect_err("inverted range");
    assert!(matches!(err, PackError::RangeOutOfBounds { .. }), "got {err:?}");
}

// ---------------------------------------------------------------------------
// Zero-member pack
// ---------------------------------------------------------------------------

#[test]
fn zero_member_pack_round_trips() {
    let full = seal(ObjectPackBuilder::new());
    let reader = ObjectPackReader::open_bytes(full).expect("open empty pack");
    assert_eq!(reader.member_count(), 0);
    assert_eq!(reader.members().count(), 0);
    reader.verify_all().expect("empty pack verifies");
    // Looking up any member is a typed miss, not a panic.
    let err = reader
        .get_member(&source_key("nope.rs"))
        .expect_err("no members");
    assert!(matches!(err, PackError::MemberNotFound { .. }), "got {err:?}");
}

// ---------------------------------------------------------------------------
// Huge member key
// ---------------------------------------------------------------------------

#[test]
fn huge_member_key_round_trips() {
    // A very long (but valid) source path: many nested segments.
    let long_path = (0..2000)
        .map(|index| format!("seg{index}"))
        .collect::<Vec<_>>()
        .join("/");
    let key = source_key(&long_path);

    let mut builder = ObjectPackBuilder::new();
    builder
        .add_member(key.clone(), Bytes::from_static(b"payload"))
        .expect("add huge key");
    let full = seal(builder);

    let reader = ObjectPackReader::open_bytes(full).expect("open");
    let got = reader.get_member(&key).expect("get huge-key member");
    assert_eq!(got.as_ref(), b"payload");
}

// ---------------------------------------------------------------------------
// Proptest: members -> pack -> read round-trip
// ---------------------------------------------------------------------------

/// Generate a set of unique-path members with arbitrary byte contents. Paths
/// are constrained to safe, non-empty, `/`-joined lowercase segments so the
/// builder always accepts them; the property under test is the round-trip, not
/// path rejection (covered elsewhere).
fn arbitrary_members() -> impl Strategy<Value = Vec<(String, Vec<u8>)>> {
    let segment = "[a-z][a-z0-9_]{0,7}";
    let path = prop::collection::vec(segment, 1..4)
        .prop_map(|segments| segments.join("/"));
    let content = prop::collection::vec(any::<u8>(), 0..4096);
    // Dedup by path via a BTreeMap so the builder never sees a duplicate.
    prop::collection::btree_map(path, content, 0..12)
        .prop_map(|map| map.into_iter().collect())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn roundtrip_members_to_pack_to_read(members in arbitrary_members()) {
        let mut builder = ObjectPackBuilder::new();
        for (path, content) in &members {
            builder
                .add_member(source_key(path), Bytes::copy_from_slice(content))
                .expect("add");
        }
        let (bytes, id) = builder.seal_to_bytes().expect("seal");

        let reader = ObjectPackReader::open_bytes(bytes.clone()).expect("open");
        prop_assert_eq!(reader.id(), id);
        prop_assert_eq!(reader.member_count(), members.len());

        for (path, content) in &members {
            let got = reader.get_member(&source_key(path)).expect("get");
            prop_assert_eq!(got.as_ref(), content.as_slice());

            // A mid-member range must also round-trip.
            if !content.is_empty() {
                let mid = (content.len() / 2) as u64;
                let end = content.len() as u64;
                let got_range = reader
                    .get_member_range(&source_key(path), mid..end)
                    .expect("range");
                prop_assert_eq!(got_range.as_ref(), &content[mid as usize..]);
            }
        }

        // Determinism: re-sealing the same members reproduces the same bytes.
        let mut rebuilder = ObjectPackBuilder::new();
        for (path, content) in members.iter().rev() {
            rebuilder
                .add_member(source_key(path), Bytes::copy_from_slice(content))
                .expect("add");
        }
        let (rebytes, reid) = rebuilder.seal_to_bytes().expect("reseal");
        prop_assert_eq!(rebytes, bytes);
        prop_assert_eq!(reid, id);
    }
}
