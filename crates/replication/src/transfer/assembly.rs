//! Receiving and complete object assembly typestates.

use std::{collections::BTreeMap, marker::PhantomData, sync::Arc};

use backend_version::{
    ObjectKey as VersionObjectKey, ObjectVersion as VersionObjectVersion, Schema,
};

use crate::{
    AuthorityClaim, ByteRange, ImmutableObjectSchema, ObjectRequest, ReplicationError,
    SparseCoverage, TransferId, TransportLimits, claim_schema_object_key,
    claim_schema_object_version,
};

use super::cas::{CanonicalCas, CanonicalDigest};
use super::checkpoint::{validate_checkpoint_contents, validate_shared_checkpoint_contents};
use super::lease::{GcRoot, LeasedTransferCheckpoint, TransferLease};
use super::protocol::{AdmittedChunk, ChunkChain, chain};
use super::{
    CheckpointChunk, CompleteObject, SharedCheckpointChunk, SharedCompleteObject,
    SharedTransferCheckpoint, TransferCheckpoint, TransferReceipt, WireTransferCheckpoint,
};

mod receive;

/// A transfer in the receiving typestate.
pub struct Receiving;
/// A transfer whose full object bytes and ordered chain have been checked.
pub struct Complete;

/// A resumable immutable object transfer.
pub struct Transfer<S = Receiving, T: Schema = ImmutableObjectSchema> {
    request: ObjectRequest<T>,
    object_len: usize,
    authority: AuthorityClaim,
    limits: TransportLimits,
    chunks: BTreeMap<u64, ChunkRecord>,
    coverage: SparseCoverage,
    state: PhantomData<S>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChunkRecord {
    sequence: u64,
    offset: u64,
    previous_chain: ChunkChain,
    chain: ChunkChain,
    bytes: Arc<[u8]>,
}

struct RetainedChunk {
    sequence: u64,
    offset: u64,
    previous_chain: ChunkChain,
    chain: ChunkChain,
    bytes: Arc<[u8]>,
}

struct ResumeParts<T: Schema> {
    request: ObjectRequest<T>,
    authority: AuthorityClaim,
    limits: TransportLimits,
    transfer_id: TransferId,
    key: VersionObjectKey<T>,
    version: VersionObjectVersion<T>,
    len: u64,
    checkpoint_authority: AuthorityClaim,
    coverage: SparseCoverage,
}

impl<T: Schema> Transfer<Complete, T> {
    /// Accepts the complete object after verifying its canonical version.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::IdentityMismatch`] when the bytes do not
    /// reproduce the requested object version.
    pub fn accept(
        self,
        verify: impl FnOnce(VersionObjectVersion<T>, &[u8]) -> bool,
    ) -> Result<CompleteObject<T>, ReplicationError> {
        let bytes = self.assembled_bytes()?;
        if !verify(self.request.version, &bytes) {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(CompleteObject {
            key: self.request.key,
            version: self.request.version,
            bytes,
            authority: self.authority,
        })
    }

    /// Accepts the complete object and retains its canonical bytes in an
    /// immutable shared allocation.
    ///
    /// This consuming form is useful when the accepted object is handed to
    /// more than one downstream owner. Verification still runs over the
    /// assembled bytes, but cloning the resulting object only increments an
    /// `Arc` count.
    ///
    /// # Errors
    ///
    /// Returns transfer completeness, corruption, range, or identity errors.
    pub fn accept_shared(
        self,
        verify: impl FnOnce(VersionObjectVersion<T>, &[u8]) -> bool,
    ) -> Result<SharedCompleteObject<T>, ReplicationError> {
        let bytes = self.assembled_bytes()?;
        if !verify(self.request.version, &bytes) {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(SharedCompleteObject {
            key: self.request.key,
            version: self.request.version,
            bytes: bytes.into(),
            authority: self.authority,
        })
    }

    /// Streams this complete transfer into a caller-owned content-addressed
    /// sink without assembling a second contiguous object buffer.
    ///
    /// The transfer's immutable chunk allocations are moved directly into
    /// [`CanonicalCas::write`].  The sink remains unpublished while the
    /// canonical object-version digest is checked; a rejected identity or
    /// failed write invokes [`CanonicalCas::abort`] so a partial object can
    /// never become visible.  This is the preferred path for large relation
    /// nodes, packs, and remote result objects whose physical owner already
    /// supports segmented storage.
    ///
    /// # Errors
    ///
    /// Returns a range, incomplete, identity, or sink error when the complete
    /// transfer cannot be admitted atomically.
    pub fn stream_into<C: CanonicalCas<T>>(
        self,
        mut cas: C,
    ) -> Result<C::Receipt, ReplicationError> {
        let Transfer {
            request,
            object_len,
            authority: _,
            limits: _,
            chunks,
            coverage: _,
            state: _,
        } = self;
        let mut session = cas.begin(request.transfer, request.key, request.version, request.len)?;
        let mut digest = CanonicalDigest::<T>::new(request.len);
        // Sequence order authenticates replay links; byte order authenticates
        // the canonical object value. They are deliberately independent for
        // sparse/out-of-order transfers, so sort only the fixed-size metadata
        // and move each Arc payload into the sink once.
        let mut chunks = chunks.into_values().collect::<Vec<_>>();
        chunks.sort_unstable_by_key(|chunk| chunk.offset);
        for chunk in chunks {
            if let Err(error) = digest.push(chunk.offset, &chunk.bytes) {
                cas.abort(session);
                return Err(error);
            }
            if let Err(error) = cas.write(&mut session, chunk.offset, chunk.bytes) {
                cas.abort(session);
                return Err(error);
            }
        }
        if object_len == 0 {
            cas.abort(session);
            return Err(ReplicationError::Incomplete);
        }
        let digest = match digest.finish() {
            Ok(digest) => digest,
            Err(error) => {
                cas.abort(session);
                return Err(error);
            }
        };
        if digest != *request.version.as_bytes() {
            cas.abort(session);
            return Err(ReplicationError::IdentityMismatch);
        }
        cas.commit(session, request.key, request.version, request.len, digest)
    }

    fn assembled_bytes(&self) -> Result<Vec<u8>, ReplicationError> {
        let mut bytes = vec![0; self.object_len];
        for chunk in self.chunks.values() {
            let start = usize::try_from(chunk.offset).map_err(|_| ReplicationError::Overflow)?;
            let end = start
                .checked_add(chunk.bytes.len())
                .ok_or(ReplicationError::Overflow)?;
            if end > bytes.len() {
                return Err(ReplicationError::Range);
            }
            bytes[start..end].copy_from_slice(&chunk.bytes);
        }
        Ok(bytes)
    }
}

impl<T: Schema> Transfer<Receiving, T> {
    /// Finishes and streams a receiving transfer into a CAS sink.
    ///
    /// This convenience consumes the receiving typestate and therefore cannot
    /// leave a second mutable transfer around after the sink takes ownership
    /// of its chunk arcs.
    ///
    /// # Errors
    ///
    /// Returns the same transfer, identity, or CAS errors as [`Self::finish`]
    /// and [`Transfer::stream_into`].
    pub fn stream_into_c<C: CanonicalCas<T>>(self, cas: C) -> Result<C::Receipt, ReplicationError> {
        self.finish()?.stream_into(cas)
    }
}
