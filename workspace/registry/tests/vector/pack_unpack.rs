#![cfg(feature = "local")]
//! Artifact-plane tests: pack → unpack → load roundtrip, and the hostile
//! inputs the unpacker must refuse — path traversal, links, bit flips,
//! clobbering an existing install.

mod common;

use common::*;
use heart::ContentHash;
use registry::vector::{JinaCodeV2, StoreError, VectorStore};
use registry::vector::local::{LOCK_FILE, LocalShardStore, PackError, pack_shard, unpack_shard};

/// Pack a live shard, unpack it elsewhere, load read-only: identical
/// search results (ids, order, and exact scores), payload included; the
/// per-machine lock file is not part of the artifact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pack_unpack_load_roundtrip_preserves_search_results() {
	let src = tempfile::tempdir().unwrap();
	let store = open_mutable_f32(src.path()).await;
	store.upsert(corpus(30, "acme")).await.unwrap();
	let before = store.search(request(basis(0), 30)).await.unwrap();
	assert_eq!(before.len(), 30);
	store.close().await.unwrap();
	settle().await;

	let (artifact, hash) = pack_shard(src.path()).unwrap();
	assert_eq!(hash, ContentHash::of_bytes(&artifact), "hash is of the compressed bytes");

	let dest_root = tempfile::tempdir().unwrap();
	let dest = dest_root.path().join("baked");
	unpack_shard(&artifact, &hash, &dest).unwrap();
	assert!(!dest.join(LOCK_FILE).exists(), "the advisory lock is per-machine state");
	assert!(
		!dest_root.path().join("baked.tmp").exists(),
		"staging dir must be gone after the atomic rename"
	);

	let baked = LocalShardStore::open_read_only(&dest, f32_schema()).await.unwrap();
	let after = VectorStore::<JinaCodeV2>::search(&baked, request(basis(0), 30)).await.unwrap();

	assert_eq!(before.len(), after.len());
	for (a, b) in before.iter().zip(&after) {
		assert_eq!(a.id, b.id, "roundtrip must preserve ordering");
		assert!(
			(a.score - b.score).abs() < 1e-6,
			"roundtrip must preserve scores: {} vs {}",
			a.score,
			b.score
		);
		assert_eq!(a.payload, b.payload, "roundtrip must preserve payload");
	}
}

/// A tar entry with `..` in its path is a tar-slip attack: rejected, and
/// nothing may exist outside (or inside) the destination afterwards.
#[test]
fn parent_dir_traversal_artifact_rejected() {
	let (artifact, hash) = hostile_artifact("../evil.txt", b"pwned");
	let root = tempfile::tempdir().unwrap();
	let dest = root.path().join("victim").join("shard");

	let err = unpack_shard(&artifact, &hash, &dest).unwrap_err();
	assert!(matches!(err, PackError::UnsafeEntry(_)), "traversal must be UnsafeEntry: {err:?}");
	assert!(!dest.exists(), "no partial extraction may remain");
	assert!(!root.path().join("victim").join("evil.txt").exists(), "tar-slip escaped!");
	assert!(!root.path().join("evil.txt").exists(), "tar-slip escaped the parent!");
}

/// An absolute entry path is equally hostile.
#[test]
fn absolute_path_artifact_rejected() {
	let (artifact, hash) = hostile_artifact("/tmp/vector-local-absolute-evil", b"pwned");
	let root = tempfile::tempdir().unwrap();
	let dest = root.path().join("shard");

	let err = unpack_shard(&artifact, &hash, &dest).unwrap_err();
	assert!(
		matches!(err, PackError::UnsafeEntry(_)),
		"absolute path must be UnsafeEntry: {err:?}"
	);
	assert!(!dest.exists());
	assert!(!std::path::Path::new("/tmp/vector-local-absolute-evil").exists());
}

/// Symlink entries are links into arbitrary filesystem locations: rejected.
#[test]
fn symlink_artifact_rejected() {
	let mut builder = tar::Builder::new(Vec::new());
	let mut header = tar::Header::new_gnu();
	header.set_entry_type(tar::EntryType::Symlink);
	header.set_size(0);
	header.set_mode(0o777);
	builder.append_link(&mut header, "innocent", "/etc/passwd").unwrap();
	let artifact = zstd::stream::encode_all(builder.into_inner().unwrap().as_slice(), 3).unwrap();
	let hash = ContentHash::of_bytes(&artifact);

	let root = tempfile::tempdir().unwrap();
	let dest = root.path().join("shard");
	let err = unpack_shard(&artifact, &hash, &dest).unwrap_err();
	assert!(matches!(err, PackError::UnsafeEntry(_)), "symlink must be UnsafeEntry: {err:?}");
	assert!(!dest.exists());
}

/// A single flipped bit fails BLAKE3 verification **before** any
/// decompression: the error is `HashMismatch` and no filesystem state is
/// created — not even the staging directory.
#[test]
fn corrupted_artifact_rejected_before_unpack() {
	let src = tempfile::tempdir().unwrap();
	std::fs::write(src.path().join("schema.json"), b"{}").unwrap();
	std::fs::write(src.path().join("data.bin"), vec![7u8; 4096]).unwrap();
	let (mut artifact, hash) = pack_shard(src.path()).unwrap();

	let mid = artifact.len() / 2;
	artifact[mid] ^= 0x01;

	let root = tempfile::tempdir().unwrap();
	let dest = root.path().join("shard");
	let err = unpack_shard(&artifact, &hash, &dest).unwrap_err();
	assert!(
		matches!(err, PackError::HashMismatch { .. }),
		"bit flip must be HashMismatch: {err:?}"
	);
	assert!(!dest.exists(), "nothing may be written for a corrupt artifact");
	assert!(!root.path().join("shard.tmp").exists(), "no staging dir for a corrupt artifact");
}

