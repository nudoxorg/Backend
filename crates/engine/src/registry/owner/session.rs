//! Durable registry owner session.
//!
//! Opening a source replays its journal into one cursor, catalog, and facts
//! frontier. Poll admits the next page only after that replay stays intact.

use std::{collections::BTreeMap, fs, io::Read, path::Path, sync::Arc};

use crate::{
    acquisition::ContentAddressedStore,
    effects::EffectKey,
    fault::{Boundary, Faults},
    journal::HashChainJournal,
};
use backend_advisory::{
    AcquisitionDecision, AcquisitionGate, AdvisoryCoverage, AdvisoryObservation,
    AdvisoryPackageDto, AdvisoryResolver, FreshnessState, MalwareCoverage, normalize_package,
};
use blake3::Hasher;

use super::super::frontier::{
    FactsMerkleMap, apply_prepared_forge_link, apply_receipt_to_catalog, facts_store_error,
    prepare_forge_link, prepare_receipt_facts, valid_forge_source_ids, validate_page_catalog,
    validate_receipt_catalog,
};
use super::super::wire::{RegistryLog, RegistryRecord};
use super::super::{
    AcquisitionLimits, AcquisitionPolicy, CanonicalFeedV1, FeedCursor, FeedRequest,
    PackageCoordinate, RegistryEndpoint, RegistryTransport, RemoteRegistry, TransportFailure,
    TransportResult,
};
use super::{
    AcquisitionError, AcquisitionIntent, AcquisitionOutcome, AcquisitionReceipt,
    ArchiveStageContext, PageAcquisition, PublishedPackage, RegistryOwner, RegistryReadiness,
    RegistryRecovery, content_store_error, coordinate_key, storage_root, verify_receipt_objects,
};

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
    pub(crate) fn advisory_for(&self, package: &super::super::RemotePackage) -> AdvisoryPackageDto {
        self.advisory_projection(package)
    }

    /// Rejects an ordered feed page whose new coordinates would exceed the
    /// durable catalog bound before callers stage any archive bytes.
    pub(crate) fn validate_page_catalog_capacity(
        &self,
        packages: &[super::super::RemotePackage],
    ) -> Result<(), AcquisitionError> {
        validate_page_catalog(&self.catalog, packages, self.limits.max_catalog_items)
    }

    /// Commits a page staged outside the owner mutex. Cursor and intent checks
    /// reject stale reservations before any journal mutation, preserving
    /// deterministic source order under concurrent callers.
    pub(crate) fn commit_reserved_page(
        &mut self,
        intent: AcquisitionIntent,
        page: &super::super::FeedPage,
        publications: Vec<PublishedPackage>,
    ) -> Result<AcquisitionReceipt, AcquisitionError> {
        if self.pending != Some(intent) || self.cursor != intent.cursor {
            return Err(AcquisitionError::StaleReservation);
        }
        if page.base != intent.cursor || page.packages.len() > self.limits.max_items {
            return Err(AcquisitionError::Transport(TransportFailure::Protocol));
        }
        self.validate_page_catalog_capacity(&page.packages)?;
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
    pub const fn source_id(&self) -> super::super::RegistryId {
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
        self.validate_page_catalog_capacity(&page.packages)?;
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
        packages: &[super::super::RemotePackage],
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
        package: &super::super::RemotePackage,
        artifact: super::super::ArchiveArtifact,
        page_bytes: usize,
        advisory: AdvisoryPackageDto,
    ) -> Result<PublishedPackage, AcquisitionError> {
        self.archive_stage()
            .verify_and_store(package, artifact, page_bytes, advisory)
    }

    fn advisory_projection(&self, package: &super::super::RemotePackage) -> AdvisoryPackageDto {
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
                let admitted = super::super::admit_registry_coordinate(&package.coordinate).ok()?;
                let identity = normalize_package(
                    admitted.ecosystem().package_type().as_str(),
                    admitted.qualified_name().as_str(),
                )
                .ok()?;
                Some(resolver.observe(
                    &identity,
                    admitted.version().as_str(),
                    matches!(
                        package.facts.standing(),
                        super::super::ReleaseStanding::Yanked
                    ),
                    matches!(
                        package.facts.standing(),
                        super::super::ReleaseStanding::Unlisted
                    ),
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
