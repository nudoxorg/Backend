//! Canonical sparse input transfer lifecycle.

use super::{
    AdmittedChunk, AuthorityClaim, BoundedFileImage, Frame, InputCas, MAX_ACTIVE_SESSIONS,
    ObjectKey, ObjectRequest, ObjectVersion, OpenOptions, PersistedTransfer, ReceivingCas,
    ReceivingCheckpoint, ReplicationError, Schema, TransferId, UnverifiedObjectRequest,
    WireReceivingCheckpoint, Write, fs,
};
use std::path::Path;

const RECEIVING_MARKER_MAGIC: &[u8; 4] = b"IRT1";
const RECEIVING_MARKER_VERSION: u16 = 1;
const RECEIVING_MARKER_OVERHEAD: usize = 4 + 2 + 32 + 4 + 32;

fn receiving_marker_path(directory: &Path, transfer: TransferId) -> std::path::PathBuf {
    directory.join(format!(".recv-{:016x}", transfer.get()))
}

fn transfer_from_marker_name(name: &str) -> Result<TransferId, ReplicationError> {
    let encoded = name
        .strip_prefix(".recv-")
        .ok_or(ReplicationError::CorruptFrame)?;
    if encoded.len() != 16 || !encoded.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ReplicationError::CorruptFrame);
    }
    let value = u64::from_str_radix(encoded, 16).map_err(|_| ReplicationError::CorruptFrame)?;
    TransferId::new(value)
}

fn transfer_from_part_name(name: &str) -> Option<TransferId> {
    let encoded = name.strip_prefix('.')?.strip_suffix(".part")?;
    if encoded.len() != 16 || !encoded.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    TransferId::new(u64::from_str_radix(encoded, 16).ok()?).ok()
}

fn encode_receiving_marker(root: [u8; 32], checkpoint: &[u8]) -> Result<Vec<u8>, ReplicationError> {
    let checkpoint_len = u32::try_from(checkpoint.len()).map_err(|_| ReplicationError::Overflow)?;
    let length = RECEIVING_MARKER_OVERHEAD
        .checked_add(checkpoint.len())
        .ok_or(ReplicationError::Overflow)?;
    let mut marker = Vec::new();
    marker
        .try_reserve_exact(length)
        .map_err(|_| ReplicationError::Backpressure)?;
    marker.extend_from_slice(RECEIVING_MARKER_MAGIC);
    marker.extend_from_slice(&RECEIVING_MARKER_VERSION.to_be_bytes());
    marker.extend_from_slice(&root);
    marker.extend_from_slice(&checkpoint_len.to_be_bytes());
    marker.extend_from_slice(checkpoint);
    let checksum = backend_engine::blake3::hash(&marker);
    marker.extend_from_slice(checksum.as_bytes());
    Ok(marker)
}

fn decode_receiving_marker(bytes: &[u8]) -> Result<([u8; 32], &[u8]), ReplicationError> {
    if bytes.len() < RECEIVING_MARKER_OVERHEAD
        || &bytes[..4] != RECEIVING_MARKER_MAGIC
        || u16::from_be_bytes([bytes[4], bytes[5]]) != RECEIVING_MARKER_VERSION
    {
        return Err(ReplicationError::CorruptFrame);
    }
    let checksum_at = bytes.len() - 32;
    if backend_engine::blake3::hash(&bytes[..checksum_at]).as_bytes() != &bytes[checksum_at..] {
        return Err(ReplicationError::CorruptFrame);
    }
    let root = bytes[6..38]
        .try_into()
        .map_err(|_| ReplicationError::CorruptFrame)?;
    let checkpoint_len = u32::from_be_bytes(
        bytes[38..42]
            .try_into()
            .map_err(|_| ReplicationError::CorruptFrame)?,
    ) as usize;
    let checkpoint_end = 42_usize
        .checked_add(checkpoint_len)
        .ok_or(ReplicationError::Overflow)?;
    if checkpoint_end != checksum_at {
        return Err(ReplicationError::CorruptFrame);
    }
    Ok((root, &bytes[42..checkpoint_end]))
}

