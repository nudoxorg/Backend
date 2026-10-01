//! Live sparse receiving session and its commit fence.

use super::super::cas::CanonicalDigest;
use super::super::lease::{GcRoot, TransferLease};
use super::super::protocol::ChunkChainDigest;
use super::validation::validate_chain_links;
use super::{
    LeasedReceivingCheckpoint, ReceivingCasSink, ReceivingCheckpoint, StagedExtent,
    WireReceivingCheckpoint,
};
use crate::{
    AdmittedChunk, AuthorityClaim, ByteRange, ImmutableObjectSchema, ObjectRequest,
    ReplicationError, SparseCoverage, TransferReceipt, TransportLimits, UnverifiedObjectRequest,
    claim_schema_object_key, claim_schema_object_version,
};
use backend_version::Schema;
use std::{collections::BTreeMap, fmt, marker::PhantomData};

/// A live transfer that moves admitted payloads into an unpublished CAS.
pub struct ReceivingCas<C, T: Schema = ImmutableObjectSchema>
where
    C: ReceivingCasSink<T>,
{
    request: Option<ObjectRequest<T>>,
    unverified_request: Option<UnverifiedObjectRequest<T>>,
    authority: AuthorityClaim,
    limits: TransportLimits,
    max_extents: usize,
    coverage: SparseCoverage,
    pub(super) extents: BTreeMap<u64, StagedExtent>,
    session: Option<C::Session>,
    marker: PhantomData<fn() -> C>,
}

/// Incremental admission hook for an unverified receiving-CAS payload.
///
/// `update` receives bounded pieces in payload order. `finish` runs only after
/// extent chains and the complete canonical byte stream have been verified,
/// and before the sink publishes the object.
pub trait ReceivingCasStreamAdmission {
    /// Checks the next bounded payload piece.
    fn update(&mut self, bytes: &[u8]) -> Result<(), ReplicationError>;

    /// Completes caller-owned admission before CAS publication.
    fn finish(&mut self) -> Result<(), ReplicationError>;
}

struct BufferingAdmission<F> {
    bytes: Vec<u8>,
    admit: F,
}

