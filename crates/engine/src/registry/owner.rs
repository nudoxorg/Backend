//! Single durable owner for registry intent, immutable bytes, and feed progress.

mod session;

#[cfg(test)]
use std::fs;
use std::{
    collections::BTreeMap,
    fmt,
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::{
    acquisition::{
        ContentAddressedStore, ContentStoreError, RawArchiveObjectId, TransferId,
        TransferResetReason,
    },
    effects::{EffectKey, effect_key},
    fault::{Boundary, Faults},
    journal::{HashChainJournal, JournalError},
};
use backend_advisory::{
    AcquisitionDecision, AcquisitionGate, AdvisoryPackageDto, AdvisoryResolver,
};

use super::frontier::FactsMerkleMap;
use super::wire::RegistryLog;
use super::{
    AcquisitionLimits, AcquisitionPolicy, CanonicalFeedV1, FeedCursor, FeedSchema,
    PackageCoordinate, PublishedArtifactClaim, RegistryEndpoint, RemoteRegistry, TransportFailure,
};

/// Durable external effect intent. The key covers every request field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcquisitionIntent<S: FeedSchema = CanonicalFeedV1> {
    /// Deterministic effect idempotency key.
    pub key: EffectKey,
    /// Exact committed cursor observed before the call.
    pub cursor: FeedCursor<RemoteRegistry, S>,
    /// Bounded page request.
    pub max_items: usize,
}

impl<S: FeedSchema> AcquisitionIntent<S> {
    pub(crate) fn new(cursor: FeedCursor<RemoteRegistry, S>, max_items: usize) -> Self {
        let mut bytes = Vec::with_capacity(74);
        bytes.extend_from_slice(b"backend.registry.acquire.v1\0");
        bytes.extend_from_slice(&cursor.registry().as_bytes());
        bytes.extend_from_slice(&S::VERSION.to_be_bytes());
        bytes.extend_from_slice(&cursor.sequence().to_be_bytes());
        bytes.extend_from_slice(&cursor.token());
        bytes.extend_from_slice(&(max_items as u64).to_be_bytes());
        Self {
            key: effect_key(&bytes),
            cursor,
            max_items,
        }
    }
}

/// One immutable archive made reachable by a committed feed step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedPackage {
    /// Exact package coordinate.
    pub coordinate: PackageCoordinate,
    pub(crate) registry: super::RegistryCoordinate,
    /// Canonical capability identity recomputed from downloaded bytes.
    pub artifact: PublishedArtifactClaim,
    /// Raw content identity used by the shared local/registry object store.
    pub raw_object: RawArchiveObjectId,
    /// Admitted archive byte extent.
    pub bytes: u64,
    /// Digest of authenticated provenance evidence.
    pub provenance: super::ProvenanceDigest,
    /// Version of the registry checksum claim used to avoid redownloading.
    pub upstream_integrity: [u8; 32],
    /// Mutable release facts, independently content-versioned.
    pub facts: super::ReleaseFacts,
    /// Versioned native registry metadata captured at the same source frontier.
    pub native_metadata: backend_library::RegistryNativeMetadata,
    /// Content IDs for versioned forge lineage facts in the owner association table.
    pub forge_source_ids: Box<[[u8; 32]]>,
    /// Versioned advisory facts and the policy decision admitted before staging.
    pub advisory: AdvisoryPackageDto,
    /// Dependency facts captured at the same immutable source frontier.
    pub dependency_facts:
        backend_library::DependencyFacts<Box<[backend_library::PackageDependencyRecord]>>,
}

/// Receipt atomically pairing archive publication and cursor advancement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcquisitionReceipt<S: FeedSchema = CanonicalFeedV1> {
    /// Effect intent this receipt completes.
    pub effect: EffectKey,
    /// Previous committed cursor.
    pub base: FeedCursor<RemoteRegistry, S>,
    /// Newly committed cursor.
    pub target: FeedCursor<RemoteRegistry, S>,
    /// Complete package set published by this page.
    pub packages: Vec<PublishedPackage>,
    /// Authenticated facts-map root after this receipt is applied.
    ///
    /// The journal carries this root beside package/association IDs so
    /// recovery can verify that replay reopened the exact same persistent
    /// projection without serializing a second copy of its rows.
    pub facts_root: [u8; 32],
}

