//! put/get roundtrip, second-get hit, integrity, invalidate, L3 stub.

use bytes::Bytes;
use cas::{Cas, ContentHash, DiskCas, MemoryCas, RegistryCas, Tiered};

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

#[tokio::test]
async fn tiered_with_stub_l3_still_gets_and_puts() {
	let dir = tempfile::tempdir().unwrap();
	let cas = Tiered::new(16, Some(DiskCas::open(dir.path()).unwrap()), Some(RegistryCas::stub()));
	let key = ContentHash::of_bytes(b"with-l3-stub");
	// Unsupported L3 must not fail the put.
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
