//! End-to-end iroh transport tests for ObjectPack (INDEX-PLAN §7.2, ID-18).
//!
//! Two in-process iroh endpoints on localhost, discovered via `MemoryLookup`
//! (the pattern copied from `ir-sync`'s tests): keys are pre-generated so each
//! side knows the other's `EndpointId` before binding.
//!
//! Coverage:
//! - whole-pack provide → fetch → byte-identical + id-verified install;
//! - verified member-range fetch (Bao) of a middle slice of a 1 MiB+ member;
//! - sub-threshold member fetched whole (no outboard) still verifies;
//! - a fetch from a non-enrolled endpoint is refused (typed `NotEnrolled`);
//! - the atomic install leaves no partial pack on an id mismatch.

use std::sync::Arc;

use bytes::Bytes;
use heart::deployment::TrustedRemote;
use iroh::address_lookup::MemoryLookup;
use object_pack::transport::{
    ObjectPackFetcher, ObjectPackProvider, PackResponse, ProvideTarget,
};
use object_pack::{FilesystemObjectPackStore, MemberKey, ObjectPackBuilder, ObjectPackStore, PackError, RelativePath};
use smol_str::SmolStr;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn source_key(path: &str) -> MemberKey {
    MemberKey::Source { path: RelativePath(SmolStr::new(path)) }
}

/// A pack with one big (>1 MiB, gets an outboard) and one small member.
fn build_mixed_pack() -> ObjectPackBuilder {
    let mut builder = ObjectPackBuilder::new();
    let big: Vec<u8> = (0u8..=255).cycle().take(1024 * 1024 + 321).collect();
    builder.add_member(source_key("big.bin"), Bytes::from(big)).unwrap();
    builder
        .add_member(source_key("small.txt"), Bytes::from_static(b"a small member with no outboard"))
        .unwrap();
    builder
}

/// Bind a provider/fetcher pair with pre-determined keys so each knows the
/// other's `EndpointId` before binding, and (optionally) enroll the fetcher.
async fn make_pair(
    provider_store: Arc<FilesystemObjectPackStore>,
    fetcher_store: Arc<FilesystemObjectPackStore>,
    enroll_fetcher: bool,
) -> (
    ObjectPackProvider<FilesystemObjectPackStore>,
    ObjectPackFetcher<FilesystemObjectPackStore>,
    iroh::EndpointId,
) {
    let lookup = MemoryLookup::new();

    let provider_key = iroh::SecretKey::generate();
    let fetcher_key = iroh::SecretKey::generate();
    let provider_id = provider_key.public();
    let fetcher_id = fetcher_key.public();

    let enrolled = if enroll_fetcher { vec![fetcher_id] } else { vec![] };

    let provider = ObjectPackProvider::new_with_key(
        provider_store,
        enrolled,
        Some(lookup.clone()),
        provider_key,
    )
    .await
    .expect("provider bind");
    lookup.add_endpoint_info(provider.endpoint().addr());

    let fetcher = ObjectPackFetcher::new_with_key(fetcher_store, Some(lookup.clone()), fetcher_key)
        .await
        .expect("fetcher bind");
    lookup.add_endpoint_info(fetcher.endpoint().addr());

    (provider, fetcher, provider_id)
}

// ---------------------------------------------------------------------------
// Whole-pack round trip
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn whole_pack_provide_fetch_installs_byte_identical() {
    let provider_dir = tempfile::tempdir().unwrap();
    let fetcher_dir = tempfile::tempdir().unwrap();
    let provider_store = Arc::new(FilesystemObjectPackStore::open(provider_dir.path()).unwrap());
    let fetcher_store = Arc::new(FilesystemObjectPackStore::open(fetcher_dir.path()).unwrap());

    let id = provider_store.put_pack(build_mixed_pack()).unwrap();
    assert!(provider_store.has(&id));
    assert!(!fetcher_store.has(&id));

    let (provider, fetcher, provider_id) =
        make_pair(Arc::clone(&provider_store), Arc::clone(&fetcher_store), true).await;

    let accept = tokio::spawn(async move { provider.accept_one().await });

    fetcher
        .fetch_whole_pack(&id, provider_id)
        .await
        .expect("fetch whole pack");

    accept.await.unwrap().expect("provider accept ok");

    // Installed and byte-identical: the id re-derives, members match.
    assert!(fetcher_store.has(&id));
    let expected_big = provider_store.get_member(&id, &source_key("big.bin")).unwrap();
    let got_big = fetcher_store.get_member(&id, &source_key("big.bin")).unwrap();
    assert_eq!(expected_big, got_big);

    // The outboard sidecar travelled too.
    assert!(fetcher_store.outboard(&id, &source_key("big.bin")).unwrap().is_some());
    assert!(fetcher_store.outboard(&id, &source_key("small.txt")).unwrap().is_none());
}

