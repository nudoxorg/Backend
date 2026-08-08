//! Storage-characteristics tests for the NDPK object-pack layer
//! (`index::pack`): real source trees from `.real-crates/` packed through the
//! real `TreeIngest` + `FilesystemObjectPackStore`, with pack bytes on disk
//! measured against raw source bytes on disk (doctrine §4).
//!
//! Scope: only each fixture's own `src/` is packed, not the whole checkout.
//! `.real-crates/<pkg>/vendor/` (offline-vendored dependency copies) and
//! `.real-crates/<pkg>/target/` (build output some prior tool run left
//! behind) are not source the package itself owns — packing them would
//! measure someone else's bytes under this package's name. `src/` is exactly
//! what `pack::tree`'s own docs describe packing ("source trees").

mod common;

use std::path::Path;

use bytes::Bytes;
use common::real_crates_root;
use index::pack::{
    FilesystemObjectPackStore, MemberKey, ObjectPackStore, RelativePath, TreeIngest,
};
use smol_str::SmolStr;

fn source_key(relative: &str) -> MemberKey {
    MemberKey::Source {
        path: RelativePath(SmolStr::new(relative)),
    }
}

struct PackResult {
    raw_bytes: u64,
    pack_disk_delta: i64,
    member_count: usize,
    pack_id: heart::object_pack::ObjectPackId,
}

/// Ingest a real crate's real `src/` tree into a fresh `FilesystemObjectPackStore`
/// and measure both sides: `raw_bytes` (sum of the real file bytes, read
/// directly off disk, independent of the pack machinery) and `pack_disk_delta`
/// (what actually landed in the store directory).
fn pack_real_source_tree(
    case: &str,
    crate_name: &str,
    crate_version: &str,
    store_dir: &Path,
) -> PackResult {
    let src_root = real_crates_root()
        .join(format!("{crate_name}-{crate_version}"))
        .join("src");
    assert!(
        src_root.is_dir(),
        "expected a real fixture at {}; run corpus/fetch.nu",
        src_root.display()
    );
    let raw_bytes = nudox_test_support::disk_bytes(&src_root);
    assert!(raw_bytes > 0, "real source tree must have non-zero bytes");

    let store = FilesystemObjectPackStore::open(store_dir).expect("open pack store");

    let ((pack_id, member_count), cost) = nudox_test_support::measured(case, store_dir, || {
        let builder =
            TreeIngest::ingest_directory(&src_root).expect("ingest a real, symlink-free source tree");
        let member_count = builder.member_count();
        let id = store.put_pack(builder).expect("seal + persist the NDPK pack");
        (id, member_count)
    });

    println!(
        "cost case={case}_detail raw_bytes={raw_bytes} pack_disk_delta_bytes={} \
         members={member_count} ratio={:.3}",
        cost.disk_delta_bytes,
        cost.disk_delta_bytes as f64 / raw_bytes as f64,
    );

    PackResult {
        raw_bytes,
        pack_disk_delta: cost.disk_delta_bytes,
        member_count,
        pack_id,
    }
}

#[test]
fn ndpk_pack_is_smaller_than_raw_for_a_real_small_crate_and_round_trips_a_real_file() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let store_dir = scratch.path().join("store");

    let result = pack_real_source_tree("index/pack_vs_raw_memchr_2_8_3", "memchr", "2.8.3", &store_dir);

    assert!(
        result.member_count >= 40,
        "memchr 2.8.3's real src/ has {} .rs files on disk; expected at least 40",
        result.member_count
    );
    assert!(
        result.pack_disk_delta > 0,
        "sealing+storing a non-empty real source tree must write real bytes to disk"
    );
    assert!(
        (result.pack_disk_delta as u64) < result.raw_bytes,
        "zstd level 19 over real Rust source (repetitive keywords/identifiers) \
         must compress smaller than raw; pack={} raw={}",
        result.pack_disk_delta,
        result.raw_bytes
    );

    // Real-content round trip: fetch one specific real file back out of the
    // pack via a range-addressable member get, and assert it is byte-for-byte
    // identical to the same file read directly off disk right now — proving
    // the pack is not lossy, not just that `put_pack` returned `Ok`.
    let store = FilesystemObjectPackStore::open(&store_dir).expect("reopen store");
    let on_disk = std::fs::read(
        real_crates_root()
            .join("memchr-2.8.3/src/memchr.rs"),
    )
    .expect("read the real file directly");
    let from_pack: Bytes = store
        .get_member(&result.pack_id, &source_key("memchr.rs"))
        .expect("range-get a real member back out of the pack");
    assert_eq!(
        from_pack.as_ref(),
        on_disk.as_slice(),
        "the NDPK round-trip of memchr's real src/memchr.rs must be byte-identical"
    );
}

#[test]
fn ndpk_pack_bytes_scale_with_a_larger_real_crate_and_stay_deterministic() {
    let scratch = tempfile::tempdir().expect("tempdir");

    let memchr_dir = scratch.path().join("store_memchr");
    let memchr = pack_real_source_tree(
        "index/pack_vs_raw_memchr_2_8_3_scaling",
        "memchr",
        "2.8.3",
        &memchr_dir,
    );

    let syn_dir = scratch.path().join("store_syn");
    let syn = pack_real_source_tree("index/pack_vs_raw_syn_1_0_109_scaling", "syn", "1.0.109", &syn_dir);

    // syn's real src/ is a substantially larger real tree than memchr's; its
    // sealed pack must be larger too, both in raw source and in pack bytes.
    assert!(
        syn.raw_bytes > memchr.raw_bytes * 2,
        "syn 1.0.109's real src/ ({} bytes) should be well over 2x memchr \
         2.8.3's ({} bytes)",
        syn.raw_bytes,
        memchr.raw_bytes
    );
    // Real finding, not assumed: pack bytes grow with source size but *not*
    // at the same ratio raw bytes do — syn's real src/ is ~2.6x memchr's raw
    // bytes, but its real sealed pack is only ~1.8x memchr's (zstd-19
    // compresses syn's more repetitive parser/match-heavy source somewhat
    // better than memchr's SIMD-heavy, per-arch-duplicated source). So this
    // asserts real monotonic growth, not a specific ratio — asserting >2x
    // here would be asserting something this measurement does not actually
    // show.
    assert!(
        syn.pack_disk_delta > memchr.pack_disk_delta,
        "syn's sealed pack ({} bytes) must be larger than memchr's ({} bytes) \
         given its larger real source",
        syn.pack_disk_delta,
        memchr.pack_disk_delta
    );
    assert!((syn.pack_disk_delta as u64) < syn.raw_bytes);

    // Determinism (format's own hard requirement, FORMAT.md): re-ingesting the
    // exact same real tree a second time must produce the exact same
    // ObjectPackId, proving the id genuinely identifies the real content, not
    // an insertion-order or timestamp artifact.
    let second_store_dir = scratch.path().join("store_syn_second");
    let second = pack_real_source_tree(
        "index/pack_vs_raw_syn_1_0_109_determinism_check",
        "syn",
        "1.0.109",
        &second_store_dir,
    );
    assert_eq!(
        syn.pack_id, second.pack_id,
        "packing the same real syn 1.0.109 src/ twice must yield the same ObjectPackId"
    );
}
