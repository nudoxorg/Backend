//! put/get roundtrip, second-get hit, integrity, invalidate, generic L3.

use bytes::Bytes;
use cas::{Cas, ContentHash, DiskCas, MemoryCas, Tiered};

#[tokio::test]
async fn memory_put_get_roundtrip() {
	let cas = MemoryCas::new();
	let key = cas.put(Bytes::from_static(b"hello-ir")).await.unwrap();
	let got = cas.get(key).await.unwrap();
	assert_eq!(got.as_deref(), Some(b"hello-ir".as_slice()));

	// Second get is still a hit.
	let again = cas.get(key).await.unwrap();
	assert_eq!(again.as_deref(), Some(b"hello-ir".as_slice()));
}

#[tokio::test]
async fn disk_put_get_roundtrip() {
	let dir = tempfile::tempdir().unwrap();
	let cas = DiskCas::open(dir.path()).unwrap();
	let key = ContentHash::of_bytes(b"job-key-seed");
	assert!(cas.put_keyed(key, Bytes::from_static(b"ir-bytes")).await.unwrap());
	assert!(!cas.put_keyed(key, Bytes::from_static(b"ir-bytes")).await.unwrap());
	// First-write-wins: different payload does not replace.
	assert!(!cas.put_keyed(key, Bytes::from_static(b"other")).await.unwrap());
	assert_eq!(cas.get(key).await.unwrap().as_deref(), Some(b"ir-bytes".as_slice()));
}

#[tokio::test]
async fn disk_invalidate_allows_rewrite() {
	let dir = tempfile::tempdir().unwrap();
	let cas = DiskCas::open(dir.path()).unwrap();
	let key = ContentHash::of_bytes(b"poison");
	assert!(cas.put_keyed(key, Bytes::from_static(b"bad")).await.unwrap());
	cas.invalidate(key).await.unwrap();
	assert!(cas.put_keyed(key, Bytes::from_static(b"good")).await.unwrap());
	assert_eq!(cas.get(key).await.unwrap().as_deref(), Some(b"good".as_slice()));
}

#[tokio::test]
async fn disk_corrupt_envelope_is_integrity_error_and_removed() {
	let dir = tempfile::tempdir().unwrap();
	let cas = DiskCas::open(dir.path()).unwrap();
	let key = ContentHash::of_bytes(b"corrupt-me");
	assert!(cas.put_keyed(key, Bytes::from_static(b"ok")).await.unwrap());

	// Overwrite the blob with garbage that fails the blake3 envelope.
	let path = dir.path().join("cas").join(key.hex());
	std::fs::write(&path, b"not-an-envelope").unwrap();

	let err = cas.get(key).await.unwrap_err();
	assert!(matches!(err, cas::CasError::Integrity { .. }));
	assert!(!path.exists(), "corrupt blob must be deleted");
	assert_eq!(cas.get(key).await.unwrap(), None);
}

#[tokio::test]
async fn tiered_l1_hit_after_disk_promote() {
	let dir = tempfile::tempdir().unwrap();
	let disk = DiskCas::open(dir.path()).unwrap();
	let key = ContentHash::of_bytes(b"surface");
	disk.put_keyed(key, Bytes::from_static(b"postcard-ir")).await.unwrap();

	// Fresh tiered stack: cold L1, warm L2.
	let tiered = Tiered::with_disk(64, DiskCas::open(dir.path()).unwrap());
	let first = tiered.get(key).await.unwrap();
	assert_eq!(first.as_deref(), Some(b"postcard-ir".as_slice()));

	// Second get is L1; still returns the value.
	let second = tiered.get(key).await.unwrap();
	assert_eq!(second.as_deref(), Some(b"postcard-ir".as_slice()));
}

#[tokio::test]
async fn tiered_put_then_get() {
	let dir = tempfile::tempdir().unwrap();
	let cas = Tiered::with_disk(32, DiskCas::open(dir.path()).unwrap());
	let key = cas.put(Bytes::from_static(b"payload")).await.unwrap();
	assert_eq!(cas.get(key).await.unwrap().as_deref(), Some(b"payload".as_slice()));

	// Survive L1-only miss by reopening disk.
	let cold = Tiered::with_disk(32, DiskCas::open(dir.path()).unwrap());
	assert_eq!(cold.get(key).await.unwrap().as_deref(), Some(b"payload".as_slice()));
}

#[tokio::test]
async fn tiered_memory_only_first_write_wins_reports_novel() {
	let cas = Tiered::memory_only(16);
	let key = ContentHash::of_bytes(b"k");
	assert!(cas.put_keyed(key, Bytes::from_static(b"a")).await.unwrap());
	assert!(!cas.put_keyed(key, Bytes::from_static(b"b")).await.unwrap());
	assert_eq!(cas.get(key).await.unwrap().as_deref(), Some(b"a".as_slice()));
}

