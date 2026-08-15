//! Where a declaration was written — the typed answer to "jump to source".
//!
//! # Why this type exists, and why it has this shape
//!
//! Until this module, an entry's origin was two independent fields on
//! [`Symbol`](super::Symbol): `source: PathBuf` and `span: Range<usize>`. That
//! pair cannot express the question a reader actually asks. Three separate
//! defects followed from it, all recorded in `docs/LIMITATIONS.md` L31/L42:
//!
//! 1. **`0..0` type-checks as "present at offset zero".** It is emitted at 50
//!    sites across six producers to mean *absent*, and nothing distinguishes it
//!    from a declaration that genuinely starts at byte 0. Absence was
//!    representable only as a value that lies about being present.
//! 2. **The empty `PathBuf` means the same thing again**, in a second field, so
//!    the two can disagree — and consumers each invented their own decoding of
//!    the pair (`nudox-engine`'s head chunker tested `is_empty() || == "."`;
//!    the GUI tested something else).
//! 3. **Byte offsets are not somewhere a person can go.** docs.rs links
//!    `src/memchr/memchr.rs.html#288-291`. A byte range can only be turned into
//!    a line by reading the file, and the engine deliberately does not read
//!    files on the documentation path — so the byte offset was rendered to the
//!    user as the literal text `bytes 8880–9435`, which is not a location any
//!    editor, browser, or human can act on.
//!
//! ## The shape chosen
//!
//! [`SourceLocation`] is a **closed enum, not an `Option` and not a struct of
//! optional parts**. Three states, each of which a consumer must handle
//! differently:
//!
//! - [`SourceLocation::Declared`] — file, byte range, *and* a 1-based
//!   line/column range. This is the only variant a "jump to source"
//!   affordance may be built on, and the only one that can be rendered as a
//!   `path:line:col`.
//! - [`SourceLocation::BytesOnly`] — file and byte range, no lines. This is
//!   the honest description of a producer that records where an item is but
//!   has never computed a line index. It is *not* a jump target.
//! - [`SourceLocation::Unlocated`] — no location, carrying [`Unlocated`], the
//!   reason. A synthesized root module and a producer that simply does not
//!   emit locations are different facts, and collapsing them is how L31 stayed
//!   mis-diagnosed for two revisions ("it may work and merely be unreachable
//!   from the keyboard").
//!
//! ## Alternatives rejected
//!
//! - **`Option<SourceLocation>`.** Rejected under doctrine §2: every consumer
//!   would unwrap it, and the `None` would carry no reason — which is the
//!   information that turns "no source link here" into a filed bug against a
//!   named producer. The reason lives *inside* the type instead.
//! - **Keep `Range<usize>` and resolve lines in the consumer.** Rejected
//!   because it pushes an encoding-sensitive computation (and a file read) into
//!   every consumer, including ones that have no filesystem — and the producer
//!   already holds the file text and, for rust-analyzer, a line index. Compute
//!   it once, where the bytes are.
//! - **A line number alone, no columns, no byte range.** Rejected in both
//!   directions: the byte range is load-bearing today (`RelSpan` occurrences
//!   are stored *relative to* the owner's span start), and a bare line cannot
//!   express a range for highlighting. `Declared` carries both, and the two are
//!   two views of the same fact rather than independent fields.
//! - **An absolute path.** Rejected because it is machine-specific: it would
//!   make an entry's content hash depend on which build machine produced it,
//!   which destroys IR-VCS lineage. [`SourceFile`] is therefore relative to the
//!   documented package's root, with `/` separators, so it is stable across
//!   machines and platforms and can be joined against a root the consumer
//!   already knows.

use std::{num::NonZeroU32, ops::Range, path::PathBuf};

// ---------------------------------------------------------------------------
// SourceFile
// ---------------------------------------------------------------------------

/// A path to a file, **relative to the documented package's root**, with `/`
/// separators on every platform.
///
/// A newtype rather than a `PathBuf` because the invariant that matters here is
/// not "is a path" but "is a path a consumer can resolve and a hash can be
/// taken over": non-empty, relative, and platform-independent. A `PathBuf`
/// admits the empty path, which is exactly the sentinel this module exists to
/// delete.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct SourceFile(Box<str>);

impl SourceFile {
    /// Build a `SourceFile` from a package-relative path, or `None` when the
    /// input cannot name a file.
    ///
    /// The `Option` is the point: a producer that cannot relativise a path is
    /// forced to choose an [`Unlocated`] reason rather than emit `""` and let
    /// every downstream consumer re-derive what that meant.
    ///
    /// Rejects the empty string and `"."` — the two values that
    /// `Symbol::source` has historically carried to mean "not recorded".
    /// Backslashes are normalised to `/` so a Windows-produced IR hashes
    /// identically to a Unix-produced one.
    pub fn new(relative: &str) -> Option<Self> {
        let normalised = relative.replace('\\', "/");
        let trimmed = normalised.trim_matches('/');
        if trimmed.is_empty() || trimmed == "." {
            return None;
        }
        Some(SourceFile(trimmed.into()))
    }