impl<F> ReceivingCasStreamAdmission for BufferingAdmission<F>
where
    F: FnMut(&[u8]) -> Result<(), ReplicationError>,
{
    fn update(&mut self, bytes: &[u8]) -> Result<(), ReplicationError> {
        self.bytes
            .try_reserve(bytes.len())
            .map_err(|_| ReplicationError::Backpressure)?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn finish(&mut self) -> Result<(), ReplicationError> {
        (self.admit)(&self.bytes)
    }
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
            .field(
                "unverified_request",
                &self
                    .unverified_request
                    .as_ref()
                    .map(|request| (request.transfer, request.len)),
            )
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
            request: Some(request),
            unverified_request: None,
            authority,
            limits,
            max_extents,
            coverage,
            extents: BTreeMap::new(),
            session: Some(session),
            marker: PhantomData,
        })
    }

    /// Starts a sparse transfer from untrusted identity claims. The claims
    /// remain untrusted until `finish` hashes the complete canonical file and
    /// verifies both the key and version before publication.
    ///
    /// # Errors
    /// Returns a request, limit, or storage error when an unpublished session
    /// cannot be opened.
    pub fn new_unverified(
        request: UnverifiedObjectRequest<T>,
        authority: AuthorityClaim,
        limits: TransportLimits,
        max_extents: usize,
        cas: &mut C,
    ) -> Result<Self, ReplicationError> {
        request.validate(limits)?;
        if max_extents == 0 {
            return Err(ReplicationError::InvalidLimits);
        }
        let coverage = SparseCoverage::new(limits.max_ranges)?;
        let session = cas.begin_unverified(request.transfer, request.len)?;
        Ok(Self {
            request: None,
            unverified_request: Some(request),
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
            request: Some(request),
            unverified_request: None,
            authority,
            limits,
            max_extents,
            coverage: checkpoint.coverage,
            extents,
            session: Some(session),
            marker: PhantomData,
        })
    }

    /// Reopens an unverified sparse transfer from a validated durable wire
    /// checkpoint. The checkpoint's claims are still compared with the full
    /// streamed bytes before the CAS object is published.
    ///
    /// # Errors
    /// Returns a stale-fence, checkpoint, limit, or storage error when the
    /// checkpoint cannot be resumed.
    pub fn resume_unverified(
        request: UnverifiedObjectRequest<T>,
        authority: AuthorityClaim,
        limits: TransportLimits,
        max_extents: usize,
        checkpoint: WireReceivingCheckpoint<T>,
        cas: &mut C,
    ) -> Result<Self, ReplicationError> {
        request.validate(limits)?;
        checkpoint.validate(limits, max_extents)?;
        if checkpoint.transfer != request.transfer
            || checkpoint.key != request.key
            || checkpoint.version != request.version
            || checkpoint.len != request.len
            || checkpoint.authority != authority
        {
            return Err(ReplicationError::StaleFence);
        }
        let session = cas.resume_unverified(request.transfer, request.len, &checkpoint)?;
        let extents = checkpoint
            .extents
            .iter()
            .copied()
            .map(|extent| {
                let extent: StagedExtent = extent.into();
                (extent.sequence, extent)
            })
            .collect();
        Ok(Self {
            request: None,
            unverified_request: Some(request),
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
        let request = self
            .request
            .as_ref()
            .ok_or(ReplicationError::IdentityMismatch)?;
        ReceivingCheckpoint::new(
            request,
            self.authority,
            self.coverage.clone(),
            self.extents.values().copied().collect(),
            self.limits,
            self.max_extents,
        )
    }

    /// Returns a bounded wire checkpoint for either a typed or unverified
    /// request. The wire form preserves claims without upgrading their trust.
    ///
    /// # Errors
    /// Returns a bounds, identity, range, replay, or chain error when the
    /// retained metadata cannot form a durable checkpoint.
    pub fn checkpoint_wire(&self) -> Result<WireReceivingCheckpoint<T>, ReplicationError> {
        let (transfer, key, version, len) = if let Some(request) = &self.request {
            (
                request.transfer,
                claim_schema_object_key(request.key).map_err(ReplicationError::from)?,
                claim_schema_object_version(request.version).map_err(ReplicationError::from)?,
                request.len,
            )
        } else {
            let request = self
                .unverified_request
                .as_ref()
                .ok_or(ReplicationError::InvalidWire)?;
            (request.transfer, request.key, request.version, request.len)
        };
        WireReceivingCheckpoint::new(
            transfer,
            key,
            version,
            len,
            self.authority,
            self.coverage.clone(),
            self.extents.values().copied().map(Into::into).collect(),
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
        let len = match self.transfer_len() {
            Ok(len) => len,
            Err(error) => {
                cas.abort(session);
                return Err(error);
            }
        };
        if !self.coverage.is_complete(len) {
            cas.abort(session);
            return Err(ReplicationError::Incomplete);
        }
        let by_sequence = self.extents.clone();
        if let Err(error) = validate_chain_links(&by_sequence, len) {
            cas.abort(session);
            return Err(error);
        }
        let mut by_offset = self.extents.values().copied().collect::<Vec<_>>();
        by_offset.sort_unstable_by_key(|extent| extent.offset);
        let mut digest = CanonicalDigest::<T>::new(len);
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
        let (key, version) = match digest.finish_identities() {
            Ok(identities) => identities,
            Err(error) => {
                cas.abort(session);
                return Err(error);
            }
        };
        if let Some(request) = &self.request
            && version != request.version
        {
            cas.abort(session);
            return Err(ReplicationError::IdentityMismatch);
        }
        if let Some(request) = &self.unverified_request
            && (key.as_bytes() != request.key.as_bytes()
                || version.as_bytes() != request.version.as_bytes())
        {
            cas.abort(session);
            return Err(ReplicationError::IdentityMismatch);
        }
        cas.commit(session, key, version, len, *version.as_bytes())
    }

    /// Finalizes an unverified sparse transfer only after a caller-owned
    /// admission callback accepts the complete bytes. The callback runs
    /// before backend-version key and version identities are derived and
    /// before the sink publishes the object. This is intended for payloads
    /// whose authority comes from a separate manifest, such as semantic IR
    /// segments; wire key/version claims remain routing hints and are not
    /// promoted to the committed object's identity.
    ///
    /// # Errors
    ///
    /// Returns an incomplete, corruption, identity, storage, or admission
    /// error when the staged bytes cannot be fully checked and committed.
    pub fn finish_unverified_with_admission(
        self,
        cas: &mut C,
        admit: impl FnMut(&[u8]) -> Result<(), ReplicationError>,
    ) -> Result<C::Receipt, ReplicationError> {
        let mut admission = BufferingAdmission {
            bytes: Vec::new(),
            admit,
        };
        self.finish_unverified_with_streaming_admission(cas, &mut admission)
    }

    /// Finalizes an unverified sparse transfer after caller-owned admission
    /// accepts its bytes incrementally. No object-sized payload allocation is
    /// made by the receiving protocol; the sink publishes only after every
    /// chunk chain, canonical object identity, and the admission final check
    /// succeeds.
    ///
    /// # Errors
    ///
    /// Returns an incomplete, corruption, identity, storage, or admission
    /// error when staged bytes cannot be fully checked and committed.
    pub fn finish_unverified_with_streaming_admission(
        mut self,
        cas: &mut C,
        admission: &mut impl ReceivingCasStreamAdmission,
    ) -> Result<C::Receipt, ReplicationError> {
        if self.unverified_request.is_none() || self.request.is_some() {
            return Err(ReplicationError::IdentityMismatch);
        }
        let Some(mut session) = self.session.take() else {
            return Err(ReplicationError::InvalidWire);
        };
        let len = match self.transfer_len() {
            Ok(len) => len,
            Err(error) => {
                cas.abort(session);
                return Err(error);
            }
        };
        if !self.coverage.is_complete(len) {
            cas.abort(session);
            return Err(ReplicationError::Incomplete);
        }
        let by_sequence = self.extents.clone();
        if let Err(error) = validate_chain_links(&by_sequence, len) {
            cas.abort(session);
            return Err(error);
        }
        let mut by_offset = self.extents.values().copied().collect::<Vec<_>>();
        by_offset.sort_unstable_by_key(|extent| extent.offset);
        let mut digest = CanonicalDigest::<T>::new(len);
        for extent in by_offset {
            let mut chunk_digest =
                ChunkChainDigest::new(extent.previous_chain, extent.sequence, extent.len);
            let mut offset = extent.offset;
            let read_result = cas.read_extent(&mut session, extent, &mut |piece| {
                chunk_digest.push(piece)?;
                digest.push(offset, piece)?;
                admission.update(piece)?;
                offset = offset
                    .checked_add(
                        u64::try_from(piece.len()).map_err(|_| ReplicationError::Overflow)?,
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
        if let Err(error) = admission.finish() {
            cas.abort(session);
            return Err(error);
        }
        // Derive the backend-store object identities only after the semantic
        // manifest has accepted these exact bytes.
        let (key, version) = match digest.finish_identities() {
            Ok(identities) => identities,
            Err(error) => {
                cas.abort(session);
                return Err(error);
            }
        };
        cas.commit(session, key, version, len, *version.as_bytes())
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
    pub fn request(&self) -> Option<&ObjectRequest<T>> {
        self.request.as_ref()
    }

    fn validate_chunk(&self, chunk: &AdmittedChunk<T>) -> Result<(), ReplicationError> {
        if chunk.transfer() != self.transfer_id()?
            || chunk.object_len() != self.transfer_len()?
            || chunk.authority() != self.authority
        {
            return Err(ReplicationError::StaleFence);
        }
        let (expected_key, expected_version) = if let Some(request) = &self.request {
            (
                claim_schema_object_key(request.key).map_err(ReplicationError::from)?,
                claim_schema_object_version(request.version).map_err(ReplicationError::from)?,
            )
        } else {
            let request = self
                .unverified_request
                .as_ref()
                .ok_or(ReplicationError::InvalidWire)?;
            (request.key, request.version)
        };
        if chunk.key().context() != expected_key.context()
            || chunk.version().context() != expected_version.context()
        {
            return Err(ReplicationError::IdentityContext);
        }
        if chunk.key() != expected_key || chunk.version() != expected_version {
            return Err(ReplicationError::StaleFence);
        }
        if chunk.sequence() >= self.transfer_len()? {
            return Err(ReplicationError::Range);
        }
        let bytes_len =
            u64::try_from(chunk.payload().len()).map_err(|_| ReplicationError::Overflow)?;
        if bytes_len == 0 || bytes_len > self.limits.max_chunk as u64 {
            return Err(ReplicationError::ChunkTooLarge);
        }
        let range = ByteRange::new(chunk.offset(), bytes_len)?;
        if range.end()? > self.transfer_len()? {
            return Err(ReplicationError::Range);
        }
        Ok(())
    }

    fn transfer_id(&self) -> Result<crate::TransferId, ReplicationError> {
        self.request
            .as_ref()
            .map(|request| request.transfer)
            .or_else(|| {
                self.unverified_request
                    .as_ref()
                    .map(|request| request.transfer)
            })
            .ok_or(ReplicationError::InvalidWire)
    }

    fn transfer_len(&self) -> Result<u64, ReplicationError> {
        self.request
            .as_ref()
            .map(|request| request.len)
            .or_else(|| self.unverified_request.as_ref().map(|request| request.len))
            .ok_or(ReplicationError::InvalidWire)
    }
}
