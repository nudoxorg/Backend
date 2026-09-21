//! Single durable owner for registry intent, immutable bytes, and feed progress.

use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use crate::{
    capability::CapabilityArtifactId,
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
    /// Canonical identity recomputed from downloaded bytes.
    pub artifact: PublishedArtifactClaim,
    /// Admitted archive byte extent.
    pub bytes: u64,
    /// Digest of authenticated provenance evidence.
    pub provenance: super::ProvenanceDigest,
    /// Version of the registry checksum claim used to avoid redownloading.
    pub upstream_integrity: [u8; 32],
    /// Mutable release facts, independently content-versioned.
    pub facts: super::ReleaseFacts,
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
    objects: PathBuf,
    cursor: FeedCursor<RemoteRegistry, S>,
    pending: Option<AcquisitionIntent<S>>,
    last_receipt: Option<AcquisitionReceipt<S>>,
    catalog: BTreeMap<PackageCoordinate, PublishedPackage>,
    facts_frontier: [u8; 32],
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
        let limits = limits.validate()?;
        let root = storage_root(root.as_ref(), &endpoint);
        fs::create_dir_all(&root)?;
        let objects = root.join("registry-objects");
        fs::create_dir_all(&objects)?;
        let (journal, recovery) =
            HashChainJournal::<RegistryLog>::open(root.join("registry.journal"))?;
        let mut cursor = FeedCursor::genesis(endpoint.id());
        let mut pending: BTreeMap<EffectKey, AcquisitionIntent> = BTreeMap::new();
        let mut last_receipt = None;
        let mut catalog = BTreeMap::new();
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
                    validate_receipt_catalog(&catalog, &receipt, limits.max_catalog_items)?;
                    apply_receipt_to_catalog(&mut catalog, &receipt);
                    cursor = receipt.target;
                    last_receipt = Some(receipt);
                }
            }
        }
        let pending = pending.into_values().next();
        let facts_frontier = catalog_facts_frontier(&catalog);
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
                facts_frontier,
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
        };
        validate_receipt_catalog(&self.catalog, &receipt, self.limits.max_catalog_items)?;
        self.faults
            .trip(Boundary::EffectConfirmed)
            .map_err(AcquisitionError::Injected)?;
        self.journal
            .append(&RegistryRecord::Committed(intent, receipt.clone()))?;
        apply_receipt_to_catalog(&mut self.catalog, &receipt);
        self.facts_frontier = catalog_facts_frontier(&self.catalog);
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
        hasher.update(&self.facts_frontier);
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

    /// Content identity of the mutable release-facts/advisory frontier.
    ///
    /// Archive claims are immutable and live in the receipt. This compact
    /// digest lets consumers key metadata snapshots without reopening any
    /// archive payloads.
    #[must_use]
    pub const fn facts_frontier(&self) -> [u8; 32] {
        self.facts_frontier
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
        let path = self
            .objects
            .join(super::transport::hex(&publication.artifact.as_bytes()));
        let mut bytes = Vec::new();
        File::open(path)?
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
        };
        validate_receipt_catalog(&self.catalog, &receipt, self.limits.max_catalog_items)?;
        self.faults
            .trip(Boundary::EffectConfirmed)
            .map_err(AcquisitionError::Injected)?;
        self.journal
            .append(&RegistryRecord::Committed(intent, receipt.clone()))?;
        apply_receipt_to_catalog(&mut self.catalog, &receipt);
        self.facts_frontier = catalog_facts_frontier(&self.catalog);
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
                        bytes: existing.bytes,
                        provenance: package.provenance,
                        upstream_integrity,
                        facts: package.facts,
                        advisory,
                        dependency_facts: package.dependency_facts.clone(),
                    });
                    continue;
                }
            }
            let artifact = match transport
                .fetch_archive(package)
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
            let publication = self.verify_and_store(package, artifact, total, advisory)?;
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
    objects: PathBuf,
    limits: AcquisitionLimits,
    faults: Arc<Faults>,
}