/// Recovered feed state. Pending intent is retried from the same cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryRecovery<S: FeedSchema = CanonicalFeedV1> {
    /// Last atomically committed cursor.
    pub cursor: FeedCursor<RemoteRegistry, S>,
    /// Durable request without a definitive terminal record.
    pub pending: Option<AcquisitionIntent<S>>,
    /// Most recent committed publication receipt.
    pub last_receipt: Option<AcquisitionReceipt<S>>,
    /// Whether a partial final journal frame was discarded.
    pub repaired_tail: bool,
}

/// Explicit result of one owner poll.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AcquisitionOutcome<S: FeedSchema = CanonicalFeedV1> {
    /// Network access is disabled by local-first policy.
    Offline {
        /// Unchanged committed cursor.
        cursor: FeedCursor<RemoteRegistry, S>,
    },
    /// No definitive response was available; the durable intent remains pending.
    Unavailable {
        /// Unchanged committed cursor.
        cursor: FeedCursor<RemoteRegistry, S>,
    },
    /// Server requested a bounded later retry; the intent remains pending.
    RetryAfter {
        /// Unchanged committed cursor.
        cursor: FeedCursor<RemoteRegistry, S>,
        /// Minimum retry delay supplied by the adapter.
        delay: Duration,
    },
    /// Feed reported no new immutable package.
    UpToDate {
        /// Unchanged committed cursor.
        cursor: FeedCursor<RemoteRegistry, S>,
    },
    /// All archives and the target cursor were durably committed.
    Published(AcquisitionReceipt<S>),
}

/// Constant-size, credential-free registry readiness observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistryReadiness {
    /// Source is configured but has not completed a poll in this process.
    Configured {
        /// Credential-free source identity.
        source: super::RegistryId,
        /// Committed cursor sequence.
        cursor: u64,
    },
    /// Network use is disabled by local-first policy.
    Offline {
        /// Credential-free source identity.
        source: super::RegistryId,
        /// Committed cursor sequence.
        cursor: u64,
    },
    /// The latest poll reached a definitive authenticated response.
    Ready {
        /// Credential-free source identity.
        source: super::RegistryId,
        /// Committed cursor sequence.
        cursor: u64,
    },
    /// The latest poll could not reach the source before its deadline.
    Unavailable {
        /// Credential-free source identity.
        source: super::RegistryId,
        /// Committed cursor sequence.
        cursor: u64,
    },
    /// The source asked this process to retry later.
    RetryScheduled {
        /// Credential-free source identity.
        source: super::RegistryId,
        /// Committed cursor sequence.
        cursor: u64,
    },
}

/// Typed acquisition failure without credential-bearing strings.
#[derive(Debug)]
pub enum AcquisitionError {
    /// Endpoint, deadline, or bound configuration is invalid.
    InvalidConfiguration,
    /// Package name or version is not canonical and bounded.
    InvalidCoordinate,
    /// Arithmetic, item, byte, or history bound was exceeded.
    Bounds,
    /// A measured source extent exceeded an explicit configured bound.
    ///
    /// This is the non-silent terminal for archives, pages, and catalogs that
    /// are larger than the operator's configured admission limit. It reports
    /// the measured extent so a cap can be raised deliberately rather than
    /// rejecting a legitimate large sdist or a long version list.
    Overrun {
        /// Extent actually observed from the source or staging operation.
        measured: u64,
        /// Configured admission limit that was exceeded.
        limit: u64,
    },
    /// Transport returned a terminal protocol, integrity, or policy rejection.
    Transport(TransportFailure),
    /// Durable history is inconsistent or noncanonical.
    CorruptJournal,
    /// Filesystem persistence failed.
    Io(std::io::Error),
    /// Generic journal failed.
    Journal(JournalError),
    /// Configured fault boundary fired.
    Injected(crate::fault::InjectedCrash),
    /// The selected version was denied by the configured advisory policy.
    AdvisoryDenied(AcquisitionDecision),
    /// A network reservation was superseded by another deterministic commit.
    /// The caller must discard its uncommitted receipt and retry from the
    /// newly visible cursor.
    StaleReservation,
}
impl fmt::Display for AcquisitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfiguration => "invalid registry configuration",
            Self::InvalidCoordinate => "invalid package coordinate",
            Self::Bounds => "registry acquisition bound exceeded",
            Self::Overrun { .. } => "registry acquisition extent exceeds configured bound",
            Self::Transport(_) => "registry transport rejected response",
            Self::CorruptJournal => "registry journal is corrupt",
            Self::Io(_) => "registry persistence failed",
            Self::Journal(_) => "registry journal failed",
            Self::Injected(_) => "registry acquisition fault injected",
            Self::AdvisoryDenied(_) => "registry acquisition denied by advisory policy",
            Self::StaleReservation => "registry acquisition reservation is stale",
        })
    }
}
impl std::error::Error for AcquisitionError {}
impl From<std::io::Error> for AcquisitionError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<JournalError> for AcquisitionError {
    fn from(value: JournalError) -> Self {
        Self::Journal(value)
    }
}

