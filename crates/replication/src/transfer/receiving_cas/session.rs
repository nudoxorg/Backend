//! Live sparse receiving session and its commit fence.

use super::super::cas::CanonicalDigest;
use super::super::lease::{GcRoot, TransferLease};
use super::super::protocol::ChunkChainDigest;
use super::validation::validate_chain_links;
use super::{LeasedReceivingCheckpoint, ReceivingCasSink, ReceivingCheckpoint, StagedExtent};
use crate::{
    AdmittedChunk, AuthorityClaim, ByteRange, ImmutableObjectSchema, ObjectRequest,
    ReplicationError, SparseCoverage, TransferReceipt, TransportLimits, claim_schema_object_key,
    claim_schema_object_version,
};
use backend_version::Schema;
use std::{collections::BTreeMap, fmt, marker::PhantomData};

/// A live transfer that moves admitted payloads into an unpublished CAS.
pub struct ReceivingCas<C, T: Schema = ImmutableObjectSchema>
where
    C: ReceivingCasSink<T>,
{
    request: ObjectRequest<T>,
    authority: AuthorityClaim,
    limits: TransportLimits,
    max_extents: usize,
    coverage: SparseCoverage,
    pub(super) extents: BTreeMap<u64, StagedExtent>,
    session: Option<C::Session>,
    marker: PhantomData<fn() -> C>,
}

impl<C, T: Schema> fmt::Debug for ReceivingCas<C, T>
where
    C: ReceivingCasSink<T>,
    C::Session: fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReceivingCas")
            .field("request", &self.request)
            .field("authority", &self.authority)
            .field("limits", &self.limits)
            .field("max_extents", &self.max_extents)
            .field("coverage", &self.coverage)
            .field("extents", &self.extents)
            .field("session", &self.session)
            .finish()
    }
}

