//! Receiving-state object transfer admission.

use std::{collections::BTreeMap, marker::PhantomData, sync::Arc};

use backend_version::{ObjectVersion as VersionObjectVersion, Schema};

use super::{
    AdmittedChunk, AuthorityClaim, ByteRange, CheckpointChunk, ChunkChain, ChunkRecord, Complete,
    CompleteObject, GcRoot, LeasedTransferCheckpoint, ObjectRequest, Receiving, ReplicationError,
    ResumeParts, RetainedChunk, SharedCheckpointChunk, SharedTransferCheckpoint, SparseCoverage,
    Transfer, TransferCheckpoint, TransferLease, TransferReceipt, TransportLimits,
    WireTransferCheckpoint, chain, claim_schema_object_key, claim_schema_object_version,
    validate_checkpoint_contents, validate_shared_checkpoint_contents,
};

impl<T: Schema> Transfer<Receiving, T> {
    /// Creates a transfer with explicit chunk and object bounds.
    ///
    /// # Errors
    ///
    /// Returns an object, range, or limit error when the request cannot be
    /// represented within the supplied bounds.
    pub fn new(
        request: ObjectRequest<T>,
        authority: AuthorityClaim,
        max_chunk: usize,
        max_object: u64,
    ) -> Result<Self, ReplicationError> {
        let limits = TransportLimits {
            max_chunk,
            max_object,
            ..TransportLimits::default()
        };
        Self::with_limits(request, authority, limits)
    }

    /// Creates a transfer with the complete negotiated transport budget.
    ///
    /// # Errors
    ///
    /// Returns an object, range, or limit error when the request or budget is
    /// invalid.
    pub fn with_limits(
        request: ObjectRequest<T>,
        authority: AuthorityClaim,
        limits: TransportLimits,
    ) -> Result<Self, ReplicationError> {
        limits.validate()?;
        request.validate(limits)?;
        let object_len =
            usize::try_from(request.len).map_err(|_| ReplicationError::ObjectTooLarge)?;
        Ok(Self {
            request,
            object_len,
            authority,
            limits,
            chunks: BTreeMap::new(),
            coverage: SparseCoverage::new(limits.max_ranges)?,
            state: PhantomData,
        })
    }

    /// Stages one admitted chunk, accepting an identical replay idempotently.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::StaleFence`] for another transfer or
    /// authority, and a replay, range, or coverage error for conflicting data.
    pub fn stage(&mut self, chunk: AdmittedChunk<T>) -> Result<TransferReceipt, ReplicationError> {
        if chunk.transfer != self.request.transfer
            || chunk.object_len != self.request.len
            || chunk.authority != self.authority
        {
            return Err(ReplicationError::StaleFence);
        }
        let expected_key =
            claim_schema_object_key(self.request.key).map_err(ReplicationError::from)?;
        let expected_version =
            claim_schema_object_version(self.request.version).map_err(ReplicationError::from)?;
        if chunk.key.context() != expected_key.context()
            || chunk.version.context() != expected_version.context()
        {
            return Err(ReplicationError::IdentityContext);
        }
        if chunk.key != expected_key || chunk.version != expected_version {
            return Err(ReplicationError::StaleFence);
        }
        if let Some(existing) = self.chunks.get(&chunk.sequence) {
            if existing.offset == chunk.offset
                && existing.previous_chain == chunk.previous_chain
                && existing.chain == chunk.chain
                && existing.bytes.as_ref() == chunk.payload()
            {
                return Ok(self.receipt());
            }
            return Err(ReplicationError::ReplayConflict);
        }
        if chunk.sequence >= self.object_len as u64 {
            return Err(ReplicationError::Range);
        }
        let range = ByteRange::new(chunk.offset, chunk.payload.len() as u64)?;
        if range.end()? > self.request.len {
            return Err(ReplicationError::Range);
        }
        if self.coverage.overlaps(range) {
            return Err(ReplicationError::ReplayConflict);
        }
        self.coverage.insert(range)?;
        self.chunks.insert(
            chunk.sequence,
            ChunkRecord {
                sequence: chunk.sequence,
                offset: chunk.offset,
                previous_chain: chunk.previous_chain,
                chain: chunk.chain,
                bytes: chunk.payload,
            },
        );
        Ok(self.receipt())
    }

