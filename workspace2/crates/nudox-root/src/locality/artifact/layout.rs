use core::mem::size_of;

use nudox_object::OBJECT_DESCRIPTOR_RECORD_BYTES;

use crate::MetadataBytes;

use super::{errors::LocalityError, header::HEADER_BYTES};

/// Semantic sparse exception-row cardinality.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub(super) struct ExceptionCount(u32);

/// Semantic promise payload cardinality.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub(super) struct PromiseCount(u32);

/// Semantic present-overlay descriptor cardinality.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub(super) struct PresentOverlayCount(u32);

/// Semantic overlay class cardinality.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub(super) struct OverlayCount(u32);

macro_rules! count {
    ($type:ident) => {
        impl From<u32> for $type {
            fn from(value: u32) -> Self {
                Self(value)
            }
        }
        impl From<$type> for u32 {
            fn from(value: $type) -> Self {
                value.0
            }
        }
    };
}

macro_rules! checked_count {
    ($type:ident) => {
        count!($type);
        impl $type {
            pub(super) const fn checked_next(self) -> Option<Self> {
                match self.0.checked_add(1) {
                    Some(next) => Some(Self(next)),
                    None => None,
                }
            }
        }
    };
}

checked_count!(ExceptionCount);
checked_count!(PromiseCount);
checked_count!(PresentOverlayCount);
count!(OverlayCount);

/// Fixed rank-directory block width: exactly four membership words.
pub(super) const RANK_BLOCK_ROWS: u32 = 256;
pub(super) const WORD_BITS: u32 = u64::BITS;
pub(super) const WORDS_PER_RANK_BLOCK: u32 = RANK_BLOCK_ROWS / WORD_BITS;
const _: () = assert!(WORDS_PER_RANK_BLOCK == 4);

/// Compact measured locality layout. Native ranges are stack-only geometry,
/// never retained by a borrowed validation witness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalityLayout {
    exceptions: ExceptionCount,
    promises: PromiseCount,
    present_overlays: PresentOverlayCount,
    bytes: MetadataBytes,
}

impl LocalityLayout {
    pub(super) fn new(
        exceptions: ExceptionCount,
        promises: PromiseCount,
        present_overlays: PresentOverlayCount,
    ) -> Result<Self, LocalityError> {
        let overlays = overlay_count(exceptions, promises)?;
        if u32::from(present_overlays) > u32::from(overlays) {
            return Err(LocalityError::PresentCountExceedsOverlays {
                present: u32::from(present_overlays),
                overlays: u32::from(overlays),
            });
        }
        let geometry = Geometry::from_counts(exceptions, promises, present_overlays, overlays);
        let bytes = usize::try_from(geometry.complete).map_err(|source| {
            LocalityError::LayoutAddressSpace {
                attempted: geometry.complete,
                source,
            }
        })?;
        Ok(Self {
            exceptions,
            promises,
            present_overlays,
            bytes: bytes.into(),
        })
    }

    pub(crate) fn from_counts(
        exceptions: u32,
        promises: u32,
        present_overlays: u32,
    ) -> Result<Self, LocalityError> {
        Self::new(exceptions.into(), promises.into(), present_overlays.into())
    }

    #[must_use]
    /// Returns the exact complete artifact extent.
    pub const fn bytes(&self) -> MetadataBytes {
        self.bytes
    }
    pub(super) const fn exceptions(&self) -> ExceptionCount {
        self.exceptions
    }
    pub(super) const fn promises(&self) -> PromiseCount {
        self.promises
    }
    pub(super) const fn present_overlays(&self) -> PresentOverlayCount {
        self.present_overlays
    }

    pub(super) fn lanes(&self) -> LaneTable {
        LaneTable::from_validated_counts(self.exceptions, self.promises, self.present_overlays)
    }
}

fn overlay_count(
    exceptions: ExceptionCount,
    promises: PromiseCount,
) -> Result<OverlayCount, LocalityError> {
    u32::from(exceptions)
        .checked_sub(u32::from(promises))
        .map(OverlayCount)
        .ok_or(LocalityError::PromiseCountExceedsExceptions {
            promises: u32::from(promises),
            exceptions: u32::from(exceptions),
        })
}

