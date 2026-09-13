//! Root leases, retention budgets, and durable mark-and-sweep for the input CAS.

use super::{
    Arc, BTreeSet, BoundedFileImage, CasGcBudget, File, InputCas, MAX_GC_BYTES_PER_CALL,
    MAX_GC_FILES_PER_CALL, MAX_RETAINED_OBJECTS, MAX_RETAINED_SIDECAR_BYTES, MAX_RETAINED_SIDECARS,
    ObjectVersion, OpenOptions, Path, ReplicationError, Schema, WireIdentity, Write, decode_hex,
    digest_file, fs, hex, is_object_name,
};

/// Result of one bounded input-CAS mark and sweep.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CasGcReport {
    /// Canonical object files removed from the receiving namespace.
    pub objects_removed: usize,
    /// Bytes reclaimed from canonical object files.
    pub bytes_removed: u64,
    /// Root journals, proof markers, or abandoned staging files removed.
    pub sidecars_removed: usize,
    /// Bytes reclaimed from those root-scoped sidecars.
    pub sidecar_bytes_removed: u64,
}

type ObjectClaimBytes<T> = Result<Option<(ObjectVersion<T>, Arc<[u8]>)>, ReplicationError>;

/// Physical work allowed for one mark-and-sweep call. The object and sidecar
/// namespaces draw from the same limits, so a caller can safely run GC from a
/// request loop without allowing one category to multiply the work bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CasGcLimits {
    /// Maximum files removed by one call.
    pub max_files: usize,
    /// Maximum bytes removed by one call.
    pub max_bytes: u64,
}

impl Default for CasGcLimits {
    fn default() -> Self {
        Self {
            max_files: MAX_GC_FILES_PER_CALL,
            max_bytes: MAX_GC_BYTES_PER_CALL,
        }
    }
}

/// The durable root lease that keeps a received closure live across restart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CasRootLease {
    root: backend_engine::MerkleRoot,
    epoch: u64,
}

impl CasRootLease {
    /// Returns the exact authenticated closure root.
    #[must_use]
    pub const fn root(self) -> backend_engine::MerkleRoot {
        self.root
    }

    /// Returns the monotone owner epoch for this lease.
    #[must_use]
    pub const fn epoch(self) -> u64 {
        self.epoch
    }
}

impl<T: Schema> InputCas<T> {
    /// Returns the last authenticated Merkle root, if one is durable.
    #[must_use]
    pub const fn admitted_root(&self) -> Option<backend_engine::MerkleRoot> {
        self.admitted_root
    }

    /// Returns the currently durable root lease and its monotone epoch.
    #[must_use]
    pub const fn root_lease(&self) -> Option<CasRootLease> {
        match self.admitted_root {
            Some(root) if self.root_epoch != 0 => Some(CasRootLease {
                root,
                epoch: self.root_epoch,
            }),
            _ => None,
        }
    }

