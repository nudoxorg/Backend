//! put/get roundtrip and second-get hit.

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
	assert_eq!(cas.get(key).await.unwrap().as_deref(), Some(b"ir-bytes".as_slice()));
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
