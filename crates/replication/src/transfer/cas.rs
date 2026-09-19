//! Streaming canonical object admission into a content-addressed sink.
//!
//! A completed [`super::Transfer`] already owns each verified chunk in an
//! `Arc`.  The usual `accept` API is useful for consumers that need one
//! contiguous byte string, but storage and replication do not need that
//! materialization.  This module provides the lower-level seam: chunks are
//! handed to a caller-owned CAS session in sequence, the canonical
//! `ObjectVersion` digest is computed while they pass through, and the sink is
//! committed only after the claimed identity has been reproduced.

use std::{marker::PhantomData, sync::Arc};

use backend_version::{ObjectKey, ObjectVersion, ObjectVersionHasher, Schema, SchemaIdentity};

use crate::{ReplicationError, TransferId};

/// A caller-owned content-addressed sink for one canonical object transfer.
///
/// The sink owns the physical policy: it may append to a pack, write extents
/// to a local CAS, or forward the same immutable `Arc` to another owner.  The
/// replication layer owns ordering, byte accounting, and identity checking.
/// Implementations must keep a session unpublished until [`Self::commit`]
/// succeeds.  [`Self::abort`] is called for every rejected or failed session.
pub trait CanonicalCas<T: Schema> {
    /// Mutable state for one in-progress object.
    type Session;
    /// Receipt or handle returned after the object is durably admitted.
    type Receipt;

    /// Opens an unpublished CAS session for one exact object claim.
    ///
    /// # Errors
    ///
    /// Returns a storage or resource error when a session cannot be opened.
    fn begin(
        &mut self,
        transfer: TransferId,
        key: ObjectKey<T>,
        version: ObjectVersion<T>,
        len: u64,
    ) -> Result<Self::Session, ReplicationError>;

    /// Appends one immutable chunk to the unpublished session.
    ///
    /// The `Arc` is moved into the sink.  A sink that needs to retain the
    /// bytes for both its staging and publication paths can clone the handle,
    /// but the protocol itself never copies the chunk body.
    ///
    /// # Errors
    ///
    /// Returns a storage or range error when the chunk cannot be staged.
    fn write(
        &mut self,
        session: &mut Self::Session,
        offset: u64,
        bytes: Arc<[u8]>,
    ) -> Result<(), ReplicationError>;

    /// Publishes a session after its canonical digest has been verified.
    ///
    /// # Errors
    ///
    /// Returns a storage or publication error when the session cannot be
    /// committed atomically.
    fn commit(
        &mut self,
        session: Self::Session,
        key: ObjectKey<T>,
        version: ObjectVersion<T>,
        len: u64,
        digest: [u8; 32],
    ) -> Result<Self::Receipt, ReplicationError>;

    /// Discards an unpublished session after any failed write or admission.
    fn abort(&mut self, session: Self::Session);
}

/// Computes the backend-version object digest directly over canonical encoded
/// bytes.  The encoded bytes are length-delimited once by the identity
/// grammar, so callers can feed chunks without assembling the complete value.
#[must_use]
pub fn canonical_object_digest<T: Schema>(
    len: u64,
    chunks: impl IntoIterator<Item = impl AsRef<[u8]>>,
) -> [u8; 32] {
    let expected = usize::try_from(len).unwrap_or(usize::MAX);
    let Ok(mut hasher) = ObjectVersionHasher::new(
        SchemaIdentity::new(T::DOMAIN, T::TYPE, T::VERSION),
        expected,
    ) else {
        return [0; 32];
    };
    for chunk in chunks {
        if hasher.update(chunk.as_ref()).is_err() {
            return [0; 32];
        }
    }
    hasher.finish().unwrap_or_default()
}

/// A reusable incremental canonical digest and byte-order checker.
///
/// This type is public for storage adapters that want to feed a stream from a
/// file descriptor or network reader directly into the same identity proof as
/// [`super::Transfer::stream_into`].
pub struct CanonicalDigest<T: Schema> {
    hasher: Option<ObjectVersionHasher>,
    expected_len: u64,
    written: u64,
    next_offset: u64,
    marker: PhantomData<fn() -> T>,
}

impl<T: Schema> CanonicalDigest<T> {
    /// Starts an incremental digest for one complete canonical value.
    #[must_use]
    pub fn new(expected_len: u64) -> Self {
        let expected = usize::try_from(expected_len).unwrap_or(usize::MAX);
        let hasher = ObjectVersionHasher::new(
            SchemaIdentity::new(T::DOMAIN, T::TYPE, T::VERSION),
            expected,
        )
        .ok();
        Self {
            hasher,
            expected_len,
            written: 0,
            next_offset: 0,
            marker: PhantomData,
        }
    }

    /// Feeds one contiguous chunk and returns an error for a gap, overlap, or
    /// arithmetic overflow.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Range`] for a gap, overlap, or excess
    /// bytes, and [`ReplicationError::Overflow`] when byte accounting wraps.
    pub fn push(&mut self, offset: u64, bytes: &[u8]) -> Result<(), ReplicationError> {
        if offset != self.next_offset {
            return Err(ReplicationError::Range);
        }
        let length = u64::try_from(bytes.len()).map_err(|_| ReplicationError::Overflow)?;
        let next = self
            .written
            .checked_add(length)
            .ok_or(ReplicationError::Overflow)?;
        if next > self.expected_len {
            return Err(ReplicationError::Range);
        }
        self.hasher
            .as_mut()
            .ok_or(ReplicationError::Overflow)?
            .update(bytes)
            .map_err(|_| ReplicationError::Range)?;
        self.written = next;
        self.next_offset = next;
        Ok(())
    }

    /// Finishes the digest, requiring exactly the declared byte length.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Incomplete`] when fewer than the declared
    /// number of bytes were incorporated.
    pub fn finish(self) -> Result<[u8; 32], ReplicationError> {
        if self.written != self.expected_len {
            return Err(ReplicationError::Incomplete);
        }
        self.hasher
            .ok_or(ReplicationError::Overflow)?
            .finish()
            .map_err(|_| ReplicationError::Incomplete)
    }

    /// Returns the number of bytes incorporated so far.
    #[must_use]
    pub const fn written(&self) -> u64 {
        self.written
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Bytes;
    impl Schema for Bytes {
        const DOMAIN: u8 = 0x71;
        const TYPE: u16 = 44;
        type Value = [u8];

        fn encode(value: &Self::Value, out: &mut Vec<u8>) {
            out.extend_from_slice(value);
        }
    }

    #[test]
    fn streaming_digest_matches_typed_version() {
        let value = b"canonical chunks";
        let expected = ObjectVersion::<Bytes>::from_value(value);
        let actual = canonical_object_digest::<Bytes>(
            value.len() as u64,
            [value[..9].as_ref(), value[9..].as_ref()],
        );
        assert_eq!(actual, *expected.as_bytes());

        let mut digest = CanonicalDigest::<Bytes>::new(value.len() as u64);
        assert!(digest.push(0, &value[..9]).is_ok());
        assert!(digest.push(9, &value[9..]).is_ok());
        assert_eq!(digest.finish().ok(), Some(actual));
    }

    #[test]
    fn streaming_digest_rejects_gaps_and_incomplete_values() {
        let mut digest = CanonicalDigest::<Bytes>::new(4);
        assert_eq!(digest.push(1, b"x"), Err(ReplicationError::Range));
        assert!(digest.push(0, b"xy").is_ok());
        assert_eq!(digest.finish(), Err(ReplicationError::Incomplete));
    }
}
