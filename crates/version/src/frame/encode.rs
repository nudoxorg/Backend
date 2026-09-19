//! Defines encode behavior for `frame`, whose purpose is to encode canonical bounded frames into caller-owned storage.
//! This module owns the encode invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Canonical write surface and prepared-frame execution.

mod layout;

use core::{
    borrow::Borrow,
    mem::{align_of, size_of},
    ops::Deref,
};

use crate::schema::{
    FORMAT_MAJOR, FORMAT_MINOR, FRAME_HEADER_BYTES, FRAME_MAGIC, FRAME_SCHEMA_ID, FrameHeader,
    LimitAmount, RowCount, SectionKind,
};
use zerocopy::{
    IntoBytes,
    byteorder::{U16, U32},
};

use self::layout::{Layout, body_end, body_offset, descriptor, preflight};

/// Exact number of bytes in one fully preflighted canonical frame.
///
/// This is a process-native slice length, but preflight proves it also fits the
/// protocol's `u32` total-length field. Keeping that proof attached prevents
/// callers from confusing a frame length with a section body or buffer
/// capacity.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EncodedFrameLength(u32);

impl From<EncodedFrameLength> for usize {
    #[allow(
        clippy::as_conversions,
        reason = "u32 frame lengths are natively representable on the explicitly supported 32/64-bit targets"
    )]
    fn from(value: EncodedFrameLength) -> Self {
        value.0 as usize
    }
}

impl From<EncodedFrameLength> for u32 {
    fn from(value: EncodedFrameLength) -> Self {
        value.0
    }
}

impl Deref for EncodedFrameLength {
    type Target = u32;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<u32> for EncodedFrameLength {
    fn as_ref(&self) -> &u32 {
        self
    }
}

impl Borrow<u32> for EncodedFrameLength {
    fn borrow(&self) -> &u32 {
        self
    }
}

impl EncodedFrameLength {
    /// Constructs the protocol scalar after layout's compile-time grammar bound
    /// and caller-shaped arena preflight have proved the native coordinate fits.
    #[allow(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "layout proves total bytes <= MAX_FRAME_BYTES <= u32::MAX before this private boundary"
    )]
    pub(super) const fn from_layout(value: usize) -> Self {
        Self(value as u32)
    }
}

/// A borrowed, known section body to encode into a canonical frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SectionInput<'a> {
    /// Finite known body kind.
    pub kind: SectionKind,
    /// Bounded logical row count.
    pub rows: RowCount,
    /// Caller-owned body bytes borrowed during encoding.
    pub bytes: &'a [u8],
}

/// A local byte count that may exceed the protocol's `u32` addressable domain.
///
/// It distinguishes input slice capacities and failed preflight arithmetic from
/// validated [`EncodedFrameLength`] values without pretending those local
/// quantities are wire values.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NativeByteCount(usize);

impl From<usize> for NativeByteCount {
    fn from(value: usize) -> Self {
        Self(value)
    }
}

impl From<NativeByteCount> for usize {
    fn from(value: NativeByteCount) -> Self {
        value.0
    }
}

