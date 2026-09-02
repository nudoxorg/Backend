//! Exact conversion between TypeScript's UTF-16 coordinates and UTF-8 source bytes.
//! OXC byte spans and TSZ semantic UTF-16 coordinates are different types that never
//! substitute for each other; one validated conversion against the exact source bridges them.
use thiserror::Error;

/// The native authority that supplied a source-coordinate fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorityOrigin {
    /// OXC parsed the syntax and reported a byte span.
    OxcSyntax,
    /// TSZ binder/checker/solver reported a semantic coordinate.
    TszSemantic,
}

/// A UTF-16 code-unit position reported by a TypeScript semantic authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Utf16Offset {
    /// Number of UTF-16 code units from the start of the source text.
    pub units: u32,
}

/// One half-open UTF-16 source range and its reporting authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceSpan {
    /// Originating parser or semantic authority.
    pub origin: AuthorityOrigin,
    /// First code unit in the half-open range.
    pub start: Utf16Offset,
    /// First code unit beyond the half-open range.
    pub end: Utf16Offset,
}

/// A checked half-open range into the caller's UTF-8 source bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Utf8Span {
    /// First byte in the range.
    pub start: usize,
    /// First byte beyond the range.
    pub end: usize,
}

/// A source coordinate that cannot identify a UTF-8 boundary.
#[derive(Debug, Error)]
pub enum SourceSpanError {
    /// The caller source is not valid UTF-8 and therefore has no TypeScript text coordinates.
    #[error("source is not UTF-8: {source}")]
    InvalidUtf8 {
        /// The exact UTF-8 validation failure retained from the caller source.
        #[source]
        source: std::str::Utf8Error,
    },
    /// The authority reported a decreasing range.
    #[error("{:?} span starts at {} after {}", .span.origin, .span.start.units, .span.end.units)]
    Reversed {
        /// The exact rejected authority span.
        span: SourceSpan,
    },
    /// A UTF-16 offset ends in a surrogate pair or lies beyond the source.
    #[error("{:?} span has no UTF-8 boundary at UTF-16 offset {}", .span.origin, .offset.units)]
    InvalidUtf16Boundary {
        /// The exact rejected authority span.
        span: SourceSpan,
        /// The code-unit offset that selected no UTF-8 boundary.
        offset: Utf16Offset,
    },
    /// Adding UTF-16 width overflowed the authority coordinate domain.
    #[error("{:?} span UTF-16 coordinate overflow", .span.origin)]
    CoordinateOverflow {
        /// The exact rejected authority span.
        span: SourceSpan,
    },
}

/// Convert one authority-provided UTF-16 span without allocating or copying source bytes.
pub fn utf16_span_to_utf8(source: &[u8], span: SourceSpan) -> Result<Utf8Span, SourceSpanError> {
    if span.start.units > span.end.units {
        return Err(SourceSpanError::Reversed { span });
    }
    let source =
        std::str::from_utf8(source).map_err(|source| SourceSpanError::InvalidUtf8 { source })?;
    let start = utf16_offset_to_utf8(source, span, span.start)?;
    let end = utf16_offset_to_utf8(source, span, span.end)?;
    Ok(Utf8Span { start, end })
}

fn utf16_offset_to_utf8(
    source: &str,
    span: SourceSpan,
    target: Utf16Offset,
) -> Result<usize, SourceSpanError> {
    let mut units = 0_u32;
    for (byte, character) in source.char_indices() {
        if units == target.units {
            return Ok(byte);
        }
        let width = u32::try_from(character.len_utf16())
            .map_err(|_| SourceSpanError::CoordinateOverflow { span })?;
        units = units
            .checked_add(width)
            .ok_or(SourceSpanError::CoordinateOverflow { span })?;
        if units > target.units {
            return Err(SourceSpanError::InvalidUtf16Boundary {
                span,
                offset: target,
            });
        }
    }
    if units == target.units {
        Ok(source.len())
    } else {
        Err(SourceSpanError::InvalidUtf16Boundary {
            span,
            offset: target,
        })
    }
}
