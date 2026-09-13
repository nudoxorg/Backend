//! Defines limits behavior for `backend-version::schema`, whose purpose is to define shared binary schema limits, identifiers, and vocabulary.
//! This module owns the limits invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::{borrow::Borrow, fmt, ops::Deref};

/// Hard maximum encoded frame bytes, including header and padding.
pub const MAX_FRAME_BYTES: u32 = 1_048_576;
/// Hard maximum number of descriptors in one frame.
pub const MAX_SECTIONS: u16 = 64;
/// Hard maximum row count named by one descriptor.
pub const MAX_ROWS: u32 = 1_000_000;
/// Hard maximum sum of section body lengths in one frame.
pub const MAX_ARENA_BYTES: u32 = 1_000_000;

/// Protocol row count bounded by [`MAX_ROWS`].
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RowCount(u32);

impl TryFrom<u32> for RowCount {
    type Error = LimitError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        if value > MAX_ROWS {
            return Err(LimitError::new(LimitKind::Rows, value, MAX_ROWS));
        }
        Ok(Self(value))
    }
}

impl Deref for RowCount {
    type Target = u32;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<u32> for RowCount {
    fn as_ref(&self) -> &u32 {
        self
    }
}

impl Borrow<u32> for RowCount {
    fn borrow(&self) -> &u32 {
        self
    }
}

/// Maximum complete frame length accepted by one decoder policy.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct FrameBytesLimit(u32);

/// Maximum descriptor count accepted by one decoder policy.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct SectionCountLimit(u16);

/// Maximum rows accepted in one descriptor by one decoder policy.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct RowCountLimit(u32);

/// Maximum sum of body bytes accepted by one decoder policy.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct ArenaBytesLimit(u32);

/// Named fixed wire-resource limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitKind {
    /// Complete encoded frame length.
    FrameBytes,
    /// Directory descriptor count.
    Sections,
    /// Rows in a single descriptor.
    Rows,
    /// Sum of section body lengths.
    ArenaBytes,
}

/// One wire-resource quantity carried beside an explicit [`LimitKind`].
///
/// The discriminator supplies the unit (frame bytes, descriptors, rows, or
/// arena bytes); this transparent value prevents the two comparison operands
/// in a public limit rejection from being mistaken for unrelated `u32`s.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LimitAmount(u32);

