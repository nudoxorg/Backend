//! Owner-side, fenced publication of one exact selected compiler closure to S3.
//!
//! The local CAS remains the admission authority. This module streams its checked closure
//! members into the storage-neutral pack format, uploads every pack with short-lived SigV4
//! capabilities, and durably records the complete pack set before the semantic head can move.
//! S3 mode is enabled with `BACKEND_S3_ENDPOINT`, `BACKEND_S3_BUCKET`, `BACKEND_S3_REGION`,
//! `BACKEND_S3_ACCESS_KEY_ID`, and `BACKEND_S3_SECRET_ACCESS_KEY`; session credentials may also
//! set `BACKEND_S3_SESSION_TOKEN`. `BACKEND_S3_PREFIX` accepts only slash-separated unreserved
//! path components and must end in `/`. With all S3 settings absent, publication stays local.

use std::{
    collections::BTreeMap,
    env, fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use backend_store::{
    ArtifactClosureClaim, ArtifactObjectClaim, ArtifactObjectReader, ClosureId, FileStore,
    ObjectId, UntrustedObjectId,
};
use backend_store_s3::{
    ImmutableS3Pack, MAX_PACK_BYTES, MAX_PACK_MANIFEST_BYTES, MAX_PACK_OBJECTS, PackReadCapability,
    PackUploadCapability, S3Endpoint, S3LayoutId, S3ObjectRoute, S3PackBuilder, S3PackId,
    S3PackRoute, S3RouteConfig, StoredPackReceipt, WorkFence,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

const ENDPOINT_ENV: &str = "BACKEND_S3_ENDPOINT";
const BUCKET_ENV: &str = "BACKEND_S3_BUCKET";
const REGION_ENV: &str = "BACKEND_S3_REGION";
const ACCESS_KEY_ENV: &str = "BACKEND_S3_ACCESS_KEY_ID";
const SECRET_KEY_ENV: &str = "BACKEND_S3_SECRET_ACCESS_KEY";
const SESSION_TOKEN_ENV: &str = "BACKEND_S3_SESSION_TOKEN";
const PREFIX_ENV: &str = "BACKEND_S3_PREFIX";
const RECEIPT_MAGIC: &[u8; 8] = b"BKS3CL01";
const RECEIPT_VERSION: u16 = 2;
const MAX_RECEIPT_PACKS: usize = 65_535;
const MAX_RECEIPT_OBJECTS: usize = 100_002;
const MAX_RECEIPT_BYTES: usize = 24 * 1024 * 1024;
const MAX_PRESIGN_SECONDS: u64 = 15 * 60;
const PACK_DATA_BUDGET: u64 =
    MAX_PACK_BYTES - MAX_PACK_MANIFEST_BYTES as u64 - (MAX_PACK_OBJECTS as u64 * 128);
const CLOSURE_PAGE_IDS: usize = 128;
static NEXT_RECEIPT_TEMP: AtomicU64 = AtomicU64::new(0);

type HmacSha256 = Hmac<Sha256>;

#[path = "s3_publication/config.rs"]
mod config;
#[path = "s3_publication/hydration.rs"]
mod hydration;
#[path = "s3_publication/publication.rs"]
mod publication;
#[path = "s3_publication/receipt.rs"]
mod receipt;
#[path = "s3_publication/signing.rs"]
mod signing;

use config::valid_prefix;
use receipt::{closure_membership_digest, pack_membership_digest};
use signing::{aws_encode, aws_timestamp, canonical_query, hex};

#[derive(Debug)]
pub(super) enum PublicationError {
    Configuration,
    Remote,
    RemoteHydration {
        operation: RemoteHydrationOperation,
        source: Option<backend_store_s3::RemoteStoreError>,
    },
    Store,
    Receipt,
    ReceiptIo,
}

#[derive(Debug)]
pub(super) enum RemoteHydrationOperation {
    OpenPack,
    FetchObject,
    OpenEnvelope,
    SeekPayload,
    ReadPayload,
}

impl fmt::Display for PublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Configuration => "S3 publication configuration is incomplete or invalid",
            Self::Remote | Self::RemoteHydration { .. } => "S3 publication failed",
            Self::Store => "selected closure could not be read or streamed",
            Self::Receipt => {
                "selected closure did not match the complete set of stored S3 pack receipts"
            }
            Self::ReceiptIo => "S3 closure receipt could not be durably recorded",
        })
    }
}

