//! Defines source behavior for the direct Clang semantic frontend of `compiler-driver`.
//! Maps libclang line/column coordinates onto exact byte-unit spans of the caller's source
//! buffer and refuses unavailable or out-of-bounds coordinates with their exact operands.

use super::{
    error::ClangError,
    protocol::{ClangDiagnostic, ClangDiagnosticSeverity, ClangSourceSpan},
};

/// Maps a diagnostic's one-based line and column onto an exact source span.
pub(crate) fn diagnostic_span<'input>(
    source: &[u8],
    severity: ClangDiagnosticSeverity,
    line: u32,
    column: u32,
) -> Result<ClangDiagnostic, ClangError<'input>> {
    let span = source_point(source, line, column)?;
    Ok(ClangDiagnostic {
        severity,
        line,
        column,
        span,
    })
}

/// Maps a one-based line and column onto a zero-byte source span at that coordinate.
///
/// One past the end of the line is a valid insertion coordinate; anything beyond is typed.
pub(crate) fn source_point<'input>(
    source: &[u8],
    line: u32,
    column: u32,
) -> Result<ClangSourceSpan, ClangError<'input>> {
    if line == 0 || column == 0 {
        return Err(ClangError::SourceCoordinateUnavailable { line, column });
    }
    let Some((start, contents)) = source_line(source, line) else {
        return Err(ClangError::SourceCoordinateUnavailable { line, column });
    };
    let column_offset = usize::try_from(column - 1)
        .map_err(|_| ClangError::SourceCoordinateColumnTooLarge { line, column })?;
    let line_end =
        start
            .checked_add(contents.len())
            .ok_or(ClangError::SourceCoordinateTooLarge {
                coordinate: source.len(),
            })?;
    let offset = start
        .checked_add(column_offset)
        .ok_or(ClangError::SourceCoordinateTooLarge {
            coordinate: column_offset,
        })?;
    if offset > line_end {
        return Err(ClangError::SourceCoordinateOutOfBounds {
            line,
            column,
            source_bytes: source.len(),
        });
    }
    let offset = u32::try_from(offset)
        .map_err(|_| ClangError::SourceCoordinateTooLarge { coordinate: offset })?;
    Ok(ClangSourceSpan {
        start: offset,
        end: offset,
    })
}

/// Returns the byte offset and contents of the one-based source line.
fn source_line(source: &[u8], wanted: u32) -> Option<(usize, &[u8])> {
    if wanted == 0 {
        return None;
    }
    let mut line = 1_u32;
    let mut start = 0_usize;
    for (index, byte) in source.iter().enumerate() {
        if *byte == b'\n' {
            if line == wanted {
                return Some((start, &source[start..index]));
            }
            line = line.checked_add(1)?;
            start = index.checked_add(1)?;
        }
    }
    (line == wanted).then_some((start, &source[start..]))
}

#[cfg(test)]
mod tests {
    use super::{ClangError, ClangSourceSpan, source_point};

    #[test]
    fn source_point_rejects_missing_or_out_of_bounds_coordinates() {
        assert!(matches!(
            source_point(b"abc", 0, 1),
            Err(ClangError::SourceCoordinateUnavailable { line: 0, column: 1 })
        ));
        assert!(matches!(
            source_point(b"abc", 2, 1),
            Err(ClangError::SourceCoordinateUnavailable { line: 2, column: 1 })
        ));
        assert!(matches!(
            source_point(b"abc", 1, 5),
            Err(ClangError::SourceCoordinateOutOfBounds {
                line: 1,
                column: 5,
                source_bytes: 3
            })
        ));
        assert_eq!(
            source_point(b"abc", 1, 4).expect("one-past-end is a valid insertion point"),
            ClangSourceSpan { start: 3, end: 3 }
        );
    }
}
