//! Durable and shared resumable transfer checkpoints.

use std::{collections::BTreeSet, fmt, sync::Arc};

use backend_version::{
    ObjectKey as VersionObjectKey, ObjectVersion as VersionObjectVersion, Schema,
};

use crate::{
    AuthorityClaim, ByteRange, ImmutableObjectSchema, ObjectRequest, ReplicationError,
    SchemaWireObjectKey, SchemaWireObjectVersion, SparseCoverage, TransferId, TransportLimits,
    WireAuthority, claim_schema_object_key, claim_schema_object_version,
};

use super::protocol::{ChunkChain, chain};

pub(super) fn validate_checkpoint_contents<T: Schema>(
    request: &ObjectRequest<T>,
    authority: AuthorityClaim,
    limits: TransportLimits,
    checkpoint: &TransferCheckpoint<T>,
) -> Result<(), ReplicationError> {
    limits.validate()?;
    request.validate(limits)?;
    if checkpoint.transfer != request.transfer
        || checkpoint.key != request.key
        || checkpoint.version != request.version
        || checkpoint.len != request.len
        || checkpoint.authority != authority
    {
        return Err(ReplicationError::StaleFence);
    }
    if checkpoint.transfer.get() == 0
        || checkpoint.len == 0
        || checkpoint.len > limits.max_object
        || checkpoint.chunks.len() > usize::try_from(checkpoint.len).unwrap_or(usize::MAX)
        || checkpoint.coverage.ranges().len() > limits.max_ranges
    {
        return Err(ReplicationError::CoverageLimit);
    }
    let mut coverage = SparseCoverage::new(limits.max_ranges)?;
    let mut sequences = BTreeSet::new();
    let mut retained_bytes = 0u64;
    for chunk in &checkpoint.chunks {
        if !sequences.insert(chunk.sequence) {
            return Err(ReplicationError::ReplayConflict);
        }
        if chunk.bytes.is_empty() || chunk.bytes.len() > limits.max_chunk {
            return Err(ReplicationError::ChunkTooLarge);
        }
        retained_bytes = retained_bytes
            .checked_add(chunk.bytes.len() as u64)
            .ok_or(ReplicationError::Overflow)?;
        if retained_bytes > checkpoint.len {
            return Err(ReplicationError::CoverageLimit);
        }
        let range = ByteRange::new(chunk.offset, chunk.bytes.len() as u64)?;
        if chunk.sequence >= checkpoint.len || range.end()? > checkpoint.len {
            return Err(ReplicationError::Range);
        }
        if chain(chunk.previous_chain, &chunk.bytes, chunk.sequence) != chunk.chain {
            return Err(ReplicationError::CorruptFrame);
        }
        if coverage.overlaps(range) {
            return Err(ReplicationError::ReplayConflict);
        }
        coverage.insert(range)?;
    }
    for range in checkpoint.coverage.ranges() {
        if range.end()? > checkpoint.len {
            return Err(ReplicationError::Range);
        }
    }
    if coverage.ranges() != checkpoint.coverage.ranges() {
        return Err(ReplicationError::IdentityMismatch);
    }
    Ok(())
}

