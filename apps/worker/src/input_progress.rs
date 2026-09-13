//! Durable page-need and receipt progress.

use super::{
    BoundedFileImage, File, InputCas, MAX_RETAINED_OBJECTS, OpenOptions, Path, Read,
    ReplicationError, Schema, Seek, SeekFrom, WireIdentity, Write, fs, hex,
};

const MAX_CLAIM_JOURNAL_BYTES: usize = MAX_RETAINED_OBJECTS * 32;

impl<T: Schema> InputCas<T> {
    fn remember_root_claim(
        &mut self,
        root_digest: [u8; 32],
        claim: WireIdentity,
    ) -> Result<(), ReplicationError> {
        let claims = self.root_claims_memory.entry(root_digest).or_default();
        if !claims.contains(&claim.as_bytes()) && claims.len() >= MAX_RETAINED_OBJECTS {
            return Err(ReplicationError::Backpressure);
        }
        claims.insert(claim.as_bytes());
        Ok(())
    }

    /// Retains one authenticated object claim in the live-root mark set.
    /// Claims are fixed width and deduplicated before append, so a hostile
    /// page walk cannot grow the mark journal without also consuming its
    /// bounded object budget.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn retain_claim(
        &mut self,
        root: backend_engine::MerkleRoot,
        claim: WireIdentity,
    ) -> Result<(), ReplicationError> {
        if claim.context() != backend_engine::IdContext::schema::<T>() {
            return Err(ReplicationError::IdentityContext);
        }
        let root_digest = root.digest().as_bytes();
        if self.sink.root.is_none()
            && self
                .root_claims_memory
                .get(&root_digest)
                .is_some_and(|claims| claims.contains(&claim.as_bytes()))
        {
            return Ok(());
        }
        let Some(directory) = self.sink.root.as_ref() else {
            let claims = self
                .root_claims_memory
                .entry(root.digest().as_bytes())
                .or_default();
            if claims.len() >= MAX_RETAINED_OBJECTS {
                return Err(ReplicationError::Backpressure);
            }
            claims.insert(claim.as_bytes());
            return Ok(());
        };
        let path = directory.join(format!(".claims-{}", hex(root.digest().as_bytes())));
        let existing = match BoundedFileImage::read_optional(&path, MAX_CLAIM_JOURNAL_BYTES)? {
            Some(bytes) => bytes.into_vec(),
            None => Vec::new(),
        };
        if existing.len() % 32 != 0 {
            return Err(ReplicationError::CorruptFrame);
        }
        if existing
            .chunks_exact(32)
            .any(|candidate| candidate == claim.as_bytes())
        {
            self.remember_root_claim(root_digest, claim)?;
            return Ok(());
        }
        if existing.len() / 32 >= MAX_RETAINED_OBJECTS {
            return Err(ReplicationError::Backpressure);
        }
        self.ensure_sidecar_budget(usize::from(!path.is_file()), 32)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|_| ReplicationError::Disconnected)?;
        file.write_all(&claim.as_bytes())
            .and_then(|()| file.sync_data())
            .map_err(|_| ReplicationError::Disconnected)?;
        self.remember_root_claim(root_digest, claim)?;
        Ok(())
    }

    /// Appends one missing immutable version to the durable need journal. The
    /// journal is fixed-width and append-only, so a large closure never
    /// becomes one allocation in the admission state machine.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn append_missing(
        &mut self,
        root: backend_engine::MerkleRoot,
        claim: WireIdentity,
    ) -> Result<(), ReplicationError> {
        if claim.context() != backend_engine::IdContext::schema::<T>() {
            return Err(ReplicationError::IdentityContext);
        }
        self.retain_claim(root, claim)?;
        let Some(directory) = self.sink.root.as_ref() else {
            let values = self
                .need_memory
                .entry(root.digest().as_bytes())
                .or_default();
            if values.len() >= self.limits.max_objects {
                return Err(ReplicationError::Backpressure);
            }
            if !values.iter().any(|digest| digest == &claim.as_bytes()) {
                values.push(claim.as_bytes());
            }
            return Ok(());
        };
        if self.declared_missing(root, claim)? {
            return Ok(());
        }
        let path = directory.join(format!(".need-{}", hex(root.digest().as_bytes())));
        let count_path = directory.join(format!(".need-count-{}", hex(root.digest().as_bytes())));
        let count = read_counter(&count_path)?
            .checked_add(1)
            .ok_or(ReplicationError::Overflow)?;
        if count > self.limits.max_objects as u64
            || usize::try_from(count).map_or(true, |count| count > MAX_RETAINED_OBJECTS)
        {
            return Err(ReplicationError::Backpressure);
        }
        let count_exists = count_path.is_file();
        self.ensure_sidecar_budget(usize::from(!count_exists), 32)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|_| ReplicationError::Disconnected)?;
        file.write_all(&claim.as_bytes())
            .and_then(|()| file.sync_data())
            .map_err(|_| ReplicationError::Disconnected)?;
        let temporary = directory.join(format!(
            ".need-count-{}.part",
            hex(root.digest().as_bytes())
        ));
        let mut count_file =
            File::create(&temporary).map_err(|_| ReplicationError::Disconnected)?;
        count_file
            .write_all(&count.to_be_bytes())
            .and_then(|()| count_file.sync_all())
            .map_err(|_| ReplicationError::Disconnected)?;
        fs::rename(temporary, count_path).map_err(|_| ReplicationError::Disconnected)
    }

    /// Starts a fresh need journal for a root offer. It is safe to remove the
    /// old journal because committed objects remain content addressed in the
    /// canonical object namespace.
    pub fn clear_missing(&mut self, root: backend_engine::MerkleRoot) {
        self.need_memory.remove(&root.digest().as_bytes());
        self.received_memory.remove(&root.digest().as_bytes());
        if let Some(directory) = self.sink.root.as_ref() {
            let path = directory.join(format!(".need-{}", hex(root.digest().as_bytes())));
            let _ = fs::remove_file(path);
            let _ = fs::remove_file(
                directory.join(format!(".need-count-{}", hex(root.digest().as_bytes()))),
            );
            let _ = fs::remove_file(
                directory.join(format!(".need-received-{}", hex(root.digest().as_bytes()))),
            );
        }
        // A currently leased root owns its claims until a replacement root
        // is durably published. Clearing a reconnect's need cursor must not
        // erase that mark set and allow live objects to be collected.
        if self.admitted_root != Some(root) {
            self.root_claims_memory.remove(&root.digest().as_bytes());
            if let Some(directory) = self.sink.root.as_ref() {
                let _ = fs::remove_file(
                    directory.join(format!(".claims-{}", hex(root.digest().as_bytes()))),
                );
            }
        }
    }

    /// Persists the exact recipe-input version discovered in a closure leaf.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn record_input_claim(
        &mut self,
        root: backend_engine::MerkleRoot,
        claim: WireIdentity,
    ) -> Result<(), ReplicationError> {
        if claim.context() != backend_engine::IdContext::schema::<T>() {
            return Err(ReplicationError::IdentityContext);
        }
        self.retain_claim(root, claim)?;
        let Some(directory) = self.sink.root.as_ref() else {
            return Ok(());
        };
        let temporary = directory.join(format!(".input-{}.part", hex(root.digest().as_bytes())));
        let target = directory.join(format!(".input-{}", hex(root.digest().as_bytes())));
        if target.is_file() {
            let existing = BoundedFileImage::read_optional(&target, 32)?
                .ok_or(ReplicationError::Disconnected)?;
            if existing.as_slice() != claim.as_bytes() {
                return Err(ReplicationError::CorruptFrame);
            }
            return Ok(());
        }
        self.ensure_sidecar_budget(1, 32)?;
        let mut file = File::create(&temporary).map_err(|_| ReplicationError::Disconnected)?;
        file.write_all(&claim.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|_| ReplicationError::Disconnected)?;
        fs::rename(&temporary, target).map_err(|_| ReplicationError::Disconnected)?;
        File::open(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| ReplicationError::Disconnected)?;
        Ok(())
    }

    /// Looks up the persisted recipe-input claim for a closure root.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn input_claim(
        &self,
        root: backend_engine::MerkleRoot,
    ) -> Result<Option<WireIdentity>, ReplicationError> {
        if self.admitted_root != Some(root)
            && !self.durable_root_matches(root)
            && !self
                .root_claims_memory
                .contains_key(&root.digest().as_bytes())
        {
            return Ok(None);
        }
        let Some(directory) = self.sink.root.as_ref() else {
            return Ok(None);
        };
        let path = directory.join(format!(".input-{}", hex(root.digest().as_bytes())));
        let bytes = match BoundedFileImage::read_optional(&path, 32)? {
            Some(bytes) => bytes.into_vec(),
            None => return Ok(None),
        };
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| ReplicationError::CorruptFrame)?;
        Ok(Some(
            backend_engine::schema_object_version_identity_claim::<T>(&bytes)
                .map_err(ReplicationError::from)?,
        ))
    }

    /// Reads one bounded need batch by byte cursor. The caller owns the cursor
    /// and can replay it after reconnect without retaining earlier batches.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn missing_batch(
        &self,
        root: backend_engine::MerkleRoot,
        cursor: u32,
        max: usize,
    ) -> Result<(Vec<WireIdentity>, Option<u32>), ReplicationError> {
        if max > self.limits.max_objects || max > MAX_RETAINED_OBJECTS {
            return Err(ReplicationError::CoverageLimit);
        }
        let root_digest = root.digest().as_bytes();
        let Some(directory) = self.sink.root.as_ref() else {
            let values = self
                .need_memory
                .get(&root_digest)
                .map_or(&[][..], Vec::as_slice);
            let start = usize::try_from(cursor).map_err(|_| ReplicationError::Overflow)?;
            if start > values.len() {
                return Err(ReplicationError::Range);
            }
            let end = start
                .checked_add(max)
                .map_or(values.len(), |candidate| candidate.min(values.len()));
            let claims = values[start..end]
                .iter()
                .map(|digest| {
                    backend_engine::schema_object_version_identity_claim::<T>(digest)
                        .map_err(ReplicationError::from)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let next = if end < values.len() {
                Some(u32::try_from(end).map_err(|_| ReplicationError::Overflow)?)
            } else {
                None
            };
            return Ok((claims, next));
        };
        let path = directory.join(format!(".need-{}", hex(root.digest().as_bytes())));
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Vec::new(), None));
            }
            Err(_) => return Err(ReplicationError::Disconnected),
        };
        let offset = u64::from(cursor)
            .checked_mul(32)
            .ok_or(ReplicationError::Overflow)?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| ReplicationError::Disconnected)?;
        let capacity = max.checked_mul(32).ok_or(ReplicationError::Overflow)?;
        let mut bytes = vec![0; capacity];
        let read = file
            .read(&mut bytes)
            .map_err(|_| ReplicationError::Disconnected)?;
        if read % 32 != 0 {
            return Err(ReplicationError::CorruptFrame);
        }
        bytes.truncate(read);
        let mut claims = Vec::with_capacity(bytes.len() / 32);
        for chunk in bytes.chunks_exact(32) {
            let digest: [u8; 32] = chunk
                .try_into()
                .map_err(|_| ReplicationError::CorruptFrame)?;
            claims.push(
                backend_engine::schema_object_version_identity_claim::<T>(&digest)
                    .map_err(ReplicationError::from)?,
            );
        }
        let read_count = u32::try_from(claims.len()).map_err(|_| ReplicationError::Overflow)?;
        let next = if claims.len() == max && read_count > 0 {
            Some(
                cursor
                    .checked_add(read_count)
                    .ok_or(ReplicationError::Overflow)?,
            )
        } else {
            None
        };
        Ok((claims, next))
    }

    /// Tests whether a version was declared by the authenticated page walk.
    /// The worker never accepts an unsolicited complete frame, even when its
    /// payload happens to hash correctly.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn declared_missing(
        &self,
        root: backend_engine::MerkleRoot,
        version: WireIdentity,
    ) -> Result<bool, ReplicationError> {
        if version.context() != backend_engine::IdContext::schema::<T>() {
            return Err(ReplicationError::IdentityContext);
        }
        if let Some(values) = self.need_memory.get(&root.digest().as_bytes()) {
            return Ok(values.iter().any(|digest| digest == &version.as_bytes()));
        }
        let Some(directory) = self.sink.root.as_ref() else {
            return Ok(false);
        };
        let path = directory.join(format!(".need-{}", hex(root.digest().as_bytes())));
        let bytes = match BoundedFileImage::read_optional(&path, MAX_CLAIM_JOURNAL_BYTES)? {
            Some(bytes) => bytes.into_vec(),
            None => return Ok(false),
        };
        if bytes.len() % 32 != 0 {
            return Err(ReplicationError::CorruptFrame);
        }
        Ok(bytes
            .chunks_exact(32)
            .any(|chunk| chunk == &version.as_bytes()[..]))
    }

    /// Checks all durable page-derived needs without materializing them.
    /// This is used only at the closure commit boundary; the normal transfer
    /// path remains cursor based and bounded.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn missing_complete(
        &self,
        root: backend_engine::MerkleRoot,
    ) -> Result<bool, ReplicationError> {
        if let Some(values) = self.need_memory.get(&root.digest().as_bytes()) {
            return Ok(values.iter().all(|digest| {
                backend_engine::schema_object_version_identity_claim::<T>(digest)
                    .is_ok_and(|claim| self.contains_claim(claim))
            }));
        }
        let Some(directory) = self.sink.root.as_ref() else {
            return Ok(true);
        };
        let count_path = directory.join(format!(".need-count-{}", hex(root.digest().as_bytes())));
        let received_path =
            directory.join(format!(".need-received-{}", hex(root.digest().as_bytes())));
        let count = read_optional_counter(&count_path)?;
        let received = read_optional_counter(&received_path)?;
        match (count, received) {
            (Some(count), Some(received)) => return Ok(count == received),
            (Some(_), None) | (None, Some(_)) => {
                return Err(ReplicationError::CorruptFrame);
            }
            (None, None) => {}
        }
        let path = directory.join(format!(".need-{}", hex(root.digest().as_bytes())));
        let bytes = match BoundedFileImage::read_optional(&path, MAX_CLAIM_JOURNAL_BYTES)? {
            Some(bytes) => bytes.into_vec(),
            None => return Ok(true),
        };
        if bytes.len() % 32 != 0 {
            return Err(ReplicationError::CorruptFrame);
        }
        for digest in bytes.chunks_exact(32) {
            let claim = backend_engine::schema_object_version_identity_claim::<T>(digest)
                .map_err(ReplicationError::from)?;
            if !self.contains_claim(claim) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Publishes one receipt in the fixed-width completion counter. Repeated
    /// receipts are idempotent across reconnects.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn mark_received(
        &mut self,
        root: backend_engine::MerkleRoot,
        version: WireIdentity,
    ) -> Result<(), ReplicationError> {
        if version.context() != backend_engine::IdContext::schema::<T>() {
            return Err(ReplicationError::IdentityContext);
        }
        self.retain_claim(root, version)?;
        let memory = self
            .received_memory
            .entry(root.digest().as_bytes())
            .or_default();
        if !memory.contains(&version.as_bytes()) && memory.len() >= self.limits.max_objects {
            return Err(ReplicationError::Backpressure);
        }
        if !memory.insert(version.as_bytes()) {
            return Ok(());
        }
        let Some(directory) = self.sink.root.as_ref() else {
            return Ok(());
        };
        let marker = directory.join(format!(
            ".need-receipt-{}-{}",
            hex(root.digest().as_bytes()),
            hex(version.as_bytes())
        ));
        if marker.exists() {
            return Ok(());
        }
        let count_path =
            directory.join(format!(".need-received-{}", hex(root.digest().as_bytes())));
        let count = read_counter(&count_path)?
            .checked_add(1)
            .ok_or(ReplicationError::Overflow)?;
        if count > self.limits.max_objects as u64
            || usize::try_from(count).map_or(true, |count| count > MAX_RETAINED_OBJECTS)
        {
            return Err(ReplicationError::Backpressure);
        }
        let count_exists = count_path.is_file();
        self.ensure_sidecar_budget(1 + usize::from(!count_exists), 32 + 8)?;
        let mut marker_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
            .map_err(|_| ReplicationError::Disconnected)?;
        marker_file
            .write_all(&version.as_bytes())
            .and_then(|()| marker_file.sync_all())
            .map_err(|_| ReplicationError::Disconnected)?;
        let temporary = directory.join(format!(
            ".need-received-{}.part",
            hex(root.digest().as_bytes())
        ));
        let mut file = File::create(&temporary).map_err(|_| ReplicationError::Disconnected)?;
        file.write_all(&count.to_be_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|_| ReplicationError::Disconnected)?;
        fs::rename(temporary, count_path).map_err(|_| ReplicationError::Disconnected)
    }
}

fn read_counter(path: &Path) -> Result<u64, ReplicationError> {
    match read_optional_counter(path)? {
        Some(value) => Ok(value),
        None => Ok(0),
    }
}

fn read_optional_counter(path: &Path) -> Result<Option<u64>, ReplicationError> {
    let bytes = match BoundedFileImage::read_optional(path, 8)? {
        Some(bytes) => bytes.into_vec(),
        None => return Ok(None),
    };
    let bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_| ReplicationError::CorruptFrame)?;
    Ok(Some(u64::from_be_bytes(bytes)))
}