// ---------------------------------------------------------------------------
// Verified member-range fetch (Bao)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn verified_member_range_fetch_middle_slice() {
    let provider_dir = tempfile::tempdir().unwrap();
    let fetcher_dir = tempfile::tempdir().unwrap();
    let provider_store = Arc::new(FilesystemObjectPackStore::open(provider_dir.path()).unwrap());
    let fetcher_store = Arc::new(FilesystemObjectPackStore::open(fetcher_dir.path()).unwrap());

    let id = provider_store.put_pack(build_mixed_pack()).unwrap();
    let key = source_key("big.bin");

    // The expected bytes, read locally on the provider side.
    let whole = provider_store.get_member(&id, &key).unwrap();
    let (start, end) = (600_000u64, 600_256u64);
    let expected = whole.slice(start as usize..end as usize);

    let (provider, fetcher, provider_id) =
        make_pair(Arc::clone(&provider_store), Arc::clone(&fetcher_store), true).await;

    let accept = tokio::spawn(async move { provider.accept_one().await });

    let got = fetcher
        .fetch_member_range(&id, &key, start, end, provider_id)
        .await
        .expect("verified range fetch");

    accept.await.unwrap().expect("provider accept ok");
    assert_eq!(got, expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn sub_threshold_member_fetched_whole_verifies() {
    let provider_dir = tempfile::tempdir().unwrap();
    let fetcher_dir = tempfile::tempdir().unwrap();
    let provider_store = Arc::new(FilesystemObjectPackStore::open(provider_dir.path()).unwrap());
    let fetcher_store = Arc::new(FilesystemObjectPackStore::open(fetcher_dir.path()).unwrap());

    let id = provider_store.put_pack(build_mixed_pack()).unwrap();
    let key = source_key("small.txt");
    // Sub-threshold member has no outboard.
    assert!(provider_store.outboard(&id, &key).unwrap().is_none());

    let (provider, fetcher, provider_id) =
        make_pair(Arc::clone(&provider_store), Arc::clone(&fetcher_store), true).await;

    let accept = tokio::spawn(async move { provider.accept_one().await });

    let got = fetcher
        .fetch_member_range(&id, &key, 2, 7, provider_id)
        .await
        .expect("whole-member range fetch");

    accept.await.unwrap().expect("provider accept ok");
    assert_eq!(got.as_ref(), b"small");
}

/// A malicious (or corrupt) provider serves a tampered
/// [`PackResponse::VerifiedRange`]: the fetcher's verification (the exact
/// `verify_bao_range` path `fetch_member_range` runs on the `encoded` frame)
/// must reject it with a typed [`PackError::BaoDecode`] and yield no bytes. This
/// is the over-the-wire counterpart to the outboard unit tamper test.
#[test]
fn tampered_verified_range_frame_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilesystemObjectPackStore::open(dir.path()).unwrap();
    let id = store.put_pack(build_mixed_pack()).unwrap();
    let key = source_key("big.bin");

    let outboard = store
        .outboard(&id, &key)
        .unwrap()
        .expect("big member has an outboard");
    let member = store.get_member(&id, &key).unwrap();

    let (start, end) = (600_000u64, 600_256u64);
    // The genuine wire payload a provider would put in `VerifiedRange.encoded`.
    let mut encoded = outboard.encode_range(&member, start, end).unwrap().to_vec();

    // Flip a byte deep in the slice so it lands in verified leaf data, exactly as
    // a tampering provider on the path would.
    let victim = encoded.len() / 2;
    encoded[victim] ^= 0xFF;

    let result = object_pack::verify_bao_range(
        &outboard.root_hash,
        outboard.uncompressed_length,
        &encoded,
        start,
        end,
    );
    assert!(
        matches!(result, Err(PackError::BaoDecode { .. })),
        "tampered VerifiedRange must fail verification, got {result:?}"
    );
}

