//! Round-trip tests for [`registry::StoreCas`] — the L3 CAS adapter over the
//! registry object store.
//!
//! Uses an in-memory `object_store::memory::InMemory` backend so these tests
//! are hermetic (no external services required). Two independent keys are
//! exercised to confirm no cross-key interference.

mod common;

use std::sync::Arc;

use bytes::Bytes;
use cas::Cas;
use heart::Connect;
use object_store::memory::InMemory;
use registry::{Store, StoreCas};

/// Builds a `StoreCas` over a fresh in-memory object store backend.
async fn memory_store_cas() -> StoreCas {
    let backend = Arc::new(InMemory::new());
    let store = Store::new(backend)
        .connect()
        .await
        .expect("in-memory backend answers the sentinel probe");
    StoreCas::new(Arc::new(store))
}

/// A byte value round-trips through `StoreCas::put` → `StoreCas::get`.
#[tokio::test]
async fn store_cas_put_then_get_roundtrips() {
    let cas = memory_store_cas().await;

    let payload = Bytes::from_static(b"store-cas-roundtrip-payload");
    let key = cas.put(payload.clone()).await.expect("put succeeds");

    let got = cas.get(key).await.expect("get succeeds");
    assert_eq!(
        got.as_deref(),
        Some(payload.as_ref()),
        "retrieved bytes must equal the stored bytes"
    );
}

/// Two distinct keys are stored independently — no cross-contamination.
#[tokio::test]
async fn store_cas_two_keys_are_independent() {
    let cas = memory_store_cas().await;

    let a = Bytes::from_static(b"store-cas-key-a");
    let b = Bytes::from_static(b"store-cas-key-b");

    let key_a = cas.put(a.clone()).await.expect("put a succeeds");
    let key_b = cas.put(b.clone()).await.expect("put b succeeds");
    assert_ne!(key_a, key_b, "different content must produce different keys");

    assert_eq!(cas.get(key_a).await.unwrap().as_deref(), Some(a.as_ref()), "key a must return a");
    assert_eq!(cas.get(key_b).await.unwrap().as_deref(), Some(b.as_ref()), "key b must return b");
}

/// A key that was never stored returns `Ok(None)` (clean miss).
#[tokio::test]
async fn store_cas_get_missing_key_returns_none() {
    let cas = memory_store_cas().await;
    let missing = cas::ContentHash::of_bytes(b"never-stored");
    let result = cas.get(missing).await.expect("missing key must not error");
    assert_eq!(result, None, "a missing key must yield Ok(None)");
}

/// `put_keyed` is idempotent: re-putting under the same key returns `false`
/// (first-write-wins).
#[tokio::test]
async fn store_cas_put_keyed_is_first_write_wins() {
    let cas = memory_store_cas().await;

    let payload = Bytes::from_static(b"fww-payload");
    let key = cas::ContentHash::of_bytes(&payload);

    assert!(
        cas.put_keyed(key, payload.clone()).await.expect("first put_keyed"),
        "first write must return true (novel)"
    );
    assert!(
        !cas.put_keyed(key, payload.clone()).await.expect("second put_keyed"),
        "second write of same key must return false (already existed)"
    );
}

/// `invalidate` is a no-op (documented behaviour): the key is still readable
/// after calling invalidate, because the object store has no per-key delete.
#[tokio::test]
async fn store_cas_invalidate_is_noop() {
    let cas = memory_store_cas().await;

    let payload = Bytes::from_static(b"invalidate-noop");
    let key = cas.put(payload.clone()).await.expect("put succeeds");

    // Invalidate should not error and should not remove the entry.
    cas.invalidate(key).await.expect("invalidate must not error");

    let still_there = cas.get(key).await.expect("get after invalidate succeeds");
    assert_eq!(
        still_there.as_deref(),
        Some(payload.as_ref()),
        "invalidate is documented as a no-op; data must remain readable"
    );
}
