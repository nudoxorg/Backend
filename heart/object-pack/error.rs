//! Defines error behavior for `heart-object-pack`, whose purpose is to write and borrow indexed immutable object packs.
//! This module owns the error invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::num::TryFromIntError;

use heart_identity::{ContentId, ContentIdDecodeError};
use heart_object::ObjectLength;
use heart_schema::UnknownSchemaId;
use thiserror::Error;

use crate::{ObjectPackBytes, ObjectPackObjectCount};

/// Exact rejection while preparing, opening, or writing a canonical object pack.
#[derive(Debug, Error, PartialEq)]
pub enum ObjectPackError {
    /// Descriptor length and caller payload length differ.
    #[error("pack input {ordinal} has {actual:?} bytes but descriptor declares {declared:?}")]
    InputLength {
        /// Input position.
        ordinal: usize,
        /// Immutable descriptor length.
        declared: ObjectLength,
        /// Caller payload length.
        actual: ObjectPackBytes,
    },
    /// Caller payload does not match its descriptor identity.
    #[error("pack input {ordinal} hashes to {actual:?}, not {expected:?}")]
    InputContent {
        /// Input position.
        ordinal: usize,
        /// Descriptor content identity.
        expected: ContentId<heart_identity::ObjectDomain>,
        /// Identity calculated from caller bytes.
        actual: ContentId<heart_identity::ObjectDomain>,
    },
    /// Inputs are not a strict ascending content-identity set.
    #[error("pack input {ordinal} content {current:?} does not strictly follow {previous:?}")]
    InputOrder {
        /// First non-strict input position.
        ordinal: usize,
        /// Preceding content identity.
        previous: ContentId<heart_identity::ObjectDomain>,
        /// Rejected current content identity.
        current: ContentId<heart_identity::ObjectDomain>,
    },
    /// A descriptor length cannot index this process address space.
    #[error("pack input {ordinal} descriptor length {declared:?} exceeds native address space")]
    DescriptorLengthAddressSpace {
        /// Input position.
        ordinal: usize,
        /// Rejected descriptor length.
        declared: ObjectLength,
        /// Native conversion source.
        #[source]
        source: TryFromIntError,
    },
    /// Exact layout addition overflowed native address space.
    #[error("pack layout cannot add {right:?} bytes to {left:?}")]
    LayoutOverflow {
        /// Existing measured extent.
        left: ObjectPackBytes,
        /// Requested additional extent.
        right: ObjectPackBytes,
    },
    /// Caller output is shorter than the measured canonical pack.
    #[error("pack output has {available:?} bytes but requires {required:?}")]
    OutputTooSmall {
        /// Exact prepared extent.
        required: ObjectPackBytes,
        /// Caller-provided extent.
        available: ObjectPackBytes,
    },
    /// A count cannot fit native directory coordinates.
    #[error("pack object count {count:?} exceeds native address space")]
    CountAddressSpace {
        /// Declared object count.
        count: ObjectPackObjectCount,
        /// Native conversion source.
        #[source]
        source: TryFromIntError,
    },
    /// Count times the fixed directory row width overflowed.
    #[error("pack directory count {count:?} overflows {row_bytes:?}-byte rows")]
    IndexLayoutOverflow {
        /// Declared object count.
        count: ObjectPackObjectCount,
        /// Typed fixed directory row width.
        row_bytes: ObjectPackBytes,
    },
    /// The fixed count header is incomplete.
    #[error("pack header needs {required:?} bytes but only {available:?} are available")]
    HeaderTruncated {
        /// Exact required prefix length.
        required: ObjectPackBytes,
        /// Supplied prefix length.
        available: ObjectPackBytes,
    },
    /// The fixed count header has trailing bytes.
    #[error("pack header has {actual:?} bytes but requires exactly {expected:?}")]
    HeaderTrailing {
        /// Exact fixed header extent.
        expected: ObjectPackBytes,
        /// Supplied extent.
        actual: ObjectPackBytes,
    },
    /// A count-selected directory is not exactly its required byte extent.
    #[error("pack directory has {actual:?} bytes but requires exactly {expected:?}")]
    DirectoryExtent {
        /// Exact count-selected index extent.
        expected: ObjectPackBytes,
        /// Complete supplied index extent.
        actual: ObjectPackBytes,
    },
    /// A directory schema cell is outside the closed schema registry.
    #[error("pack directory row {ordinal} has an unknown schema")]
    DirectorySchema {
        /// Directory row containing the rejected schema cell.
        ordinal: usize,
        /// Exact closed-schema conversion rejection.
        #[source]
        source: UnknownSchemaId,
    },
    /// A directory content cell carries the wrong closed identity authority.
    #[error("pack directory row {ordinal} has an invalid content identity")]
    DirectoryContent {
        /// Directory row containing the rejected content cell.
        ordinal: usize,
        /// Exact checked identity decode failure.
        #[source]
        source: ContentIdDecodeError,
    },
    /// Directory content cells are not strictly ascending.
    #[error("pack directory content {current:?} does not follow {previous:?} at row {ordinal}")]
    DirectoryOrder {
        /// First non-strict directory row.
        ordinal: usize,
        /// Complete preceding canonical content cell.
        previous: [u8; 32],
        /// Complete rejected canonical content cell.
        current: [u8; 32],
    },
    /// A directory descriptor length cannot address this process.
    #[error("pack directory row {ordinal} length {declared:?} exceeds native address space")]
    DirectoryLengthAddressSpace {
        /// Directory row carrying the descriptor length.
        ordinal: usize,
        /// Exact decoded descriptor length.
        declared: ObjectLength,
        /// Native coordinate conversion rejection.
        #[source]
        source: TryFromIntError,
    },
    /// A directory cumulative addition overflowed native coordinates.
    #[error("pack directory row {ordinal} cannot add {right:?} bytes to {left:?}")]
    DirectoryCumulativeOverflow {
        /// Directory row whose declared length was added.
        ordinal: usize,
        /// Exact preceding cumulative body extent.
        left: ObjectPackBytes,
        /// Exact decoded descriptor length extent.
        right: ObjectPackBytes,
    },
    /// A directory encoded cumulative end cannot address this process.
    #[error("pack directory row {ordinal} body end {observed} exceeds native address space")]
    DirectoryEndAddressSpace {
        /// Directory row carrying the encoded cumulative end.
        ordinal: usize,
        /// Exact encoded cumulative end.
        observed: u64,
        /// Native coordinate conversion rejection.
        #[source]
        source: TryFromIntError,
    },
    /// A directory cumulative end differs from descriptor-length accumulation.
    #[error("pack directory row {ordinal} ends at {observed:?}, expected {expected:?}")]
    DirectoryCumulativeEnd {
        /// Directory row with the inconsistent cumulative end.
        ordinal: usize,
        /// Exact descriptor-length accumulation.
        expected: ObjectPackBytes,
        /// Exact encoded cumulative end.
        observed: ObjectPackBytes,
    },
    /// A complete-pack view was not exactly the validated index plus all bodies.
    #[error("pack has {actual:?} bytes but requires exactly {expected:?}")]
    PackExtent {
        /// Validated complete pack extent.
        expected: ObjectPackBytes,
        /// Complete supplied pack extent.
        actual: ObjectPackBytes,
    },
    /// A selected borrowed body did not reproduce its validated content identity.
    #[error("pack object {expected:?} hashes to {actual:?}")]
    ObjectContent {
        /// Identity claimed by the selected validated descriptor.
        expected: ContentId<heart_identity::ObjectDomain>,
        /// Identity calculated from the selected borrowed body.
        actual: ContentId<heart_identity::ObjectDomain>,
    },
}