    /// The path as written, for joining against a package root or rendering.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// LineCol
// ---------------------------------------------------------------------------

/// A 1-based line and column.
///
/// `NonZeroU32` on both axes is the mechanical guarantee that replaces the
/// `0..0` sentinel: "line 0" is not a value this type can hold, so a caller
/// cannot construct a plausible-looking location out of zeroes. Editors,
/// `rustc` diagnostics and docs.rs anchors are all 1-based, so this is also the
/// representation a consumer can print without adjusting.
///
/// The column counts **UTF-8 bytes** from the start of the line, matching
/// `rustc`'s convention for ASCII source and the byte offsets in
/// [`SourceLocation::Declared::bytes`]. Callers converting from a 0-based
/// tool (rust-analyzer, LSP) should use [`LineCol::from_zero_based`] rather
/// than adding one by hand at each site.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct LineCol {
    /// 1-based line number.
    pub line: NonZeroU32,
    /// 1-based column, in UTF-8 bytes from the start of the line.
    pub column: NonZeroU32,
}

impl LineCol {
    /// Convert from a 0-based `(line, column)` pair as produced by
    /// rust-analyzer's `line-index`, LSP, and tree-sitter.
    ///
    /// Saturating rather than wrapping on `u32::MAX` so that a pathological
    /// input cannot silently become line 0 — the one value this type exists to
    /// exclude.
    pub fn from_zero_based(line: u32, column: u32) -> Self {
        LineCol {
            line: NonZeroU32::new(line.saturating_add(1)).unwrap_or(NonZeroU32::MAX),
            column: NonZeroU32::new(column.saturating_add(1)).unwrap_or(NonZeroU32::MAX),
        }
    }

    /// The line as a plain integer, for formatting.
    pub fn line(self) -> u32 {
        self.line.get()
    }

    /// The column as a plain integer, for formatting.
    pub fn column(self) -> u32 {
        self.column.get()
    }
}

// ---------------------------------------------------------------------------
// ByteSpan
// ---------------------------------------------------------------------------

/// A half-open byte range within a [`SourceFile`].
///
/// `u32` rather than `usize` because this value is hashed and serialised: a
/// `usize` would make an IR produced on a 32-bit host encode differently from
/// the same IR on a 64-bit one. No source file this system can lower approaches
/// 4 GiB.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct ByteSpan {
    /// Inclusive start offset.
    pub start: u32,
    /// Exclusive end offset.
    pub end: u32,
}

impl ByteSpan {
    /// Build a span, or `None` when it is empty or inverted.
    ///
    /// An empty span is rejected because `0..0` — and `N..N` generally — is
    /// precisely the "absent, but shaped like present" value this module
    /// exists to make unrepresentable. A caller holding an empty range has no
    /// location and must say so with an [`Unlocated`] reason.
    pub fn new(start: u32, end: u32) -> Option<Self> {
        (end > start).then_some(ByteSpan { start, end })
    }

    /// The equivalent `Range<usize>`, for the legacy `Symbol::span` field and
    /// for slicing file text.
    pub fn as_range(self) -> Range<usize> {
        self.start as usize..self.end as usize
    }
}

// ---------------------------------------------------------------------------
// Unlocated
// ---------------------------------------------------------------------------

/// Why an entry has no source location.
///
/// **Deliberately exhaustive** (no `#[non_exhaustive]`), for the same reason
/// `ProducerError` is — see doctrine §3. This enum is internal to the
/// workspace, and its whole value is that adding a reason breaks every match
/// and forces each consumer to decide what the new kind of absence means. A
/// wildcard arm here would recreate exactly the collapse this type was written
/// to undo.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum Unlocated {
    /// The producer invented this entry; no file ever contained it.
    ///
    /// Root modules, `$return` pseudo-parameters, and desugared receivers are
    /// all of this kind. This is the one variant that is *correct* rather than
    /// a gap: there is nothing to jump to, and there never will be.
    Synthesized,

    /// The item exists only after macro expansion, so no range in any file
    /// covers its declaration.
    ///
    /// Distinct from [`Unlocated::Synthesized`]: a macro-expanded item *does*
    /// have an origin (the macro call site), it is simply not the item's own
    /// text. A future pass can point at the call site; a synthesized entry has
    /// nowhere to point at all.
    MacroExpanded,

    /// The producer read this declaration out of a real file and does not
    /// record where.
    ///
    /// This is the L31 backlog, in-band. Grep for it to count what is left:
    /// every construction of this variant is a producer that has not been
    /// migrated, and the count is the size of the remaining work.
    ProducerRecordsNoLocation,

    /// The declaration's file lies outside the documented package's root, so
    /// no package-relative [`SourceFile`] can name it.
    ///
    /// Re-exports of dependency items reach this. Recorded separately from
    /// [`Unlocated::ProducerRecordsNoLocation`] because it is not a producer
    /// gap — the producer knows exactly where the item is, and the *relative*
    /// path is what cannot express it.
    OutsideDocumentedPackage,
}