    /// Returns a sparse checkpoint that can be retained across reconnect.
    #[must_use]
    pub fn checkpoint(&self) -> TransferCheckpoint<T> {
        TransferCheckpoint {
            transfer: self.request.transfer,
            key: self.request.key,
            version: self.request.version,
            len: self.request.len,
            authority: self.authority,
            chunks: self
                .chunks
                .values()
                .map(|chunk| CheckpointChunk {
                    offset: chunk.offset,
                    sequence: chunk.sequence,
                    previous_chain: chunk.previous_chain,
                    chain: chunk.chain,
                    // Preserve the historical Vec based checkpoint API. New
                    // callers retaining an in-memory checkpoint should use
                    // [`Self::checkpoint_shared`] to keep this allocation
                    // shared with the transfer.
                    bytes: chunk.bytes.to_vec(),
                })
                .collect(),
            coverage: self.coverage.clone(),
        }
    }

    /// Returns an in-memory checkpoint whose chunk bodies share their
    /// immutable allocations with this transfer.
    ///
    /// The legacy [`Self::checkpoint`] method remains available for durable
    /// callers that require public `Vec<u8>` fields. This form is intended for
    /// reconnect and resume paths where cloning multi-megabyte chunks would
    /// otherwise multiply memory use.
    #[must_use]
    pub fn checkpoint_shared(&self) -> SharedTransferCheckpoint<T> {
        SharedTransferCheckpoint {
            transfer: self.request.transfer,
            key: self.request.key,
            version: self.request.version,
            len: self.request.len,
            authority: self.authority,
            chunks: self
                .chunks
                .values()
                .map(|chunk| SharedCheckpointChunk {
                    offset: chunk.offset,
                    sequence: chunk.sequence,
                    previous_chain: chunk.previous_chain,
                    chain: chunk.chain,
                    bytes: Arc::clone(&chunk.bytes),
                })
                .collect(),
            coverage: self.coverage.clone(),
        }
    }

    /// Returns a durable checkpoint carrying an owner lease and explicit GC
    /// roots.  The transfer is still receiving; consuming the checkpoint is
    /// the caller's choice, so the active transfer remains available for a
    /// retry while the durable record is written.
    ///
    /// # Errors
    ///
    /// Returns a bounds, identity, lease, or root error when the checkpoint
    /// cannot be admitted for durable resume.
    pub fn checkpoint_leased<K>(
        &self,
        lease: TransferLease<K>,
        roots: Vec<GcRoot<T, K>>,
        limits: TransportLimits,
    ) -> Result<LeasedTransferCheckpoint<T, K>, ReplicationError> {
        LeasedTransferCheckpoint::new(self.checkpoint(), lease, roots, limits)
    }

    /// Returns a fixed-shape wire checkpoint suitable for durable storage.
    ///
    /// The checkpoint remains a claim until
    /// [`WireTransferCheckpoint::admit_against`]
    /// revalidates every retained chunk and recomputes its coverage.
    ///
    /// # Errors
    ///
    /// Returns an identity encoding error if a fixed-width claim cannot be
    /// represented on the wire.
    pub fn checkpoint_wire(&self) -> Result<WireTransferCheckpoint<T>, ReplicationError> {
        Ok(WireTransferCheckpoint {
            transfer: self.request.transfer,
            key: claim_schema_object_key(self.request.key).map_err(ReplicationError::from)?,
            version: claim_schema_object_version(self.request.version)
                .map_err(ReplicationError::from)?,
            len: self.request.len,
            authority: self.authority,
            chunks: self
                .chunks
                .values()
                .map(|chunk| CheckpointChunk {
                    offset: chunk.offset,
                    sequence: chunk.sequence,
                    previous_chain: chunk.previous_chain,
                    chain: chunk.chain,
                    bytes: chunk.bytes.to_vec(),
                })
                .collect(),
            coverage: self.coverage.clone(),
        })
    }