pub(super) fn validate_shared_checkpoint_contents<T: Schema>(
    request: &ObjectRequest<T>,
    authority: AuthorityClaim,
    limits: TransportLimits,
    checkpoint: &SharedTransferCheckpoint<T>,
) -> Result<(), ReplicationError> {
    limits.validate()?;
    request.validate(limits)?;
    if checkpoint.transfer != request.transfer
        || checkpoint.key != request.key
        || checkpoint.version != request.version
        || checkpoint.len != request.len
        || checkpoint.authority != authority
    {
        return Err(ReplicationError::StaleFence);
    }
    if checkpoint.transfer.get() == 0
        || checkpoint.len == 0
        || checkpoint.len > limits.max_object
        || checkpoint.chunks.len() > usize::try_from(checkpoint.len).unwrap_or(usize::MAX)
        || checkpoint.coverage.ranges().len() > limits.max_ranges
    {
        return Err(ReplicationError::CoverageLimit);
    }
    let mut coverage = SparseCoverage::new(limits.max_ranges)?;
    let mut sequences = BTreeSet::new();
    let mut retained_bytes = 0u64;
    for chunk in &checkpoint.chunks {
        if !sequences.insert(chunk.sequence) {
            return Err(ReplicationError::ReplayConflict);
        }
        if chunk.bytes.is_empty() || chunk.bytes.len() > limits.max_chunk {
            return Err(ReplicationError::ChunkTooLarge);
        }
        retained_bytes = retained_bytes
            .checked_add(chunk.bytes.len() as u64)
            .ok_or(ReplicationError::Overflow)?;
        if retained_bytes > checkpoint.len {
            return Err(ReplicationError::CoverageLimit);
        }
        let range = ByteRange::new(chunk.offset, chunk.bytes.len() as u64)?;
        if chunk.sequence >= checkpoint.len || range.end()? > checkpoint.len {
            return Err(ReplicationError::Range);
        }
        if chain(chunk.previous_chain, &chunk.bytes, chunk.sequence) != chunk.chain {
            return Err(ReplicationError::CorruptFrame);
        }
        if coverage.overlaps(range) {
            return Err(ReplicationError::ReplayConflict);
        }
        coverage.insert(range)?;
    }
    for range in checkpoint.coverage.ranges() {
        if range.end()? > checkpoint.len {
            return Err(ReplicationError::Range);
        }
    }
    if coverage.ranges() != checkpoint.coverage.ranges() {
        return Err(ReplicationError::IdentityMismatch);
    }
    Ok(())
}

/// A bounded resumable transfer checkpoint.
pub struct TransferCheckpoint<T: Schema = ImmutableObjectSchema> {
    /// Transfer identity.
    pub transfer: TransferId,
    /// Logical object key.
    pub key: VersionObjectKey<T>,
    /// Complete object version.
    pub version: VersionObjectVersion<T>,
    /// Complete object length.
    pub len: u64,
    /// Authority fence.
    pub authority: AuthorityClaim,
    /// Retained chunk bodies and chain links.
    pub chunks: Vec<CheckpointChunk>,
    /// Coalesced sparse coverage.
    pub coverage: SparseCoverage,
}
impl<T: Schema> Clone for TransferCheckpoint<T> {
    fn clone(&self) -> Self {
        Self {
            transfer: self.transfer,
            key: self.key,
            version: self.version,
            len: self.len,
            authority: self.authority,
            chunks: self.chunks.clone(),
            coverage: self.coverage.clone(),
        }
    }
}
impl<T: Schema> fmt::Debug for TransferCheckpoint<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransferCheckpoint")
            .field("transfer", &self.transfer)
            .field("key", &self.key)
            .field("version", &self.version)
            .field("len", &self.len)
            .field("authority", &self.authority)
            .field("chunks", &self.chunks)
            .field("coverage", &self.coverage)
            .finish()
    }
}
impl<T: Schema> PartialEq for TransferCheckpoint<T> {
    fn eq(&self, other: &Self) -> bool {
        self.transfer == other.transfer
            && self.key == other.key
            && self.version == other.version
            && self.len == other.len
            && self.authority == other.authority
            && self.chunks == other.chunks
            && self.coverage == other.coverage
    }
}
impl<T: Schema> Eq for TransferCheckpoint<T> {}
impl<T: Schema> TransferCheckpoint<T> {
    /// Validates retained chunk structure and coverage without allocating a
    /// wire checkpoint copy.
    ///
    /// This is the durable checkpoint's local structural check. The transfer
    /// request and authority fence are checked separately by
    /// [`crate::Transfer::resume`] or [`WireTransferCheckpoint::admit_against`].
    ///
    /// # Errors
    ///
    /// Returns a size, range, coverage, replay, or corruption error when a
    /// retained chunk cannot be represented within `limits`.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.transfer.get() == 0 || self.len == 0 || self.len > limits.max_object {
            return Err(ReplicationError::ObjectTooLarge);
        }
        if self.chunks.len() > usize::try_from(self.len).unwrap_or(usize::MAX)
            || self.coverage.ranges().len() > limits.max_ranges
        {
            return Err(ReplicationError::CoverageLimit);
        }
        let mut coverage = SparseCoverage::new(limits.max_ranges)?;
        let mut sequences = BTreeSet::new();
        let mut retained_bytes = 0u64;
        for chunk in &self.chunks {
            if !sequences.insert(chunk.sequence) {
                return Err(ReplicationError::ReplayConflict);
            }
            if chunk.bytes.is_empty() || chunk.bytes.len() > limits.max_chunk {
                return Err(ReplicationError::ChunkTooLarge);
            }
            retained_bytes = retained_bytes
                .checked_add(chunk.bytes.len() as u64)
                .ok_or(ReplicationError::Overflow)?;
            if retained_bytes > self.len {
                return Err(ReplicationError::CoverageLimit);
            }
            let range = ByteRange::new(chunk.offset, chunk.bytes.len() as u64)?;
            if chunk.sequence >= self.len || range.end()? > self.len {
                return Err(ReplicationError::Range);
            }
            if chain(chunk.previous_chain, &chunk.bytes, chunk.sequence) != chunk.chain {
                return Err(ReplicationError::CorruptFrame);
            }
            if coverage.overlaps(range) {
                return Err(ReplicationError::ReplayConflict);
            }
            coverage.insert(range)?;
        }
        if coverage.ranges() != self.coverage.ranges() {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(())
    }

    /// Converts a validated checkpoint to an untrusted durable claim.
    ///
    /// # Errors
    ///
    /// Returns an identity encoding error if a fixed-width claim cannot be
    /// represented on the wire.
    pub fn to_wire(&self) -> Result<WireTransferCheckpoint<T>, ReplicationError> {
        Ok(WireTransferCheckpoint {
            transfer: self.transfer,
            key: claim_schema_object_key(self.key).map_err(ReplicationError::from)?,
            version: claim_schema_object_version(self.version).map_err(ReplicationError::from)?,
            len: self.len,
            authority: self.authority,
            chunks: self.chunks.clone(),
            coverage: self.coverage.clone(),
        })
    }

    /// Consumes this checkpoint into its wire claim without cloning the
    /// retained chunk vector. Chunk bodies remain owned `Vec<u8>` values at
    /// this explicit wire boundary.
    ///
    /// # Errors
    ///
    /// Returns an identity encoding error if a fixed-width claim cannot be
    /// represented on the wire.
    pub fn into_wire(self) -> Result<WireTransferCheckpoint<T>, ReplicationError> {
        Ok(WireTransferCheckpoint {
            transfer: self.transfer,
            key: claim_schema_object_key(self.key).map_err(ReplicationError::from)?,
            version: claim_schema_object_version(self.version).map_err(ReplicationError::from)?,
            len: self.len,
            authority: self.authority,
            chunks: self.chunks,
            coverage: self.coverage,
        })
    }
}

