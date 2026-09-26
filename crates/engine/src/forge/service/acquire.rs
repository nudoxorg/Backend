//! Acquires forge sources and admits them into the shared content store.

use super::*;

impl ForgeAcquisitionService {
    /// Opens a durable content store and reloads its credential-free journal.
    pub fn open(
        root: impl Into<PathBuf>,
        policy: ForgeAcquisitionPolicy,
        limits: ForgeAcquisitionLimits,
    ) -> Result<Self, ForgeAcquisitionError> {
        let limits = limits.validate().map_err(ForgeAcquisitionError::Rejected)?;
        let root = root.into();
        fs::create_dir_all(&root).map_err(ForgeAcquisitionError::Io)?;
        let store = ContentAddressedStore::open(root.join("content"))
            .map_err(ForgeAcquisitionError::Content)?;
        let catalog = Arc::new(Mutex::new(BTreeMap::new()));
        let catalog_for_recovery = Arc::clone(&catalog);
        let (journal, _) = HashChainJournal::<ForgeLog>::open_streaming_with(
            root.join("forge.journal"),
            JournalLimits::default(),
            move |frame| {
                let record = ForgeLog::decode(frame.payload)?;
                if !record.coordinate().identity_is_valid() {
                    return Err(JournalError::Corrupt("forge coordinate identity"));
                }
                catalog_for_recovery
                    .lock()
                    .map_err(|_| JournalError::Corrupt("forge catalog lock"))?
                    .insert(record.coordinate().identity(), record);
                Ok(())
            },
        )
        .map_err(ForgeAcquisitionError::Journal)?;
        Ok(Self {
            store,
            journal: Arc::new(journal),
            catalog,
            policy,
            limits,
        })
    }

    /// Returns the shared content store used by this forge owner.
    #[must_use]
    pub const fn store(&self) -> &ContentAddressedStore {
        &self.store
    }

    /// Returns one exact cached source without network I/O.
    pub fn reference(
        &self,
        coordinate: &ForgeCoordinate,
    ) -> Result<Option<Arc<ForgeAcquisitionResult>>, ForgeAcquisitionError> {
        let event = self
            .catalog
            .lock()
            .map_err(|_| ForgeAcquisitionError::Corrupt)?
            .get(&coordinate.identity())
            .cloned();
        match event {
            Some(ForgeJournalEvent::Published(record)) => {
                if !record.coordinate.identity_is_valid() || record.coordinate != *coordinate {
                    return Err(ForgeAcquisitionError::Corrupt);
                }
                self.rehydrate(record).map(Some)
            }
            Some(ForgeJournalEvent::Tombstone {
                coordinate: stored, ..
            }) => {
                if !stored.identity_is_valid() || stored != *coordinate {
                    return Err(ForgeAcquisitionError::Corrupt);
                }
                Ok(None)
            }
            None => Ok(None),
        }
    }