    /// Resumes a transfer from a bounded retained checkpoint.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::StaleFence`] for a mismatched checkpoint or
    /// a range/size error for malformed retained chunks.
    pub fn resume(
        request: ObjectRequest<T>,
        authority: AuthorityClaim,
        limits: TransportLimits,
        checkpoint: TransferCheckpoint<T>,
    ) -> Result<Self, ReplicationError> {
        if checkpoint.transfer != request.transfer
            || checkpoint.key != request.key
            || checkpoint.version != request.version
            || checkpoint.len != request.len
            || checkpoint.authority != authority
        {
            return Err(ReplicationError::StaleFence);
        }
        validate_checkpoint_contents(&request, authority, limits, &checkpoint)?;
        Self::resume_checkpoint(request, authority, limits, checkpoint)
    }

    /// Resumes from a shared in-memory checkpoint without copying chunk
    /// bodies. The checkpoint is consumed so ownership of each `Arc` moves
    /// directly into the receiving transfer.
    ///
    /// # Errors
    ///
    /// Returns the same bounded range, replay, and corruption errors as
    /// [`Self::resume`].
    pub fn resume_shared(
        request: ObjectRequest<T>,
        authority: AuthorityClaim,
        limits: TransportLimits,
        checkpoint: SharedTransferCheckpoint<T>,
    ) -> Result<Self, ReplicationError> {
        if checkpoint.transfer != request.transfer
            || checkpoint.key != request.key
            || checkpoint.version != request.version
            || checkpoint.len != request.len
            || checkpoint.authority != authority
        {
            return Err(ReplicationError::StaleFence);
        }
        validate_shared_checkpoint_contents(&request, authority, limits, &checkpoint)?;
        Self::resume_shared_checkpoint(request, authority, limits, checkpoint)
    }

    fn resume_checkpoint(
        request: ObjectRequest<T>,
        authority: AuthorityClaim,
        limits: TransportLimits,
        checkpoint: TransferCheckpoint<T>,
    ) -> Result<Self, ReplicationError> {
        let TransferCheckpoint {
            transfer,
            key,
            version,
            len,
            authority: checkpoint_authority,
            chunks,
            coverage,
        } = checkpoint;
        Self::resume_parts(
            ResumeParts {
                request,
                authority,
                limits,
                transfer_id: transfer,
                key,
                version,
                len,
                checkpoint_authority,
                coverage,
            },
            chunks.into_iter().map(|chunk| RetainedChunk {
                offset: chunk.offset,
                sequence: chunk.sequence,
                previous_chain: chunk.previous_chain,
                chain: chunk.chain,
                bytes: chunk.bytes.into(),
            }),
        )
    }

    fn resume_shared_checkpoint(
        request: ObjectRequest<T>,
        authority: AuthorityClaim,
        limits: TransportLimits,
        checkpoint: SharedTransferCheckpoint<T>,
    ) -> Result<Self, ReplicationError> {
        let SharedTransferCheckpoint {
            transfer,
            key,
            version,
            len,
            authority: checkpoint_authority,
            chunks,
            coverage,
        } = checkpoint;
        Self::resume_parts(
            ResumeParts {
                request,
                authority,
                limits,
                transfer_id: transfer,
                key,
                version,
                len,
                checkpoint_authority,
                coverage,
            },
            chunks.into_iter().map(|chunk| RetainedChunk {
                offset: chunk.offset,
                sequence: chunk.sequence,
                previous_chain: chunk.previous_chain,
                chain: chunk.chain,
                bytes: chunk.bytes,
            }),
        )
    }