impl From<u32> for LimitAmount {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<u16> for LimitAmount {
    fn from(value: u16) -> Self {
        Self(u32::from(value))
    }
}

impl From<LimitAmount> for u32 {
    fn from(value: LimitAmount) -> Self {
        value.0
    }
}

impl Deref for LimitAmount {
    type Target = u32;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<u32> for LimitAmount {
    fn as_ref(&self) -> &u32 {
        self
    }
}

impl Borrow<u32> for LimitAmount {
    fn borrow(&self) -> &u32 {
        self
    }
}

impl fmt::Display for LimitAmount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// An attempted runtime policy or observed wire value exceeded a maximum.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{kind:?} limit {requested} exceeds {maximum}")]
pub struct LimitError {
    /// Constrained resource.
    pub kind: LimitKind,
    /// Requested or observed amount.
    pub requested: LimitAmount,
    /// Applicable maximum.
    pub maximum: LimitAmount,
}

impl LimitError {
    const fn new(kind: LimitKind, requested: u32, maximum: u32) -> Self {
        Self {
            kind,
            requested: LimitAmount(requested),
            maximum: LimitAmount(maximum),
        }
    }
}

macro_rules! checked_limit {
    ($name:ident, $raw:ty, $maximum:expr, $kind:expr) => {
        impl TryFrom<$raw> for $name {
            type Error = LimitError;

            fn try_from(value: $raw) -> Result<Self, Self::Error> {
                if value > $maximum {
                    return Err(LimitError::new(
                        $kind,
                        u32::from(value),
                        u32::from($maximum),
                    ));
                }
                Ok(Self(value))
            }
        }

        impl From<$name> for $raw {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl Deref for $name {
            type Target = $raw;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl AsRef<$raw> for $name {
            fn as_ref(&self) -> &$raw {
                self
            }
        }

        impl Borrow<$raw> for $name {
            fn borrow(&self) -> &$raw {
                self
            }
        }
    };
}

checked_limit!(FrameBytesLimit, u32, MAX_FRAME_BYTES, LimitKind::FrameBytes);
checked_limit!(SectionCountLimit, u16, MAX_SECTIONS, LimitKind::Sections);
checked_limit!(RowCountLimit, u32, MAX_ROWS, LimitKind::Rows);
checked_limit!(ArenaBytesLimit, u32, MAX_ARENA_BYTES, LimitKind::ArenaBytes);

/// Per-decoder policy whose fields are individually incapable of exceeding wire maxima.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodeLimits {
    /// Maximum accepted complete frame length.
    pub frame_bytes: FrameBytesLimit,
    /// Maximum accepted descriptor count.
    pub sections: SectionCountLimit,
    /// Maximum accepted rows in a descriptor.
    pub rows: RowCountLimit,
    /// Maximum accepted sum of body lengths.
    pub arena_bytes: ArenaBytesLimit,
}

impl DecodeLimits {
    /// The exact fixed wire maxima, expressed as individually validated policy fields.
    pub const MAXIMUM: Self = Self {
        frame_bytes: FrameBytesLimit(MAX_FRAME_BYTES),
        sections: SectionCountLimit(MAX_SECTIONS),
        rows: RowCountLimit(MAX_ROWS),
        arena_bytes: ArenaBytesLimit(MAX_ARENA_BYTES),
    };
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self::MAXIMUM
    }
}

#[cfg(test)]
mod tests {
    use core::mem::size_of;

    use super::{
        DecodeLimits, FrameBytesLimit, LimitAmount, LimitError, LimitKind, MAX_FRAME_BYTES,
        MAX_ROWS, RowCount, RowCountLimit, SectionCountLimit,
    };

    #[test]
    fn each_policy_field_is_individually_bounded_at_its_raw_boundary() {
        assert_eq!(
            FrameBytesLimit::try_from(MAX_FRAME_BYTES + 1),
            Err(LimitError::new(
                LimitKind::FrameBytes,
                MAX_FRAME_BYTES + 1,
                MAX_FRAME_BYTES
            ))
        );
        assert_eq!(SectionCountLimit::try_from(1), Ok(SectionCountLimit(1)));
        assert_eq!(RowCountLimit::try_from(2), Ok(RowCountLimit(2)));
        assert_eq!(
            u32::from(DecodeLimits::MAXIMUM.frame_bytes),
            MAX_FRAME_BYTES
        );
        assert_eq!(RowCount::try_from(1).map(|rows| *rows), Ok(1));
        assert_eq!(
            RowCount::try_from(MAX_ROWS + 1),
            Err(LimitError::new(LimitKind::Rows, MAX_ROWS + 1, MAX_ROWS))
        );
    }

    #[test]
    fn policy_struct_literals_cannot_smuggle_unvalidated_raw_values() -> Result<(), LimitError> {
        let limits = DecodeLimits {
            frame_bytes: FrameBytesLimit::try_from(64)?,
            sections: SectionCountLimit::try_from(1)?,
            rows: RowCountLimit::try_from(2)?,
            arena_bytes: DecodeLimits::MAXIMUM.arena_bytes,
        };
        assert_eq!(*limits.frame_bytes, 64);
        assert_eq!(size_of::<FrameBytesLimit>(), size_of::<u32>());
        assert_eq!(size_of::<LimitAmount>(), size_of::<u32>());
        assert_eq!(u32::from(LimitAmount::from(17_u16)), 17);
        Ok(())
    }
}
