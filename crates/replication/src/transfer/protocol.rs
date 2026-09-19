//! Wire chunk frames and typestate admission.

use std::{fmt, sync::Arc};

use backend_version::{
    ObjectKey as VersionObjectKey, ObjectVersion as VersionObjectVersion, Schema,
};

use crate::{
    AuthorityClaim, ImmutableObjectSchema, ReplicationError, SchemaWireObjectKey,
    SchemaWireObjectVersion, TransferId, TransportLimits, WireAuthority, claim_schema_object_key,
    claim_schema_object_version,
};

/// A chunk-chain digest used for ordered replay/tamper detection.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ChunkChain(pub [u8; 32]);

/// Incremental verifier for one authenticated chunk extent.
///
/// The extent length is committed before its bytes, so a CAS adapter can read
/// a staged extent in bounded pieces without rebuilding a temporary `Vec`.
pub struct ChunkChainDigest {
    hasher: blake3::Hasher,
    expected_len: u64,
    written: u64,
}
impl ChunkChainDigest {
    /// Starts a digest for one sequence/link pair.
    #[must_use]
    pub fn new(previous: ChunkChain, sequence: u64, expected_len: u64) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.replication.chunk.v2\0");
        hasher.update(&previous.0);
        hasher.update(&sequence.to_be_bytes());
        hasher.update(&expected_len.to_be_bytes());
        Self {
            hasher,
            expected_len,
            written: 0,
        }
    }

    /// Feeds one bounded extent piece.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Range`] when the piece exceeds the
    /// declared extent and [`ReplicationError::Overflow`] on byte-accounting
    /// overflow.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), ReplicationError> {
        let length = u64::try_from(bytes.len()).map_err(|_| ReplicationError::Overflow)?;
        let next = self
            .written
            .checked_add(length)
            .ok_or(ReplicationError::Overflow)?;
        if next > self.expected_len {
            return Err(ReplicationError::Range);
        }
        self.hasher.update(bytes);
        self.written = next;
        Ok(())
    }

    /// Finishes the extent digest, requiring its exact declared length.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Incomplete`] when the staged extent does
    /// not yield its declared number of bytes.
    pub fn finish(self) -> Result<ChunkChain, ReplicationError> {
        if self.written != self.expected_len {
            return Err(ReplicationError::Incomplete);
        }
        Ok(ChunkChain(*self.hasher.finalize().as_bytes()))
    }
}

pub(super) fn chain(prev: ChunkChain, bytes: &[u8], sequence: u64) -> ChunkChain {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.replication.chunk.v2\0");
    hasher.update(&prev.0);
    hasher.update(&sequence.to_be_bytes());
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
    ChunkChain(*hasher.finalize().as_bytes())
}

/// An unvalidated wire frame. All identity fields are untrusted claims.
pub struct Frame<T: Schema = ImmutableObjectSchema> {
    /// Transfer identifier.
    pub transfer: TransferId,
    /// Untrusted logical object key.
    pub key: SchemaWireObjectKey<T>,
    /// Untrusted complete object version.
    pub version: SchemaWireObjectVersion<T>,
    /// Complete canonical object length.
    pub object_len: u64,
    /// Byte offset of this payload.
    pub offset: u64,
    /// Chunk sequence number.
    pub sequence: u64,
    /// Chain digest before this chunk.
    pub previous_chain: ChunkChain,
    /// Chain digest after this chunk.
    pub chain: ChunkChain,
    /// Bounded payload bytes.
    pub payload: Vec<u8>,
    /// Untrusted authority claim.
    pub authority: WireAuthority,
}

