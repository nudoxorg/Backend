#![allow(
    clippy::unwrap_used,
    reason = "These focused cache tests use fixed valid fixtures and assert the resulting behavior."
)]

use super::*;
use crate::{
    AuthorityIdentity, FlowSchema, Input, InputKind, InputManifest, InputManifestId,
    NativeRequestInput, ProfileSchema, SemanticBasisSchema, SessionKey, typed_of,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};

fn fixture() -> (AuthorityIdentity, InputManifestId, SessionKey) {
    let authority = AuthorityIdentity {
        producer: typed_of(b"producer"),
        toolchain: typed_of(b"toolchain"),
        contract: typed_of(b"contract"),
    };
    let manifest =
        InputManifest::new(vec![Input::new(InputKind::Source, "main", b"one").unwrap()]).unwrap();
    let key = SessionKey::new(
        authority,
        &manifest,
        typed_of::<ProfileSchema>(b"profile"),
        typed_of::<FlowSchema>(b"flow"),
        typed_of::<SemanticBasisSchema>(b"semantic"),
    );
    (authority, manifest.digest(), key)
}

#[test]
fn warm_prepare_reuses_the_same_arc_without_copying_request_body() {
    let (authority, manifest, session) = fixture();
    let key = PreparationKey::new(
        authority,
        manifest,
        session,
        1,
        "rust",
        &[NativeRequestInput::new("main", b"one").unwrap()],
    )
    .unwrap();
    let cache = PreparationCache::new(PreparationCacheConfig::new(4, 4096).unwrap());
    let first = cache
        .prepare(
            key,
            "rust",
            vec![NativeRequestInput::new("main", b"one").unwrap()],
        )
        .unwrap();
    let pointer = first.bytes().as_ptr();
    let first_shared = first.shared_bytes();
    drop(first);
    let second = cache
        .prepare(
            key,
            "rust",
            vec![NativeRequestInput::new("main", b"one").unwrap()],
        )
        .unwrap();
    assert_eq!(pointer, second.bytes().as_ptr());
    assert!(Arc::ptr_eq(&first_shared, &second.shared_bytes()));
    assert_eq!(cache.stats().hits, 1);
    assert_eq!(cache.stats().misses, 1);
}

#[test]
fn request_identity_rejects_language_or_input_aliases() {
    let (authority, manifest, session) = fixture();
    let key = PreparationKey::new(
        authority,
        manifest,
        session,
        1,
        "rust",
        &[NativeRequestInput::new("main", b"one").unwrap()],
    )
    .unwrap();
    let cache = PreparationCache::new(PreparationCacheConfig::new(4, 4096).unwrap());
    assert!(matches!(
        cache.prepare(key, "swift", Vec::new()),
        Err(PreparationError::KeyMismatch)
    ));
    assert!(matches!(
        cache.prepare(
            key,
            "rust",
            vec![NativeRequestInput::new("main", b"two").unwrap()]
        ),
        Err(PreparationError::KeyMismatch)
    ));
}

#[test]
fn lazy_warm_prepare_does_not_build_inputs() {
    let (authority, manifest, session) = fixture();
    let input = NativeRequestInput::new("main", b"one").unwrap();
    let key = PreparationKey::new(
        authority,
        manifest,
        session,
        1,
        "rust",
        std::slice::from_ref(&input),
    )
    .unwrap();
    let cache = PreparationCache::new(PreparationCacheConfig::new(4, 4096).unwrap());
    cache.prepare(key, "rust", vec![input]).unwrap();
    let built = Arc::new(AtomicBool::new(false));
    let marker = Arc::clone(&built);
    cache
        .prepare_with(key, "rust", move || {
            marker.store(true, Ordering::Relaxed);
            vec![NativeRequestInput::new("main", b"wrong").unwrap()]
        })
        .unwrap();
    assert!(!built.load(Ordering::Relaxed));
}

#[test]
fn concurrent_miss_encodes_once() {
    let (authority, manifest, session) = fixture();
    let input = NativeRequestInput::new("main", b"one").unwrap();
    let key = PreparationKey::new(
        authority,
        manifest,
        session,
        1,
        "rust",
        std::slice::from_ref(&input),
    )
    .unwrap();
    let cache = Arc::new(PreparationCache::new(
        PreparationCacheConfig::new(4, 4096).unwrap(),
    ));
    let barrier = Arc::new(Barrier::new(8));
    let mut workers = Vec::new();
    for _ in 0..8 {
        let cache = Arc::clone(&cache);
        let barrier = Arc::clone(&barrier);
        let input = input.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            let prepared = cache.prepare(key, "rust", vec![input]).unwrap();
            assert!(!prepared.bytes().is_empty());
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(cache.stats().misses, 1);
}

#[test]
fn revocation_and_generation_evict_without_decoding() {
    let (authority, manifest, session) = fixture();
    let cache = PreparationCache::new(PreparationCacheConfig::new(1, 4096).unwrap());
    let first = PreparationKey::new(authority, manifest, session, 1, "rust", &[]).unwrap();
    let second = PreparationKey::new(authority, manifest, session, 2, "rust", &[]).unwrap();
    let _ = cache.prepare(first, "rust", Vec::new()).unwrap();
    let _ = cache.prepare(second, "rust", Vec::new()).unwrap();
    assert!(cache.borrow(first).is_none());
    assert_eq!(cache.stats().evictions, 1);
    assert_eq!(cache.invalidate_toolchain(authority.toolchain), 1);
    assert!(cache.borrow(second).is_none());
}

#[test]
fn mismatched_versions_cannot_enter_cache() {
    let (authority, _manifest, session) = fixture();
    let other = InputManifest::new(vec![
        Input::new(InputKind::Source, "other", b"two").unwrap(),
    ])
    .unwrap();
    assert_eq!(
        PreparationKey::new(authority, other.digest(), session, 1, "rust", &[]),
        Err(PreparationError::KeyMismatch)
    );
    assert_eq!(
        PreparationCacheConfig::new(0, 1),
        Err(PreparationError::Capacity)
    );
    assert_eq!(
        PreparationCacheConfig::new(1, 0),
        Err(PreparationError::Capacity)
    );
}