    fn resume_parts(
        parts: ResumeParts<T>,
        chunks: impl IntoIterator<Item = RetainedChunk>,
    ) -> Result<Self, ReplicationError> {
        if parts.transfer_id != parts.request.transfer
            || parts.key != parts.request.key
            || parts.version != parts.request.version
            || parts.len != parts.request.len
            || parts.checkpoint_authority != parts.authority
        {
            return Err(ReplicationError::StaleFence);
        }
        let mut transfer = Self::with_limits(parts.request, parts.authority, parts.limits)?;
        if parts.coverage.ranges().len() > parts.limits.max_ranges {
            return Err(ReplicationError::CoverageLimit);
        }
        let checkpoint_ranges = parts.coverage.ranges().to_vec();
        let mut retained_bytes = 0u64;
        let mut retained_chunks = 0usize;
        for chunk in chunks {
            retained_chunks = retained_chunks
                .checked_add(1)
                .ok_or(ReplicationError::Overflow)?;
            if retained_chunks > transfer.object_len {
                return Err(ReplicationError::CoverageLimit);
            }
            if chunk.bytes.is_empty() || chunk.bytes.len() > parts.limits.max_chunk {
                return Err(ReplicationError::ChunkTooLarge);
            }
            retained_bytes = retained_bytes
                .checked_add(chunk.bytes.len() as u64)
                .ok_or(ReplicationError::Overflow)?;
            if retained_bytes > transfer.request.len {
                return Err(ReplicationError::CoverageLimit);
            }
            let range = ByteRange::new(chunk.offset, chunk.bytes.len() as u64)?;
            if chunk.sequence >= transfer.object_len as u64 || range.end()? > transfer.request.len {
                return Err(ReplicationError::Range);
            }
            if chain(chunk.previous_chain, &chunk.bytes, chunk.sequence) != chunk.chain {
                return Err(ReplicationError::CorruptFrame);
            }
            if transfer.chunks.contains_key(&chunk.sequence) || transfer.coverage.overlaps(range) {
                return Err(ReplicationError::ReplayConflict);
            }
            transfer.coverage.insert(range)?;
            transfer.chunks.insert(
                chunk.sequence,
                ChunkRecord {
                    sequence: chunk.sequence,
                    offset: chunk.offset,
                    previous_chain: chunk.previous_chain,
                    chain: chunk.chain,
                    bytes: chunk.bytes,
                },
            );
        }
        if transfer.coverage.ranges() != checkpoint_ranges.as_slice() {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(transfer)
    }

    /// Finishes a complete transfer, checking every expected sequence link.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Incomplete`] when coverage or sequence
    /// links are missing and [`ReplicationError::CorruptFrame`] for a bad link.
    pub fn finish(self) -> Result<Transfer<Complete, T>, ReplicationError> {
        if !self.coverage.is_complete(self.request.len) {
            return Err(ReplicationError::Incomplete);
        }
        let Some(&last_sequence) = self.chunks.keys().next_back() else {
            // A complete transfer always carries at least one authenticated
            // chunk; empty coverage never mints completion.
            return Err(ReplicationError::Incomplete);
        };
        let sequence_count = usize::try_from(last_sequence)
            .ok()
            .and_then(|sequence| sequence.checked_add(1))
            .ok_or(ReplicationError::Overflow)?;
        if self.chunks.len() != sequence_count {
            return Err(ReplicationError::Incomplete);
        }
        let mut previous = ChunkChain([0; 32]);
        for sequence in 0..=last_sequence {
            let chunk = self
                .chunks
                .get(&sequence)
                .ok_or(ReplicationError::Incomplete)?;
            if chunk.previous_chain != previous {
                return Err(ReplicationError::CorruptFrame);
            }
            previous = chunk.chain;
        }
        Ok(Transfer {
            request: self.request,
            object_len: self.object_len,
            authority: self.authority,
            limits: self.limits,
            chunks: self.chunks,
            coverage: self.coverage,
            state: PhantomData,
        })
    }

    /// Validates and accepts a complete object in one consuming operation.
    ///
    /// # Errors
    ///
    /// Returns transfer completeness, corruption, range, or identity errors.
    pub fn validate(
        self,
        verify: impl FnOnce(VersionObjectVersion<T>, &[u8]) -> bool,
    ) -> Result<CompleteObject<T>, ReplicationError> {
        self.finish()?.accept(verify)
    }

    /// Validates and accepts a complete object using the object-version hash
    /// defined by the replication schema.
    ///
    /// # Errors
    ///
    /// Returns transfer completeness, corruption, range, or identity errors.
    pub fn accept_canonical(self) -> Result<CompleteObject<T>, ReplicationError>
    where
        T: Schema<Value = [u8]>,
    {
        self.finish()?
            .accept(|version, bytes| VersionObjectVersion::<T>::from_value(bytes) == version)
    }

    fn receipt(&self) -> TransferReceipt {
        TransferReceipt {
            coverage: self.coverage.clone(),
            chunks: self.chunks.len(),
        }
    }
}
