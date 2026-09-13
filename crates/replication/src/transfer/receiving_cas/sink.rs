//! Storage capability for unpublished sparse transfer sessions.

use std::sync::Arc;

use backend_version::{ObjectKey, ObjectVersion, Schema};

use crate::{ReplicationError, TransferId};

use super::{ReceivingCheckpoint, StagedExtent};

/// A storage sink whose session remains unpublished until final verification.
pub trait ReceivingCasSink<T: Schema> {
    /// Mutable unpublished state for one transfer.
    type Session;
    /// Receipt or handle for the atomically published object.
    type Receipt;

    /// Opens an empty unpublished sparse session.
    ///
    /// # Errors
    ///
    /// Returns a storage or resource error when the session cannot be opened.
    fn begin(
        &mut self,
        transfer: TransferId,
        key: ObjectKey<T>,
        version: ObjectVersion<T>,
        len: u64,
    ) -> Result<Self::Session, ReplicationError>;

    /// Reopens staged extents from an authenticated durable checkpoint.
    ///
    /// # Errors
    ///
    /// Returns a storage or checkpoint error when retained extents are unavailable.
    fn resume(
        &mut self,
        transfer: TransferId,
        key: ObjectKey<T>,
        version: ObjectVersion<T>,
        len: u64,
        checkpoint: &ReceivingCheckpoint<T>,
    ) -> Result<Self::Session, ReplicationError>;

    /// Writes one authenticated extent into the unpublished session.
    ///
    /// # Errors
    ///
    /// Returns a storage or resource error when the extent cannot be retained.
    fn write(
        &mut self,
        session: &mut Self::Session,
        extent: StagedExtent,
        bytes: Arc<[u8]>,
    ) -> Result<(), ReplicationError>;

    /// Reads one staged extent in bounded pieces.
    ///
    /// # Errors
    ///
    /// Returns a storage or corruption error when the extent cannot be read.
    fn read_extent(
        &mut self,
        session: &mut Self::Session,
        extent: StagedExtent,
        visitor: &mut dyn FnMut(&[u8]) -> Result<(), ReplicationError>,
    ) -> Result<(), ReplicationError>;

    /// Publishes the fully verified sparse session atomically.
    ///
    /// # Errors
    ///
    /// Returns a storage or publication error when the session cannot be committed.
    fn commit(
        &mut self,
        session: Self::Session,
        key: ObjectKey<T>,
        version: ObjectVersion<T>,
        len: u64,
        digest: [u8; 32],
    ) -> Result<Self::Receipt, ReplicationError>;

    /// Removes all unpublished extents and releases the session.
    fn abort(&mut self, session: Self::Session);
}
