//! Single durable owner for registry intent, immutable bytes, and feed progress.

use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File},
    io::{self, Read},
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
    AcquisitionDecision, AcquisitionGate, AdvisoryCoverage, AdvisoryObservation,
    AdvisoryPackageDto, AdvisoryResolver, FreshnessState, MalwareCoverage, normalize_package,
};
use blake3::Hasher;

use super::wire::{RegistryLog, RegistryRecord};
use super::{
    AcquisitionLimits, AcquisitionPolicy, CanonicalFeedV1, FeedCursor, FeedRequest, FeedSchema,
    PackageCoordinate, PublishedArtifactClaim, RegistryEndpoint, RegistryTransport, RemoteRegistry,
    TransportFailure, TransportResult,
};
use super::frontier::{
    apply_prepared_forge_link, apply_receipt_to_catalog, prepare_forge_link,
    prepare_receipt_facts,
    facts_store_error, validate_receipt_catalog, valid_forge_source_ids, FactsMerkleMap,
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

impl RegistryOwner {
    /// Opens and validates one production v1 feed owner.
    ///
    /// # Errors
    /// Returns a bounds, filesystem, journal, or recovered-history error.
    pub fn open(
        root: impl AsRef<Path>,
        endpoint: RegistryEndpoint,
        policy: AcquisitionPolicy,
        limits: AcquisitionLimits,
    ) -> Result<(Self, RegistryRecovery), AcquisitionError> {
        Self::open_with_objects(root, endpoint, policy, limits, None)
    }

    /// Opens one source owner while placing immutable archive objects in a
    /// caller-selected shared CAS directory.
    ///
    /// The journal, cursor, pending intent, and source catalog still live
    /// below this endpoint's isolated [`storage_root`]. Only bytes whose
    /// content-addressed identity is identical can be reused by another
    /// source owner. This is the composition seam used by the multi-registry
    /// router; the ordinary [`Self::open`] layout remains source-local for
    /// compatibility with direct engine callers.
    pub fn open_with_shared_objects(
        root: impl AsRef<Path>,
        endpoint: RegistryEndpoint,
        policy: AcquisitionPolicy,
        limits: AcquisitionLimits,
        shared_objects: impl AsRef<Path>,
    ) -> Result<(Self, RegistryRecovery), AcquisitionError> {
        Self::open_with_objects(
            root,
            endpoint,
            policy,
            limits,
            Some(shared_objects.as_ref()),
        )
    }

    fn open_with_objects(
        root: impl AsRef<Path>,
        endpoint: RegistryEndpoint,
        policy: AcquisitionPolicy,
        limits: AcquisitionLimits,
        shared_objects: Option<&Path>,
    ) -> Result<(Self, RegistryRecovery), AcquisitionError> {
        let limits = limits.validate()?;
        let root = storage_root(root.as_ref(), &endpoint);
        fs::create_dir_all(&root)?;
        let objects_root = shared_objects
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.join("registry-content"));
        let objects = ContentAddressedStore::open(objects_root).map_err(content_store_error)?;
        let (journal, recovery) =
            HashChainJournal::<RegistryLog>::open(root.join("registry.journal"))?;
        let mut cursor = FeedCursor::genesis(endpoint.id());
        let mut pending: BTreeMap<EffectKey, AcquisitionIntent> = BTreeMap::new();
        let mut last_receipt = None;
        let mut catalog = BTreeMap::new();
        let mut forge_associations = BTreeMap::new();
        let mut forge_refcounts = BTreeMap::new();
        let mut facts_map = FactsMerkleMap::try_new().map_err(facts_store_error)?;
        for frame in recovery.frames {
            match RegistryLog::decode_record(&frame.payload)? {
                RegistryRecord::Prepared(intent) => {
                    if intent.cursor != cursor
                        || pending.insert(intent.key, intent).is_some()
                        || pending.len() != 1
                    {
                        return Err(AcquisitionError::CorruptJournal);
                    }
                }
                RegistryRecord::Settled(intent) => {
                    if pending.remove(&intent.key).is_none() {
                        return Err(AcquisitionError::CorruptJournal);
                    }
                }
                RegistryRecord::Committed(intent, receipt) => {
                    let Some(persisted) = pending.remove(&receipt.effect) else {
                        return Err(AcquisitionError::CorruptJournal);
                    };
                    if persisted != intent {
                        return Err(AcquisitionError::CorruptJournal);
                    }
                    if intent.cursor != receipt.base
                        || receipt.base != cursor
                        || receipt.target.registry() != endpoint.id()
                        || receipt.target.sequence()
                            != cursor
                                .sequence()
                                .checked_add(1)
                                .ok_or(AcquisitionError::Bounds)?
                    {
                        return Err(AcquisitionError::CorruptJournal);
                    }
                    verify_receipt_objects(&objects, &receipt)?;
                    validate_receipt_catalog(
                        &catalog,
                        &forge_associations,
                        &receipt,
                        limits.max_catalog_items,
                    )?;
                    let prepared_facts = prepare_receipt_facts(&facts_map, &receipt)?;
                    if prepared_facts.target_root() != receipt.facts_root {
                        return Err(AcquisitionError::CorruptJournal);
                    }
                    apply_receipt_to_catalog(
                        &mut catalog,
                        &mut forge_associations,
                        &mut forge_refcounts,
                        &mut facts_map,
                        &receipt,
                        prepared_facts,
                    )?;
                    cursor = receipt.target;
                    last_receipt = Some(receipt);
                }
                RegistryRecord::ForgeLinked {
                    coordinate,
                    associations,
                    facts_root,
                } => {
                    let prepared = prepare_forge_link(
                        &catalog,
                        &forge_associations,
                        &facts_map,
                        &coordinate,
                        associations,
                    )?;
                    if prepared.target_root() != facts_root {
                        return Err(AcquisitionError::CorruptJournal);
                    }
                    apply_prepared_forge_link(
                        &mut catalog,
                        &mut forge_associations,
                        &mut forge_refcounts,
                        &mut facts_map,
                        prepared,
                    )?;
                }
            }
        }
        for package in catalog.values() {
            if !valid_forge_source_ids(package, &forge_associations) {
                return Err(AcquisitionError::CorruptJournal);
            }
        }
        let pending = pending.into_values().next();
        let report = RegistryRecovery {
            cursor,
            pending,
            last_receipt: last_receipt.clone(),
            repaired_tail: recovery.truncated_tail,
        };
        let readiness = match policy {
            AcquisitionPolicy::Offline => RegistryReadiness::Offline {
                source: endpoint.id(),
                cursor: cursor.sequence(),
            },
            AcquisitionPolicy::Online => RegistryReadiness::Configured {
                source: endpoint.id(),
                cursor: cursor.sequence(),
            },
        };
        Ok((
            Self {
                endpoint,
                policy,
                limits,
                journal,
                objects,
                cursor,
                pending,
                last_receipt,
                catalog,
                forge_associations,
                forge_refcounts,
                facts_map,
                faults: Arc::new(Faults::default()),
                readiness,
                advisory_gate: None,
                advisory_resolver: None,
            },
            report,
        ))
    }

    /// Installs deterministic crash seams used by recovery tests.
    #[must_use]
    pub fn with_faults(mut self, faults: Arc<Faults>) -> Self {
        self.faults = faults;
        self
    }

    /// Installs the explicit product advisory gate. The default engine owner leaves this unset
    /// for compatibility fixtures; production compositions must choose a policy before network
    /// acquisition is enabled.
    #[must_use]
    pub fn with_advisory_gate(mut self, gate: AcquisitionGate) -> Self {
        self.advisory_gate = Some(gate);
        self
    }

    /// Installs the immutable advisory frontier used to resolve native
    /// registry releases before archive staging. The resolver is deliberately
    /// independent of the registry transport, so a refreshed security
    /// frontier can change policy without downloading the archive again.
    #[must_use]
    pub fn with_advisory_resolver(mut self, resolver: Arc<dyn AdvisoryResolver>) -> Self {
        self.advisory_resolver = Some(resolver);
        self
    }

    /// Reserves the next feed effect while holding the owner lock only for
    /// durable intent admission. The returned intent is safe to use for
    /// network I/O after the lock is released.
    pub(crate) fn begin_intent(&mut self) -> Result<AcquisitionIntent, AcquisitionError> {
        if let Some(intent) = self.pending {
            return Ok(intent);
        }
        let intent = AcquisitionIntent::new(self.cursor, self.limits.max_items);
        self.faults
            .trip(Boundary::EffectPrepared)
            .map_err(AcquisitionError::Injected)?;
        self.journal.append(&RegistryRecord::Prepared(intent))?;
        self.pending = Some(intent);
        Ok(intent)
    }

    /// Returns the bounded network request for a previously admitted intent.
    pub(crate) const fn request_for_intent(&self, intent: AcquisitionIntent) -> FeedRequest {
        FeedRequest {
            cursor: intent.cursor,
            max_items: intent.max_items,
        }
    }

    /// Builds an archive staging context. It contains only immutable owner
    /// policy and paths, so the network and archive stream can run without
    /// holding the owner mutex.
    pub(crate) fn archive_stage(&self) -> ArchiveStageContext {
        ArchiveStageContext {
            objects: self.objects.clone(),
            limits: self.limits,
            faults: Arc::clone(&self.faults),
        }
    }

    /// Projects policy for one page row without doing network or filesystem
    /// work. Callers use this during the short reservation phase.
    pub(crate) fn advisory_for(&self, package: &super::RemotePackage) -> AdvisoryPackageDto {
        self.advisory_projection(package)
    }

    /// Commits a page staged outside the owner mutex. Cursor and intent checks
    /// reject stale reservations before any journal mutation, preserving
    /// deterministic source order under concurrent callers.
    pub(crate) fn commit_reserved_page(
        &mut self,
        intent: AcquisitionIntent,
        page: &super::FeedPage,
        publications: Vec<PublishedPackage>,
    ) -> Result<AcquisitionReceipt, AcquisitionError> {
        if self.pending != Some(intent) || self.cursor != intent.cursor {
            return Err(AcquisitionError::StaleReservation);
        }
        if page.base != intent.cursor || page.packages.len() > self.limits.max_items {
            return Err(AcquisitionError::Transport(TransportFailure::Protocol));
        }
        let target = self.cursor.advance(page.next_token)?;
        let receipt = AcquisitionReceipt {
            effect: intent.key,
            base: self.cursor,
            target,
            packages: publications,
            facts_root: [0; 32],
        };
        validate_receipt_catalog(
            &self.catalog,
            &self.forge_associations,
            &receipt,
            self.limits.max_catalog_items,
        )?;
        let prepared_facts = prepare_receipt_facts(&self.facts_map, &receipt)?;
        let mut receipt = receipt;
        receipt.facts_root = prepared_facts.target_root();
        self.faults
            .trip(Boundary::EffectConfirmed)
            .map_err(AcquisitionError::Injected)?;
        self.journal
            .append(&RegistryRecord::Committed(intent, receipt.clone()))?;
        apply_receipt_to_catalog(
            &mut self.catalog,
            &mut self.forge_associations,
            &mut self.forge_refcounts,
            &mut self.facts_map,
            &receipt,
            prepared_facts,
        )?;
        self.cursor = target;
        self.readiness = RegistryReadiness::Ready {
            source: self.endpoint.id(),
            cursor: target.sequence(),
        };
        self.pending = None;
        self.last_receipt = Some(receipt.clone());
        Ok(receipt)
    }

    /// Settles a metadata-only response after validating that its reservation
    /// still names the current cursor.
    pub(crate) fn settle_reserved(
        &mut self,
        intent: AcquisitionIntent,
    ) -> Result<(), AcquisitionError> {
        if self.pending != Some(intent) || self.cursor != intent.cursor {
            return Err(AcquisitionError::StaleReservation);
        }
        self.journal.append(&RegistryRecord::Settled(intent))?;
        self.pending = None;
        self.readiness = RegistryReadiness::Ready {
            source: self.endpoint.id(),
            cursor: self.cursor.sequence(),
        };
        Ok(())
    }
    /// Current committed cursor.
    #[must_use]
    pub const fn cursor(&self) -> FeedCursor<RemoteRegistry, CanonicalFeedV1> {
        self.cursor
    }

    /// Credential-free source identity used by the acquisition layer.
    #[must_use]
    pub const fn source_id(&self) -> super::RegistryId {
        self.endpoint.id()
    }

    /// Returns whether this owner is configured for local-only operation.
    #[must_use]
    pub const fn is_offline(&self) -> bool {
        matches!(self.policy, AcquisitionPolicy::Offline)
    }

    /// Stable nonzero policy/advisory frontier digest for one source.
    #[must_use]
    pub fn policy_epoch(&self) -> u64 {
        let mut hasher = Hasher::new();
        hasher.update(b"nudox.registry.policy-frontier.v1\0");
        hasher.update(&[u8::from(matches!(self.policy, AcquisitionPolicy::Online))]);
        hasher.update(&[match self.advisory_gate.map(|gate| gate.offline) {
            None => 0,
            Some(backend_advisory::OfflinePolicy::AllowCached) => 1,
            Some(backend_advisory::OfflinePolicy::Warn) => 2,
            Some(backend_advisory::OfflinePolicy::FailClosed) => 3,
        }]);
        hasher.update(&self.facts_map.root());
        let digest = hasher.finalize();
        u64::from_be_bytes(
            digest.as_bytes()[..8]
                .try_into()
                .expect("fixed digest prefix"),
        )
        .max(1)
    }

    /// Re-evaluates persisted advisory facts against the current gate before
    /// allowing a local cache hit. A changed gate therefore invalidates old
    /// warnings without touching immutable archive bytes.
    #[must_use]
    pub fn cached_policy_allows(&self, package: &PublishedPackage) -> bool {
        if matches!(&package.advisory.decision, AcquisitionDecision::Deny(_)) {
            return false;
        }
        let Some(gate) = self.advisory_gate else {
            return true;
        };
        if package.advisory.yanked || package.advisory.unlisted {
            return false;
        }
        match gate.offline {
            backend_advisory::OfflinePolicy::FailClosed => {
                package.advisory.coverage == AdvisoryCoverage::Complete
                    && matches!(
                        package.advisory.freshness,
                        FreshnessState::Fresh | FreshnessState::NotModified
                    )
            }
            backend_advisory::OfflinePolicy::AllowCached
            | backend_advisory::OfflinePolicy::Warn => true,
        }
    }

    /// Most recently committed protocol receipt, if one exists.
    #[must_use]
    pub fn last_receipt(&self) -> Option<&AcquisitionReceipt> {
        self.last_receipt.as_ref()
    }

    /// Returns the immutable artifact already published for a coordinate.
    #[must_use]
    pub fn published(&self, coordinate: &PackageCoordinate) -> Option<&PublishedPackage> {
        self.catalog.get(coordinate)
    }

    /// Iterates the complete recovered local catalog in canonical coordinate order.
    ///
    /// This is a borrowed owner view: callers cannot mutate registry progress
    /// or manufacture publication records, and iteration performs no network effect.
    pub fn published_packages(&self) -> impl ExactSizeIterator<Item = &PublishedPackage> {
        self.catalog.values()
    }

    /// Joins normalized forge lineage facts for one published release.
    ///
    /// The returned projection is bounded and cloned for transport; the
    /// owner retains one shared association table keyed by content ID.
    pub fn forge_sources_for(
        &self,
        package: &PublishedPackage,
    ) -> Result<Box<[backend_library::RegistryForgeAssociation]>, AcquisitionError> {
        if !valid_forge_source_ids(package, &self.forge_associations) {
            return Err(AcquisitionError::CorruptJournal);
        }
        package
            .forge_source_ids
            .iter()
            .map(|association_id| {
                self.forge_associations
                    .get(association_id)
                    .cloned()
                    .ok_or(AcquisitionError::CorruptJournal)
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Vec::into_boxed_slice)
    }

    /// Persists the exact forge receipt facts joined to one registry release.
    ///
    /// Associations are normalized by their content identity in the owner
    /// journal. Replacing a link changes the facts frontier and therefore the
    /// next source snapshot/delta root without re-reading archive bytes.
    pub fn link_forge_associations(
        &mut self,
        coordinate: &PackageCoordinate,
        associations: Box<[backend_library::RegistryForgeAssociation]>,
    ) -> Result<(), AcquisitionError> {
        if !self.catalog.contains_key(coordinate) {
            return Err(AcquisitionError::InvalidCoordinate);
        }
        if associations.len() > backend_library::MAX_REGISTRY_FORGE_ASSOCIATIONS {
            return Err(AcquisitionError::Bounds);
        }
        let prepared = prepare_forge_link(
            &self.catalog,
            &self.forge_associations,
            &self.facts_map,
            coordinate,
            associations,
        )?;
        let event = RegistryRecord::ForgeLinked {
            coordinate: coordinate.clone(),
            associations: prepared.associations().to_vec().into_boxed_slice(),
            facts_root: prepared.target_root(),
        };
        self.journal.append(&event)?;
        apply_prepared_forge_link(
            &mut self.catalog,
            &mut self.forge_associations,
            &mut self.forge_refcounts,
            &mut self.facts_map,
            prepared,
        )?;
        Ok(())
    }

    /// Content identity of the mutable release-facts/advisory frontier.
    ///
    /// Archive claims are immutable and live in the receipt. This compact
    /// digest lets consumers key metadata snapshots without reopening any
    /// archive payloads.
    #[must_use]
    pub fn facts_frontier(&self) -> [u8; 32] {
        self.facts_map.root()
    }

    /// Returns a constant-size status suitable for client readiness projection.
    #[must_use]
    pub const fn readiness(&self) -> RegistryReadiness {
        self.readiness
    }

    /// Loads a published archive as the canonical object accepted by capability installation.
    ///
    /// # Errors
    /// Returns corruption when the durable bytes no longer match the committed receipt.
    pub fn read_artifact(
        &self,
        coordinate: &PackageCoordinate,
    ) -> Result<Option<Arc<backend_store::TypedObject>>, AcquisitionError> {
        let Some(publication) = self.catalog.get(coordinate) else {
            return Ok(None);
        };
        self.objects
            .verify_object(publication.raw_object, publication.bytes)
            .map_err(content_store_error)?;
        let mut bytes = Vec::new();
        self.objects
            .open_object(publication.raw_object)
            .map_err(content_store_error)?
            .take(publication.bytes.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).map_err(|_| AcquisitionError::Bounds)? != publication.bytes {
            return Err(AcquisitionError::CorruptJournal);
        }
        let admitted = publication.artifact.admit(&bytes)?;
        let key = coordinate_key(coordinate);
        let object = crate::capability::capability_artifact_object(&key, &bytes);
        if object.version().as_slice() != admitted.as_bytes() {
            return Err(AcquisitionError::CorruptJournal);
        }
        Ok(Some(object))
    }

    /// Performs one durable, bounded feed effect.
    ///
    /// # Errors
    /// Returns a typed transport, integrity, persistence, or bounds failure.
    #[expect(
        clippy::too_many_lines,
        reason = "one owner poll keeps the durable effect transitions together"
    )]
    pub fn poll<T: RegistryTransport>(
        &mut self,
        transport: &mut T,
    ) -> Result<AcquisitionOutcome, AcquisitionError> {
        if self.policy == AcquisitionPolicy::Offline {
            self.readiness = RegistryReadiness::Offline {
                source: self.endpoint.id(),
                cursor: self.cursor.sequence(),
            };
            return Ok(AcquisitionOutcome::Offline {
                cursor: self.cursor,
            });
        }
        let intent = if let Some(intent) = self.pending {
            intent
        } else {
            let intent = AcquisitionIntent::new(self.cursor, self.limits.max_items);
            self.faults
                .trip(Boundary::EffectPrepared)
                .map_err(AcquisitionError::Injected)?;
            self.journal.append(&RegistryRecord::Prepared(intent))?;
            self.pending = Some(intent);
            intent
        };
        let page = match transport
            .fetch_page(FeedRequest {
                cursor: intent.cursor,
                max_items: intent.max_items,
            })
            .map_err(AcquisitionError::Transport)?
        {
            TransportResult::Available(page) => page,
            TransportResult::Unavailable => {
                self.readiness = RegistryReadiness::Unavailable {
                    source: self.endpoint.id(),
                    cursor: self.cursor.sequence(),
                };
                return Ok(AcquisitionOutcome::Unavailable {
                    cursor: self.cursor,
                });
            }
            TransportResult::RetryAfter(delay) => {
                self.readiness = RegistryReadiness::RetryScheduled {
                    source: self.endpoint.id(),
                    cursor: self.cursor.sequence(),
                };
                return Ok(AcquisitionOutcome::RetryAfter {
                    cursor: self.cursor,
                    delay,
                });
            }
            TransportResult::NotModified => {
                // A 304 is meaningful only after this owner has committed a
                // metadata frontier. Accepting it at genesis turns an
                // unconditional or malicious response into a false empty
                // registry.
                if self.cursor.sequence() == 0 {
                    return Err(AcquisitionError::Transport(TransportFailure::Protocol));
                }
                self.journal.append(&RegistryRecord::Settled(intent))?;
                self.pending = None;
                self.readiness = RegistryReadiness::Ready {
                    source: self.endpoint.id(),
                    cursor: self.cursor.sequence(),
                };
                return Ok(AcquisitionOutcome::UpToDate {
                    cursor: self.cursor,
                });
            }
        };
        if page.base != intent.cursor || page.packages.len() > self.limits.max_items {
            return Err(AcquisitionError::Transport(TransportFailure::Protocol));
        }
        if page.packages.is_empty() && page.next_token == self.cursor.token() {
            self.journal.append(&RegistryRecord::Settled(intent))?;
            self.pending = None;
            self.readiness = RegistryReadiness::Ready {
                source: self.endpoint.id(),
                cursor: self.cursor.sequence(),
            };
            return Ok(AcquisitionOutcome::UpToDate {
                cursor: self.cursor,
            });
        }
        let publications = match self.acquire_page(transport, &page.packages)? {
            PageAcquisition::Ready(publications) => publications,
            PageAcquisition::Unavailable => {
                self.readiness = RegistryReadiness::Unavailable {
                    source: self.endpoint.id(),
                    cursor: self.cursor.sequence(),
                };
                return Ok(AcquisitionOutcome::Unavailable {
                    cursor: self.cursor,
                });
            }
            PageAcquisition::RetryAfter(delay) => {
                self.readiness = RegistryReadiness::RetryScheduled {
                    source: self.endpoint.id(),
                    cursor: self.cursor.sequence(),
                };
                return Ok(AcquisitionOutcome::RetryAfter {
                    cursor: self.cursor,
                    delay,
                });
            }
        };
        let target = self.cursor.advance(page.next_token)?;
        let receipt = AcquisitionReceipt {
            effect: intent.key,
            base: self.cursor,
            target,
            packages: publications,
            facts_root: [0; 32],
        };
        validate_receipt_catalog(
            &self.catalog,
            &self.forge_associations,
            &receipt,
            self.limits.max_catalog_items,
        )?;
        let prepared_facts = prepare_receipt_facts(&self.facts_map, &receipt)?;
        let mut receipt = receipt;
        receipt.facts_root = prepared_facts.target_root();
        self.faults
            .trip(Boundary::EffectConfirmed)
            .map_err(AcquisitionError::Injected)?;
        self.journal
            .append(&RegistryRecord::Committed(intent, receipt.clone()))?;
        apply_receipt_to_catalog(
            &mut self.catalog,
            &mut self.forge_associations,
            &mut self.forge_refcounts,
            &mut self.facts_map,
            &receipt,
            prepared_facts,
        )?;
        self.cursor = target;
        self.readiness = RegistryReadiness::Ready {
            source: self.endpoint.id(),
            cursor: target.sequence(),
        };
        self.pending = None;
        self.last_receipt = Some(receipt.clone());
        Ok(AcquisitionOutcome::Published(receipt))
    }

    fn acquire_page<T: RegistryTransport>(
        &self,
        transport: &mut T,
        packages: &[super::RemotePackage],
    ) -> Result<PageAcquisition, AcquisitionError> {
        let mut total = 0usize;
        let mut publications = Vec::with_capacity(packages.len());
        for package in packages {
            let advisory = self.advisory_projection(package);
            if matches!(advisory.decision, AcquisitionDecision::Deny(_)) {
                return Err(AcquisitionError::AdvisoryDenied(advisory.decision));
            }
            if let Some(existing) = self.catalog.get(&package.coordinate) {
                let upstream_integrity = package.integrity_version();
                if existing.upstream_integrity == upstream_integrity {
                    publications.push(PublishedPackage {
                        coordinate: existing.coordinate.clone(),
                        registry: existing.registry.clone(),
                        artifact: existing.artifact,
                        raw_object: existing.raw_object,
                        bytes: existing.bytes,
                        provenance: package.provenance,
                        upstream_integrity,
                        facts: package.facts,
                        native_metadata: package.native_metadata.clone(),
                        forge_source_ids: existing.forge_source_ids.clone(),
                        advisory,
                        dependency_facts: package.dependency_facts.clone(),
                    });
                    continue;
                }
            }
            let stage = self.archive_stage();
            let transfer = stage.open_transfer(package)?;
            let checkpoint = transfer.checkpoint();
            let artifact = match transport
                .fetch_archive_resumable(package, checkpoint)
                .map_err(AcquisitionError::Transport)?
            {
                TransportResult::Available(artifact) => artifact,
                TransportResult::Unavailable => return Ok(PageAcquisition::Unavailable),
                TransportResult::RetryAfter(delay) => {
                    return Ok(PageAcquisition::RetryAfter(delay));
                }
                TransportResult::NotModified => {
                    return Err(AcquisitionError::Transport(TransportFailure::Protocol));
                }
            };
            let archive_bytes =
                usize::try_from(artifact.length()).map_err(|_| AcquisitionError::Bounds)?;
            total = total
                .checked_add(archive_bytes)
                .ok_or(AcquisitionError::Bounds)?;
            let publication = stage
                .verify_and_store_with_transfer(package, artifact, total, advisory, transfer)?;
            publications.push(publication);
        }
        Ok(PageAcquisition::Ready(publications))
    }

    pub(crate) fn verify_and_store(
        &self,
        package: &super::RemotePackage,
        artifact: super::ArchiveArtifact,
        page_bytes: usize,
        advisory: AdvisoryPackageDto,
    ) -> Result<PublishedPackage, AcquisitionError> {
        self.archive_stage()
            .verify_and_store(package, artifact, page_bytes, advisory)
    }

    fn advisory_projection(&self, package: &super::RemotePackage) -> AdvisoryPackageDto {
        let Some(gate) = self.advisory_gate else {
            return package
                .advisory
                .as_ref()
                .map(|observation| {
                    AdvisoryPackageDto::from_observation(observation, AcquisitionDecision::Allow)
                })
                .unwrap_or_else(AdvisoryPackageDto::unknown);
        };
        let observation = self
            .advisory_resolver
            .as_ref()
            .and_then(|resolver| {
                let admitted = super::admit_registry_coordinate(&package.coordinate).ok()?;
                let identity = normalize_package(
                    admitted.ecosystem().package_type().as_str(),
                    admitted.qualified_name().as_str(),
                )
                .ok()?;
                Some(resolver.observe(
                    &identity,
                    admitted.version().as_str(),
                    matches!(package.facts.standing(), super::ReleaseStanding::Yanked),
                    matches!(package.facts.standing(), super::ReleaseStanding::Unlisted),
                ))
            })
            .or_else(|| package.advisory.as_ref().cloned())
            .unwrap_or(AdvisoryObservation {
                advisories: Box::new([]),
                coverage: AdvisoryCoverage::Unknown,
                freshness: FreshnessState::Unknown,
                offline: true,
                yanked: false,
                unlisted: false,
                malware: MalwareCoverage::NotCovered,
            });
        let decision = gate.decide(&observation);
        AdvisoryPackageDto::from_observation(&observation, decision)
    }
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