/// Stack-only computed byte coordinates for one proven count tuple.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::locality) struct LaneTable {
    pub(super) rows: usize,
    pub(super) promise_bits: usize,
    pub(super) promise_ranks: usize,
    pub(super) providers: usize,
    pub(super) overlay_bits: usize,
    pub(super) overlay_ranks: usize,
    pub(super) present: usize,
    pub(super) basis: usize,
    pub(super) complete: usize,
    pub(super) overlay_count: OverlayCount,
}

impl LaneTable {
    pub(super) fn from_validated_counts(
        exceptions: ExceptionCount,
        promises: PromiseCount,
        present_overlays: PresentOverlayCount,
    ) -> Self {
        let overlays = OverlayCount(u32::from(exceptions) - u32::from(promises));
        let geometry = Geometry::from_counts(exceptions, promises, present_overlays, overlays);
        Self {
            rows: native_from_proof(geometry.rows),
            promise_bits: native_from_proof(geometry.promise_bits),
            promise_ranks: native_from_proof(geometry.promise_ranks),
            providers: native_from_proof(geometry.providers),
            overlay_bits: native_from_proof(geometry.overlay_bits),
            overlay_ranks: native_from_proof(geometry.overlay_ranks),
            present: native_from_proof(geometry.present),
            basis: native_from_proof(geometry.basis),
            complete: native_from_proof(geometry.complete),
            overlay_count: overlays,
        }
    }
}

/// The sole wide-arithmetic authority for the grammar. Every u32 count times
/// its fixed record width is far below `u64::MAX`, so no lane sum can overflow.
struct Geometry {
    rows: u64,
    promise_bits: u64,
    promise_ranks: u64,
    providers: u64,
    overlay_bits: u64,
    overlay_ranks: u64,
    present: u64,
    basis: u64,
    complete: u64,
}

impl Geometry {
    #[allow(
        clippy::as_conversions,
        reason = "all widths derive from fixed Rust wire declarations; u32-count products are mathematically below u64::MAX"
    )]
    fn from_counts(
        exceptions: ExceptionCount,
        promises: PromiseCount,
        present_overlays: PresentOverlayCount,
        overlays: OverlayCount,
    ) -> Self {
        let rows = HEADER_BYTES as u64;
        let promise_bits = rows + u64::from(u32::from(exceptions)) * size_of::<u32>() as u64;
        let promise_ranks = promise_bits + u64::from(bit_bytes(exceptions));
        let providers =
            promise_ranks + u64::from(rank_prefix_count(exceptions)) * size_of::<u32>() as u64;
        let overlay_bits = providers + u64::from(u32::from(promises)) * size_of::<u64>() as u64;
        let overlay_ranks = overlay_bits + u64::from(bit_bytes(overlays));
        let present =
            overlay_ranks + u64::from(rank_prefix_count(overlays)) * size_of::<u32>() as u64;
        let basis = present
            + u64::from(u32::from(present_overlays)) * OBJECT_DESCRIPTOR_RECORD_BYTES as u64;
        let complete = basis + if u32::from(overlays) == 0 { 0 } else { 32 };
        Self {
            rows,
            promise_bits,
            promise_ranks,
            providers,
            overlay_bits,
            overlay_ranks,
            present,
            basis,
            complete,
        }
    }
}

#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "LocalityLayout accepted only a geometry whose complete u64 byte extent converted to usize; each coordinate is no larger"
)]
const fn native_from_proof(offset: u64) -> usize {
    offset as usize
}

pub(super) fn bit_bytes<Count: Into<u32>>(count: Count) -> u32 {
    count.into().div_ceil(u8::BITS)
}

pub(super) fn rank_blocks<Count: Into<u32>>(count: Count) -> u32 {
    count.into().div_ceil(RANK_BLOCK_ROWS)
}

/// Directories omit block zero's always-zero prefix.
pub(super) fn rank_prefix_count<Count: Into<u32>>(count: Count) -> u32 {
    rank_blocks(count).saturating_sub(1)
}