/// Verify that a real L3 backend wired via `Tiered::with_l3` participates in
/// read-through and promotion: a value in L3 only is served and promoted to
/// L1/L2 on the first read.
#[tokio::test]
async fn tiered_with_l3_promotes_on_read() {
	let l3 = MemoryCas::new();
	let key = ContentHash::of_bytes(b"l3-payload");
	l3.put_keyed(key, Bytes::from_static(b"from-l3")).await.unwrap();

	// L1 (16 cap), no L2, l3 = MemoryCas
	let tiered = Tiered::with_l3(16, None, l3);

	// Cold L1 — should fall through to L3 and return the value.
	let first = tiered.get(key).await.unwrap();
	assert_eq!(first.as_deref(), Some(b"from-l3".as_slice()), "L3 read-through failed");

	// Second get should hit L1 (promoted).
	let second = tiered.get(key).await.unwrap();
	assert_eq!(second.as_deref(), Some(b"from-l3".as_slice()), "L1 promotion failed");
}

/// Verify that a put to a Tiered stack with a live L3 writes through to L3.
#[tokio::test]
async fn tiered_with_l3_put_reaches_l3() {
	let dir = tempfile::tempdir().unwrap();
	let l3 = MemoryCas::new();
	let tiered = Tiered::with_l3(16, Some(DiskCas::open(dir.path()).unwrap()), l3);

	let key = tiered.put(Bytes::from_static(b"through-l3")).await.unwrap();
	assert_eq!(
		tiered.get(key).await.unwrap().as_deref(),
		Some(b"through-l3".as_slice()),
		"put-then-get through l3 failed"
	);
}

/// When the L3 (NoL3 sentinel) returns Unsupported, puts and gets must still
/// succeed using L1/L2 only.
#[tokio::test]
async fn tiered_with_no_l3_still_works() {
	let dir = tempfile::tempdir().unwrap();
	let cas = Tiered::new(16, Some(DiskCas::open(dir.path()).unwrap()), None::<cas::NoL3>);
	let key = ContentHash::of_bytes(b"with-l3-absent");
	assert!(cas.put_keyed(key, Bytes::from_static(b"v")).await.unwrap());
	assert_eq!(cas.get(key).await.unwrap().as_deref(), Some(b"v".as_slice()));
}

#[tokio::test]
async fn tiered_invalidate_clears_l1_and_l2() {
	let dir = tempfile::tempdir().unwrap();
	let cas = Tiered::with_disk(16, DiskCas::open(dir.path()).unwrap());
	let key = ContentHash::of_bytes(b"drop-me");
	cas.put_keyed(key, Bytes::from_static(b"old")).await.unwrap();
	cas.invalidate(key).await.unwrap();
	assert_eq!(cas.get(key).await.unwrap(), None);
	assert!(cas.put_keyed(key, Bytes::from_static(b"new")).await.unwrap());
	assert_eq!(cas.get(key).await.unwrap().as_deref(), Some(b"new".as_slice()));
}

/// Verify that `DynCas` erasure round-trips: a `Tiered<MemoryCas>` (with a
/// live L3) can be erased to `&dyn DynCas` and its put/get/invalidate all
/// reach the underlying tiers correctly. This is the same erasure that
/// `ForgeContext::cas()` performs.
#[tokio::test]
async fn dyn_cas_erasure_round_trips() {
	let l3 = MemoryCas::new();
	let tiered = Tiered::with_l3(16, None, l3);

	// Erase to `&dyn DynCas` — the same pointer `ForgeContext::cas()` returns.
	let erased: &dyn cas::DynCas = &tiered;

	let key = ContentHash::of_bytes(b"erased-payload");

	// Put through the erased pointer.
	cas::DynCas::put_keyed(erased, key, Bytes::from_static(b"erased-payload"))
		.await
		.unwrap();

	// Get through the erased pointer.
	let got = cas::DynCas::get(erased, key).await.unwrap();
	assert_eq!(got.as_deref(), Some(b"erased-payload".as_slice()), "DynCas get failed");

	// Invalidate through the erased pointer, then confirm it is gone.
	cas::DynCas::invalidate(erased, key).await.unwrap();
	let after = cas::DynCas::get(erased, key).await.unwrap();
	assert!(after.is_none(), "DynCas invalidate must remove the entry");
}

/// Verify that a `Tiered::with_l3` (with disk L2 + MemoryCas L3) erased to
/// `&dyn DynCas` correctly promotes from L3 on cold-L1 reads.
#[tokio::test]
async fn dyn_cas_tiered_l3_promote_via_erased_pointer() {
	let dir = tempfile::tempdir().unwrap();
	let l3 = MemoryCas::new();
	let key = ContentHash::of_bytes(b"l3-only");
	// Pre-seed L3 directly (before handing it to the Tiered).
	Cas::put_keyed(&l3, key, Bytes::from_static(b"l3-value")).await.unwrap();

	let tiered = Tiered::with_l3(16, Some(DiskCas::open(dir.path()).unwrap()), l3);
	let erased: &dyn cas::DynCas = &tiered;

	// Cold L1 + cold L2 — must fall through to L3 through the erased pointer.
	let got = cas::DynCas::get(erased, key).await.unwrap();
	assert_eq!(
		got.as_deref(),
		Some(b"l3-value".as_slice()),
		"DynCas erased pointer must promote from L3"
	);

	// Second read through the same erased pointer — now L1-hot.
	let hot = cas::DynCas::get(erased, key).await.unwrap();
	assert_eq!(hot.as_deref(), Some(b"l3-value".as_slice()), "L1 hit via erased pointer");
}
