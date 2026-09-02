//! Exercises the `compiler-driver` TypeScript source-coordinate boundary through its public surface.
//! The cases prove exact UTF-8 slicing, exact rejection operands, and exact source retention.
//! Cross-unit substitution is rejected by rustc itself in `compile_fail.rs`.

use compiler_driver::{
    AuthorityOrigin, SourceSpan, SourceSpanError, Utf8Span, Utf16Offset, utf16_span_to_utf8,
};
use thiserror::Error;

#[derive(Debug, Error)]
enum CoordinateContract {
    #[error("the exact UTF-16 span was unexpectedly accepted as {0:?}")]
    Accepted(Utf8Span),
    #[error("the rejected coordinate was {0:?}, not an invalid UTF-16 boundary")]
    RejectedOtherwise(SourceSpanError),
    #[error("the rejected coordinate was {0:?}, not a decreasing range")]
    Unrejected(SourceSpanError),
}

#[test]
fn tsz_utf16_coordinates_select_the_exact_utf8_non_ascii_slice() -> Result<(), SourceSpanError> {
    let source = "const café = 😀;";
    let span = SourceSpan {
        origin: AuthorityOrigin::TszSemantic,
        start: Utf16Offset { units: 6 },
        end: Utf16Offset { units: 10 },
    };

    let actual = utf16_span_to_utf8(source.as_bytes(), span)?;

    assert_eq!(actual, Utf8Span { start: 6, end: 11 });
    assert_eq!(
        &source.as_bytes()[actual.start..actual.end],
        "café".as_bytes()
    );
    Ok(())
}

#[test]
fn a_utf16_offset_inside_a_surrogate_pair_is_rejected_with_its_source_span()
-> Result<(), CoordinateContract> {
    let span = SourceSpan {
        origin: AuthorityOrigin::OxcSyntax,
        start: Utf16Offset { units: 11 },
        end: Utf16Offset { units: 12 },
    };

    match utf16_span_to_utf8("const x = 😀;".as_bytes(), span) {
        Err(SourceSpanError::InvalidUtf16Boundary {
            span: retained,
            offset: Utf16Offset { units: 11 },
        }) => {
            assert_eq!(retained, span);
            Ok(())
        }
        Err(otherwise) => Err(CoordinateContract::RejectedOtherwise(otherwise)),
        Ok(accepted) => Err(CoordinateContract::Accepted(accepted)),
    }
}

#[test]
fn a_decreasing_authority_span_is_rejected_retaining_the_exact_range()
-> Result<(), CoordinateContract> {
    let span = SourceSpan {
        origin: AuthorityOrigin::TszSemantic,
        start: Utf16Offset { units: 7 },
        end: Utf16Offset { units: 3 },
    };

    match utf16_span_to_utf8(b"const x = 1;".as_slice(), span) {
        Err(SourceSpanError::Reversed { span: retained }) => {
            assert_eq!(retained, span);
            Ok(())
        }
        Err(otherwise) => Err(CoordinateContract::Unrejected(otherwise)),
        Ok(accepted) => Err(CoordinateContract::Accepted(accepted)),
    }
}

#[test]
fn a_non_utf8_source_has_no_typescript_text_coordinates() -> Result<(), CoordinateContract> {
    let span = SourceSpan {
        origin: AuthorityOrigin::OxcSyntax,
        start: Utf16Offset { units: 0 },
        end: Utf16Offset { units: 1 },
    };

    match utf16_span_to_utf8(&[0xff, 0xfe], span) {
        Err(SourceSpanError::InvalidUtf8 { source }) => {
            assert_eq!(source.valid_up_to(), 0);
            Ok(())
        }
        Err(otherwise) => Err(CoordinateContract::Unrejected(otherwise)),
        Ok(accepted) => Err(CoordinateContract::Accepted(accepted)),
    }
}

#[test]
fn an_empty_source_selects_an_empty_byte_span() -> Result<(), SourceSpanError> {
    let span = SourceSpan {
        origin: AuthorityOrigin::TszSemantic,
        start: Utf16Offset { units: 0 },
        end: Utf16Offset { units: 0 },
    };

    let actual = utf16_span_to_utf8(b"".as_slice(), span)?;

    assert_eq!(actual, Utf8Span { start: 0, end: 0 });
    Ok(())
}

#[test]
fn an_end_offset_at_the_exact_source_limit_selects_the_whole_byte_tail()
-> Result<(), SourceSpanError> {
    let source = "const x = 😀;";
    let span = SourceSpan {
        origin: AuthorityOrigin::TszSemantic,
        start: Utf16Offset { units: 10 },
        end: Utf16Offset { units: 13 },
    };

    let actual = utf16_span_to_utf8(source.as_bytes(), span)?;

    assert_eq!(actual, Utf8Span { start: 10, end: 15 });
    assert_eq!(
        &source.as_bytes()[actual.start..actual.end],
        "😀;".as_bytes()
    );
    Ok(())
}

#[test]
fn an_offset_beyond_the_source_is_rejected_with_the_exact_boundary()
-> Result<(), CoordinateContract> {
    let span = SourceSpan {
        origin: AuthorityOrigin::OxcSyntax,
        start: Utf16Offset { units: 13 },
        end: Utf16Offset { units: 14 },
    };

    match utf16_span_to_utf8("const x = 😀;".as_bytes(), span) {
        Err(SourceSpanError::InvalidUtf16Boundary {
            span: retained,
            offset: Utf16Offset { units: 14 },
        }) => {
            assert_eq!(retained, span);
            Ok(())
        }
        Err(otherwise) => Err(CoordinateContract::RejectedOtherwise(otherwise)),
        Ok(accepted) => Err(CoordinateContract::Accepted(accepted)),
    }
}
