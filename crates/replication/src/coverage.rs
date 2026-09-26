//! Bounded byte coverage, version negotiation ranges, and allocation limits.

use backend_version::Schema;
use core::{mem::size_of, num::NonZeroU64};

use crate::ReplicationError;

mod sparse;

/// A bounded half-open byte interval.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ByteRange {
    /// First byte included in the interval.
    pub start: u64,
    /// Number of bytes in the interval.
    pub len: u64,
}
impl ByteRange {
    /// Creates an interval after checking that its end does not overflow.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Overflow`] when the end would overflow.
    pub fn new(start: u64, len: u64) -> Result<Self, ReplicationError> {
        start.checked_add(len).ok_or(ReplicationError::Overflow)?;
        Ok(Self { start, len })
    }
    /// Returns the first byte after this interval.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Overflow`] when the end would overflow.
    pub fn end(self) -> Result<u64, ReplicationError> {
        self.start
            .checked_add(self.len)
            .ok_or(ReplicationError::Overflow)
    }
    /// Returns whether this interval contains a byte position.
    #[must_use]
    pub fn contains(self, position: u64) -> bool {
        self.start <= position
            && self
                .start
                .checked_add(self.len)
                .is_some_and(|end| position < end)
    }
    /// Returns whether two intervals overlap or touch.
    #[must_use]
    pub fn touches(self, other: Self) -> bool {
        let Some(self_end) = self.start.checked_add(self.len) else {
            return false;
        };
        let Some(other_end) = other.start.checked_add(other.len) else {
            return false;
        };
        self.start <= other_end && other.start <= self_end
    }
    /// Merges two touching intervals.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Range`] for disjoint intervals or
    /// [`ReplicationError::Overflow`] when the merged end overflows.
    pub fn merge(self, other: Self) -> Result<Self, ReplicationError> {
        if !self.touches(other) {
            return Err(ReplicationError::Range);
        }
        let start = self.start.min(other.start);
        let end = self.end()?.max(other.end()?);
        Self::new(start, end - start)
    }
}

/// A bounded set of sorted, coalesced covered intervals.
#[derive(Clone, Debug)]
pub struct SparseCoverage {
    ranges: Vec<ByteRange>,
    max_ranges: usize,
}

impl PartialEq for SparseCoverage {
    fn eq(&self, other: &Self) -> bool {
        // The interval budget is an admission policy for future mutations;
        // it is deliberately absent from the wire grammar. Two admitted
        // observations with the same canonical intervals therefore describe
        // the same coverage even when their local envelopes differ.
        self.ranges == other.ranges
    }
}

impl Eq for SparseCoverage {}

/// A bounded transfer identifier.
///
/// The private nonzero representation prevents wire decoders, tests, and
/// application code from manufacturing an invalid token:
///
/// ```compile_fail
/// use backend_replication::TransferId;
/// let _forged = TransferId(0);
/// let _default = TransferId::default();
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TransferId(NonZeroU64);
impl TransferId {
    /// Creates a nonzero transfer identifier.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::InvalidIdentifier`] for zero.
    pub const fn new(value: u64) -> Result<Self, ReplicationError> {
        match NonZeroU64::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(ReplicationError::InvalidIdentifier),
        }
    }

    /// Returns the validated numeric transfer identifier.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// A pure recipe attempt identifier.
///
/// ```compile_fail
/// use backend_replication::AttemptId;
/// let _forged = AttemptId(0);
/// let _default = AttemptId::default();
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AttemptId(NonZeroU64);
impl AttemptId {
    /// Creates a nonzero attempt identifier.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::InvalidIdentifier`] for zero.
    pub const fn new(value: u64) -> Result<Self, ReplicationError> {
        match NonZeroU64::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(ReplicationError::InvalidIdentifier),
        }
    }

    /// Returns the validated numeric attempt identifier.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Input accepted by [`Fence::new`].
pub trait FenceInput {
    /// Converts the input into the exact wire fence representation.
    fn into_fence_bytes(self) -> [u8; 32];
}

impl FenceInput for [u8; 32] {
    fn into_fence_bytes(self) -> [u8; 32] {
        self
    }
}

impl FenceInput for u64 {
    fn into_fence_bytes(self) -> [u8; 32] {
        let mut bytes = [0; 32];
        bytes[24..].copy_from_slice(&self.to_be_bytes());
        bytes
    }
}

/// A scheduler-owned exact publication fence.
///
/// ```compile_fail
/// use backend_replication::Fence;
/// let _forged = Fence([1; 32]);
/// let _default = Fence::default();
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Fence([u8; 32]);
impl Fence {
    /// Creates a nonzero fence from a fixed-width token or a legacy numeric
    /// seed. Numeric seeds are widened into the low eight bytes and are only a
    /// compatibility constructor; wire values always contain all 32 bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::InvalidIdentifier`] for zero.
    pub fn new<I: FenceInput>(value: I) -> Result<Self, ReplicationError> {
        Self::from_bytes(value.into_fence_bytes())
    }
    /// Creates a fence from its exact fixed-width wire token.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::InvalidIdentifier`] for an all-zero token.
    pub fn from_bytes(value: [u8; 32]) -> Result<Self, ReplicationError> {
        if value == [0; 32] {
            Err(ReplicationError::InvalidIdentifier)
        } else {
            Ok(Self(value))
        }
    }
    /// Creates a compatibility fence from a nonzero numeric seed.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::InvalidIdentifier`] for zero.
    pub const fn from_u64(value: u64) -> Result<Self, ReplicationError> {
        if value == 0 {
            return Err(ReplicationError::InvalidIdentifier);
        }
        let encoded = value.to_be_bytes();
        let mut bytes = [0; 32];
        bytes[24] = encoded[0];
        bytes[25] = encoded[1];
        bytes[26] = encoded[2];
        bytes[27] = encoded[3];
        bytes[28] = encoded[4];
        bytes[29] = encoded[5];
        bytes[30] = encoded[6];
        bytes[31] = encoded[7];
        Ok(Self(bytes))
    }
    /// Returns the exact fence token.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
    /// Returns whether this token is the invalid all-zero value.
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.0 == [0; 32]
    }
}