/// Sole owner of one registry's feed progression and archive publication.
pub struct RegistryOwner<S: FeedSchema = CanonicalFeedV1> {
    endpoint: RegistryEndpoint,
    policy: AcquisitionPolicy,
    limits: AcquisitionLimits,
    journal: HashChainJournal<RegistryLog>,
    objects: ContentAddressedStore,
    cursor: FeedCursor<RemoteRegistry, S>,
    pending: Option<AcquisitionIntent<S>>,
    last_receipt: Option<AcquisitionReceipt<S>>,
    catalog: BTreeMap<PackageCoordinate, PublishedPackage>,
    forge_associations: BTreeMap<[u8; 32], backend_library::RegistryForgeAssociation>,
    forge_refcounts: BTreeMap<[u8; 32], u32>,
    facts_map: FactsMerkleMap,
    faults: Arc<Faults>,
    readiness: RegistryReadiness,
    advisory_gate: Option<AcquisitionGate>,
    advisory_resolver: Option<Arc<dyn AdvisoryResolver>>,
}

impl<S: FeedSchema> fmt::Debug for RegistryOwner<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegistryOwner")
            .field("endpoint", &self.endpoint)
            .field("policy", &self.policy)
            .field("limits", &self.limits)
            .field("cursor", &self.cursor)
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
}

/// Deterministic on-disk root for one registry source.
///
/// The single acquisition authority stores every source under
/// `root/<ecosystem>/<registry-id>`, where the registry id is the stable
/// credential-free hash of the admitted endpoint. Distinct ecosystems and
/// distinct endpoints therefore never share a journal or object directory,
/// and the same endpoint always resolves to the same cache root across
/// processes and restarts. Version selection is pinned by the exact package
/// coordinate committed in the receipt, and archive bytes are content
/// addressed, so a warm cache is reproducible from the path alone.
#[must_use]
pub fn storage_root(root: &Path, endpoint: &RegistryEndpoint) -> PathBuf {
    root.join(endpoint.ecosystem().as_str())
        .join(super::transport::hex(&endpoint.id().as_bytes()))
}

enum PageAcquisition {
    Ready(Vec<PublishedPackage>),
    Unavailable,
    RetryAfter(Duration),
}

/// Immutable archive-write authority detached from the registry owner's
/// cursor mutex. It is safe to clone for speculative reservations: only the
/// final journal commit mutates owner state, and content-addressed publishing
/// is first-writer-wins.
#[derive(Clone)]
pub(crate) struct ArchiveStageContext {
    objects: ContentAddressedStore,
    limits: AcquisitionLimits,
    faults: Arc<Faults>,
}

impl ArchiveStageContext {
    pub(crate) fn open_transfer(
        &self,
        package: &super::RemotePackage,
    ) -> Result<crate::acquisition::ResumableTransfer, AcquisitionError> {
        let transfer_id = archive_transfer_id(package);
        self.objects
            .resume_or_start(transfer_id, None, None)
            .map_err(content_store_error)
    }