impl<T: Schema> InputCas<T> {
    /// Loads durable sparse-transfer checkpoints after a process restart.
    /// Checkpoints are bounded and checksummed; incomplete temporary marker
    /// files are ignored because the previous atomic checkpoint remains the
    /// only resumable state.
    /// # Errors
    /// Returns an error when durable checkpoint state is malformed or exceeds
    /// the transfer and sidecar budgets.
    pub(super) fn load_transfer_checkpoints(&mut self) -> Result<(), ReplicationError> {
        let Some(directory) = self.sink.root.clone() else {
            return Ok(());
        };
        let maximum =
            WireReceivingCheckpoint::<T>::max_encoded_len(self.limits.max_ranges, self.max_extents)
                .and_then(|bytes| bytes.checked_add(RECEIVING_MARKER_OVERHEAD))
                .ok_or(ReplicationError::Overflow)?;
        for entry in fs::read_dir(&directory).map_err(|_| ReplicationError::Disconnected)? {
            let entry = entry.map_err(|_| ReplicationError::Disconnected)?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !name.starts_with(".recv-") {
                continue;
            }
            if name.ends_with(".tmp") {
                let _ = fs::remove_file(entry.path());
                continue;
            }
            let transfer = transfer_from_marker_name(&name)?;
            let Some(image) = BoundedFileImage::read_optional(&entry.path(), maximum)? else {
                continue;
            };
            let (root, checkpoint_bytes) = decode_receiving_marker(image.as_slice())?;
            let maximum_checkpoint = maximum
                .checked_sub(RECEIVING_MARKER_OVERHEAD)
                .ok_or(ReplicationError::Overflow)?;
            let checkpoint = WireReceivingCheckpoint::<T>::decode_bounded(
                checkpoint_bytes,
                maximum_checkpoint,
                self.limits,
                self.max_extents,
            )?;
            if checkpoint.transfer != transfer {
                return Err(ReplicationError::CorruptFrame);
            }
            if self.partial.len() >= MAX_ACTIVE_SESSIONS {
                return Err(ReplicationError::Backpressure);
            }
            match self
                .sink
                .restore_partial_reservation(transfer, checkpoint.len)
            {
                Ok(()) => {}
                Err(ReplicationError::CorruptFrame) => {
                    // A missing staging file is expected if the process
                    // stopped after publishing the complete object but
                    // before retiring its checkpoint. The next replay checks
                    // the committed object and cleans this marker.
                    let part = directory.join(format!(".{:016x}.part", transfer.get()));
                    match fs::symlink_metadata(&part) {
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Ok(metadata)
                            if metadata.is_file() && !metadata.file_type().is_symlink() =>
                        {
                            return Err(ReplicationError::CorruptFrame);
                        }
                        _ => return Err(ReplicationError::CorruptFrame),
                    }
                }
                Err(error) => return Err(error),
            }
            self.partial
                .insert(transfer, PersistedTransfer { root, checkpoint });
        }
        self.remove_orphan_transfer_parts(&directory)?;
        Ok(())
    }

    fn remove_orphan_transfer_parts(&mut self, directory: &Path) -> Result<(), ReplicationError> {
        for entry in fs::read_dir(directory).map_err(|_| ReplicationError::Disconnected)? {
            let entry = entry.map_err(|_| ReplicationError::Disconnected)?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(transfer) = transfer_from_part_name(&name) else {
                continue;
            };
            if !self.partial.contains_key(&transfer) {
                self.sink.discard_partial(transfer)?;
            }
        }
        Ok(())
    }