/// A durable checkpoint whose object identities are still untrusted claims.
pub struct WireTransferCheckpoint<T: Schema = ImmutableObjectSchema> {
    /// Transfer identity claim.
    pub transfer: TransferId,
    /// Untrusted logical object key.
    pub key: SchemaWireObjectKey<T>,
    /// Untrusted complete object version.
    pub version: SchemaWireObjectVersion<T>,
    /// Complete object length claim.
    pub len: u64,
    /// Opaque authority claim retained as a resume fence.
    pub authority: WireAuthority,
    /// Retained chunk bodies and chain links.
    pub chunks: Vec<CheckpointChunk>,
    /// Claimed sparse coverage.
    pub coverage: SparseCoverage,
}
impl<T: Schema> Clone for WireTransferCheckpoint<T> {
    fn clone(&self) -> Self {
        Self {
            transfer: self.transfer,
            key: self.key,
            version: self.version,
            len: self.len,
            authority: self.authority,
            chunks: self.chunks.clone(),
            coverage: self.coverage.clone(),
        }
    }
}
impl<T: Schema> fmt::Debug for WireTransferCheckpoint<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WireTransferCheckpoint")
            .field("transfer", &self.transfer)
            .field("key", &self.key)
            .field("version", &self.version)
            .field("len", &self.len)
            .field("authority", &self.authority)
            .field("chunks", &self.chunks)
            .field("coverage", &self.coverage)
            .finish()
    }
}
impl<T: Schema> PartialEq for WireTransferCheckpoint<T> {
    fn eq(&self, other: &Self) -> bool {
        self.transfer == other.transfer
            && self.key == other.key
            && self.version == other.version
            && self.len == other.len
            && self.authority == other.authority
            && self.chunks == other.chunks
            && self.coverage == other.coverage
    }
}
impl<T: Schema> Eq for WireTransferCheckpoint<T> {}
impl<T: Schema> WireTransferCheckpoint<T> {
    /// Checks the untrusted checkpoint structure without granting typed object
    /// identities.
    ///
    /// # Errors
    ///
    /// Returns a size, range, coverage, or corruption error when the durable
    /// claim cannot be retained within `limits`.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.transfer.get() == 0 {
            return Err(ReplicationError::InvalidIdentifier);
        }
        if self.len == 0 || self.len > limits.max_object {
            return Err(ReplicationError::ObjectTooLarge);
        }
        if self.chunks.len() > usize::try_from(self.len).unwrap_or(usize::MAX) {
            return Err(ReplicationError::CoverageLimit);
        }
        if self.coverage.ranges().len() > limits.max_ranges {
            return Err(ReplicationError::CoverageLimit);
        }
        self.key
            .admit_context(backend_version::IdContext::object_key::<T>())?;
        self.version
            .admit_context(backend_version::IdContext::schema::<T>())?;
        for range in self.coverage.ranges() {
            if range.end()? > self.len {
                return Err(ReplicationError::Range);
            }
        }
        for chunk in &self.chunks {
            if chunk.bytes.is_empty() || chunk.bytes.len() > limits.max_chunk {
                return Err(ReplicationError::ChunkTooLarge);
            }
            let range = ByteRange::new(chunk.offset, chunk.bytes.len() as u64)?;
            if chunk.sequence >= self.len || range.end()? > self.len {
                return Err(ReplicationError::Range);
            }
            if chain(chunk.previous_chain, &chunk.bytes, chunk.sequence) != chunk.chain {
                return Err(ReplicationError::CorruptFrame);
            }
        }
        let retained_bytes = self.chunks.iter().try_fold(0u64, |total, chunk| {
            total
                .checked_add(chunk.bytes.len() as u64)
                .ok_or(ReplicationError::Overflow)
        })?;
        if retained_bytes > self.len {
            return Err(ReplicationError::CoverageLimit);
        }
        Ok(())
    }

    /// Admits the bounded durable envelope while retaining object identities
    /// as untrusted claims. Use [`Self::admit_against`] to obtain a typed
    /// checkpoint and resume it.
    ///
    /// # Errors
    ///
    /// Returns a size, range, coverage, chain, or context error when the
    /// checkpoint is malformed or exceeds `limits`.
    pub fn admit(self, limits: TransportLimits) -> Result<Self, ReplicationError> {
        self.validate(limits)?;
        Ok(self)
    }

    /// Admits a durable checkpoint against the caller-owned typed request and
    /// verifies its retained closure.
    ///
    /// # Errors
    ///
    /// Returns an identity, range, chain, coverage, or limit error when the
    /// checkpoint claims cannot reconstruct the retained transfer state.
    pub fn admit_against(
        self,
        expected: &ObjectRequest<T>,
        authority: AuthorityClaim,
        limits: TransportLimits,
    ) -> Result<TransferCheckpoint<T>, ReplicationError> {
        self.validate(limits)?;
        expected.validate(limits)?;
        if self.transfer != expected.transfer
            || self.len != expected.len
            || self.authority != authority
        {
            return Err(ReplicationError::StaleFence);
        }
        let expected_key = claim_schema_object_key(expected.key).map_err(ReplicationError::from)?;
        let expected_version =
            claim_schema_object_version(expected.version).map_err(ReplicationError::from)?;
        if self.key.context() != expected_key.context()
            || self.version.context() != expected_version.context()
        {
            return Err(ReplicationError::IdentityContext);
        }
        if self.key != expected_key || self.version != expected_version {
            return Err(ReplicationError::StaleFence);
        }
        let checkpoint = TransferCheckpoint {
            transfer: self.transfer,
            key: expected.key,
            version: expected.version,
            len: self.len,
            authority: self.authority,
            chunks: self.chunks,
            coverage: self.coverage,
        };
        validate_checkpoint_contents(expected, authority, limits, &checkpoint)?;
        Ok(checkpoint)
    }
}