/// A scheduler-owned cancellation identity observed by a worker.
///
/// ```compile_fail
/// use backend_replication::CancellationId;
/// let _forged = CancellationId([1; 32]);
/// let _default = CancellationId::default();
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CancellationId([u8; 32]);
impl CancellationId {
    /// Creates a nonzero cancellation identity.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::InvalidIdentifier`] for an all-zero token.
    pub fn new(value: [u8; 32]) -> Result<Self, ReplicationError> {
        if value == [0; 32] {
            Err(ReplicationError::InvalidIdentifier)
        } else {
            Ok(Self(value))
        }
    }
    /// Returns the exact cancellation token for transport and observation.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
    /// Returns whether this token is invalid.
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.0 == [0; 32]
    }
}

/// An authority epoch.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AuthorityEpoch(pub u64);
/// A versioned revocation observation.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RevocationVersion(pub u64);

/// A schema or protocol version interval.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VersionRange {
    /// Lowest supported version, inclusive.
    pub min: u16,
    /// Highest supported version, inclusive.
    pub max: u16,
}
impl VersionRange {
    /// Returns a validated range.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::InvalidVersionRange`] when `min` is above
    /// `max`.
    pub const fn new(min: u16, max: u16) -> Result<Self, ReplicationError> {
        if min > max {
            Err(ReplicationError::InvalidVersionRange)
        } else {
            Ok(Self { min, max })
        }
    }
    /// Returns the highest mutually supported version.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::NoCommonVersion`] when the ranges do not
    /// overlap.
    pub const fn negotiate(self, peer: Self) -> Result<u16, ReplicationError> {
        if self.min > self.max || peer.min > peer.max {
            return Err(ReplicationError::InvalidVersionRange);
        }
        let low = if self.min > peer.min {
            self.min
        } else {
            peer.min
        };
        let high = if self.max < peer.max {
            self.max
        } else {
            peer.max
        };
        if low <= high {
            Ok(high)
        } else {
            Err(ReplicationError::NoCommonVersion)
        }
    }
}

/// A domain-separated schema descriptor advertised during negotiation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SchemaDescriptor {
    /// Schema domain byte.
    pub domain: u8,
    /// Schema type number.
    pub type_id: u16,
    /// Supported canonical versions.
    pub versions: VersionRange,
}
impl SchemaDescriptor {
    /// Describes a backend-version schema.
    #[must_use]
    pub const fn of<T: Schema>() -> Self {
        Self {
            domain: T::DOMAIN,
            type_id: T::TYPE,
            versions: VersionRange {
                min: T::VERSION as u16,
                max: T::VERSION as u16,
            },
        }
    }
    /// Negotiates this descriptor with a peer descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::NoCommonSchema`] for different domains or
    /// type numbers, or [`ReplicationError::NoCommonVersion`] for disjoint
    /// version ranges.
    pub fn negotiate(self, peer: Self) -> Result<Self, ReplicationError> {
        if self.domain != peer.domain || self.type_id != peer.type_id {
            return Err(ReplicationError::NoCommonSchema);
        }
        if self.versions.min > self.versions.max || peer.versions.min > peer.versions.max {
            return Err(ReplicationError::InvalidVersionRange);
        }
        Ok(Self {
            domain: self.domain,
            type_id: self.type_id,
            versions: VersionRange {
                min: if self.versions.min > peer.versions.min {
                    self.versions.min
                } else {
                    peer.versions.min
                },
                max: self.versions.negotiate(peer.versions)?,
            },
        })
    }
}

/// A bounded transfer and message budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportLimits {
    /// Maximum estimated encoded message size.
    pub max_frame: usize,
    /// Maximum chunk payload size.
    pub max_chunk: usize,
    /// Maximum immutable object size.
    pub max_object: u64,
    /// Maximum object summaries in one root summary.
    pub max_objects: usize,
    /// Maximum range requests in one message.
    pub max_ranges: usize,
    /// Maximum capability entries in one advertisement.
    pub max_capabilities: usize,
    /// Maximum bytes in a range key.
    pub max_key_bytes: usize,
    /// Maximum recipe input identities.
    pub max_inputs: usize,
}
impl Default for TransportLimits {
    fn default() -> Self {
        Self {
            max_frame: 1024 * 1024,
            max_chunk: 64 * 1024,
            max_object: 64 * 1024 * 1024,
            max_objects: 4096,
            max_ranges: 1024,
            max_capabilities: 1024,
            max_key_bytes: 4096,
            max_inputs: 4096,
        }
    }
}
impl TransportLimits {
    /// Validates the budget itself before it is used for allocation.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::InvalidLimits`] for zero or inconsistent
    /// limits.
    pub const fn validate(self) -> Result<Self, ReplicationError> {
        if self.max_frame == 0
            || self.max_chunk == 0
            || self.max_object == 0
            || self.max_objects == 0
            || self.max_ranges == 0
            || self.max_capabilities == 0
            || self.max_key_bytes == 0
            || self.max_inputs == 0
            || self.max_chunk > self.max_frame
        {
            Err(ReplicationError::InvalidLimits)
        } else {
            Ok(self)
        }
    }
}