    pub(crate) fn verify_and_store(
        &self,
        package: &super::RemotePackage,
        artifact: super::ArchiveArtifact,
        page_bytes: usize,
        advisory: AdvisoryPackageDto,
    ) -> Result<PublishedPackage, AcquisitionError> {
        let bytes = usize::try_from(artifact.length()).map_err(|_| AcquisitionError::Bounds)?;
        if bytes > self.limits.max_archive_bytes {
            return Err(AcquisitionError::Overrun {
                measured: u64::try_from(bytes).map_err(|_| AcquisitionError::Bounds)?,
                limit: u64::try_from(self.limits.max_archive_bytes)
                    .map_err(|_| AcquisitionError::Bounds)?,
            });
        }
        if page_bytes > self.limits.max_page_archive_bytes {
            return Err(AcquisitionError::Overrun {
                measured: u64::try_from(page_bytes).map_err(|_| AcquisitionError::Bounds)?,
                limit: u64::try_from(self.limits.max_page_archive_bytes)
                    .map_err(|_| AcquisitionError::Bounds)?,
            });
        }
        let transfer = self.open_transfer(package)?;
        self.verify_and_store_with_transfer(package, artifact, page_bytes, advisory, transfer)
    }

    pub(crate) fn verify_and_store_with_transfer(
        &self,
        package: &super::RemotePackage,
        artifact: super::ArchiveArtifact,
        page_bytes: usize,
        advisory: AdvisoryPackageDto,
        mut transfer: crate::acquisition::ResumableTransfer,
    ) -> Result<PublishedPackage, AcquisitionError> {
        let bytes = usize::try_from(artifact.length()).map_err(|_| AcquisitionError::Bounds)?;
        if bytes > self.limits.max_archive_bytes {
            return Err(AcquisitionError::Overrun {
                measured: u64::try_from(bytes).map_err(|_| AcquisitionError::Bounds)?,
                limit: u64::try_from(self.limits.max_archive_bytes)
                    .map_err(|_| AcquisitionError::Bounds)?,
            });
        }
        if page_bytes > self.limits.max_page_archive_bytes {
            return Err(AcquisitionError::Overrun {
                measured: u64::try_from(page_bytes).map_err(|_| AcquisitionError::Bounds)?,
                limit: u64::try_from(self.limits.max_page_archive_bytes)
                    .map_err(|_| AcquisitionError::Bounds)?,
            });
        }
        let bytes = u64::try_from(bytes).map_err(|_| AcquisitionError::Bounds)?;
        let offset = transfer.resume_offset();
        let start = artifact.start_offset();
        if let Some(reason) = artifact.reset_reason() {
            transfer
                .restart_from_zero(Some(reason))
                .map_err(content_store_error)?;
        } else if start == 0 && offset != 0 {
            // A custom adapter or a server that ignored Range returned a full
            // representation. Replaying it from zero is safe; appending it to
            // the old prefix would create a sparse/overlapping object.
            transfer
                .restart_from_zero(Some(TransferResetReason::RangeIgnored))
                .map_err(content_store_error)?;
        }
        if transfer.resume_offset() != start {
            return Err(AcquisitionError::CorruptJournal);
        }
        if start != 0 && artifact.validator().is_none() {
            return Err(AcquisitionError::Transport(TransportFailure::Protocol));
        }
        if let Some(validator) = artifact.validator().cloned() {
            transfer
                .set_validator(Some(validator))
                .map_err(content_store_error)?;
        }
        let total_length = artifact.length();
        transfer
            .bind_expected_length(total_length)
            .map_err(content_store_error)?;
        let mut reader = artifact
            .into_reader(self.limits.max_archive_bytes)
            .map_err(AcquisitionError::Transport)?;
        transfer
            .append(&mut reader, total_length)
            .map_err(content_store_error)?;
        if transfer.resume_offset() != total_length {
            return Err(AcquisitionError::Transport(TransportFailure::Protocol));
        }
        if let Err(error) = self.faults.trip(Boundary::ObjectWrite) {
            return Err(AcquisitionError::Injected(error));
        }
        let mut capability = None;
        let admission = transfer
            .finish_verified(|path, length, _raw| {
                let mut file = File::open(path).map_err(ContentStoreError::from)?;
                let artifact = package
                    .stream_to(&mut file, &mut io::sink(), length)
                    .map_err(|_| ContentStoreError::VerificationRejected { quarantine: None })?;
                capability = Some(artifact);
                Ok(())
            })
            .map_err(content_store_error)?;
        if admission.bytes() != bytes {
            return Err(AcquisitionError::CorruptJournal);
        }
        let capability = capability.ok_or(AcquisitionError::CorruptJournal)?;
        let registry = super::admit_registry_coordinate(&package.coordinate)?;
        Ok(PublishedPackage {
            coordinate: package.coordinate.clone(),
            registry,
            artifact: PublishedArtifactClaim::verified(capability),
            raw_object: admission.object(),
            bytes,
            provenance: package.provenance,
            upstream_integrity: package.integrity_version(),
            facts: package.facts,
            native_metadata: package.native_metadata.clone(),
            forge_source_ids: Box::new([]),
            advisory,
            dependency_facts: package.dependency_facts.clone(),
        })
    }
}

