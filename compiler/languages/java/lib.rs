//! Java doclet boundary with explicit UTF-16-to-UTF-8 coordinate conversion.
//! It preserves overload and throws facts from the old helper.
//! Recorded fixtures provide a runtime-independent proof.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod oracle;

pub use oracle::{
    Constant, DecodeError, JavaOutput, JavaType, ToolingUnavailable, TypeMirror, Utf16Span, decode,
    probe_javadoc, probe_javadoc_path, utf16_span_to_utf8,
};