impl std::error::Error for PublicationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RemoteHydration {
                source: Some(source),
                ..
            } => Some(source),
            _ => None,
        }
    }
}

/// Optional configured S3 pack publisher. An absent configuration keeps the local CAS path.
pub(super) struct S3ClosurePublisher {
    route: S3PackRoute,
    endpoint: String,
    bucket: String,
    region: String,
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
    prefix: String,
    receipt_root: PathBuf,
    receipt_cache: Mutex<Option<(ReceiptCacheKey, Arc<ExactS3ClosureReceipt>)>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReceiptCacheKey {
    closure: [u8; 32],
    attempt_id: [u8; 16],
    epoch: u64,
    attempt_fence: [u8; 32],
}

/// Exact Turso candidate authority carried to owner-side S3 publication.
///
/// Remote publications retain the real cluster assignment scope. Local compiler
/// publications retain the Turso candidate attempt directly and have no worker
/// assignment. `storage_fence` adapts either identity to the canonical pack
/// protocol's shared fencing fields; for a local candidate its `work_id` is the
/// actual Turso attempt ID, not a fabricated worker work ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PublicationFence {
    namespace_id: [u8; 16],
    attempt_id: [u8; 16],
    epoch: u64,
    attempt_fence: [u8; 32],
    input_digest: [u8; 32],
    base_generation: u64,
    base_root: Option<[u8; 32]>,
    assignment: Option<backend_engine::cluster_transport::AssignmentScope>,
}

/// The owner/index side of one selected closure's physical remote storage proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ExactS3ClosureReceipt {
    closure: [u8; 32],
    target_root: [u8; 32],
    publication_fence: PublicationFence,
    fence: WorkFence,
    object_count: u64,
    payload_bytes: u64,
    membership_digest: [u8; 32],
    object_ids: Vec<[u8; 32]>,
    packs: Vec<PackReceiptSummary>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PackReceiptSummary {
    pack_id: [u8; 32],
    layout_id: [u8; 32],
    pack_bytes: u64,
    manifest_bytes: u32,
    object_count: u32,
    sha256: [u8; 32],
    manifest_sha256: [u8; 32],
    member_digest: [u8; 32],
}

/// Small exact selected-generation identity needed to reopen its local receipt.
/// This deliberately omits assignment data: that remains in the durable receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RemoteClosureSelection {
    pub(super) closure: [u8; 32],
    pub(super) target_root: [u8; 32],
    pub(super) candidate_id: [u8; 32],
    pub(super) namespace_id: [u8; 16],
    pub(super) generation: u64,
    pub(super) attempt_id: [u8; 16],
    pub(super) epoch: u64,
    pub(super) attempt_fence: [u8; 32],
    pub(super) input_digest: [u8; 32],
}

/// Owner-minted allowlist of the only closure members with a current remote
/// read path: versioned semantic-plane segments. Its fields stay private so
/// callers cannot turn an arbitrary object ID into a GC eviction exception.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct VerifiedRemoteSegmentSet {
    closure: ClosureId,
    object_ids: Vec<[u8; 32]>,
}

impl VerifiedRemoteSegmentSet {
    pub(super) const fn closure(&self) -> ClosureId {
        self.closure
    }

    pub(super) fn object_ids(&self) -> &[[u8; 32]] {
        &self.object_ids
    }
}

impl RemoteClosureSelection {
    pub(super) fn from_selected(selected: &backend_extension_turso::SelectedGeneration) -> Self {
        let (attempt_id, epoch) = selected.attempt();
        let (_, attempt_fence) = selected.scheduler_fence();
        Self {
            closure: *selected.closure_id(),
            target_root: *selected.target_root(),
            candidate_id: *selected.candidate_id(),
            namespace_id: selected.namespace().namespace_id(),
            generation: selected.generation(),
            attempt_id: *attempt_id,
            epoch,
            attempt_fence,
            input_digest: *selected.input_digest(),
        }
    }
}