fn content_store_error(error: ContentStoreError) -> AcquisitionError {
    match error {
        ContentStoreError::Io(error) if error.kind() == io::ErrorKind::NotFound => {
            AcquisitionError::CorruptJournal
        }
        ContentStoreError::Io(error) => AcquisitionError::Io(error),
        ContentStoreError::Bounds { maximum } => AcquisitionError::Overrun {
            measured: maximum,
            limit: maximum,
        },
        ContentStoreError::ArchiveBytesLimit
        | ContentStoreError::ArchiveEntryLimit
        | ContentStoreError::ArchivePathLimit => AcquisitionError::Bounds,
        ContentStoreError::DigestMismatch { .. }
        | ContentStoreError::VerificationRejected { .. } => {
            AcquisitionError::Transport(TransportFailure::Integrity)
        }
        ContentStoreError::LengthMismatch { .. }
        | ContentStoreError::TransferStateMismatch
        | ContentStoreError::TransferValidatorChanged
        | ContentStoreError::TransferBusy
        | ContentStoreError::CorruptObject { .. }
        | ContentStoreError::InvalidArchivePath
        | ContentStoreError::DuplicateArchivePath => AcquisitionError::CorruptJournal,
    }
}

fn archive_transfer_id(package: &super::RemotePackage) -> TransferId {
    let mut transfer_key = Vec::with_capacity(
        package.coordinate.as_str().len() + std::mem::size_of_val(&package.integrity_version()),
    );
    transfer_key.extend_from_slice(package.coordinate.as_str().as_bytes());
    transfer_key.extend_from_slice(&package.integrity_version());
    TransferId::from_parts(&transfer_key, None, None)
}

fn verify_receipt_objects(
    store: &ContentAddressedStore,
    receipt: &AcquisitionReceipt,
) -> Result<(), AcquisitionError> {
    for package in &receipt.packages {
        store
            .verify_object(package.raw_object, package.bytes)
            .map_err(content_store_error)?;
        let file = store
            .open_object(package.raw_object)
            .map_err(content_store_error)?;
        let _ = package.artifact.admit_reader(file, package.bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod immutable_object_tests {
    use super::*;

    #[test]
    fn concurrent_first_writers_publish_one_complete_object() {
        let directory = std::env::temp_dir().join(format!(
            "backend-registry-object-race-{}-{}",
            std::process::id(),
            package_test_nonce()
        ));
        let store = Arc::new(ContentAddressedStore::open(&directory).expect("object store"));
        let bytes = Arc::<[u8]>::from(vec![0x5a; 256 * 1024]);
        let mut writers = Vec::new();
        for _ in 0..16 {
            let store = Arc::clone(&store);
            let bytes = Arc::clone(&bytes);
            writers.push(std::thread::spawn(move || {
                store.admit_reader(None, bytes.as_ref(), bytes.len() as u64)
            }));
        }
        let mut published = 0;
        let mut object = None;
        for writer in writers {
            let admission = writer.join().expect("writer thread").expect("publication");
            published += usize::from(admission.was_published());
            object = Some(admission.object());
        }
        assert_eq!(published, 1);
        let object = object.expect("object");
        assert_eq!(
            fs::read(store.object_path(object)).expect("published object"),
            &*bytes
        );
        fs::remove_dir_all(directory).expect("cleanup");
    }

    fn package_test_nonce() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
}

fn coordinate_key(coordinate: &PackageCoordinate) -> Vec<u8> {
    coordinate.as_str().as_bytes().to_vec()
}
