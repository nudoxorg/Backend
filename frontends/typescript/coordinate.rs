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
    /// Inclusive first UTF-8 byte.
    pub start: u32,
    /// Exclusive UTF-8 byte after the span.
    pub end: u32,
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
    /// A cursor was asked to revisit source text it had already passed.
    #[error("monotonic coordinate cursor cannot move backward")]
    NonMonotonic,
}

/// A no-allocation forward converter for source positions already in byte order.
///
/// Each byte between the initial cursor position and the furthest admitted
/// endpoint is decoded at most once. It is the lowering-path converter for
/// source-order spans; [`Utf8Span::to_utf16`] remains the simpler cold lookup.
pub struct Utf8ToUtf16Cursor<'source> {
    source: &'source str,
    byte: usize,
    code_unit: u32,
    #[cfg(test)]
    bytes_walked: usize,
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

impl<'source> Utf8ToUtf16Cursor<'source> {
    /// Starts a forward conversion at the beginning of `source`.
    #[must_use]
    pub const fn new(source: &'source str) -> Self {
        Self {
            source,
            byte: 0,
            code_unit: 0,
            #[cfg(test)]
            bytes_walked: 0,
        }
    }

    /// Converts one non-overlapping, source-ordered byte span without rescanning a prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if `span` precedes the cursor, lies outside `source`,
    /// or splits a UTF-8 scalar encoding.
    pub fn to_utf16(&mut self, span: Utf8Span) -> Result<Utf16Span, CoordinateError> {
        let start = self.advance_to(span.start)?;
        let end = self.advance_to(span.end)?;
        Utf16Span::try_from(start..end)
    }

    fn advance_to(&mut self, byte: u32) -> Result<u32, CoordinateError> {
        let byte = source_byte(byte, self.source)?;
        if byte < self.byte {
            return Err(CoordinateError::NonMonotonic);
        }

        let segment = &self.source[self.byte..byte];
        let width = segment.chars().try_fold(0_u32, |units, scalar| {
            let width =
                u32::try_from(scalar.len_utf16()).map_err(|_| CoordinateError::OutOfBounds)?;
            units.checked_add(width).ok_or(CoordinateError::OutOfBounds)
        })?;
        self.code_unit = self
            .code_unit
            .checked_add(width)
            .ok_or(CoordinateError::OutOfBounds)?;
        #[cfg(test)]
        {
            self.bytes_walked = self
                .bytes_walked
                .checked_add(segment.len())
                .ok_or(CoordinateError::OutOfBounds)?;
        }
        self.byte = byte;
        Ok(self.code_unit)
    }
}

impl Utf8Span {
    /// Converts this exact byte range into the equivalent UTF-16 range for `source`.
    ///
    /// # Errors
    ///
    /// Returns an error when either byte endpoint lies outside `source` or
    /// splits a UTF-8 scalar encoding. For source-order bulk conversion, use
    /// [`Utf8ToUtf16Cursor`] to avoid repeatedly scanning the same prefix.
    pub fn to_utf16(self, source: &str) -> Result<Utf16Span, CoordinateError> {
        Utf8ToUtf16Cursor::new(source).to_utf16(self)
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

#[cfg(test)]
mod tests {
    use super::{CoordinateError, Utf8Span, Utf8ToUtf16Cursor};

    #[derive(Debug, thiserror::Error)]
    enum CursorTestError {
        #[error(transparent)]
        Coordinate(#[from] CoordinateError),
        #[error("cursor walked {actual} bytes, expected {expected}")]
        WorkBound { actual: usize, expected: usize },
    }

    #[test]
    fn source_order_cursor_walks_each_byte_once_across_many_spans() -> Result<(), CursorTestError> {
        const REPEATS: usize = 1_024;
        const CHUNK_BYTES: usize = 5;
        let source = "x🚀".repeat(REPEATS);
        let mut cursor = Utf8ToUtf16Cursor::new(&source);

        for chunk in 0..REPEATS {
            let start = chunk
                .checked_mul(CHUNK_BYTES)
                .and_then(|offset| offset.checked_add(1))
                .ok_or(CoordinateError::OutOfBounds)?;
            let end = start.checked_add(4).ok_or(CoordinateError::OutOfBounds)?;
            cursor.to_utf16(Utf8Span::try_from(
                u32::try_from(start).map_err(|_| CoordinateError::OutOfBounds)?
                    ..u32::try_from(end).map_err(|_| CoordinateError::OutOfBounds)?,
            )?)?;
        }

        (cursor.bytes_walked == source.len())
            .then_some(())
            .ok_or(CursorTestError::WorkBound {
                actual: cursor.bytes_walked,
                expected: source.len(),
            })
    }
}