pub(super) trait SelectedClosurePublisher: Send + Sync {
    fn publish_closure(
        &self,
        store: &FileStore,
        closure: ClosureId,
        target_root: [u8; 32],
        expected_count: u64,
        budget: backend_store::ArtifactBudget,
        publication_fence: PublicationFence,
    ) -> Result<ExactS3ClosureReceipt, PublicationError>;

    fn hydrate_object(
        &self,
        store: &FileStore,
        selected: RemoteClosureSelection,
        object_id: UntrustedObjectId,
        expected_schema: backend_version::SchemaIdentity,
        expected_payload_len: u64,
    ) -> Result<Vec<u8>, PublicationError>;

    /// Whether this exact live Turso generation has a fresh, durable owner
    /// receipt proving its complete closure was stored remotely. GC may use a
    /// remote-backed index root only after this check succeeds.
    fn has_durable_selected_closure(
        &self,
        store: &FileStore,
        selected: RemoteClosureSelection,
    ) -> Result<bool, PublicationError>;

    /// Returns a private allowlist for segment payloads whose normal range
    /// read path can cold-hydrate from this exact selected closure.
    fn verified_remote_segments(
        &self,
        store: &FileStore,
        selected: &backend_extension_turso::SelectedGeneration,
    ) -> Result<Option<VerifiedRemoteSegmentSet>, PublicationError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt() -> ExactS3ClosureReceipt {
        let publication_fence = PublicationFence {
            namespace_id: [2; 16],
            attempt_id: [3; 16],
            epoch: 7,
            attempt_fence: [4; 32],
            input_digest: [9; 32],
            base_generation: 0,
            base_root: None,
            assignment: None,
        };
        ExactS3ClosureReceipt {
            closure: [1; 32],
            target_root: [11; 32],
            publication_fence,
            fence: WorkFence {
                namespace_id: [2; 16],
                work_id: [3; 16],
                attempt: 7,
                fence: [4; 32],
                closure_root: [1; 32],
            },
            object_count: 2,
            payload_bytes: 24,
            membership_digest: closure_membership_digest([1; 32], &[[10; 32], [12; 32]]),
            object_ids: vec![[10; 32], [12; 32]],
            packs: vec![PackReceiptSummary {
                pack_id: [6; 32],
                layout_id: [7; 32],
                pack_bytes: 400,
                manifest_bytes: 128,
                object_count: 2,
                sha256: [8; 32],
                manifest_sha256: [13; 32],
                member_digest: pack_membership_digest([6; 32], [[10; 32], [12; 32]], 2),
            }],
        }
    }

    #[test]
    fn exact_closure_receipt_roundtrips_and_rejects_truncation_and_mismatch() {
        let receipt = receipt();
        let encoded = receipt.encode().expect("encode exact closure receipt");
        assert_eq!(
            ExactS3ClosureReceipt::decode(&encoded).expect("decode exact closure receipt"),
            receipt.clone()
        );
        assert!(ExactS3ClosureReceipt::decode(&encoded[..encoded.len() - 1]).is_err());

        let mut mismatched = receipt.clone();
        mismatched.packs[0].object_count = 1;
        let mismatched_bytes = mismatched.encode().expect("encode malformed receipt");
        assert!(ExactS3ClosureReceipt::decode(&mismatched_bytes).is_err());

        assert!(
            receipt
                .validate(
                    [9; 32],
                    receipt.target_root,
                    receipt.publication_fence,
                    receipt.object_count,
                )
                .is_err()
        );
        assert!(
            receipt
                .validate(
                    receipt.closure,
                    receipt.target_root,
                    receipt.publication_fence,
                    receipt.object_count + 1,
                )
                .is_err()
        );
        let mut wrong_attempt = receipt.publication_fence;
        wrong_attempt.input_digest = [10; 32];
        assert!(
            receipt
                .validate(
                    receipt.closure,
                    receipt.target_root,
                    wrong_attempt,
                    receipt.object_count,
                )
                .is_err()
        );
        assert!(
            receipt
                .validate(
                    receipt.closure,
                    [12; 32],
                    receipt.publication_fence,
                    receipt.object_count,
                )
                .is_err()
        );
        let selected = RemoteClosureSelection {
            closure: receipt.closure,
            target_root: receipt.target_root,
            candidate_id: [10; 32],
            namespace_id: receipt.publication_fence.namespace_id,
            generation: 1,
            attempt_id: receipt.publication_fence.attempt_id,
            epoch: receipt.publication_fence.epoch,
            attempt_fence: receipt.publication_fence.attempt_fence,
            input_digest: receipt.publication_fence.input_digest,
        };
        receipt
            .validate_selected(selected)
            .expect("exact selected receipt identity");
        assert!(
            receipt
                .validate_selected(RemoteClosureSelection {
                    attempt_fence: [0x55; 32],
                    ..selected
                })
                .is_err()
        );
        assert!(
            receipt
                .validate_selected(RemoteClosureSelection {
                    generation: 2,
                    ..selected
                })
                .is_err()
        );
        assert!(
            receipt
                .validate_selected(RemoteClosureSelection {
                    candidate_id: [11; 32],
                    ..selected
                })
                .is_err()
        );
    }

