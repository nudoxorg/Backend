//! Defines validate error behavior for `heart-view`, whose purpose is to validate and borrow canonical framed data without allocating.
//! This module owns the validate error invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Public structural validation failures.
#![allow(
    missing_docs,
    reason = "Each documented variant names and explains its complete structured payload; repeating field labels would not add meaning."
)]

use heart_schema::{LimitError, LimitKind, SECTION_DESCRIPTOR_BYTES, SectionCompatibilityError};

/// A precise structural reason untrusted frame bytes could not become a validation witness.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ValidateError {
    /// The input ended before a fixed-width field was available.
    #[error("truncated at {offset}: need {needed} bytes, have {available}")]
    Truncated {
        offset: usize,
        needed: usize,
        available: usize,
    },
    /// The first four bytes are not the frame magic.
    #[error("invalid frame magic {observed:?}")]
    BadMagic {
        /// Exact four bytes observed at the envelope start.
        observed: [u8; 4],
    },
    /// The envelope version has no explicit decoder support.
    #[error("unsupported frame version {major}.{minor}")]
    UnsupportedVersion { major: u8, minor: u8 },
    /// The compact schema tag is outside the closed Wave 1 registry.
    #[error("unsupported schema {wire:#010x}")]
    UnsupportedSchema { wire: u32 },
    /// A required-zero header field was nonzero.
    #[error("nonzero reserved header field at {offset}")]
    NonzeroHeaderReserved { offset: usize },
    /// The declared frame length is not exactly the supplied slice length.
    #[error("declared frame length {declared} differs from {actual}")]
    TotalLengthMismatch { declared: u32, actual: usize },
    /// The declared length cannot contain the fixed header.
    #[error("frame length {declared} is shorter than {minimum}")]
    FrameTooShort { declared: usize, minimum: usize },
    /// A decoded value exceeded the selected per-decoder policy.
    #[error("{kind:?} value {observed} exceeds {maximum}")]
    LimitExceeded {
        kind: LimitKind,
        observed: u32,
        maximum: u32,
    },
    /// Header length differs from the exact fixed-header-plus-directory length.
    #[error("header length {declared} differs from {expected}")]
    HeaderLengthMismatch { declared: u16, expected: usize },
    /// Header plus directory would extend outside the declared frame.
    #[error("header length {header_bytes} extends outside frame length {total_bytes}")]
    HeaderOutsideFrame {
        header_bytes: usize,
        total_bytes: usize,
    },
    /// One section directory descriptor violates a local structural invariant.
    #[error("descriptor {index}: {error}")]
    Descriptor { index: u16, error: DescriptorError },
    /// Bytes remain after the last canonical section body.
    #[error("canonical frame ends at {expected_end}, not {declared_total}")]
    TrailingBytes {
        expected_end: usize,
        declared_total: usize,
    },
}

/// A local structural failure in one section directory descriptor.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DescriptorError {
    /// Flags and kind do not describe a supported or explicitly optional section.
    #[error("incompatible kind {kind:#04x} with flags {flags:#04x}: {source}")]
    IncompatibleKind {
        kind: u8,
        flags: u8,
        /// Exact compatibility law rejection.
        #[source]
        source: SectionCompatibilityError,
    },
    /// The descriptor's required-zero field was nonzero.
    #[error("nonzero reserved bytes")]
    NonzeroReserved,
    /// Directory codes must be strictly increasing, so duplicates are forbidden.
    #[error("duplicate kind {kind:#04x}")]
    DuplicateKind { kind: u8 },
    /// Directory codes must be strictly increasing in canonical frames.
    #[error("kind {next:#04x} follows {previous:#04x}")]
    OutOfOrderKind { previous: u8, next: u8 },
    /// A body offset starts before the header and directory.
    #[error("body offset {offset} is before {minimum}")]
    OffsetBeforeBodies { offset: usize, minimum: usize },
    /// A body offset is not aligned to the protocol's fixed alignment.
    #[error("body offset {offset} is not aligned to {alignment}")]
    MisalignedOffset { offset: usize, alignment: usize },
    /// A body begins before the preceding body ends.
    #[error("body offset {offset} overlaps prior end {previous_end}")]
    OverlappingBody { offset: usize, previous_end: usize },
    /// A body begins at an aligned but noncanonical gap after the preceding body.
    #[error("body offset {actual} differs from canonical {expected}")]
    NoncanonicalOffset { expected: usize, actual: usize },
    /// The body span extends outside the declared frame length.
    #[error("body {offset}..+{length} exceeds frame {total}")]
    BodyOutsideFrame {
        offset: usize,
        length: usize,
        total: usize,
    },
    /// An alignment byte differs from zero, creating a noncanonical representation.
    #[error("nonzero padding at {offset}")]
    NonzeroPadding { offset: usize },
}

/// A descriptor or body changed after frame validation, or the reborrow logic
/// drifted from the validator's checked ranges.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SectionReadError {
    /// The retained descriptor ordinal no longer selects one complete record.
    #[error(
        "validated descriptor {index} has {available} bytes, expected {SECTION_DESCRIPTOR_BYTES}"
    )]
    DescriptorWindow { index: u16, available: usize },
    /// A body offset cannot be represented by the target address space.
    #[error("validated descriptor {index} body offset {raw} does not fit this target")]
    BodyOffset {
        index: u16,
        raw: u32,
        #[source]
        source: core::num::TryFromIntError,
    },
    /// A body length cannot be represented by the target address space.
    #[error("validated descriptor {index} body length {raw} does not fit this target")]
    BodyLength {
        index: u16,
        raw: u32,
        #[source]
        source: core::num::TryFromIntError,
    },
    /// The retained body range no longer lies inside the immutable frame.
    #[error("validated descriptor {index} body {offset}..+{length} exceeds {available} bytes")]
    BodyWindow {
        index: u16,
        offset: usize,
        length: usize,
        available: usize,
    },
    /// The retained row scalar no longer preserves the protocol limit.
    #[error("validated descriptor {index} row count changed")]
    Rows {
        index: u16,
        #[source]
        source: LimitError,
    },
}