impl<T: Schema> Clone for Frame<T> {
    fn clone(&self) -> Self {
        Self {
            transfer: self.transfer,
            key: self.key,
            version: self.version,
            object_len: self.object_len,
            offset: self.offset,
            sequence: self.sequence,
            previous_chain: self.previous_chain,
            chain: self.chain,
            payload: self.payload.clone(),
            authority: self.authority,
        }
    }
}
impl<T: Schema> fmt::Debug for Frame<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Frame")
            .field("transfer", &self.transfer)
            .field("key", &self.key)
            .field("version", &self.version)
            .field("object_len", &self.object_len)
            .field("offset", &self.offset)
            .field("sequence", &self.sequence)
            .field("previous_chain", &self.previous_chain)
            .field("chain", &self.chain)
            .field("payload", &self.payload)
            .field("authority", &self.authority)
            .finish()
    }
}
impl<T: Schema> PartialEq for Frame<T> {
    fn eq(&self, other: &Self) -> bool {
        self.transfer == other.transfer
            && self.key == other.key
            && self.version == other.version
            && self.object_len == other.object_len
            && self.offset == other.offset
            && self.sequence == other.sequence
            && self.previous_chain == other.previous_chain
            && self.chain == other.chain
            && self.payload == other.payload
            && self.authority == other.authority
    }
}
impl<T: Schema> Eq for Frame<T> {}

/// Payload and placement fields used to construct a chunk frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChunkParts {
    /// Complete canonical object length.
    pub object_len: u64,
    /// Byte offset of the payload.
    pub offset: u64,
    /// Chunk sequence number.
    pub sequence: u64,
    /// Chain digest before the payload.
    pub previous_chain: ChunkChain,
    /// Payload bytes.
    pub payload: Vec<u8>,
}

impl<T: Schema> Frame<T> {
    /// Constructs a wire frame from trusted identities and computes its chain.
    ///
    /// # Errors
    ///
    /// Returns an identity encoding error if a fixed-width claim cannot be
    /// represented on the wire.
    pub fn new(
        transfer: TransferId,
        key: VersionObjectKey<T>,
        version: VersionObjectVersion<T>,
        parts: ChunkParts,
        authority: AuthorityClaim,
    ) -> Result<Self, ReplicationError> {
        let next = chain(parts.previous_chain, &parts.payload, parts.sequence);
        Ok(Self {
            transfer,
            key: claim_schema_object_key(key).map_err(ReplicationError::from)?,
            version: claim_schema_object_version(version).map_err(ReplicationError::from)?,
            object_len: parts.object_len,
            offset: parts.offset,
            sequence: parts.sequence,
            previous_chain: parts.previous_chain,
            chain: next,
            payload: parts.payload,
            authority,
        })
    }
    /// Checks bounded structure, identity contexts, and the chunk chain
    /// without taking ownership of the payload.
    ///
    /// Keeping this check borrowed lets queue admission inspect a frame before
    /// it is moved into the receiving typestate. The consuming [`Self::admit`]
    /// path then moves the same allocation into an immutable shared owner.
    ///
    /// # Errors
    ///
    /// Returns an object, range, chain, or identity error when the frame is
    /// malformed or exceeds negotiated limits.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if 192usize.saturating_add(self.payload.len()) > limits.max_frame {
            return Err(ReplicationError::MessageTooLarge);
        }
        if self.transfer.get() == 0
            || self.payload.is_empty()
            || self.payload.len() > limits.max_chunk
        {
            return Err(ReplicationError::ChunkTooLarge);
        }
        if self.object_len == 0
            || self.object_len > limits.max_object
            || self.offset >= self.object_len
        {
            return Err(ReplicationError::ObjectTooLarge);
        }
        let end = self
            .offset
            .checked_add(self.payload.len() as u64)
            .ok_or(ReplicationError::Overflow)?;
        if end > self.object_len {
            return Err(ReplicationError::Range);
        }
        // A sequence number is independent of the byte offset so a resumable
        // request may carry sparse or overlapping claims. The object length
        // bounds the number of one-byte chunks a peer can retain.
        if self.sequence >= self.object_len {
            return Err(ReplicationError::Range);
        }
        if chain(self.previous_chain, &self.payload, self.sequence) != self.chain {
            return Err(ReplicationError::CorruptFrame);
        }
        // The backend-version digest is still untrusted here: this frame has
        // no canonical object preimage with which to prove either identity.
        // The transfer request supplies the caller-owned typed identities and
        // performs the exact comparison before staging the chunk.
        self.key
            .admit_context(backend_version::IdContext::object_key::<T>())?;
        self.version
            .admit_context(backend_version::IdContext::schema::<T>())?;
        Ok(())
    }

    /// Admits bounded structure, identity contexts, and the chunk chain.
    ///
    /// # Errors
    ///
    /// Returns an object, range, chain, or identity error when the frame is
    /// malformed or exceeds negotiated limits.
    pub fn admit(self, limits: TransportLimits) -> Result<AdmittedChunk<T>, ReplicationError> {
        self.validate(limits)?;
        Ok(AdmittedChunk {
            transfer: self.transfer,
            key: self.key,
            version: self.version,
            object_len: self.object_len,
            offset: self.offset,
            sequence: self.sequence,
            previous_chain: self.previous_chain,
            chain: self.chain,
            payload: self.payload.into(),
            authority: self.authority,
        })
    }
}