    #[test]
    fn durable_gc_receipt_lookup_ignores_hot_cache_and_rejects_stale_authority() {
        let scratch = std::env::temp_dir().join(format!(
            "backend-s3-durable-gc-receipt-{}-{}",
            std::process::id(),
            NEXT_RECEIPT_TEMP.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = fs::remove_dir_all(&scratch);
        fs::create_dir_all(&scratch).expect("create scratch directory");
        let receipt_root = scratch.join("receipts");
        let cas_root = scratch.join("cas");
        let store = FileStore::open(&cas_root, 1024 * 1024).expect("open test CAS");
        let endpoint = "https://s3.example.com";
        let route_config = S3RouteConfig::new(
            [S3Endpoint::https(endpoint).expect("valid test endpoint")],
            MAX_PACK_BYTES,
            3,
            Duration::ZERO,
        )
        .expect("valid S3 route");
        let receipt = receipt();
        let selection = RemoteClosureSelection {
            closure: receipt.closure,
            target_root: receipt.target_root,
            candidate_id: [10; 32],
            namespace_id: receipt.publication_fence.namespace_id,
            generation: 1,
            attempt_id: receipt.publication_fence.attempt_id,
            epoch: receipt.publication_fence.epoch,
            attempt_fence: receipt.publication_fence.attempt_fence,
            input_digest: receipt.publication_fence.input_digest,
        };
        let cache_key = ReceiptCacheKey {
            closure: selection.closure,
            attempt_id: selection.attempt_id,
            epoch: selection.epoch,
            attempt_fence: selection.attempt_fence,
        };
        let publisher = S3ClosurePublisher {
            route: S3PackRoute::new(S3ObjectRoute::new(route_config)),
            endpoint: endpoint.to_owned(),
            bucket: "backend-bucket".to_owned(),
            region: "us-east-1".to_owned(),
            access_key: "AKIDEXAMPLE".to_owned(),
            secret_key: "test-secret".to_owned(),
            session_token: None,
            prefix: "packs/".to_owned(),
            receipt_root: receipt_root.clone(),
            receipt_cache: Mutex::new(Some((cache_key, Arc::new(receipt.clone())))),
        };

        assert!(
            publisher
                .durable_selected_receipt(&store, selection)
                .expect("missing receipt is conservative")
                .is_none()
        );

        fs::create_dir_all(&receipt_root).expect("create receipt directory");
        let stale_path = receipt_root.join(format!(
            "v2-{}-{}-{}-{}.receipt",
            hex(&selection.closure),
            hex(&selection.attempt_id),
            selection.epoch,
            hex(&selection.attempt_fence),
        ));
        let mut stale = receipt;
        stale.target_root = [0x55; 32];
        fs::write(
            &stale_path,
            stale.encode().expect("encode valid stale receipt"),
        )
        .expect("write stale receipt");
        assert!(
            publisher
                .durable_selected_receipt(&store, selection)
                .expect("stale receipt is conservative")
                .is_none()
        );

        drop(publisher);
        drop(store);
        let _ = fs::remove_dir_all(&scratch);
    }

    #[test]
    fn owner_receipt_hydration_uses_real_loopback_s3_range_gets() {
        use backend_semantic::ir::VersionedPlaneSegmentSchema;
        use backend_store::{ClosureCompositionBudget, ClosureMembershipChange, TypedObject};
        use backend_version::{ObjectKey, Schema};

        let scratch = std::env::temp_dir().join(format!(
            "backend-s3-owner-range-{}-{}",
            std::process::id(),
            NEXT_RECEIPT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&scratch).expect("create owner S3 test root");
        let store =
            FileStore::open(scratch.join("cas"), 4 * 1024 * 1024).expect("open owner test CAS");
        let payload = b"semantic-plane-segment-over-real-s3-range";
        let key = ObjectKey::<VersionedPlaneSegmentSchema>::from_value(payload.as_slice());
        let object = TypedObject::from_value(&key, payload.as_slice());
        let object_id = store.write_object(&object).expect("write checked segment");
        let pinned = store
            .compose_closure_index(
                None,
                &[ClosureMembershipChange::add(object_id)],
                ClosureCompositionBudget::new(4, 1, 1024 * 1024, 1024 * 1024),
            )
            .expect("compose exact selected test closure");
        let closure = pinned.receipt().closure();

        let server = backend_store_s3::test_support::LoopbackS3::start()
            .expect("start actual loopback S3-compatible HTTP service");
        let route = server
            .route(MAX_PACK_BYTES)
            .expect("construct production route for loopback origin");
        let publication_fence = PublicationFence {
            namespace_id: [0x19; 16],
            attempt_id: [0x31; 16],
            epoch: 7,
            attempt_fence: [0x54; 32],
            input_digest: [0x63; 32],
            base_generation: 0,
            base_root: None,
            assignment: None,
        };
        let publisher = S3ClosurePublisher {
            route,
            endpoint: server.origin().to_owned(),
            bucket: "backend-bucket".to_owned(),
            region: "us-east-1".to_owned(),
            access_key: "AKIDTEST".to_owned(),
            secret_key: "loopback-only-test-key".to_owned(),
            session_token: None,
            prefix: "packs/".to_owned(),
            receipt_root: scratch.join("receipts"),
            receipt_cache: Mutex::new(None),
        };
        let selected_publisher: &dyn SelectedClosurePublisher = &publisher;
        selected_publisher
            .publish_closure(
                &store,
                closure,
                [0x72; 32],
                1,
                backend_store::ArtifactBudget::new(4, 4, 1024 * 1024, 4096, 4),
                publication_fence,
            )
            .expect("publish and durably receipt the exact closure");
        let selected = RemoteClosureSelection {
            closure: *closure.as_bytes(),
            target_root: [0x72; 32],
            // The test closure uses the segment as its selected member. Real
            // compiler selections bind this field to the checked envelope ID.
            candidate_id: *object_id.as_bytes(),
            namespace_id: publication_fence.namespace_id,
            generation: 1,
            attempt_id: publication_fence.attempt_id,
            epoch: publication_fence.epoch,
            attempt_fence: publication_fence.attempt_fence,
            input_digest: publication_fence.input_digest,
        };
        assert!(
            publisher
                .has_durable_selected_closure(&store, selected)
                .expect("reopen exact durable receipt")
        );
        let hydrated = selected_publisher
            .hydrate_object(
                &store,
                selected,
                UntrustedObjectId::from_bytes(*object_id.as_bytes()),
                backend_semantic::ir::VERSIONED_PLANE_SEGMENT_SCHEMA,
                u64::try_from(payload.len()).expect("test segment length fits u64"),
            )
            .expect("fetch full verified envelope through production S3 pack route");
        assert_eq!(hydrated, payload);
        let stats = server.stats();
        assert_eq!(
            stats.puts, 1,
            "publisher must send one real conditional PUT"
        );
        assert!(
            stats.range_gets >= 3,
            "cold owner hydration must fetch the root, proof page, and envelope via real range GETs: {stats:?}"
        );
        assert!(server.stored_object().is_some());

        drop(publisher);
        drop(store);
        drop(server);
        let _ = fs::remove_dir_all(&scratch);
    }

    #[test]
    fn receipt_member_inventory_and_pack_directory_fit_explicit_byte_bound() {
        let member_bytes = MAX_RECEIPT_OBJECTS
            .checked_mul(32)
            .expect("bounded member inventory size");
        let pack_bytes = MAX_RECEIPT_PACKS
            .checked_mul(176)
            .expect("bounded pack directory size");
        let maximum_encoded_bytes = 700_usize
            .checked_add(member_bytes)
            .and_then(|bytes| bytes.checked_add(pack_bytes))
            .expect("bounded receipt size");
        assert_eq!(member_bytes, 3_200_064);
        assert!(maximum_encoded_bytes <= MAX_RECEIPT_BYTES);
        assert!(MAX_RECEIPT_BYTES <= 24 * 1024 * 1024);
    }

    #[test]
    fn aws_signing_helpers_use_canonical_encoding_and_utc_dates() {
        let query = BTreeMap::from([
            (
                "X-Amz-Credential".to_owned(),
                "key/20260928/us-east-1/s3/aws4_request".to_owned(),
            ),
            ("X-Amz-Date".to_owned(), "20260928T000000Z".to_owned()),
        ]);
        assert_eq!(
            canonical_query(&query),
            "X-Amz-Credential=key%2F20260928%2Fus-east-1%2Fs3%2Faws4_request&X-Amz-Date=20260928T000000Z"
        );
        assert_eq!(aws_encode("a b+%"), "a%20b%2B%25");
        assert_eq!(aws_timestamp(1_790_000_000), "20260921T141320Z");
    }

    #[test]
    fn presign_matches_sigv4_golden_vector_and_rejects_noncanonical_prefixes() {
        let endpoint = "https://s3.example.com";
        let route_config = S3RouteConfig::new(
            [S3Endpoint::https(endpoint).expect("valid test endpoint")],
            MAX_PACK_BYTES,
            3,
            Duration::ZERO,
        )
        .expect("valid S3 route");
        let publisher = S3ClosurePublisher {
            route: S3PackRoute::new(S3ObjectRoute::new(route_config)),
            endpoint: endpoint.to_owned(),
            bucket: "backend-bucket".to_owned(),
            region: "us-east-1".to_owned(),
            access_key: "AKIDEXAMPLE".to_owned(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_owned(),
            session_token: Some("test+session/=".to_owned()),
            prefix: "compiler-v2/".to_owned(),
            receipt_root: PathBuf::from("/tmp/backend-s3-test-receipts"),
            receipt_cache: Mutex::new(None),
        };
        let path = format!("/{}/{}item-01", publisher.bucket, publisher.prefix);
        assert_eq!(path, "/backend-bucket/compiler-v2/item-01");
        assert!(valid_prefix(&publisher.prefix));
        assert!(!valid_prefix("compiler%2Fv2/"));
        assert!(!valid_prefix("compiler//v2/"));

        let scope = "20130524/us-east-1/s3/aws4_request";
        let credential = format!("{}/{scope}", publisher.access_key);
        let checksum = BASE64.encode(Sha256::digest(b"remote pack checksum"));
        let url = publisher
            .presign(
                "PUT",
                &path,
                "20130524T000000Z",
                900,
                &credential,
                scope,
                &[
                    ("host", String::new()),
                    ("if-none-match", "*".to_owned()),
                    ("x-amz-checksum-sha256", checksum),
                ],
                "host;if-none-match;x-amz-checksum-sha256",
            )
            .expect("presign test PUT");
        assert_eq!(
            url,
            "https://s3.example.com/backend-bucket/compiler-v2/item-01?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIDEXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request&X-Amz-Date=20130524T000000Z&X-Amz-Expires=900&X-Amz-Security-Token=test%2Bsession%2F%3D&X-Amz-SignedHeaders=host%3Bif-none-match%3Bx-amz-checksum-sha256&X-Amz-Signature=3153b538cf2df64fd6800ea0570b16313a364c130d3a715c18411812187d3e15"
        );
    }
}