impl Deref for NativeByteCount {
    type Target = usize;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<usize> for NativeByteCount {
    fn as_ref(&self) -> &usize {
        self
    }
}

impl Borrow<usize> for NativeByteCount {
    fn borrow(&self) -> &usize {
        self
    }
}

/// Native input count before it has passed the closed canonical vocabulary bound.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InputSectionCount(usize);

impl From<usize> for InputSectionCount {
    fn from(value: usize) -> Self {
        Self(value)
    }
}

impl From<InputSectionCount> for usize {
    fn from(value: InputSectionCount) -> Self {
        value.0
    }
}

impl Deref for InputSectionCount {
    type Target = usize;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<usize> for InputSectionCount {
    fn as_ref(&self) -> &usize {
        self
    }
}

impl Borrow<usize> for InputSectionCount {
    fn borrow(&self) -> &usize {
        self
    }
}

/// A reason canonical frame sizing or encoding could not proceed.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EncodeError {
    /// The caller supplied more inputs than the closed canonical kind vocabulary permits.
    #[error("section count {count:?} exceeds {maximum}")]
    TooManySections {
        /// Supplied descriptor count.
        count: InputSectionCount,
        /// Fixed protocol maximum.
        maximum: LimitAmount,
    },
    /// Two descriptors name the same kind; canonical frames have at most one of each kind.
    #[error("duplicate section kind {kind:?}")]
    DuplicateKind {
        /// Repeated known section kind.
        kind: SectionKind,
    },
    /// The descriptors are not in strict ascending wire-kind order.
    #[error("section kind {next:?} follows {previous:?}")]
    OutOfOrderKind {
        /// Earlier, larger kind.
        previous: SectionKind,
        /// Later, smaller kind.
        next: SectionKind,
    },
    /// Sum of body byte lengths exceeds the fixed arena maximum.
    #[error("adding {added:?} body bytes to {current:?} exceeds {maximum}")]
    ArenaTooLarge {
        /// Body bytes accumulated before this section.
        current: NativeByteCount,
        /// This section's requested body bytes.
        added: NativeByteCount,
        /// Fixed protocol maximum.
        maximum: LimitAmount,
    },
    /// Caller-owned output has insufficient capacity; no output is written before this error.
    #[error("output has {available:?} bytes but needs {required:?}")]
    OutputTooShort {
        /// Exact preflighted output length.
        required: EncodedFrameLength,
        /// Caller-provided output length.
        available: NativeByteCount,
    },
}

/// Computes the exact canonical byte length before writing or allocating output storage.
///
/// # Errors
///
/// Returns [`EncodeError`] for excessive resources, arithmetic overflow, or noncanonical order.
pub fn encoded_len(sections: &[SectionInput<'_>]) -> Result<EncodedFrameLength, EncodeError> {
    Ok(PreparedFrame::new(sections)?.encoded_len)
}

/// Encodes sections into a caller-owned buffer and returns the exact bytes written.
///
/// # Errors
///
/// Invalid input and insufficient output capacity return [`EncodeError`] before any write.
pub fn encode_into(
    sections: &[SectionInput<'_>],
    output: &mut [u8],
) -> Result<EncodedFrameLength, EncodeError> {
    PreparedFrame::new(sections)?.encode_into(output)
}

/// A preflighted canonical layout whose length and output ranges are already proven.
pub struct PreparedFrame<'sections, 'body> {
    sections: &'sections [SectionInput<'body>],
    layout: Layout,
    /// Exact caller-owned output bytes required by this validated layout.
    pub encoded_len: EncodedFrameLength,
}

impl<'sections, 'body> PreparedFrame<'sections, 'body> {
    /// Preflights borrowed sections into one canonical frame layout.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError`] when ordering, resource bounds, or layout arithmetic is invalid.
    pub fn new(sections: &'sections [SectionInput<'body>]) -> Result<Self, EncodeError> {
        let layout = preflight(sections)?;
        Ok(Self {
            encoded_len: layout.total_bytes,
            layout,
            sections,
        })
    }

    /// Writes this already-preflighted frame into caller-owned storage.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::OutputTooShort`] before writing any byte when capacity is insufficient.
    #[allow(
        clippy::indexing_slicing,
        reason = "Every range is a PreparedFrame proof fact and output capacity is checked before the first write."
    )]
    pub fn encode_into(&self, output: &mut [u8]) -> Result<EncodedFrameLength, EncodeError> {
        let layout = self.layout;
        let total_bytes = usize::from(layout.total_bytes);
        if output.len() < total_bytes {
            return Err(EncodeError::OutputTooShort {
                required: layout.total_bytes,
                available: output.len().into(),
            });
        }
        let frame = &mut output[..total_bytes];
        let header = FrameHeader {
            magic: FRAME_MAGIC,
            major: FORMAT_MAJOR,
            minor: FORMAT_MINOR,
            header_bytes: U16::new(layout.header_bytes),
            total_bytes: U32::new(u32::from(layout.total_bytes)),
            section_count: U16::new(layout.section_count),
            reserved_a: U16::new(0),
            schema: U32::new(u32::from(FRAME_SCHEMA_ID)),
            reserved_b: U32::new(0),
        };
        frame[..FRAME_HEADER_BYTES].copy_from_slice(header.as_bytes());
        let mut previous_end = usize::from(layout.header_bytes);
        for (index, section) in self.sections.iter().enumerate() {
            let offset = body_offset(previous_end);
            let end = body_end(offset, section.bytes.len());
            let (descriptor_offset, descriptor) = descriptor(index, section, offset);
            frame[previous_end..offset].fill(0);
            frame[descriptor_offset..descriptor_offset + crate::schema::SECTION_DESCRIPTOR_BYTES]
                .copy_from_slice(descriptor.as_bytes());
            frame[offset..end].copy_from_slice(section.bytes);
            previous_end = end;
        }
        Ok(layout.total_bytes)
    }
}

const _: [(); size_of::<EncodedFrameLength>()] = [(); size_of::<u32>()];
const _: [(); align_of::<EncodedFrameLength>()] = [(); align_of::<u32>()];

#[cfg(test)]
#[path = "encode/tests.rs"]
mod tests;