    /// Receives one frame against a root-authenticated object claim while
    /// keeping its key and version untrusted until the full sparse object has
    /// been streamed through the canonical identity hashers.
    /// # Errors
    /// Returns an error when the frame, checkpoint, root, identity, authority,
    /// range, chain, or durable state is invalid.
    pub fn ingest_authenticated_frame(
        &mut self,
        root: backend_engine::MerkleRoot,
        frame: Frame<T>,
        authority: AuthorityClaim,
    ) -> Result<bool, ReplicationError> {
        frame.validate(self.limits)?;
        if frame.authority != authority {
            return Err(ReplicationError::StaleAuthority);
        }
        let version_claim =
            backend_engine::schema_object_version_identity_claim::<T>(frame.version.as_bytes())
                .map_err(ReplicationError::from)?;
        if !self.declared_missing(root, version_claim)? {
            return Err(ReplicationError::IdentityMismatch);
        }
        let transfer = frame.transfer;
        let request = UnverifiedObjectRequest::new(
            transfer,
            frame.key,
            frame.version,
            frame.object_len,
            self.limits,
        )?;
        if let Some(partial) = self.partial.get(&transfer)
            && partial.root == root.digest().as_bytes()
            && (partial.checkpoint.key != request.key
                || partial.checkpoint.version != request.version
                || partial.checkpoint.len != request.len)
        {
            // The same closure and transfer ID cannot be rebound to a
            // different object claim. Keep the valid staged extents intact
            // so a rejected replay cannot erase another in-flight object.
            return Err(ReplicationError::ReplayConflict);
        }
        if self.sink.contains_claims(frame.key, frame.version)? {
            self.discard_transfer(transfer)?;
            return Ok(true);
        }
        if !self.partial_matches(root, &request, authority) {
            self.discard_transfer(transfer)?;
        }
        if !self.active.contains_key(&transfer) {
            if self.active.len() >= MAX_ACTIVE_SESSIONS {
                return Err(ReplicationError::Backpressure);
            }
            let session = if let Some(partial) = self.partial.remove(&transfer) {
                match ReceivingCas::resume_unverified(
                    request,
                    authority,
                    self.limits,
                    self.max_extents,
                    partial.checkpoint,
                    &mut self.sink,
                ) {
                    Ok(session) => session,
                    Err(ReplicationError::Disconnected | ReplicationError::CorruptFrame) => {
                        self.sink.discard_partial(transfer)?;
                        self.retire_transfer_checkpoint(transfer)?;
                        ReceivingCas::new_unverified(
                            request,
                            authority,
                            self.limits,
                            self.max_extents,
                            &mut self.sink,
                        )?
                    }
                    Err(error) => return Err(error),
                }
            } else {
                ReceivingCas::new_unverified(
                    request,
                    authority,
                    self.limits,
                    self.max_extents,
                    &mut self.sink,
                )?
            };
            self.active.insert(transfer, session);
        }
        let session = self
            .active
            .get_mut(&transfer)
            .ok_or(ReplicationError::Disconnected)?;
        session.stage(&mut self.sink, frame.admit(self.limits)?)?;
        let checkpoint = session.checkpoint_wire()?;
        self.persist_transfer_checkpoint(root, transfer, checkpoint.clone())?;
        self.partial.insert(
            transfer,
            PersistedTransfer {
                root: root.digest().as_bytes(),
                checkpoint: checkpoint.clone(),
            },
        );
        if !checkpoint.coverage.is_complete(checkpoint.len) {
            return Ok(false);
        }
        let session = self
            .active
            .remove(&transfer)
            .ok_or(ReplicationError::Disconnected)?;
        let result = session.finish(&mut self.sink);
        self.retire_transfer_checkpoint(transfer)?;
        result.map(|_| true)
    }

    fn partial_matches(
        &self,
        root: backend_engine::MerkleRoot,
        request: &UnverifiedObjectRequest<T>,
        authority: AuthorityClaim,
    ) -> bool {
        self.partial.get(&request.transfer).is_some_and(|partial| {
            partial.root == root.digest().as_bytes()
                && partial.checkpoint.key == request.key
                && partial.checkpoint.version == request.version
                && partial.checkpoint.len == request.len
                && partial.checkpoint.authority == authority
        })
    }

