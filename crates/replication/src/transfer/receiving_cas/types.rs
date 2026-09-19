//! Immutable metadata for sparse receiving sessions.

use backend_version::Schema;

use crate::{AdmittedChunk, ByteRange, ReplicationError};

/// A content identity for one staged sparse extent.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExtentId(pub(crate) [u8; 32]);

impl ExtentId {
    /// Returns the fixed-width content identity.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Authenticated metadata for an extent already moved into a CAS session.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StagedExtent {
    /// Content identity derived from the chunk chain.
    pub id: ExtentId,
    /// Byte placement in the complete canonical value.
    pub offset: u64,
    /// Extent length in bytes.
    pub len: u64,
    /// Ordered replay sequence.
    pub sequence: u64,
    /// Link expected before this extent.
    pub previous_chain: super::super::protocol::ChunkChain,
    /// Authenticated link after this extent.
    pub chain: super::super::protocol::ChunkChain,
}

impl StagedExtent {
    /// Returns the range occupied by this extent.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Overflow`] when the offset and length do
    /// not fit in a bounded byte range.
    pub fn range(self) -> Result<ByteRange, ReplicationError> {
        ByteRange::new(self.offset, self.len)
    }

    pub(super) fn from_chunk(chunk: &AdmittedChunk<impl Schema>) -> Result<Self, ReplicationError> {
        Ok(Self {
            id: ExtentId(chunk.chain().0),
            offset: chunk.offset(),
            len: u64::try_from(chunk.payload().len()).map_err(|_| ReplicationError::Overflow)?,
            sequence: chunk.sequence(),
            previous_chain: chunk.previous_chain(),
            chain: chunk.chain(),
        })
    }
}

/// A wire-safe claim for one staged extent. It carries no object bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WireStagedExtent {
    /// Content identity derived from the authenticated chunk chain.
    pub id: ExtentId,
    /// Byte placement in the complete canonical value.
    pub offset: u64,
    /// Extent length in bytes.
    pub len: u64,
    /// Ordered replay sequence.
    pub sequence: u64,
    /// Link expected before this extent.
    pub previous_chain: super::super::protocol::ChunkChain,
    /// Claimed link after this extent.
    pub chain: super::super::protocol::ChunkChain,
}

impl From<StagedExtent> for WireStagedExtent {
    fn from(value: StagedExtent) -> Self {
        Self {
            id: value.id,
            offset: value.offset,
            len: value.len,
            sequence: value.sequence,
            previous_chain: value.previous_chain,
            chain: value.chain,
        }
    }
}

impl From<WireStagedExtent> for StagedExtent {
    fn from(value: WireStagedExtent) -> Self {
        Self {
            id: value.id,
            offset: value.offset,
            len: value.len,
            sequence: value.sequence,
            previous_chain: value.previous_chain,
            chain: value.chain,
        }
    }
}