// ---------------------------------------------------------------------------
// SourceLocation
// ---------------------------------------------------------------------------

/// Where a declaration was written.
///
/// See the [module documentation](self) for why this is an enum with a reason
/// rather than an `Option`, and why `Declared` carries lines as well as bytes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SourceLocation {
    /// A complete location: file, byte range, and a 1-based line/column range.
    ///
    /// The only variant on which a jump-to-source affordance may be built.
    Declared {
        /// Package-relative path of the file.
        file: SourceFile,
        /// Byte range of the declaration within `file`.
        bytes: ByteSpan,
        /// Line/column of the first byte of the declaration.
        start: LineCol,
        /// Line/column of the byte one past the declaration's last.
        end: LineCol,
    },

    /// A file and a byte range, with no line information.
    ///
    /// The honest description of a producer that records *where* an item is
    /// but has never built a line index over the file. A consumer may show
    /// this — labelled as bytes — but must not present it as a jump target,
    /// because byte offsets are not addressable by any editor or browser.
    BytesOnly {
        /// Package-relative path of the file.
        file: SourceFile,
        /// Byte range of the declaration within `file`.
        bytes: ByteSpan,
    },

    /// No location, and the reason why.
    Unlocated(Unlocated),
}

impl SourceLocation {
    /// Recover a location from the legacy [`Symbol::source`](super::Symbol)
    /// and [`Symbol::span`](super::Symbol) pair.
    ///
    /// # Why this exists rather than a migration of every producer
    ///
    /// Six of the seven producers still write the legacy pair, and their
    /// construction sites are spread across tracks owned by other work. This
    /// function is the **single** place the pair is decoded — the ad-hoc
    /// `is_empty() || == "."` test that used to live in `nudox-engine`'s head
    /// chunker, and a second copy of it in the GUI, are both replaced by a call
    /// here. Every un-migrated producer therefore gets the same, named verdict,
    /// and moving one producer to a real location is a local change.
    ///
    /// The result is never [`SourceLocation::Declared`]: the legacy pair has no
    /// room for line information, so no amount of decoding can invent it. That
    /// is the whole reason `Declared` needs a producer-side change.
    pub fn from_legacy(source: &std::path::Path, span: &Range<usize>) -> Self {
        let Some(file) = SourceFile::new(&source.to_string_lossy()) else {
            return SourceLocation::Unlocated(Unlocated::ProducerRecordsNoLocation);
        };
        let start = u32::try_from(span.start).unwrap_or(u32::MAX);
        let end = u32::try_from(span.end).unwrap_or(u32::MAX);
        // A real path with an empty span (`None`): the producer named the file
        // and then wrote the `0..0` sentinel. Reporting `BytesOnly` with an
        // empty range would re-tell the original lie.
        ByteSpan::new(start, end).map_or(
            SourceLocation::Unlocated(Unlocated::ProducerRecordsNoLocation),
            |bytes| SourceLocation::BytesOnly { file, bytes },
        )
    }

    /// The serde fallback for entries encoded before this field existed.
    ///
    /// Named rather than a `Default` impl on purpose: `SourceLocation` has no
    /// `Default`, so no caller can reach an absent location through
    /// `..Default::default()`. Only a decoder of old bytes may, and it says so
    /// by name.
    pub fn legacy_encoding() -> Self {
        SourceLocation::Unlocated(Unlocated::ProducerRecordsNoLocation)
    }

    /// The file, when one is known.
    pub fn file(&self) -> Option<&SourceFile> {
        match self {
            SourceLocation::Declared { file, .. } | SourceLocation::BytesOnly { file, .. } => {
                Some(file)
            }
            SourceLocation::Unlocated(_) => None,
        }
    }

    /// The byte range, when one is known.
    pub fn bytes(&self) -> Option<ByteSpan> {
        match self {
            SourceLocation::Declared { bytes, .. } | SourceLocation::BytesOnly { bytes, .. } => {
                Some(*bytes)
            }
            SourceLocation::Unlocated(_) => None,
        }
    }

