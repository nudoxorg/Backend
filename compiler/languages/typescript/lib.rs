//! Owns the in-process TypeScript and TSX syntax-and-binding authority boundary.
//! Preserves OXC's arena-borrowed program, module record, symbols, and diagnostics.
//! Deliberately stops before TypeScript checker facts, which need a separate typed adapter.
#![forbid(unsafe_code)]

mod authority;
mod coordinate;
mod error;

pub use authority::{OxcModule, analyze};
pub use coordinate::{CoordinateError, Utf8Span, Utf8ToUtf16Cursor, Utf16Span};
pub use error::AuthorityError;