impl<C, T: Schema> ReceivingCas<C, T>
where
    C: ReceivingCasSink<T>,
{
    /// Starts an incremental receiving transfer with an explicit metadata
    /// budget. Payload allocations leave the protocol as soon as `stage`
    /// succeeds; only this bounded extent table remains in memory.
    ///
    /// # Errors
    ///
    /// Returns a request, limit, or storage error when an unpublished session
    /// cannot be opened.
    pub fn new(
        request: ObjectRequest<T>,
        authority: AuthorityClaim,
        limits: TransportLimits,
        max_extents: usize,
        cas: &mut C,
    ) -> Result<Self, ReplicationError> {
        limits.validate()?;
        request.validate(limits)?;
        if max_extents == 0 {
            return Err(ReplicationError::InvalidLimits);
        }
        let coverage = SparseCoverage::new(limits.max_ranges)?;
        let session = cas.begin(request.transfer, request.key, request.version, request.len)?;
        Ok(Self {
            request,
            authority,
            limits,
            max_extents,
            coverage,
            extents: BTreeMap::new(),
            session: Some(session),
            marker: PhantomData,
        })
    }

    /// Reopens a durable byte-free checkpoint after validating its typed
    /// identities. The CAS is responsible for checking that every extent ID
    /// still exists in its unpublished namespace.
    ///
    /// # Errors
    ///
    /// Returns a stale-fence, checkpoint, limit, or storage error when the
    /// checkpoint cannot be resumed.
    pub fn resume(
        request: ObjectRequest<T>,
        authority: AuthorityClaim,
        limits: TransportLimits,
        max_extents: usize,
        checkpoint: ReceivingCheckpoint<T>,
        cas: &mut C,
    ) -> Result<Self, ReplicationError> {
        checkpoint.validate_against(&request, authority, limits, max_extents)?;
        let session = cas.resume(
            request.transfer,
            request.key,
            request.version,
            request.len,
            &checkpoint,
        )?;
        let extents = checkpoint
            .extents
            .iter()
            .copied()
            .map(|extent| (extent.sequence, extent))
            .collect();
        Ok(Self {
            request,
            authority,
            limits,
            max_extents,
            coverage: checkpoint.coverage,
            extents,
            session: Some(session),
            marker: PhantomData,
        })
    }

    /// Stages one admitted chunk directly into the unpublished sparse CAS.
    /// Replaying the exact metadata is idempotent; conflicting metadata or
    /// placement is rejected before the CAS sees a second write.
    ///
    /// # Errors
    ///
    /// Returns a stale-fence, range, replay, coverage, corruption, or storage
    /// error when the chunk cannot be admitted atomically.
    pub fn stage(
        &mut self,
        cas: &mut C,
        chunk: AdmittedChunk<T>,
    ) -> Result<TransferReceipt, ReplicationError> {
        self.validate_chunk(&chunk)?;
        if let Some(existing) = self.extents.get(&chunk.sequence) {
            if *existing == StagedExtent::from_chunk(&chunk)? {
                return Ok(self.receipt());
            }
            return Err(ReplicationError::ReplayConflict);
        }
        if self.extents.len() >= self.max_extents {
            return Err(ReplicationError::CoverageLimit);
        }
        let extent = StagedExtent::from_chunk(&chunk)?;
        let range = extent.range()?;
        if self.coverage.overlaps(range) {
            return Err(ReplicationError::ReplayConflict);
        }
        if let Some(previous) = self.extents.get(&chunk.sequence.saturating_sub(1))
            && chunk.previous_chain() != previous.chain
        {
            return Err(ReplicationError::CorruptFrame);
        }
        if let Some(successor) = self.extents.get(&chunk.sequence.saturating_add(1))
            && successor.previous_chain != chunk.chain()
        {
            return Err(ReplicationError::CorruptFrame);
        }
        let mut coverage = self.coverage.clone();
        coverage.insert(range)?;
        let sequence = chunk.sequence();
        let Some(session) = self.session.as_mut() else {
            return Err(ReplicationError::InvalidWire);
        };
        if let Err(error) = cas.write(session, extent, chunk.into_payload()) {
            if let Some(session) = self.session.take() {
                cas.abort(session);
            }
            return Err(error);
        }
        self.coverage = coverage;
        self.extents.insert(sequence, extent);
        Ok(self.receipt())
    }

    /// Returns a byte-free checkpoint. The CAS session remains owned by this
    /// transfer and keeps all staged payloads unpublished.
    ///
    /// # Errors
    ///
    /// Returns a bounds, identity, range, replay, or chain error when the
    /// retained metadata cannot form a durable checkpoint.
    pub fn checkpoint(&self) -> Result<ReceivingCheckpoint<T>, ReplicationError> {
        ReceivingCheckpoint::new(
            &self.request,
            self.authority,
            self.coverage.clone(),
            self.extents.values().copied().collect(),
            self.limits,
            self.max_extents,
        )
    }

    /// Returns a byte-free checkpoint protected by an owner lease and the
    /// exact object roots that must remain live while the sparse session is
    /// resumed. This is the durable handoff used across process restart.
    ///
    /// # Errors
    ///
    /// Returns a bounds, lease, root, identity, range, replay, or chain error
    /// when the checkpoint cannot be protected for restart.
    pub fn checkpoint_leased<K>(
        &self,
        lease: TransferLease<K>,
        roots: Vec<GcRoot<T, K>>,
    ) -> Result<LeasedReceivingCheckpoint<T, K>, ReplicationError> {
        LeasedReceivingCheckpoint::new(
            self.checkpoint()?,
            lease,
            roots,
            self.limits,
            self.max_extents,
        )
    }

    /// Finalizes the staged CAS by verifying chain links and reading extents
    /// in byte order into the incremental canonical digest. No object-sized
    /// allocation is created.
    ///
    /// # Errors
    ///
    /// Returns an incomplete, corruption, identity, storage, or publication
    /// error when the staged object cannot be atomically admitted.
    pub fn finish(mut self, cas: &mut C) -> Result<C::Receipt, ReplicationError> {
        let Some(mut session) = self.session.take() else {
            return Err(ReplicationError::InvalidWire);
        };
        if !self.coverage.is_complete(self.request.len) {
            cas.abort(session);
            return Err(ReplicationError::Incomplete);
        }
        let by_sequence = self.extents.clone();
        if let Err(error) = validate_chain_links(&by_sequence, self.request.len) {
            cas.abort(session);
            return Err(error);
        }
        let mut by_offset = self.extents.values().copied().collect::<Vec<_>>();
        by_offset.sort_unstable_by_key(|extent| extent.offset);
        let mut digest = CanonicalDigest::<T>::new(self.request.len);
        for extent in by_offset {
            let mut chunk_digest =
                ChunkChainDigest::new(extent.previous_chain, extent.sequence, extent.len);
            let mut offset = extent.offset;
            let read_result = cas.read_extent(&mut session, extent, &mut |bytes| {
                chunk_digest.push(bytes)?;
                digest.push(offset, bytes)?;
                offset = offset
                    .checked_add(
                        u64::try_from(bytes.len()).map_err(|_| ReplicationError::Overflow)?,
                    )
                    .ok_or(ReplicationError::Overflow)?;
                Ok(())
            });
            if let Err(error) = read_result
                .and_then(|()| chunk_digest.finish())
                .and_then(|chain| {
                    if chain == extent.chain
                        && offset
                            == extent
                                .offset
                                .checked_add(extent.len)
                                .ok_or(ReplicationError::Overflow)?
                    {
                        Ok(())
                    } else {
                        Err(ReplicationError::CorruptFrame)
                    }
                })
            {
                cas.abort(session);
                return Err(error);
            }
        }
        let digest = match digest.finish() {
            Ok(digest) => digest,
            Err(error) => {
                cas.abort(session);
                return Err(error);
            }
        };
        if digest != *self.request.version.as_bytes() {
            cas.abort(session);
            return Err(ReplicationError::IdentityMismatch);
        }
        cas.commit(
            session,
            self.request.key,
            self.request.version,
            self.request.len,
            digest,
        )
    }

    /// Explicitly discards all unpublished extents.
    pub fn abort(self, cas: &mut C) {
        if let Some(session) = self.session {
            cas.abort(session);
        }
    }

    /// Returns a cheap bounded progress receipt.
    #[must_use]
    pub fn receipt(&self) -> TransferReceipt {
        TransferReceipt {
            coverage: self.coverage.clone(),
            chunks: self.extents.len(),
        }
    }

    /// Returns the active request without exposing CAS session internals.
    #[must_use]
    pub const fn request(&self) -> &ObjectRequest<T> {
        &self.request
    }

    fn validate_chunk(&self, chunk: &AdmittedChunk<T>) -> Result<(), ReplicationError> {
        if chunk.transfer() != self.request.transfer
            || chunk.object_len() != self.request.len
            || chunk.authority() != self.authority
        {
            return Err(ReplicationError::StaleFence);
        }
        let expected_key =
            claim_schema_object_key(self.request.key).map_err(ReplicationError::from)?;
        let expected_version =
            claim_schema_object_version(self.request.version).map_err(ReplicationError::from)?;
        if chunk.key().context() != expected_key.context()
            || chunk.version().context() != expected_version.context()
        {
            return Err(ReplicationError::IdentityContext);
        }
        if chunk.key() != expected_key || chunk.version() != expected_version {
            return Err(ReplicationError::StaleFence);
        }
        if chunk.sequence() >= self.request.len {
            return Err(ReplicationError::Range);
        }
        let bytes_len =
            u64::try_from(chunk.payload().len()).map_err(|_| ReplicationError::Overflow)?;
        if bytes_len == 0 || bytes_len > self.limits.max_chunk as u64 {
            return Err(ReplicationError::ChunkTooLarge);
        }
        let range = ByteRange::new(chunk.offset(), bytes_len)?;
        if range.end()? > self.request.len {
            return Err(ReplicationError::Range);
        }
        Ok(())
    }
}