    /// The 1-based line/column range, present only on
    /// [`SourceLocation::Declared`].
    ///
    /// A consumer building a jump target should match on this: `None` means
    /// "there is nowhere to send the user", and rendering a link anyway is the
    /// defect L31 describes.
    pub fn lines(&self) -> Option<(LineCol, LineCol)> {
        match self {
            SourceLocation::Declared { start, end, .. } => Some((*start, *end)),
            SourceLocation::BytesOnly { .. } | SourceLocation::Unlocated(_) => None,
        }
    }

    /// Project back onto the legacy `(Symbol::source, Symbol::span)` pair.
    ///
    /// The inverse of [`SourceLocation::from_legacy`], and the **only**
    /// supported way for a migrated producer to fill those two fields — so the
    /// typed value and the legacy pair cannot disagree. Line information is
    /// dropped here, because the pair has nowhere to put it; the full location
    /// travels on the entry (see `Entry::location`).
    pub fn legacy_pair(&self) -> (PathBuf, Range<usize>) {
        self.file().map_or_else(
            || (PathBuf::new(), 0..0),
            |file| {
                (
                    PathBuf::from(file.as_str()),
                    self.bytes().map_or(0..0, ByteSpan::as_range),
                )
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sentinel this module exists to delete must not survive a round trip
    /// through the decoder: an empty path with a `0..0` span is *absent*, and
    /// the decoded value must say so by name rather than report byte 0.
    #[test]
    fn legacy_zero_sentinel_decodes_to_a_named_absence_not_to_offset_zero() {
        let decoded = SourceLocation::from_legacy(std::path::Path::new(""), &(0..0));
        assert_eq!(
            decoded,
            SourceLocation::Unlocated(Unlocated::ProducerRecordsNoLocation)
        );
        assert!(decoded.bytes().is_none());
        assert!(decoded.file().is_none());
    }

    /// A producer that names a file but writes the `0..0` span has still not
    /// recorded a location; reporting `BytesOnly { bytes: 0..0 }` would carry
    /// the original lie forward under a new name.
    #[test]
    fn legacy_real_path_with_empty_span_is_absent_not_a_zero_length_span() {
        let decoded = SourceLocation::from_legacy(std::path::Path::new("src/lib.rs"), &(0..0));
        assert_eq!(
            decoded,
            SourceLocation::Unlocated(Unlocated::ProducerRecordsNoLocation)
        );
    }

    /// Decoding the legacy pair can never manufacture line information — the
    /// pair does not contain any. This is the invariant that makes
    /// `Declared` a real signal: it is reachable only from a producer that
    /// computed lines.
    #[test]
    fn legacy_decoding_never_yields_a_jump_target() {
        let decoded = SourceLocation::from_legacy(std::path::Path::new("src/lib.rs"), &(10..42));
        assert_eq!(decoded.lines(), None, "legacy pair carries no line index");
        assert_eq!(decoded.bytes(), ByteSpan::new(10, 42));
    }

    /// A migrated producer's location must survive projection onto the legacy
    /// pair and back, so the two representations cannot drift while both exist.
    #[test]
    fn declared_location_projects_onto_the_legacy_pair_without_changing_file_or_bytes() {
        let declared = SourceLocation::Declared {
            file: SourceFile::new("src/memchr/memchr.rs").expect("non-empty relative path"),
            bytes: ByteSpan::new(8880, 9435).expect("non-empty span"),
            start: LineCol::from_zero_based(287, 0),
            end: LineCol::from_zero_based(290, 1),
        };
        let (path, span) = declared.legacy_pair();
        assert_eq!(path, PathBuf::from("src/memchr/memchr.rs"));
        assert_eq!(span, 8880..9435);

        let round_tripped = SourceLocation::from_legacy(&path, &span);
        assert_eq!(round_tripped.file(), declared.file());
        assert_eq!(round_tripped.bytes(), declared.bytes());
    }

    /// 1-based conversion, stated as the invariant a consumer relies on: what
    /// rust-analyzer calls line 287 is what an editor calls line 288.
    #[test]
    fn zero_based_tool_offsets_become_one_based_editor_coordinates() {
        let lc = LineCol::from_zero_based(287, 4);
        assert_eq!(lc.line(), 288);
        assert_eq!(lc.column(), 5);
    }

    /// Paths that historically meant "not recorded" must be rejected at
    /// construction, so no `SourceFile` in existence can name nothing.
    #[test]
    fn source_file_rejects_every_spelling_of_the_absent_path() {
        assert_eq!(SourceFile::new(""), None);
        assert_eq!(SourceFile::new("."), None);
        assert_eq!(SourceFile::new("/"), None);
    }

    /// Windows-produced IR must hash and compare equal to Unix-produced IR for
    /// the same file, or content identity becomes a property of the build host.
    #[test]
    fn backslash_separators_normalise_so_the_same_file_has_one_identity() {
        assert_eq!(
            SourceFile::new(r"src\memchr\memchr.rs"),
            SourceFile::new("src/memchr/memchr.rs")
        );
    }
}