impl ArchiveStageContext {
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
        let mut reader = artifact
            .into_reader(self.limits.max_archive_bytes)
            .map_err(AcquisitionError::Transport)?;
        let (temporary, artifact) = stage_verified_archive(&self.objects, package, &mut reader)?;
        let registry = match super::admit_registry_coordinate(&package.coordinate) {
            Ok(registry) => registry,
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
        };
        if let Err(error) = self.faults.trip(Boundary::ObjectWrite) {
            let _ = fs::remove_file(&temporary);
            return Err(AcquisitionError::Injected(error));
        }
        publish_immutable(
            &self.objects,
            artifact,
            temporary,
            u64::try_from(bytes).map_err(|_| AcquisitionError::Bounds)?,
        )?;
        Ok(PublishedPackage {
            coordinate: package.coordinate.clone(),
            registry,
            artifact: PublishedArtifactClaim::verified(artifact),
            bytes: u64::try_from(bytes).map_err(|_| AcquisitionError::Bounds)?,
            provenance: package.provenance,
            upstream_integrity: package.integrity_version(),
            facts: package.facts,
            advisory,
            dependency_facts: package.dependency_facts.clone(),
        })
    }
}

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn stage_verified_archive(
    directory: &Path,
    package: &super::RemotePackage,
    reader: &mut super::transport::ArchiveReader,
) -> Result<(PathBuf, CapabilityArtifactId), AcquisitionError> {
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = directory.join(format!(".archive.{}.{}.tmp", std::process::id(), sequence));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    let length = reader.length();
    let artifact = match package.stream_to(reader, &mut file, length) {
        Ok(artifact) => artifact,
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(AcquisitionError::Transport(error));
        }
    };
    if let Err(error) = file.sync_all() {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    drop(file);
    Ok((temporary, artifact))
}

fn write_immutable<R: Read>(
    directory: &Path,
    id: CapabilityArtifactId,
    mut reader: R,
    bytes: u64,
) -> Result<(), AcquisitionError> {
    let name = super::transport::hex(id.as_bytes());
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = directory.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    if let Err(error) = copy_exact(&mut reader, &mut file, bytes) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = file.sync_all() {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    drop(file);
    publish_immutable(directory, id, temporary, bytes)
}

fn publish_immutable(
    directory: &Path,
    id: CapabilityArtifactId,
    temporary: PathBuf,
    bytes: u64,
) -> Result<(), AcquisitionError> {
    let name = super::transport::hex(id.as_bytes());
    let target = directory.join(&name);
    if target.exists() {
        let same = files_equal(&temporary, &target, bytes);
        let _ = fs::remove_file(&temporary);
        let same = same?;
        if !same {
            return Err(AcquisitionError::CorruptJournal);
        }
        return Ok(());
    }
    match fs::hard_link(&temporary, &target) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let same = files_equal(&temporary, &target, bytes);
            let _ = fs::remove_file(&temporary);
            let same = same?;
            if !same {
                return Err(AcquisitionError::CorruptJournal);
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
    }
    fs::remove_file(&temporary)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn copy_exact<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    expected: u64,
) -> Result<(), AcquisitionError> {
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(read).map_err(|_| AcquisitionError::Bounds)?)
            .ok_or(AcquisitionError::Bounds)?;
        if total > expected {
            return Err(AcquisitionError::Overrun {
                measured: total,
                limit: expected,
            });
        }
        writer.write_all(&buffer[..read])?;
    }
    if total != expected {
        return Err(AcquisitionError::CorruptJournal);
    }
    Ok(())
}