/// One retained checkpoint chunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointChunk {
    /// Byte offset.
    pub offset: u64,
    /// Sequence number.
    pub sequence: u64,
    /// Chain digest before the chunk.
    pub previous_chain: ChunkChain,
    /// Chain digest after the chunk.
    pub chain: ChunkChain,
    /// Chunk bytes.
    pub bytes: Vec<u8>,
}

/// One retained checkpoint chunk backed by an immutable shared allocation.
///
/// This is the in-memory counterpart to [`CheckpointChunk`]. It is kept
/// separate so durable wire claims retain their historical `Vec<u8>` API,
/// while reconnect paths can clone and resume checkpoints without copying
/// large payloads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SharedCheckpointChunk {
    /// Byte offset.
    pub offset: u64,
    /// Sequence number.
    pub sequence: u64,
    /// Chain digest before the chunk.
    pub previous_chain: ChunkChain,
    /// Chain digest after the chunk.
    pub chain: ChunkChain,
    /// Immutable chunk bytes shared by transfer and checkpoint owners.
    pub bytes: Arc<[u8]>,
}
impl SharedCheckpointChunk {
    /// Returns the retained bytes as a borrowed slice.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns another cheap handle to the retained bytes.
    #[must_use]
    pub fn bytes_shared(&self) -> Arc<[u8]> {
        Arc::clone(&self.bytes)
    }
}