/// Wrong *expected* hash (a lying manifest) is exactly as fatal as
/// corrupted bytes.
#[test]
fn lying_manifest_hash_rejected() {
	let src = tempfile::tempdir().unwrap();
	std::fs::write(src.path().join("f"), b"bytes").unwrap();
	let (artifact, _) = pack_shard(src.path()).unwrap();
	let lying = ContentHash::of_bytes(b"not these bytes");

	let root = tempfile::tempdir().unwrap();
	let err = unpack_shard(&artifact, &lying, &root.path().join("shard")).unwrap_err();
	assert!(matches!(err, PackError::HashMismatch { .. }));
}

/// Upgrades are whole-directory swaps: unpacking over an existing install
/// is refused, never merged.
#[test]
fn existing_destination_rejected() {
	let src = tempfile::tempdir().unwrap();
	std::fs::write(src.path().join("f"), b"bytes").unwrap();
	let (artifact, hash) = pack_shard(src.path()).unwrap();

	let root = tempfile::tempdir().unwrap();
	let dest = root.path().join("shard");
	unpack_shard(&artifact, &hash, &dest).unwrap();
	let err = unpack_shard(&artifact, &hash, &dest).unwrap_err();
	assert!(matches!(err, PackError::DestinationExists(_)), "no in-place merges: {err:?}");
}

/// Packing a fixed byte tree twice yields identical artifact bytes (sorted
/// entries + deterministic headers) — the id is stable for a stable tree.
#[test]
fn pack_is_deterministic_for_a_fixed_tree() {
	let src = tempfile::tempdir().unwrap();
	std::fs::create_dir(src.path().join("segments")).unwrap();
	std::fs::write(src.path().join("segments").join("a"), vec![1u8; 512]).unwrap();
	std::fs::write(src.path().join("schema.json"), b"{}").unwrap();

	let (bytes_a, hash_a) = pack_shard(src.path()).unwrap();
	let (bytes_b, hash_b) = pack_shard(src.path()).unwrap();
	assert_eq!(bytes_a, bytes_b);
	assert_eq!(hash_a, hash_b);
}

/// A packed shard opened read-only refuses a mismatched schema exactly
/// like a locally created one (the artifact carries `schema.json`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unpacked_shard_still_validates_schema() {
	let src = tempfile::tempdir().unwrap();
	let store = open_mutable_f32(src.path()).await;
	store.upsert(corpus(5, "acme")).await.unwrap();
	store.close().await.unwrap();
	settle().await;

	let (artifact, hash) = pack_shard(src.path()).unwrap();
	let root = tempfile::tempdir().unwrap();
	let dest = root.path().join("shard");
	unpack_shard(&artifact, &hash, &dest).unwrap();

	let mut wrong = f32_schema();
	wrong.dim = 384;
	let err = LocalShardStore::open_read_only(&dest, wrong).await.unwrap_err();
	assert!(matches!(err, StoreError::Corrupt(_)), "unpacked shard schema gate: {err:?}");
}

/// Build a `tar.zst` artifact containing one regular-file entry at an
/// attacker-chosen path, correctly hashed (the hash gate must not be what
/// saves us here).
///
/// This deliberately does NOT go through `Header::set_path` /
/// `Builder::append_data`: as of this `tar` version both validate the path
/// and refuse to write an absolute path or one containing `..` ("paths in
/// archives must be relative" / "must not have `..`") — which would make
/// this fixture unable to construct the very attack the unpacker is being
/// tested against. A real attacker is not bound by this crate's writer-side
/// niceties (a hand-crafted archive, or one from an older/different tar
/// implementation, can carry any bytes in the name field), so the fixture
/// writes the hostile name directly into the header and appends the raw
/// header + body with `Builder::append`, which performs no path validation
/// at all — exercising the same read path (`unpack_shard` → `entry.path()`
/// → `validate_safe_path`) a genuine hostile archive would.
fn hostile_artifact(entry_path: &str, contents: &[u8]) -> (Vec<u8>, ContentHash) {
	let mut builder = tar::Builder::new(Vec::new());
	let mut header = tar::Header::new_gnu();
	header.set_size(contents.len() as u64);
	header.set_mode(0o644);
	{
		let name_bytes = entry_path.as_bytes();
		let gnu = header.as_gnu_mut().expect("new_gnu() header is always GNU");
		assert!(
			name_bytes.len() < gnu.name.len(),
			"fixture path {entry_path:?} does not fit in the 100-byte GNU name field"
		);
		gnu.name = [0u8; 100];
		gnu.name[..name_bytes.len()].copy_from_slice(name_bytes);
	}
	header.set_cksum();
	builder.append(&header, contents).unwrap();
	let tar_bytes = builder.into_inner().unwrap();

	// The fixture is only as hostile as the bytes it actually wrote — verify
	// the archive really carries the attacker-chosen path before trusting a
	// green test against it (a benign fixture would pass for the wrong
	// reason).
	let mut check = tar::Archive::new(tar_bytes.as_slice());
	let mut saw_hostile_entry = false;
	for entry in check.entries().unwrap() {
		let entry = entry.unwrap();
		let raw_path = entry.header().path_bytes();
		assert_eq!(
			&*raw_path,
			entry_path.as_bytes(),
			"fixture archive entry must carry the raw hostile path unmodified"
		);
		saw_hostile_entry = true;
	}
	assert!(saw_hostile_entry, "fixture archive must contain exactly the hostile entry");

	let artifact = zstd::stream::encode_all(tar_bytes.as_slice(), 3).unwrap();
	let hash = ContentHash::of_bytes(&artifact);
	(artifact, hash)
}