fn files_equal(left: &Path, right: &Path, expected: u64) -> Result<bool, AcquisitionError> {
    if fs::metadata(left)?.len() != expected || fs::metadata(right)?.len() != expected {
        return Ok(false);
    }
    let mut left = File::open(left)?;
    let mut right = File::open(right)?;
    let mut left_buffer = [0_u8; 64 * 1024];
    let mut right_buffer = [0_u8; 64 * 1024];
    loop {
        let left_read = left.read(&mut left_buffer)?;
        let right_read = right.read(&mut right_buffer)?;
        if left_read != right_read {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
        if left_buffer[..left_read] != right_buffer[..right_read] {
            return Ok(false);
        }
    }
}
fn verify_receipt_objects(
    directory: &Path,
    receipt: &AcquisitionReceipt,
) -> Result<(), AcquisitionError> {
    for package in &receipt.packages {
        let path = directory.join(super::transport::hex(&package.artifact.as_bytes()));
        let mut bytes = Vec::new();
        File::open(path)?
            .take(package.bytes.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).map_err(|_| AcquisitionError::Bounds)? != package.bytes {
            return Err(AcquisitionError::CorruptJournal);
        }
        let _ = package.artifact.admit(&bytes)?;
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
            TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).expect("object directory");
        let bytes = Arc::<[u8]>::from(vec![0x5a; 256 * 1024]);
        let id = CapabilityArtifactId::from_value(&bytes);
        let mut writers = Vec::new();
        for _ in 0..16 {
            let directory = directory.clone();
            let bytes = Arc::clone(&bytes);
            writers.push(std::thread::spawn(move || {
                write_immutable(&directory, id, bytes.as_ref(), bytes.len() as u64)
            }));
        }
        for writer in writers {
            writer.join().expect("writer thread").expect("publication");
        }
        let target = directory.join(super::super::transport::hex(id.as_bytes()));
        assert_eq!(fs::read(target).expect("published object"), &*bytes);
        assert_eq!(
            fs::read_dir(&directory)
                .expect("directory")
                .filter_map(Result::ok)
                .count(),
            1
        );
        fs::remove_dir_all(directory).expect("cleanup");
    }
}

fn validate_receipt_catalog(
    catalog: &BTreeMap<PackageCoordinate, PublishedPackage>,
    receipt: &AcquisitionReceipt,
    maximum: usize,
) -> Result<(), AcquisitionError> {
    if receipt
        .packages
        .windows(2)
        .any(|pair| pair[0].coordinate >= pair[1].coordinate)
    {
        return Err(AcquisitionError::CorruptJournal);
    }
    let mut additional = 0usize;
    for package in &receipt.packages {
        match catalog.get(&package.coordinate) {
            Some(existing) if existing.artifact != package.artifact => {
                return Err(AcquisitionError::CorruptJournal);
            }
            Some(_) => {}
            None => additional = additional.checked_add(1).ok_or(AcquisitionError::Bounds)?,
        }
    }
    let total = catalog
        .len()
        .checked_add(additional)
        .ok_or(AcquisitionError::Bounds)?;
    if total > maximum {
        return Err(AcquisitionError::Overrun {
            measured: u64::try_from(total).map_err(|_| AcquisitionError::Bounds)?,
            limit: u64::try_from(maximum).map_err(|_| AcquisitionError::Bounds)?,
        });
    }
    Ok(())
}

fn apply_receipt_to_catalog(
    catalog: &mut BTreeMap<PackageCoordinate, PublishedPackage>,
    receipt: &AcquisitionReceipt,
) {
    for package in &receipt.packages {
        catalog.insert(package.coordinate.clone(), package.clone());
    }
}

fn catalog_facts_frontier(catalog: &BTreeMap<PackageCoordinate, PublishedPackage>) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.registry.facts-frontier.v1\0");
    for package in catalog.values() {
        let coordinate = package.coordinate.as_str().as_bytes();
        hasher.update(&(coordinate.len() as u64).to_be_bytes());
        hasher.update(coordinate);
        hasher.update(&package.artifact.as_bytes());
        hasher.update(&package.bytes.to_be_bytes());
        hasher.update(&package.facts.version());
        // Advisory DTOs are already the canonical wire projection. Keeping
        // their serialized bytes in the frontier means withdrawals, coverage,
        // and policy changes invalidate only metadata roots.
        if let Ok(bytes) = serde_json::to_vec(&package.advisory) {
            hasher.update(&(bytes.len() as u64).to_be_bytes());
            hasher.update(&bytes);
        } else {
            hasher.update(&0_u64.to_be_bytes());
        }
    }
    *hasher.finalize().as_bytes()
}

fn coordinate_key(coordinate: &PackageCoordinate) -> Vec<u8> {
    coordinate.as_str().as_bytes().to_vec()
}
