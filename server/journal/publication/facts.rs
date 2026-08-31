//! Defines publication facts behavior for `server-journal`, whose purpose is to persist and recover generation publication with bounded ownership.
//! This module owns the publication facts invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use super::errors::PublicationLimitError;
use crate::{ReceiptFacts, format::CHECKSUM_BYTES};
use heart_hydration::VerifiedGenerationFacts;

/// The fixed paths owned by one local durable publication directory.
#[derive(Clone, Debug)]
pub struct PublicationPaths {
    pub(super) directory: PathBuf,
}

impl PublicationPaths {
    /// Names the directory containing one journal, immutable fact, and visible head.
    #[must_use]
    pub fn in_directory(directory: &Path) -> Self {
        Self {
            directory: directory.to_path_buf(),
        }
    }

    pub(super) fn journal(&self) -> PathBuf {
        self.directory.join("journal")
    }

    pub(super) fn fact(&self) -> PathBuf {
        self.directory.join("publication.fact")
    }

    pub(super) fn head(&self) -> PathBuf {
        self.directory.join("publication.head")
    }

    pub(super) fn head_temp(&self) -> PathBuf {
        self.directory.join("publication.head.tmp")
    }
}

/// Bounded admission and physical group sizes for one publisher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationLimits {
    pub(super) queue_capacity: NonZeroUsize,
    pub(super) group_capacity: NonZeroUsize,
}

impl PublicationLimits {
    /// Validates the nonzero queue and reusable group capacities.
    ///
    /// The validation includes both frame-credit products, even though only the group product is
    /// retained as a byte buffer. This keeps an admitted frame-byte bound representable for every
    /// legal queue size.
    pub fn new(
        queue_capacity: NonZeroUsize,
        group_capacity: NonZeroUsize,
    ) -> Result<Self, PublicationLimitError> {
        queue_capacity
            .get()
            .checked_mul(crate::JOURNAL_FRAME_BYTES)
            .ok_or(PublicationLimitError::FrameBytesOverflow { queue_capacity })?;
        group_capacity
            .get()
            .checked_mul(crate::JOURNAL_FRAME_BYTES)
            .ok_or(PublicationLimitError::FrameBytesOverflow {
                queue_capacity: group_capacity,
            })?;
        Ok(Self {
            queue_capacity,
            group_capacity,
        })
    }
}

/// Checksum identity of the immutable publication fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImmutablePublicationIdentity {
    /// BLAKE3 checksum of the complete canonical fact payload.
    pub checksum: [u8; CHECKSUM_BYTES],
}

/// Checksum identity of the compact visible publication head.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationHeadIdentity {
    /// BLAKE3 checksum of the complete canonical head payload.
    pub checksum: [u8; CHECKSUM_BYTES],
}

/// Independently readable facts linking a stable journal receipt to both publication artifacts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct PublicationFacts {
    /// The exact validated generation facts whose stable journal receipt is retained below.
    ///
    /// These fields are decoded from the immutable fact on reopen, so clients can bind
    /// independently persisted semantic artifacts to the journal-selected generation without
    /// treating a mutable selector as authority.
    pub generation: VerifiedGenerationFacts,
    /// The stable journal receipt named by the immutable publication fact.
    pub stable: ReceiptFacts,
    /// Identity of the immutable publication fact.
    pub immutable: ImmutablePublicationIdentity,
    /// Identity of the visible compact publication head.
    pub head: PublicationHeadIdentity,
}