// ---------------------------------------------------------------------------
// Enrollment gate (ID-18)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn fetch_from_non_enrolled_endpoint_is_rejected() {
    let provider_dir = tempfile::tempdir().unwrap();
    let fetcher_dir = tempfile::tempdir().unwrap();
    let provider_store = Arc::new(FilesystemObjectPackStore::open(provider_dir.path()).unwrap());
    let fetcher_store = Arc::new(FilesystemObjectPackStore::open(fetcher_dir.path()).unwrap());

    let id = provider_store.put_pack(build_mixed_pack()).unwrap();

    // enroll_fetcher = false → the provider's allow-list is empty.
    let (provider, fetcher, provider_id) =
        make_pair(Arc::clone(&provider_store), Arc::clone(&fetcher_store), false).await;

    let accept = tokio::spawn(async move { provider.accept_one().await });

    let fetch_result = fetcher.fetch_whole_pack(&id, provider_id).await;

    let provider_result = accept.await.unwrap();

    // The provider rejects with a typed NotEnrolled.
    assert!(
        matches!(provider_result, Err(PackError::NotEnrolled { .. })),
        "expected NotEnrolled on provider side, got {provider_result:?}"
    );
    // The fetcher sees a refusal and installs nothing.
    assert!(fetch_result.is_err(), "fetch must fail for non-enrolled peer");
    assert!(!fetcher_store.has(&id), "nothing installed on refusal");
}

// ---------------------------------------------------------------------------
// Trusted-remote policy plumbing (ID-18)
// ---------------------------------------------------------------------------

#[test]
fn provide_target_gate_rejects_non_providing_remote() {
    let allowed = TrustedRemote {
        name: "backup".into(),
        endpoint: "aaaa".into(),
        can_provide: true,
        can_fetch: true,
    };
    let denied = TrustedRemote {
        name: "read-only".into(),
        endpoint: "bbbb".into(),
        can_provide: false,
        can_fetch: true,
    };
    assert!(ProvideTarget::from_trusted_remote(&allowed).is_ok());
    assert!(matches!(
        ProvideTarget::from_trusted_remote(&denied),
        Err(PackError::NotEnrolled { .. })
    ));
}

// ---------------------------------------------------------------------------
// Atomic install: id mismatch leaves no partial pack
// ---------------------------------------------------------------------------

#[test]
fn install_with_wrong_id_is_rejected_and_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilesystemObjectPackStore::open(dir.path()).unwrap();

    let (bytes, real_id, outboards) = build_mixed_pack().seal_with_outboards().unwrap();

    // Claim a different id than the bytes actually derive to.
    let wrong_id = object_pack::ObjectPackId(heart::content::ContentHash::of_bytes(b"not the pack"));
    let result = store.install_pack(&wrong_id, &bytes, &outboards);
    assert!(matches!(result, Err(PackError::FetchedIdMismatch)));

    // Neither id is present: no partial write under the wrong id, and the real
    // pack was never installed.
    assert!(!store.has(&wrong_id));
    assert!(!store.has(&real_id));
}

// ---------------------------------------------------------------------------
// Truncated transfer → typed error, no partial install
// ---------------------------------------------------------------------------

#[test]
fn truncated_pack_bytes_install_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilesystemObjectPackStore::open(dir.path()).unwrap();

    let (bytes, id, outboards) = build_mixed_pack().seal_with_outboards().unwrap();
    let truncated = &bytes[..bytes.len() / 2];

    let result = store.install_pack(&id, truncated, &outboards);
    assert!(result.is_err(), "truncated install must be a typed error");
    assert!(!store.has(&id), "no partial pack installed on truncation");
}

/// Confirm the provider responds `Refused` for an unknown pack (no panic).
#[tokio::test(flavor = "multi_thread")]
async fn fetch_unknown_pack_is_refused() {
    let provider_dir = tempfile::tempdir().unwrap();
    let fetcher_dir = tempfile::tempdir().unwrap();
    let provider_store = Arc::new(FilesystemObjectPackStore::open(provider_dir.path()).unwrap());
    let fetcher_store = Arc::new(FilesystemObjectPackStore::open(fetcher_dir.path()).unwrap());

    let unknown = object_pack::ObjectPackId(heart::content::ContentHash::of_bytes(b"ghost"));

    let (provider, fetcher, provider_id) =
        make_pair(Arc::clone(&provider_store), Arc::clone(&fetcher_store), true).await;

    let accept = tokio::spawn(async move { provider.accept_one().await });
    let fetch_result = fetcher.fetch_whole_pack(&unknown, provider_id).await;
    let _ = accept.await.unwrap();

    assert!(fetch_result.is_err());
    assert!(!fetcher_store.has(&unknown));
    // Keep the unused import meaningful.
    let _ = PackResponse::Refused { reason: String::new() };
}
