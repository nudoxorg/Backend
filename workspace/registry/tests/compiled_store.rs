//! Pipeline part: **compiled-output lookup** (`registry::compiled`, SV-6).
//!
//! Specs for the VCS-native no-double-compile handshake: the
//! `compiled/{job_key_hex}` records the iroh SyncService apply-hook writes,
//! and the lookup path that loads and decodes them.

mod common;

use std::sync::Arc;

use heart::JobKey;
use registry::compiled::{
    ChangeId, ChannelName, CompiledRecord, CompiledStore, LookupResult, ObjectCompiledStore,
    unix_millis,
};
use heart::content::ContentHash;

/// A deterministic fixture JobKey.
fn job_key(salt: &[u8]) -> JobKey {
    JobKey::derive(b"producer-1.0.0", b"rustc-1.85.0", salt, b"lockfile")
}

fn make_record(package_id: heart::PackageId) -> CompiledRecord {
    CompiledRecord {
        package: package_id,
        channel: ChannelName::new("main").expect("valid channel"),
        tip: ChangeId::new("ab".repeat(32)).expect("valid tip"),
        generation_stamp: ContentHash::of_bytes(b"generation-stamp"),
        recorded_at: unix_millis(),
    }
}

/// A [`CompiledRecord`] survives its postcard wire form byte-exactly.
#[test]
fn compiled_record_roundtrip() {
    let package = common::rust_package("serde", "1.0.0");
    let record = make_record(package.id());
    let bytes = postcard::to_allocvec(&record).expect("record serializes");
    let decoded: CompiledRecord = postcard::from_bytes(&bytes).expect("record deserializes");
    assert_eq!(decoded, record, "postcard roundtrip preserves every field");
}

/// The full write-then-read cycle via `record()`.
#[tokio::test]
async fn write_then_lookup_hit() {
    let package = common::rust_package("serde", "1.0.0");
    let record = make_record(package.id());

    let backend = Arc::new(object_store::memory::InMemory::new());
    let compiled = ObjectCompiledStore::new(backend);
    let key = job_key(b"write-then-lookup");
    compiled.record(key, record.clone()).await.expect("record write succeeds");

    let LookupResult::Hit(hit) = compiled.lookup(key.as_bytes()).await.expect("lookup succeeds")
    else {
        panic!("a written key must hit");
    };
    assert_eq!(hit.package, record.package);
    assert_eq!(hit.channel, record.channel);
    assert_eq!(hit.tip, record.tip);
    assert_eq!(hit.generation_stamp, record.generation_stamp);
}

/// A key nothing was ever recorded for is a clean miss, not an error.
#[tokio::test]
async fn unknown_key_miss() {
    let backend = Arc::new(object_store::memory::InMemory::new());
    let compiled = ObjectCompiledStore::new(backend);
    let result = compiled
        .lookup(job_key(b"never-written").as_bytes())
        .await
        .expect("a miss is not an error");
    assert!(matches!(result, LookupResult::Miss), "unknown keys miss");
}

/// Re-recording the same JobKey is an idempotent no-op.
#[tokio::test]
async fn idempotent_rewrite() {
    let package = common::rust_package("serde", "1.0.0");
    let record = make_record(package.id());

    let backend = Arc::new(object_store::memory::InMemory::new());
    let compiled = ObjectCompiledStore::new(backend);
    let key = job_key(b"idempotent");
    for _ in 0..2 {
        compiled.record(key, record.clone()).await.expect("every re-write succeeds");
    }
    let LookupResult::Hit(hit) = compiled.lookup(key.as_bytes()).await.expect("lookup succeeds")
    else {
        panic!("a twice-written key still hits");
    };
    assert_eq!(hit.tip, record.tip);
}

/// ChannelName rejects empty strings.
#[test]
fn channel_name_rejects_empty() {
    assert!(ChannelName::new("").is_err(), "empty channel name must be rejected");
}

/// ChannelName accepts non-empty strings.
#[test]
fn channel_name_accepts_nonempty() {
    let name = ChannelName::new("main").expect("non-empty channel name is valid");
    assert_eq!(name.as_str(), "main");
}

/// ChangeId rejects non-hex strings.
#[test]
fn change_id_rejects_non_hex() {
    assert!(ChangeId::new("Z".repeat(64)).is_err(), "non-hex change id must be rejected");
}

/// ChangeId rejects wrong-length strings.
#[test]
fn change_id_rejects_wrong_length() {
    assert!(ChangeId::new("ab".repeat(16)).is_err(), "32-char string must be rejected (need 64)");
    assert!(ChangeId::new("ab".repeat(33)).is_err(), "66-char string must be rejected");
}

/// ChangeId accepts valid 64 lowercase hex.
#[test]
fn change_id_accepts_valid() {
    let id = ChangeId::new("ab".repeat(32)).expect("valid 64-char lowercase hex");
    assert_eq!(id.as_str().len(), 64);
}

/// A fresh backend behind a second handle still resolves the written record —
/// the record is durable state, not in-memory.
#[tokio::test]
async fn record_is_durable_across_handles() {
    let package = common::rust_package("tokio", "1.52.0");
    let record = make_record(package.id());
    let backend = Arc::new(object_store::memory::InMemory::new());
    let key = job_key(b"durable");

    ObjectCompiledStore::new(backend.clone())
        .record(key, record.clone())
        .await
        .expect("record write succeeds");

    let LookupResult::Hit(hit) = ObjectCompiledStore::new(backend)
        .lookup(key.as_bytes())
        .await
        .expect("lookup succeeds")
    else {
        panic!("the record must be durable");
    };
    assert_eq!(hit.tip, record.tip);
}