/// An in-memory transfer checkpoint with shared immutable chunk bodies.
///
/// Use [`crate::Transfer::resume_shared`] to move this checkpoint into a receiving
/// transfer without copying its payload allocations. Use
/// [`Self::into_wire`] only when a durable `Vec<u8>` wire representation is
/// required.
pub struct SharedTransferCheckpoint<T: Schema = ImmutableObjectSchema> {
    /// Transfer identity.
    pub transfer: TransferId,
    /// Logical object key.
    pub key: VersionObjectKey<T>,
    /// Complete object version.
    pub version: VersionObjectVersion<T>,
    /// Complete object length.
    pub len: u64,
    /// Authority fence.
    pub authority: AuthorityClaim,
    /// Retained chunks backed by shared immutable bytes.
    pub chunks: Vec<SharedCheckpointChunk>,
    /// Coalesced sparse coverage.
    pub coverage: SparseCoverage,
}
impl<T: Schema> Clone for SharedTransferCheckpoint<T> {
    fn clone(&self) -> Self {
        Self {
            transfer: self.transfer,
            key: self.key,
            version: self.version,
            len: self.len,
            authority: self.authority,
            chunks: self.chunks.clone(),
            coverage: self.coverage.clone(),
        }
    }
}
impl<T: Schema> fmt::Debug for SharedTransferCheckpoint<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedTransferCheckpoint")
            .field("transfer", &self.transfer)
            .field("key", &self.key)
            .field("version", &self.version)
            .field("len", &self.len)
            .field("authority", &self.authority)
            .field("chunks", &self.chunks)
            .field("coverage", &self.coverage)
            .finish()
    }
}
impl<T: Schema> PartialEq for SharedTransferCheckpoint<T> {
    fn eq(&self, other: &Self) -> bool {
        self.transfer == other.transfer
            && self.key == other.key
            && self.version == other.version
            && self.len == other.len
            && self.authority == other.authority
            && self.chunks == other.chunks
            && self.coverage == other.coverage
    }
}
impl<T: Schema> Eq for SharedTransferCheckpoint<T> {}
impl<T: Schema> SharedTransferCheckpoint<T> {
    /// Returns a cheap clone of one retained chunk body.
    #[must_use]
    pub fn chunk_bytes_shared(&self, index: usize) -> Option<Arc<[u8]>> {
        self.chunks
            .get(index)
            .map(SharedCheckpointChunk::bytes_shared)
    }

    /// Converts the shared checkpoint to a durable wire claim. This is the
    /// explicit ownership boundary where each retained body is serialized into
    /// a `Vec<u8>` for persistence.
    ///
    /// # Errors
    ///
    /// Returns an identity encoding error if a fixed-width claim cannot be
    /// represented on the wire.
    pub fn into_wire(self) -> Result<WireTransferCheckpoint<T>, ReplicationError> {
        Ok(WireTransferCheckpoint {
            transfer: self.transfer,
            key: claim_schema_object_key(self.key).map_err(ReplicationError::from)?,
            version: claim_schema_object_version(self.version).map_err(ReplicationError::from)?,
            len: self.len,
            authority: self.authority,
            chunks: self
                .chunks
                .into_iter()
                .map(|chunk| CheckpointChunk {
                    offset: chunk.offset,
                    sequence: chunk.sequence,
                    previous_chain: chunk.previous_chain,
                    chain: chunk.chain,
                    bytes: chunk.bytes.to_vec(),
                })
                .collect(),
            coverage: self.coverage,
        })
    }
}
