//! Symbol metadata that every [`crate::entry::Entry`] carries regardless of kind.
//!
//! A [`Symbol`] is the stable, language-agnostic face of any named declaration:
//! it records the canonical name, visibility, optional documentation string,
//! source location, aliases, deprecation notice, and outbound doc-links. All
//! string data is interned through the per-arena [`crate::entry::StringInterner`]
//! so that in-memory entries are compact.
//!
//! [`SymbolBuf`] is the owned twin: a `String`-bearing struct produced by
//! producers / builders and consumed by [`crate::entry::StringInterner`] to
//! mint the [`StrId`]-based [`Symbol`].

use serde::{Deserialize, Serialize};

use nudox_change::StableRef;

use crate::index::StrId;

// ---------------------------------------------------------------------------
// Visibility
// ---------------------------------------------------------------------------

/// Visibility / access-control level.
///
/// The numeric values are part of the wire format and **must not be reused**.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[repr(u8)]
pub enum Visibility {
    /// Unrestricted public access.
    Public = 0,
    /// Accessible only within the current declaration.
    Private = 1,
    /// Accessible within the declaration and subclasses.
    Protected = 2,
    /// Assembly / module-internal (e.g. C# `internal`).
    Internal = 3,
    /// Package-level visibility (e.g. Java default).
    Package = 4,
    /// Crate-level visibility (e.g. Rust `pub(crate)`).
    Crate = 5,
}

impl Visibility {
    /// Try to parse a raw `u8` back to a visibility.
    #[inline]
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Public),
            1 => Some(Self::Private),
            2 => Some(Self::Protected),
            3 => Some(Self::Internal),
            4 => Some(Self::Package),
            5 => Some(Self::Crate),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ByteSpan
// ---------------------------------------------------------------------------

/// A half-open byte range `[start, end)` within a source file.
///
/// Both bounds are UTF-8 byte offsets (not codepoints, not lines). `start == end`
/// denotes a zero-width marker (e.g. a synthetic declaration with no source).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[repr(C)]
pub struct ByteSpan {
    /// Byte offset of the first byte of the span.
    pub start: u32,
    /// Byte offset one past the last byte of the span.
    pub end: u32,
}

impl ByteSpan {
    /// A zero-width marker span at byte offset 0 (used for synthetic entries).
    pub const ZERO: Self = Self { start: 0, end: 0 };

    /// Construct a span from start/end offsets.
    #[inline]
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Length in bytes.
    #[inline]
    pub const fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    /// True if this is a zero-width marker.
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

// ---------------------------------------------------------------------------
// Deprecation (interned)
// ---------------------------------------------------------------------------

/// An optional deprecation notice attached to a symbol.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Deprecation {
    /// Human-readable deprecation message (interned).
    pub note: Option<StrId>,
    /// Version or date since when the symbol has been deprecated (interned).
    pub since: Option<StrId>,
}

// ---------------------------------------------------------------------------
// DocLink (interned)
// ---------------------------------------------------------------------------

/// An outbound link from a doc-comment to another symbol.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct DocLink {
    /// The target symbol, cross-package-safe.
    pub target: StableRef,
    /// Optional link label as rendered in the documentation (interned).
    pub label: Option<StrId>,
}

// ---------------------------------------------------------------------------
// Symbol (interned handles)
// ---------------------------------------------------------------------------

/// The complete symbol record for one arena [`crate::entry::Entry`].
///
/// String fields are stored as [`StrId`] handles into the per-arena
/// [`crate::entry::StringInterner`]; retrieve them with
/// [`crate::entry::StringInterner::resolve`].
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Symbol {
    /// Canonical declaration name (without qualifiers).
    pub name: StrId,
    /// Access-control level.
    pub visibility: Visibility,
    /// Documentation comment text, if present.
    pub documentation: Option<StrId>,
    /// Source file path relative to the package root.
    pub source_path: StrId,
    /// Byte span of the declaration in `source_path`.
    pub span: ByteSpan,
    /// Alternative names by which the symbol is known (e.g. re-exports).
    pub aliases: Box<[StrId]>,
    /// Deprecation notice, if the symbol is deprecated.
    pub deprecation: Option<Deprecation>,
    /// Outbound links extracted from the documentation comment.
    pub doc_links: Box<[DocLink]>,
}

// ---------------------------------------------------------------------------
// DeprecationBuf / DocLinkBuf / SymbolBuf (owned twins)
// ---------------------------------------------------------------------------

/// Owned twin of [`Deprecation`] used in producer / builder APIs.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DeprecationBuf {
    pub note: Option<String>,
    pub since: Option<String>,
}

/// Owned twin of [`DocLink`] used in producer / builder APIs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DocLinkBuf {
    pub target: StableRef,
    pub label: Option<String>,
}

/// Owned twin of [`Symbol`] used by producers and the [`crate::builder::EntryBuilder`].
///
/// Pass a `SymbolBuf` to the builder's `add_*` methods; the builder will intern
/// all strings and produce the compact [`Symbol`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SymbolBuf {
    /// Canonical declaration name.
    pub name: String,
    /// Access-control level.
    pub visibility: Visibility,
    /// Optional documentation text.
    pub documentation: Option<String>,
    /// Source file path (package-relative).
    pub source_path: String,
    /// Source byte span.
    pub span: ByteSpan,
    /// Alternative names.
    pub aliases: Vec<String>,
    /// Deprecation notice, if any.
    pub deprecation: Option<DeprecationBuf>,
    /// Doc-links extracted from the documentation comment.
    pub doc_links: Vec<DocLinkBuf>,
}

impl SymbolBuf {
    /// Minimal constructor: name + visibility + source location; everything else
    /// left empty / absent.
    pub fn new(
        name: impl Into<String>,
        visibility: Visibility,
        source_path: impl Into<String>,
        span: ByteSpan,
    ) -> Self {
        Self {
            name: name.into(),
            visibility,
            documentation: None,
            source_path: source_path.into(),
            span,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_roundtrip() {
        for vis in [
            Visibility::Public,
            Visibility::Private,
            Visibility::Protected,
            Visibility::Internal,
            Visibility::Package,
            Visibility::Crate,
        ] {
            assert_eq!(Visibility::from_u8(vis as u8), Some(vis));
        }
        assert_eq!(Visibility::from_u8(255), None);
    }

    #[test]
    fn byte_span_len() {
        let s = ByteSpan::new(10, 20);
        assert_eq!(s.len(), 10);
        assert!(!s.is_empty());
        assert!(ByteSpan::ZERO.is_empty());
    }
}
