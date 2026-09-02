//! Distinguishes OXC UTF-8 byte ranges from TypeScript checker UTF-16 ranges.
//! Converts only exact scalar boundaries and rejects positions inside surrogate pairs.
//! Uses compact two-word value types with one shared ordering invariant per unit.

use core::ops::Range;

use oxc_span::Span;
use thiserror::Error;

/// A valid half-open range measured in UTF-8 source bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Utf8Span {
    start: u32,
    end: u32,
}

/// A valid half-open range measured in UTF-16 code units.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Utf16Span {
    start: u32,
    end: u32,
}

/// Source-coordinate conversion could not preserve exact text boundaries.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CoordinateError {
    /// A source range ends before it begins.
    #[error("source range ends before it begins")]
    Reversed,
    /// A coordinate falls outside the caller-provided text.
    #[error("source coordinate lies outside the supplied text")]
    OutOfBounds,
    /// A byte coordinate splits one UTF-8 scalar encoding.
    #[error("UTF-8 coordinate splits a scalar encoding")]
    Utf8Boundary,
    /// A UTF-16 coordinate splits the two units of one non-BMP scalar.
    #[error("UTF-16 coordinate splits a surrogate pair")]
    SurrogateBoundary,
}

impl TryFrom<Range<u32>> for Utf8Span {
    type Error = CoordinateError;

    /// Admits an ordered byte range before it is checked against concrete source text.
    fn try_from(range: Range<u32>) -> Result<Self, Self::Error> {
        ordered(range.start, range.end).map(|(start, end)| Self { start, end })
    }
}

impl TryFrom<Span> for Utf8Span {
    type Error = CoordinateError;

    /// Admits an OXC byte span without erasing the source-coordinate unit.
    fn try_from(span: Span) -> Result<Self, Self::Error> {
        (span.start..span.end).try_into()
    }
}

impl TryFrom<Range<u32>> for Utf16Span {
    type Error = CoordinateError;

    /// Admits an ordered code-unit range before it is checked against concrete source text.
    fn try_from(range: Range<u32>) -> Result<Self, Self::Error> {
        ordered(range.start, range.end).map(|(start, end)| Self { start, end })
    }
}

impl From<Utf8Span> for Range<u32> {
    /// Erases only the UTF-8 unit proof for an API that explicitly requires raw byte offsets.
    fn from(span: Utf8Span) -> Self {
        span.start..span.end
    }
}

impl From<Utf16Span> for Range<u32> {
    /// Erases only the UTF-16 unit proof for an API that explicitly requires raw code-unit offsets.
    fn from(span: Utf16Span) -> Self {
        span.start..span.end
    }
}

impl Utf8Span {
    /// Converts this exact byte range into the equivalent UTF-16 range for `source`.
    ///
    /// # Errors
    ///
    /// Returns an error when either byte endpoint lies outside `source` or
    /// splits a UTF-8 scalar encoding.
    pub fn to_utf16(self, source: &str) -> Result<Utf16Span, CoordinateError> {
        let start = source_byte(self.start, source)?;
        let end = source_byte(self.end, source)?;
        let start = source[..start].encode_utf16().count();
        let end = source[..end].encode_utf16().count();
        Utf16Span::try_from(
            u32::try_from(start).map_err(|_| CoordinateError::OutOfBounds)?
                ..u32::try_from(end).map_err(|_| CoordinateError::OutOfBounds)?,
        )
    }
}

impl Utf16Span {
    /// Converts this exact UTF-16 range into the equivalent UTF-8 byte range for `source`.
    ///
    /// # Errors
    ///
    /// Returns an error when either code-unit endpoint lies outside `source`
    /// or falls between the two units of a non-BMP scalar.
    pub fn to_utf8(self, source: &str) -> Result<Utf8Span, CoordinateError> {
        let start = usize::try_from(self.start).map_err(|_| CoordinateError::OutOfBounds)?;
        let end = usize::try_from(self.end).map_err(|_| CoordinateError::OutOfBounds)?;
        let mut units = 0_usize;
        let mut start_byte = None;
        let mut end_byte = None;

        for (byte, scalar) in source.char_indices() {
            if units == start {
                start_byte = Some(byte);
            }
            if units == end {
                end_byte = Some(byte);
            }
            let width = scalar.len_utf16();
            let next = units
                .checked_add(width)
                .ok_or(CoordinateError::OutOfBounds)?;
            if width == 2 && (start == units + 1 || end == units + 1) {
                return Err(CoordinateError::SurrogateBoundary);
            }
            units = next;
        }

        if units == start {
            start_byte = Some(source.len());
        }
        if units == end {
            end_byte = Some(source.len());
        }

        Utf8Span::try_from(
            u32::try_from(start_byte.ok_or(CoordinateError::OutOfBounds)?)
                .map_err(|_| CoordinateError::OutOfBounds)?
                ..u32::try_from(end_byte.ok_or(CoordinateError::OutOfBounds)?)
                    .map_err(|_| CoordinateError::OutOfBounds)?,
        )
    }
}

fn ordered(start: u32, end: u32) -> Result<(u32, u32), CoordinateError> {
    (start <= end)
        .then_some((start, end))
        .ok_or(CoordinateError::Reversed)
}

fn source_byte(offset: u32, source: &str) -> Result<usize, CoordinateError> {
    let offset = usize::try_from(offset).map_err(|_| CoordinateError::OutOfBounds)?;
    (offset <= source.len())
        .then_some(offset)
        .ok_or(CoordinateError::OutOfBounds)
        .and_then(|offset| {
            source
                .is_char_boundary(offset)
                .then_some(offset)
                .ok_or(CoordinateError::Utf8Boundary)
        })
}