    /// Performs a bounded mark and sweep from the current root lease.
    ///
    /// Object versions, node markers, proofs, and root-scoped input journals
    /// are retained only while reachable from that lease.  A missing or
    /// malformed live-root journal fails closed rather than guessing which
    /// object bytes are safe to delete.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn reclaim_unleased(&mut self) -> Result<CasGcReport, ReplicationError> {
        self.reclaim_unleased_with_limits(CasGcLimits::default())
    }

    /// Performs one bounded sweep with an explicit global work budget.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn reclaim_unleased_with_limits(
        &mut self,
        limits: CasGcLimits,
    ) -> Result<CasGcReport, ReplicationError> {
        if limits.max_files == 0 || limits.max_bytes == 0 {
            return Err(ReplicationError::InvalidLimits);
        }
        let live_root = self.admitted_root;
        let live_root_digest = live_root.map(|root| root.digest().as_bytes()).or_else(|| {
            self.durable_root_claim
                .map(|claim| claim.digest().as_bytes())
        });
        let live_versions = self.live_versions(live_root_digest)?;
        let mut budget = CasGcBudget::new(limits);
        let (objects_removed, bytes_removed) =
            self.sink.reclaim_unleased(&live_versions, &mut budget)?;
        let mut report = CasGcReport {
            objects_removed,
            bytes_removed,
            sidecars_removed: 0,
            sidecar_bytes_removed: 0,
        };
        if let Some(directory) = self.sink.root.clone() {
            let (removed_objects, removed_bytes) =
                self.reclaim_sidecars(&directory, live_root_digest, &mut budget)?;
            report.sidecars_removed = removed_objects;
            report.sidecar_bytes_removed = removed_bytes;
        }
        Ok(report)
    }

    /// Returns the committed object count and bytes currently retained by
    /// this worker namespace.  Staged transfers are accounted separately and
    /// cannot push the namespace past the admission budget.
    #[must_use]
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn retained_usage(&self) -> (usize, u64) {
        self.sink.usage()
    }

    pub(super) fn within_sidecar_budget(&self) -> Result<bool, ReplicationError> {
        let (count, bytes) = self.sidecar_usage()?;
        Ok(count <= MAX_RETAINED_SIDECARS && bytes <= MAX_RETAINED_SIDECAR_BYTES)
    }

    pub(super) fn ensure_sidecar_budget(
        &self,
        additional_count: usize,
        additional_bytes: u64,
    ) -> Result<(), ReplicationError> {
        let (count, bytes) = self.sidecar_usage()?;
        let count = count
            .checked_add(additional_count)
            .ok_or(ReplicationError::Overflow)?;
        let bytes = bytes
            .checked_add(additional_bytes)
            .ok_or(ReplicationError::Overflow)?;
        if count > MAX_RETAINED_SIDECARS || bytes > MAX_RETAINED_SIDECAR_BYTES {
            Err(ReplicationError::Backpressure)
        } else {
            Ok(())
        }
    }

    fn sidecar_usage(&self) -> Result<(usize, u64), ReplicationError> {
        let Some(directory) = self.sink.root.as_ref() else {
            return Ok((0, 0));
        };
        let mut count = 0_usize;
        let mut bytes = 0_u64;
        for entry in fs::read_dir(directory)
            .map_err(|_| ReplicationError::Disconnected)?
            .take(MAX_GC_FILES_PER_CALL)
        {
            let entry = entry.map_err(|_| ReplicationError::Disconnected)?;
            let metadata = entry
                .metadata()
                .map_err(|_| ReplicationError::Disconnected)?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !metadata.is_file() || is_object_name(&name) {
                continue;
            }
            count = count.checked_add(1).ok_or(ReplicationError::Overflow)?;
            bytes = bytes
                .checked_add(metadata.len())
                .ok_or(ReplicationError::Overflow)?;
        }
        Ok((count, bytes))
    }

    fn live_versions(
        &self,
        root_digest: Option<[u8; 32]>,
    ) -> Result<BTreeSet<[u8; 32]>, ReplicationError> {
        let Some(root_digest) = root_digest else {
            return Ok(self
                .root_claims_memory
                .values()
                .flatten()
                .copied()
                .collect());
        };
        let mut claims = match self.root_claims_memory.get(&root_digest) {
            Some(claims) => claims.clone(),
            None => BTreeSet::new(),
        };
        let Some(directory) = self.sink.root.as_ref() else {
            return Ok(claims);
        };
        let root_name = hex(root_digest);
        let claims_path = directory.join(format!(".claims-{root_name}"));
        match BoundedFileImage::read_optional(&claims_path, MAX_RETAINED_OBJECTS * 32)? {
            Some(bytes) => {
                let bytes = bytes.as_slice();
                if bytes.len() % 32 != 0 || bytes.len() / 32 > MAX_RETAINED_OBJECTS {
                    return Err(ReplicationError::CoverageLimit);
                }
                for claim in bytes.chunks_exact(32) {
                    let claim: [u8; 32] = claim
                        .try_into()
                        .map_err(|_| ReplicationError::CorruptFrame)?;
                    claims.insert(claim);
                }
            }
            None => {}
        }
        let input_path = directory.join(format!(".input-{root_name}"));
        match BoundedFileImage::read_optional(&input_path, 32)? {
            Some(bytes) => {
                let claim: [u8; 32] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| ReplicationError::CorruptFrame)?;
                claims.insert(claim);
            }
            None => {}
        }
        let need_path = directory.join(format!(".need-{root_name}"));
        match BoundedFileImage::read_optional(&need_path, MAX_RETAINED_OBJECTS * 32)? {
            Some(bytes) => {
                let bytes = bytes.as_slice();
                if bytes.len() % 32 != 0 || bytes.len() / 32 > MAX_RETAINED_OBJECTS {
                    return Err(ReplicationError::CoverageLimit);
                }
                for claim in bytes.chunks_exact(32) {
                    let claim: [u8; 32] = claim
                        .try_into()
                        .map_err(|_| ReplicationError::CorruptFrame)?;
                    claims.insert(claim);
                }
            }
            None => {}
        }
        Ok(claims)
    }

    fn reclaim_sidecars(
        &self,
        directory: &Path,
        live_root_digest: Option<[u8; 32]>,
        budget: &mut CasGcBudget,
    ) -> Result<(usize, u64), ReplicationError> {
        let root_name = live_root_digest.map(hex);
        let mut removed_objects = 0_usize;
        let mut removed_bytes = 0_u64;
        for entry in fs::read_dir(directory).map_err(|_| ReplicationError::Disconnected)? {
            let entry = entry.map_err(|_| ReplicationError::Disconnected)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let active_transfer_part = self
                .active
                .keys()
                .any(|transfer| name == format!(".{:016x}.part", transfer.get()));
            let remove = if let Some(digest) = name.strip_prefix(".proof-") {
                decode_hex(digest).is_none_or(|digest| {
                    live_root_digest.is_none() || !self.node_index.contains(digest)
                })
            } else if let Some(digest) = name.strip_prefix(".node-") {
                // Canonical node preimages are an immutable cross-root cache.
                // A completion marker is published only after that exact
                // preimage has been admitted and fsynced, so any later root may
                // safely reuse it by digest. The sidecar budget bounds this
                // cache; dropping the final root lease sweeps it completely.
                live_root_digest.is_none() && decode_hex(digest).is_some()
            } else if let Some(root_name) = root_name.as_deref() {
                let input_name = format!(".input-{root_name}");
                let claims_name = format!(".claims-{root_name}");
                let need_prefix = format!(".need-{root_name}");
                let need_count_prefix = format!(".need-count-{root_name}");
                let need_received_prefix = format!(".need-received-{root_name}");
                let need_receipt_prefix = format!(".need-receipt-{root_name}-");
                (name.starts_with(".input-") && name != input_name)
                    || (name.starts_with(".claims-") && name != claims_name)
                    || (name.starts_with(".need-")
                        && !name.starts_with(&need_prefix)
                        && !name.starts_with(&need_count_prefix)
                        && !name.starts_with(&need_received_prefix)
                        && !name.starts_with(&need_receipt_prefix))
                    || name == ".ROOT.part"
                    || name == ".WORKSPACE.part"
                    || name == ".WORKSPACE_MANIFEST.part"
                    || (Path::new(name)
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("part"))
                        && !active_transfer_part)
            } else {
                name.starts_with(".input-")
                    || name.starts_with(".claims-")
                    || name.starts_with(".need-")
                    || name.starts_with(".proof-")
                    || name.starts_with(".node-")
                    || (Path::new(name)
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("part"))
                        && !active_transfer_part)
            };
            if !remove {
                continue;
            }
            let metadata = entry
                .metadata()
                .map_err(|_| ReplicationError::Disconnected)?;
            if metadata.is_file() {
                if !budget.take(metadata.len()) {
                    break;
                }
                fs::remove_file(entry.path()).map_err(|_| ReplicationError::Disconnected)?;
                removed_objects = removed_objects
                    .checked_add(1)
                    .ok_or(ReplicationError::Overflow)?;
                removed_bytes = removed_bytes
                    .checked_add(metadata.len())
                    .ok_or(ReplicationError::Overflow)?;
            }
        }
        File::open(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| ReplicationError::Disconnected)?;
        Ok((removed_objects, removed_bytes))
    }

    /// Tests whether an authenticated node closure was completely received in
    /// a prior session. Node digests are content addressed and therefore can
    /// be reused across roots and reconnects.
    #[must_use]
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn contains_node(&self, digest: [u8; 32]) -> bool {
        self.node_index.contains(digest)
    }

    /// Publishes a canonical node-preimage marker atomically. The admitted
    /// proof is itself the complete executable value for this content digest;
    /// row-object summaries are transport metadata and are not duplicated in
    /// the receiving CAS.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn record_node(&mut self, digest: [u8; 32]) -> Result<(), ReplicationError> {
        if !self.node_index.contains(digest) {
            self.ensure_sidecar_budget(1, 32)?;
        }
        self.node_index.record(digest)
    }

    /// Stores the canonical preimage that authenticated a node page. Proofs
    /// are content addressed separately from completion markers: a proof may
    /// be durable while its subtree is still incomplete after a crash.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn record_node_proof(
        &mut self,
        digest: [u8; 32],
        proof: &[u8],
    ) -> Result<(), ReplicationError> {
        if proof.is_empty() || proof.len() > 64 * 1024 {
            return Err(ReplicationError::MessageTooLarge);
        }
        let Some(directory) = self.sink.root.as_ref() else {
            return Ok(());
        };
        let temporary = directory.join(format!(".proof-{}.part", hex(digest)));
        let target = directory.join(format!(".proof-{}", hex(digest)));
        if target.is_file() {
            let existing = BoundedFileImage::read_optional(&target, 64 * 1024)?
                .ok_or(ReplicationError::Disconnected)?;
            if existing.as_slice() != proof {
                return Err(ReplicationError::CorruptFrame);
            }
            return Ok(());
        }
        self.ensure_sidecar_budget(1, proof.len() as u64)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|_| ReplicationError::Disconnected)?;
        file.write_all(proof)
            .and_then(|()| file.sync_all())
            .map_err(|_| ReplicationError::Disconnected)?;
        fs::rename(temporary, target).map_err(|_| ReplicationError::Disconnected)?;
        File::open(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| ReplicationError::Disconnected)
    }

    /// Reads a bounded canonical node proof without scanning the proof index.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn node_proof(&self, digest: [u8; 32]) -> Result<Option<Vec<u8>>, ReplicationError> {
        let Some(directory) = self.sink.root.as_ref() else {
            return Ok(None);
        };
        let path = directory.join(format!(".proof-{}", hex(digest)));
        Ok(BoundedFileImage::read_optional(&path, 64 * 1024)?.map(BoundedFileImage::into_vec))
    }

    /// Returns an immutable warm input without copying it when cached.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn get(
        &mut self,
        version: ObjectVersion<T>,
    ) -> Result<Option<Arc<[u8]>>, ReplicationError> {
        self.sink.read(version)
    }

    /// Checks immutable presence without reading object bytes into memory.
    #[must_use]
    pub fn contains(&self, version: ObjectVersion<T>) -> bool {
        self.sink.contains(version)
    }

    /// Checks presence from an authenticated schema-version wire claim without
    /// loading the object body.
    #[must_use]
    pub fn contains_claim(&self, claim: WireIdentity) -> bool {
        if claim.context() != backend_engine::IdContext::schema::<T>() {
            return false;
        }
        if self
            .sink
            .committed
            .keys()
            .any(|version| version.as_bytes() == &claim.as_bytes())
        {
            return true;
        }
        self.sink.root.as_ref().is_some_and(|root| {
            let path = root.join(hex(claim.as_bytes()));
            let Ok(metadata) = fs::metadata(&path) else {
                return false;
            };
            metadata.is_file()
                && digest_file::<T>(&path, metadata.len())
                    .is_ok_and(|digest| digest == claim.as_bytes())
        })
    }

    /// Looks up a wire claim only after recomputing its typed version digest
    /// from the retained bytes. A context match by itself never grants a
    /// typed object.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn get_claim(&mut self, claim: WireIdentity) -> ObjectClaimBytes<T>
    where
        T: Schema<Value = [u8]>,
    {
        if claim.context() != backend_engine::IdContext::schema::<T>() {
            return Err(ReplicationError::IdentityContext);
        }
        let Some(root) = self.sink.root.clone() else {
            return Ok(None);
        };
        let path = root.join(hex(claim.as_bytes()));
        if !path.is_file() {
            return Ok(None);
        }
        let maximum = usize::try_from(self.limits.max_object)
            .map_err(|_| ReplicationError::MessageTooLarge)?;
        let Some(bytes) = BoundedFileImage::read_optional(&path, maximum)? else {
            return Ok(None);
        };
        let bytes: Arc<[u8]> = Arc::from(bytes.into_vec().into_boxed_slice());
        let typed_claim = backend_engine::UntrustedId::<T>::from_wire(
            &claim.as_bytes(),
            backend_engine::IdContext::schema::<T>(),
        )
        .map_err(|_| ReplicationError::CorruptFrame)?;
        let typed = ObjectVersion::<T>::admit_value(typed_claim, bytes.as_ref())
            .map_err(|_| ReplicationError::CorruptFrame)?;
        Ok(Some((typed, bytes)))
    }
}