/// A frame that passed wire-size, context, range, and chain admission.
pub struct AdmittedChunk<T: Schema = ImmutableObjectSchema> {
    pub(crate) transfer: TransferId,
    pub(crate) key: SchemaWireObjectKey<T>,
    pub(crate) version: SchemaWireObjectVersion<T>,
    pub(crate) object_len: u64,
    pub(crate) offset: u64,
    pub(crate) sequence: u64,
    pub(crate) previous_chain: ChunkChain,
    pub(crate) chain: ChunkChain,
    pub(crate) payload: Arc<[u8]>,
    pub(crate) authority: AuthorityClaim,
}
impl<T: Schema> Clone for AdmittedChunk<T> {
    fn clone(&self) -> Self {
        Self {
            transfer: self.transfer,
            key: self.key,
            version: self.version,
            object_len: self.object_len,
            offset: self.offset,
            sequence: self.sequence,
            previous_chain: self.previous_chain,
            chain: self.chain,
            payload: Arc::clone(&self.payload),
            authority: self.authority,
        }
    }
}
impl<T: Schema> fmt::Debug for AdmittedChunk<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdmittedChunk")
            .field("transfer", &self.transfer)
            .field("key", &self.key)
            .field("version", &self.version)
            .field("object_len", &self.object_len)
            .field("offset", &self.offset)
            .field("sequence", &self.sequence)
            .field("previous_chain", &self.previous_chain)
            .field("chain", &self.chain)
            .field("payload", &self.payload)
            .field("authority", &self.authority)
            .finish()
    }
}
impl<T: Schema> PartialEq for AdmittedChunk<T> {
    fn eq(&self, other: &Self) -> bool {
        self.transfer == other.transfer
            && self.key == other.key
            && self.version == other.version
            && self.object_len == other.object_len
            && self.offset == other.offset
            && self.sequence == other.sequence
            && self.previous_chain == other.previous_chain
            && self.chain == other.chain
            && self.payload == other.payload
            && self.authority == other.authority
    }
}
impl<T: Schema> Eq for AdmittedChunk<T> {}
impl<T: Schema> AdmittedChunk<T> {
    /// Returns this chunk's transfer identifier.
    #[must_use]
    pub const fn transfer(&self) -> TransferId {
        self.transfer
    }
    /// Returns the schema-context-admitted, still-untrusted object key claim.
    #[must_use]
    pub const fn key(&self) -> SchemaWireObjectKey<T> {
        self.key
    }
    /// Returns the schema-context-admitted, still-untrusted object version claim.
    #[must_use]
    pub const fn version(&self) -> SchemaWireObjectVersion<T> {
        self.version
    }
    /// Returns the complete object length.
    #[must_use]
    pub const fn object_len(&self) -> u64 {
        self.object_len
    }
    /// Returns this chunk's byte offset.
    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.offset
    }
    /// Returns this chunk's sequence number.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
    /// Returns the chain digest preceding this chunk.
    #[must_use]
    pub const fn previous_chain(&self) -> ChunkChain {
        self.previous_chain
    }
    /// Returns the chain digest after this chunk.
    #[must_use]
    pub const fn chain(&self) -> ChunkChain {
        self.chain
    }
    /// Returns this chunk's payload.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
    /// Returns a cheap shared handle to this admitted chunk payload.
    #[must_use]
    pub fn payload_shared(&self) -> Arc<[u8]> {
        Arc::clone(&self.payload)
    }

    /// Moves the admitted payload into a storage session without cloning its
    /// reference-counted allocation.
    pub(crate) fn into_payload(self) -> Arc<[u8]> {
        self.payload
    }
    /// Returns the authority admitted on the frame.
    #[must_use]
    pub const fn authority(&self) -> AuthorityClaim {
        self.authority
    }
}