    fn persist_transfer_checkpoint(
        &mut self,
        root: backend_engine::MerkleRoot,
        transfer: TransferId,
        checkpoint: WireReceivingCheckpoint<T>,
    ) -> Result<(), ReplicationError> {
        let Some(directory) = self.sink.root.clone() else {
            return Err(ReplicationError::Disconnected);
        };
        let maximum =
            WireReceivingCheckpoint::<T>::max_encoded_len(self.limits.max_ranges, self.max_extents)
                .ok_or(ReplicationError::Overflow)?;
        let checkpoint_bytes = checkpoint.encode_bounded(maximum, self.limits, self.max_extents)?;
        let marker = encode_receiving_marker(root.digest().as_bytes(), &checkpoint_bytes)?;
        let target = receiving_marker_path(&directory, transfer);
        let temporary = directory.join(format!(".recv-{:016x}.tmp", transfer.get()));
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(ReplicationError::Disconnected),
        }
        let (target_exists, old_len) = match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                (true, metadata.len())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (false, 0),
            _ => return Err(ReplicationError::CorruptFrame),
        };
        let new_len = u64::try_from(marker.len()).map_err(|_| ReplicationError::Overflow)?;
        self.ensure_sidecar_budget(usize::from(!target_exists), new_len.saturating_sub(old_len))?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|_| ReplicationError::Disconnected)?;
        file.write_all(&marker)
            .and_then(|()| file.sync_all())
            .map_err(|_| ReplicationError::Disconnected)?;
        fs::rename(&temporary, &target).map_err(|_| ReplicationError::Disconnected)?;
        backend_platform::durability::open_directory(&directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| ReplicationError::Disconnected)?;
        self.partial.insert(
            transfer,
            PersistedTransfer {
                root: root.digest().as_bytes(),
                checkpoint,
            },
        );
        Ok(())
    }

    fn retire_transfer_checkpoint(&mut self, transfer: TransferId) -> Result<(), ReplicationError> {
        self.partial.remove(&transfer);
        let Some(directory) = self.sink.root.as_ref() else {
            return Ok(());
        };
        let path = receiving_marker_path(directory, transfer);
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(ReplicationError::Disconnected),
        }
        backend_platform::durability::open_directory(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| ReplicationError::Disconnected)
    }

    pub(super) fn discard_transfer(
        &mut self,
        transfer: TransferId,
    ) -> Result<(), ReplicationError> {
        if let Some(session) = self.active.remove(&transfer) {
            session.abort(&mut self.sink);
        } else {
            self.sink.discard_partial(transfer)?;
        }
        self.retire_transfer_checkpoint(transfer)
    }

    /// Starts a canonical sparse transfer.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn begin(
        &mut self,
        request: ObjectRequest<T>,
        authority: AuthorityClaim,
    ) -> Result<(), ReplicationError> {
        let transfer = request.transfer;
        if !self.active.contains_key(&transfer) && self.active.len() >= MAX_ACTIVE_SESSIONS {
            return Err(ReplicationError::Backpressure);
        }
        let session = ReceivingCas::new(
            request,
            authority,
            self.limits,
            self.max_extents,
            &mut self.sink,
        )?;
        self.active.insert(transfer, session);
        Ok(())
    }

    /// Admits a complete one-frame object when the object itself is the proof
    /// preimage. This is used for the small product relation-root input; large
    /// values use [`Self::begin`], [`Self::stage`], and a durable checkpoint.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn ingest_complete_frame(
        &mut self,
        frame: Frame<T>,
        authority: AuthorityClaim,
    ) -> Result<ObjectVersion<T>, ReplicationError>
    where
        T: Schema<Value = [u8]>,
    {
        frame.validate(self.limits)?;
        if frame.offset != 0 || frame.object_len != frame.payload.len() as u64 {
            return Err(ReplicationError::Range);
        }
        let transfer = frame.transfer;
        let key = ObjectKey::<T>::admit_value(frame.key.into_untrusted(), &frame.payload)
            .map_err(|_| ReplicationError::IdentityMismatch)?;
        let version =
            ObjectVersion::<T>::admit_value(frame.version.into_untrusted(), &frame.payload)
                .map_err(|_| ReplicationError::IdentityMismatch)?;
        let request = ObjectRequest::whole(
            frame.transfer,
            key,
            version,
            frame.object_len,
            self.limits.max_ranges,
        )?;
        self.begin(request, authority)?;
        let admitted = frame.admit(self.limits)?;
        self.stage(admitted)?;
        self.finish(transfer)
    }

    /// Stages one bounded frame against an already authenticated object
    /// summary. This is the multi-frame sibling of
    /// [`Self::ingest_complete_frame`]: no bytes are concatenated while a
    /// transfer is in flight, and completion remains gated by the canonical
    /// receiving typestate.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn ingest_frame(
        &mut self,
        frame: Frame<T>,
        key: ObjectKey<T>,
        version: ObjectVersion<T>,
        authority: AuthorityClaim,
    ) -> Result<Option<ObjectVersion<T>>, ReplicationError> {
        frame.validate(self.limits)?;
        if frame.key
            != backend_engine::claim_schema_object_key(key).map_err(ReplicationError::from)?
            || frame.version
                != backend_engine::claim_schema_object_version(version)
                    .map_err(ReplicationError::from)?
            || frame.object_len == 0
        {
            return Err(ReplicationError::IdentityMismatch);
        }
        let transfer = frame.transfer;
        let object_len = frame.object_len;
        if !self.active.contains_key(&frame.transfer) {
            if self.active.len() >= MAX_ACTIVE_SESSIONS {
                return Err(ReplicationError::Backpressure);
            }
            let request = ObjectRequest::whole(
                frame.transfer,
                key,
                version,
                frame.object_len,
                self.limits.max_ranges,
            )?;
            self.begin(request, authority)?;
        }
        self.stage(frame.admit(self.limits)?)?;
        if !self.checkpoint(transfer)?.coverage.is_complete(object_len) {
            return Ok(None);
        }
        match self.finish(transfer) {
            Ok(version) => Ok(Some(version)),
            Err(error) => Err(error),
        }
    }

    /// Admits and stages a wire chunk against the retained typed request.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn stage(&mut self, chunk: AdmittedChunk<T>) -> Result<(), ReplicationError> {
        let transfer = chunk.transfer();
        let session = self
            .active
            .get_mut(&transfer)
            .ok_or(ReplicationError::Disconnected)?;
        session.stage(&mut self.sink, chunk).map(|_| ())
    }

    /// Reopens a checkpoint recovered from the owner journal after process
    /// restart. The canonical checkpoint validator rechecks every identity,
    /// authority, extent, and coverage claim before any bytes are exposed.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn resume(
        &mut self,
        request: ObjectRequest<T>,
        authority: AuthorityClaim,
        checkpoint: ReceivingCheckpoint<T>,
    ) -> Result<(), ReplicationError> {
        let transfer = request.transfer;
        if self.active.contains_key(&transfer) {
            return Err(ReplicationError::ReplayConflict);
        }
        let session = ReceivingCas::resume(
            request,
            authority,
            self.limits,
            self.max_extents,
            checkpoint,
            &mut self.sink,
        )?;
        self.active.insert(transfer, session);
        Ok(())
    }

    /// Returns a byte-free durable checkpoint for reconnect/restart handoff.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn checkpoint(
        &self,
        transfer: TransferId,
    ) -> Result<ReceivingCheckpoint<T>, ReplicationError> {
        self.active
            .get(&transfer)
            .ok_or(ReplicationError::Disconnected)?
            .checkpoint()
    }

    /// Finishes a transfer and atomically publishes its canonical object.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn finish(&mut self, transfer: TransferId) -> Result<ObjectVersion<T>, ReplicationError> {
        let session = self
            .active
            .remove(&transfer)
            .ok_or(ReplicationError::Disconnected)?;
        session.finish(&mut self.sink)
    }

    /// Aborts an unpublished transfer while retaining committed objects.
    pub fn abort(&mut self, transfer: TransferId) -> Result<(), ReplicationError> {
        self.discard_transfer(transfer)
    }
}
