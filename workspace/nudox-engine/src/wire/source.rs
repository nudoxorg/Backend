//! Where a declaration was written, in the GUI's vocabulary.
//!
//! # Why this mirrors `nudox_ir::entry::SourceLocation` instead of re-exporting it
//!
//! Two reasons, and the first is mechanical: the wire is also the MCP schema
//! (LR-2), so every type here must implement `JsonSchema`. `nudox-ir` does not
//! depend on `schemars` and must not — it sits four crates below the seam —
//! and the orphan rule forbids implementing a foreign trait for a foreign type
//! from here. A re-export could only be schema'd as `String`, which would
//! describe the protocol wrongly.
//!
//! The second is the same reason `KindTag` mirrors `KindDiscriminant` and
//! `Provenance` mirrors the store's: what the GUI needs to know about a
//! location is not identical to what the IR needs to record. The IR
//! distinguishes *why* a location is missing so producers can be held to
//! account; the GUI needs that too, but as a closed set it can render, and it
//! needs the `#[non_exhaustive]` + version-boundary discipline every other wire
//! enum carries (§L2, LR-12).
//!
//! # The contract this type exists to state
//!
//! Before it, `SymbolHead` carried `source_path: Option<SharedStr>` and
//! `source_span: Option<[u32; 2]>`. Three things were wrong with that and all
//! three were visible in shipped frames (`docs/LIMITATIONS.md` L42.2):
//!
//! - the path could be `<file-id-806>`, a producer's internal index formatted
//!   to look like a filename;
//! - the span was **bytes**, which no editor, browser, or human can navigate
//!   to, so the GUI rendered the literal text `bytes 8880–9435`;
//! - `None` meant four different things — synthesized entry, macro-expanded
//!   item, dependency file, or a producer that simply does not record
//!   locations — and the reader could not tell which, so nobody could file the
//!   bug.
//!
//! [`SourceLocation::Declared`] is the only variant a jump affordance may be
//! built on. Rendering a link for the others is the defect, not the fix.

use std::num::NonZeroU32;

use schemars::JsonSchema;
use serde::Serialize;

use super::SharedStr;

/// A 1-based line and column in a source file.
///
/// `NonZeroU32` crosses the seam intact rather than being flattened to `u32`,
/// because "line 0" is the shape the old byte-offset sentinel took and a
/// consumer should not have to trust a comment that it cannot occur.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct LineCol {
    /// 1-based line number, as an editor counts.
    pub line: NonZeroU32,
    /// 1-based column, in UTF-8 bytes from the start of the line.
    pub column: NonZeroU32,
}

/// Why a declaration has no source location.
///
/// A closed set the GUI can render as an explanation rather than as an absence
/// — "this entry is synthesized" and "this producer does not record locations"
/// are different sentences to show a reader, and only the second is a bug.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UnlocatedReason {
    /// The producer invented this entry; no file ever contained it.
    Synthesized,
    /// The item exists only after macro expansion.
    MacroExpanded,
    /// The producer read the declaration from a file and does not record
    /// where. This one is an open item on `docs/LIMITATIONS.md` L31.
    ProducerRecordsNoLocation,
    /// The file is outside the documented package, so no package-relative path
    /// names it.
    OutsideDocumentedPackage,
    /// A reason this build of the GUI does not know about (LR-12).
    Unknown,
}

/// Where a declaration was written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SourceLocation {
    /// A complete location: package-relative file, byte range, and a 1-based
    /// line/column range.
    ///
    /// The only variant that can be turned into somewhere a user can go.
    Declared {
        /// Path relative to the package root, `/`-separated.
        file: SharedStr,
        /// `[start, end)` byte offsets within `file`.
        bytes: [u32; 2],
        /// Line/column of the declaration's first byte.
        start: LineCol,
        /// Line/column one past the declaration's last byte.
        end: LineCol,
    },

    /// A file and a byte range, with no line information.
    ///
    /// Show it, labelled as bytes; do not link it. Every producer except Rust
    /// is here or worse today.
    BytesOnly {
        /// Path relative to the package root, `/`-separated.
        file: SharedStr,
        /// `[start, end)` byte offsets within `file`.
        bytes: [u32; 2],
    },

    /// No location, and the reason why.
    Unlocated {
        /// Which kind of absence this is.
        reason: UnlocatedReason,
    },
}

impl SourceLocation {
    /// Project an IR location onto the wire.
    ///
    /// The **only** conversion between the two vocabularies, and deliberately
    /// exhaustive on both enums — no `_` arm — so that a new IR variant or a
    /// new absence reason fails to compile here rather than being silently
    /// folded into whichever bucket was nearest. That is the failure mode
    /// doctrine §8 describes as a silent repair, and the old
    /// `source_path.is_empty()` test in the head chunker was one.
    pub fn from_ir(loc: &nudox_ir::entry::SourceLocation) -> Self {
        use nudox_ir::entry::{SourceLocation as IrLoc, Unlocated as IrUnlocated};

        fn line_col(lc: nudox_ir::entry::LineCol) -> LineCol {
            LineCol {
                line: lc.line,
                column: lc.column,
            }
        }

        match loc {
            IrLoc::Declared {
                file,
                bytes,
                start,
                end,
            } => SourceLocation::Declared {
                file: SharedStr::from(file.as_str()),
                bytes: [bytes.start, bytes.end],
                start: line_col(*start),
                end: line_col(*end),
            },
            IrLoc::BytesOnly { file, bytes } => SourceLocation::BytesOnly {
                file: SharedStr::from(file.as_str()),
                bytes: [bytes.start, bytes.end],
            },
            IrLoc::Unlocated(reason) => SourceLocation::Unlocated {
                reason: match reason {
                    IrUnlocated::Synthesized => UnlocatedReason::Synthesized,
                    IrUnlocated::MacroExpanded => UnlocatedReason::MacroExpanded,
                    IrUnlocated::ProducerRecordsNoLocation => {
                        UnlocatedReason::ProducerRecordsNoLocation
                    }
                    IrUnlocated::OutsideDocumentedPackage => {
                        UnlocatedReason::OutsideDocumentedPackage
                    }
                },
            },
        }
    }

    /// `path:line:col`, the form every editor, `rustc` diagnostic, and shell
    /// tool accepts — or `None` when this location cannot be navigated to.
    ///
    /// Returning `None` for `BytesOnly` is the point: a byte offset formatted
    /// as `file:8880` reads as a line number and sends the reader to the wrong
    /// place, which is worse than sending them nowhere.
    pub fn jump_target(&self) -> Option<String> {
        match self {
            SourceLocation::Declared { file, start, .. } => {
                Some(format!("{}:{}:{}", file, start.line, start.column))
            }
            SourceLocation::BytesOnly { .. } | SourceLocation::Unlocated { .. } => None,
        }
    }
}
