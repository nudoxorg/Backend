//! Negative type proof for the TypeScript source-coordinate boundary.
//! A validated UTF-8 byte span must never substitute for a UTF-16 authority span.
//! Successful compilation would let byte offsets select UTF-16 code units.
use compiler_driver::{Utf8Span, utf16_span_to_utf8};

/// Attempts the forbidden cross-unit conversion that the type checker must reject.
fn cross_unit(source: &[u8], bytes: Utf8Span) {
    let _ = utf16_span_to_utf8(source, bytes);
}

/// Keeps the compile-fail fixture executable without invoking the impossible helper.
fn main() {}