    /// Acquires one exact source through a typed transport and the shared CAS.
    pub fn acquire<T: ForgeTransport>(
        &self,
        coordinate: &ForgeCoordinate,
        transport: &mut T,
    ) -> ForgeAcquisitionOutcome {
        match self.reference(coordinate) {
            Ok(Some(result)) => return ForgeAcquisitionOutcome::Hit(result),
            Err(ForgeAcquisitionError::Corrupt) => return ForgeAcquisitionOutcome::Corrupt,
            Err(_) => {}
            Ok(None) => {}
        }
        if self.policy == ForgeAcquisitionPolicy::Offline {
            return ForgeAcquisitionOutcome::Offline;
        }
        let resolution = match transport.resolve(coordinate) {
            Ok(resolution) => resolution,
            Err(ForgeTransportError::Unavailable) => return ForgeAcquisitionOutcome::Unavailable,
            Err(ForgeTransportError::RetryAfter(millis)) => {
                return ForgeAcquisitionOutcome::RetryAfter(millis);
            }
            Err(ForgeTransportError::NotFound) => {
                return match self.commit_tombstone(coordinate, ForgeRejectReason::RevisionMismatch)
                {
                    Ok(()) => {
                        ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::RevisionMismatch)
                    }
                    Err(_) => ForgeAcquisitionOutcome::Corrupt,
                };
            }
            Err(ForgeTransportError::Bounds) => {
                return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Bounds);
            }
            Err(ForgeTransportError::Protocol | ForgeTransportError::Policy) => {
                return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Policy);
            }
            Err(ForgeTransportError::Integrity) => {
                return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Integrity);
            }
        };
        if resolution.validate_for(coordinate).is_err() {
            return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::RevisionMismatch);
        }
        let archive = match transport.fetch_archive(coordinate, &resolution) {
            Ok(archive) => archive,
            Err(ForgeTransportError::Unavailable) => return ForgeAcquisitionOutcome::Unavailable,
            Err(ForgeTransportError::RetryAfter(millis)) => {
                return ForgeAcquisitionOutcome::RetryAfter(millis);
            }
            Err(ForgeTransportError::Bounds) => {
                return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Bounds);
            }
            Err(ForgeTransportError::Integrity) => {
                return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Integrity);
            }
            Err(
                ForgeTransportError::NotFound
                | ForgeTransportError::Protocol
                | ForgeTransportError::Policy,
            ) => return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Archive),
        };
        let metadata = match transport.fetch_metadata(coordinate, &resolution) {
            Ok(metadata) => metadata,
            Err(ForgeTransportError::Bounds) => {
                return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Bounds);
            }
            Err(ForgeTransportError::Protocol) => {
                return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Metadata);
            }
            Err(ForgeTransportError::Policy) => {
                return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Policy);
            }
            Err(ForgeTransportError::Unavailable | ForgeTransportError::NotFound) => {
                ForgeRepositoryMetadata::unavailable(
                    coordinate.owner(),
                    ForgeUnavailableReason::Unreachable,
                )
            }
            Err(ForgeTransportError::RetryAfter(_)) => ForgeRepositoryMetadata::unavailable(
                coordinate.owner(),
                ForgeUnavailableReason::Unreachable,
            ),
            Err(ForgeTransportError::Integrity) => {
                return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Integrity);
            }
        };
        match self.admit(coordinate, resolution, archive, metadata) {
            Ok(result) => ForgeAcquisitionOutcome::Hit(Arc::new(result)),
            Err(ForgeAcquisitionError::Rejected(reason)) => {
                ForgeAcquisitionOutcome::Rejected(reason)
            }
            Err(ForgeAcquisitionError::Content(_)) => {
                ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Integrity)
            }
            Err(
                ForgeAcquisitionError::Io(_)
                | ForgeAcquisitionError::Journal(_)
                | ForgeAcquisitionError::Corrupt,
            ) => ForgeAcquisitionOutcome::Corrupt,
        }
    }

    fn rehydrate(
        &self,
        record: ForgeJournalRecord,
    ) -> Result<Arc<ForgeAcquisitionResult>, ForgeAcquisitionError> {
        record
            .resolution
            .validate_for(&record.coordinate)
            .map_err(|_| ForgeAcquisitionError::Corrupt)?;
        if record.source != record.coordinate.identity()
            || record.cursor != resolution_cursor(&record.resolution)
        {
            return Err(ForgeAcquisitionError::Corrupt);
        }
        let mut entries = Vec::with_capacity(record.tree.len());
        for entry in record.tree {
            let object = RawArchiveObjectId::from_encoded(entry.object);
            let _ = self
                .store
                .verify_object(object, self.limits.archive_budget.max_entry_bytes)
                .map_err(|_| ForgeAcquisitionError::Corrupt)?;
            entries.push(ManifestEntry {
                path: entry.path,
                object,
                mode: entry.mode,
            });
        }
        let tree =
            Arc::new(TreeManifest::new(entries).map_err(|_| ForgeAcquisitionError::Corrupt)?);
        let archive = RawArchiveObjectId::from_encoded(record.archive);
        let _ = self
            .store
            .verify_object(archive, self.limits.max_archive_bytes)
            .map_err(|_| ForgeAcquisitionError::Corrupt)?;
        let empty =
            Arc::new(TreeManifest::new(Vec::new()).map_err(|_| ForgeAcquisitionError::Corrupt)?);
        let base = SourceSnapshot::new(record.source, [0; ID_BYTES], 0, empty, Vec::new())
            .map_err(|_| ForgeAcquisitionError::Corrupt)?;
        if base.id().to_bytes() != record.base_snapshot {
            return Err(ForgeAcquisitionError::Corrupt);
        }
        let target = SourceSnapshot::new_with_frontier(
            record.source,
            record.cursor,
            0,
            record.facts_frontier,
            Arc::clone(&tree),
            Vec::new(),
        )
        .map_err(|_| ForgeAcquisitionError::Corrupt)?;
        if record.facts_frontier != metadata_frontier(&record.metadata, &record.manifests) {
            return Err(ForgeAcquisitionError::Corrupt);
        }
        if target.id().to_bytes() != record.target_snapshot {
            return Err(ForgeAcquisitionError::Corrupt);
        }
        let changes = tree
            .entries()
            .iter()
            .map(|entry| DeltaChange {
                path: Arc::clone(&entry.path),
                before: None,
                before_mode: None,
                after: Some(entry.object),
                after_mode: Some(entry.mode),
            })
            .collect();
        let delta = Arc::new(
            AcquisitionDelta::new(&base, Arc::new(target.clone()), changes)
                .map_err(|_| ForgeAcquisitionError::Corrupt)?,
        );
        if delta.id().as_bytes() != &record.delta {
            return Err(ForgeAcquisitionError::Corrupt);
        }
        let receipt = ForgeReceipt {
            coordinate: record.coordinate.identity(),
            commit: record.resolution.commit.clone(),
            remote_tree: record.resolution.tree.clone(),
            tree: tree.id().to_bytes(),
            archive: record.archive,
            snapshot: record.target_snapshot,
            delta: record.delta,
            observed_at_millis: record.observed_at_millis,
        };
        Ok(Arc::new(ForgeAcquisitionResult {
            coordinate: record.coordinate,
            resolution: record.resolution,
            archive,
            tree,
            metadata: record.metadata,
            manifests: record.manifests.into_boxed_slice(),
            snapshot: Arc::new(target),
            delta,
            receipt,
        }))
    }

    fn admit(
        &self,
        coordinate: &ForgeCoordinate,
        resolution: ForgeResolution,
        archive: ForgeArchive,
        metadata: ForgeRepositoryMetadata,
    ) -> Result<ForgeAcquisitionResult, ForgeAcquisitionError> {
        let format = archive.format;
        let root_prefix = archive.root_prefix.clone();
        let archive_id = self
            .store
            .admit_reader(None, archive.into_reader(), self.limits.max_archive_bytes)
            .map_err(content_error)?
            .object();
        let mut archive_object = self
            .store
            .open_object(archive_id)
            .map_err(ForgeAcquisitionError::Content)?;
        let files = super::archive::extract_archive(
            &mut archive_object,
            format,
            root_prefix.as_deref(),
            &self.limits.archive_budget,
        )
        .map_err(ForgeAcquisitionError::Rejected)?;
        let manifests = super::manifest::discover_manifests(&files, coordinate.subdir())
            .map_err(ForgeAcquisitionError::Rejected)?;
        let mut builder = ArchiveManifestBuilder::new(self.limits.archive_budget);
        for file in files {
            let object = self
                .store
                .admit_reader(
                    None,
                    Cursor::new(file.bytes),
                    self.limits.archive_budget.max_entry_bytes,
                )
                .map_err(content_error)?
                .object();
            builder
                .push_file(Arc::clone(&file.path), object, file.bytes_len, file.mode)
                .map_err(ForgeAcquisitionError::Content)?;
        }
        let tree = Arc::new(
            builder
                .finish()
                .map_err(ForgeAcquisitionError::Content)?
                .tree()
                .clone(),
        );
        // Parse from the same bounded file rows that were admitted into the
        // shared CAS. The raw archive is never cloned or re-read into a
        // second full archive buffer.
        let empty =
            Arc::new(TreeManifest::new(Vec::new()).map_err(|_| ForgeAcquisitionError::Corrupt)?);
        let source = coordinate.identity();
        let cursor = resolution_cursor(&resolution);
        let facts_frontier = metadata_frontier(&metadata, &manifests);
        let base = SourceSnapshot::new(source, [0; ID_BYTES], 0, empty, Vec::new())
            .map_err(|_| ForgeAcquisitionError::Corrupt)?;
        let target = Arc::new(
            SourceSnapshot::new_with_frontier(
                source,
                cursor,
                0,
                facts_frontier,
                Arc::clone(&tree),
                Vec::new(),
            )
            .map_err(|_| ForgeAcquisitionError::Corrupt)?,
        );
        let changes = tree
            .entries()
            .iter()
            .map(|entry| DeltaChange {
                path: Arc::clone(&entry.path),
                before: None,
                before_mode: None,
                after: Some(entry.object),
                after_mode: Some(entry.mode),
            })
            .collect();
        let delta = Arc::new(
            AcquisitionDelta::new(&base, Arc::clone(&target), changes)
                .map_err(|_| ForgeAcquisitionError::Corrupt)?,
        );
        let receipt = ForgeReceipt {
            coordinate: coordinate.identity(),
            commit: resolution.commit.clone(),
            remote_tree: resolution.tree.clone(),
            tree: tree.id().to_bytes(),
            archive: archive_id.to_bytes(),
            snapshot: target.id().to_bytes(),
            delta: delta.id().to_bytes(),
            observed_at_millis: now_millis(),
        };
        let record = ForgeJournalRecord {
            coordinate: coordinate.clone(),
            resolution: resolution.clone(),
            archive: archive_id.to_bytes(),
            tree: tree
                .entries()
                .iter()
                .map(|entry| ForgeJournalEntry {
                    path: Arc::clone(&entry.path),
                    object: entry.object.to_bytes(),
                    mode: entry.mode,
                })
                .collect(),
            metadata: metadata.clone(),
            manifests: manifests.clone(),
            source,
            cursor,
            facts_frontier,
            base_snapshot: base.id().to_bytes(),
            target_snapshot: target.id().to_bytes(),
            delta: delta.id().to_bytes(),
            observed_at_millis: receipt.observed_at_millis,
        };
        self.commit_record(record)?;
        Ok(ForgeAcquisitionResult {
            coordinate: coordinate.clone(),
            resolution,
            archive: archive_id,
            tree,
            metadata,
            manifests: manifests.into_boxed_slice(),
            snapshot: target,
            delta,
            receipt,
        })
    }

    fn commit_record(&self, record: ForgeJournalRecord) -> Result<(), ForgeAcquisitionError> {
        self.journal
            .append(&ForgeJournalEvent::Published(record.clone()))
            .map_err(ForgeAcquisitionError::Journal)?;
        self.catalog
            .lock()
            .map_err(|_| ForgeAcquisitionError::Corrupt)?
            .insert(
                record.coordinate.identity(),
                ForgeJournalEvent::Published(record),
            );
        Ok(())
    }

    fn commit_tombstone(
        &self,
        coordinate: &ForgeCoordinate,
        reason: ForgeRejectReason,
    ) -> Result<(), ForgeAcquisitionError> {
        let event = ForgeJournalEvent::Tombstone {
            coordinate: coordinate.clone(),
            reason,
        };
        self.journal
            .append(&event)
            .map_err(ForgeAcquisitionError::Journal)?;
        self.catalog
            .lock()
            .map_err(|_| ForgeAcquisitionError::Corrupt)?
            .insert(coordinate.identity(), event);
        Ok(())
    }
}
