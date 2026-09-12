//! Defines source behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the source invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Source text and related symbols for one declaration: the source button and the hover card.

use compiler_ir::LinkKind;
use compiler_vocabulary::Language;
use interface_documents::{ByteSpan, Direction, Symbol, Text};
use interface_identity::PackageCoordinate;
use interface_search::{Coverage, ResultLimit, Score};

use crate::{PageError, PageLocator};

/// Most bytes one source excerpt carries.
pub const MAX_SOURCE_BYTES: usize = 256 * 1024;
/// Most context lines either side of a declaration.
pub const MAX_CONTEXT_LINES: u8 = 200;
/// Context lines when a surface does not choose.
pub const DEFAULT_CONTEXT_LINES: u8 = 12;

/// How many lines before and after the declaration an excerpt includes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextLines(u8);

impl ContextLines {
    /// Clamps into `0..=MAX_CONTEXT_LINES`.
    #[must_use]
    pub const fn clamped(lines: u8) -> Self {
        if lines > MAX_CONTEXT_LINES {
            Self(MAX_CONTEXT_LINES)
        } else {
            Self(lines)
        }
    }

    /// The count.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl Default for ContextLines {
    fn default() -> Self {
        Self(DEFAULT_CONTEXT_LINES)
    }
}

/// One-based line ordinal.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LineNumber(pub u32);

/// One source request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRequest {
    /// Which declaration.
    pub locator: PageLocator,
    /// Lines of context either side.
    pub context: ContextLines,
}

/// One source excerpt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceText {
    /// The declaration.
    pub symbol: Symbol,
    /// Language the compiler proved, for highlighting.
    pub language: Language,
    /// Package-relative file path.
    pub file: Text,
    /// The declaration's span in the whole file.
    pub span: ByteSpan,
    /// Line number of the excerpt's first line.
    pub first_line: LineNumber,
    /// The excerpt.
    pub text: Text,
    /// The declaration's span inside `text`.
    pub highlight: ByteSpan,
    /// Whether the excerpt was cut to [`MAX_SOURCE_BYTES`].
    pub truncated: bool,
}

/// Exact source failure.
#[derive(Debug)]
pub enum SourceError {
    /// The declaration could not be located.
    Page(PageError),
    /// The image retained no source span for it.
    NoSpan {
        /// The declaration.
        symbol: Symbol,
    },
    /// The package's sources are not on this machine.
    NoSourceRoot {
        /// The package.
        package: PackageCoordinate,
    },
    /// The file could not be read.
    Unreadable {
        /// Package-relative file path.
        file: Text,
        /// Bounded description.
        detail: Box<str>,
    },
    /// The span points outside the file.
    SpanOutOfFile {
        /// Package-relative file path.
        file: Text,
        /// Span as retained.
        span: ByteSpan,
        /// File length observed.
        length: usize,
    },
}

impl SourceError {
    /// Stable cause slug, the same word on every surface.
    #[must_use]
    pub const fn slug(&self) -> &'static str {
        match self {
            Self::Page(_) => "page",
            Self::NoSpan { .. } => "source-no-span",
            Self::NoSourceRoot { .. } => "source-root-absent",
            Self::Unreadable { .. } => "source-unreadable",
            Self::SpanOutOfFile { .. } => "source-span-out-of-file",
        }
    }

    /// One line in the failure's own words.
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::Page(_) => "the declaration could not be located".to_owned(),
            Self::NoSpan { .. } => "the image retained no source span for this declaration".to_owned(),
            Self::NoSourceRoot { .. } => {
                "the package sources are not on this machine; add it again to fetch them".to_owned()
            }
            Self::Unreadable { detail, .. } => format!("the source file could not be read: {detail}"),
            Self::SpanOutOfFile { length, .. } => {
                format!("the retained span lies outside the {length}-byte file")
            }
        }
    }
}

/// One related request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelatedRequest {
    /// Which declaration.
    pub locator: PageLocator,
    /// Most rows.
    pub limit: ResultLimit,
}

/// Why one symbol is related.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Relation {
    /// Joined by a canonical link.
    Linked {
        /// Link kind.
        kind: LinkKind,
        /// Which way, relative to the subject.
        direction: Direction,
    },
    /// Shares the subject's parent.
    Sibling,
    /// Close in the vector space.
    Semantic {
        /// Similarity as the lane scored it.
        score: Score,
    },
    /// Shares a name stem.
    Lexical {
        /// Match strength as the lane scored it.
        score: Score,
    },
}

/// One related symbol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelatedRow {
    /// The symbol.
    pub symbol: Symbol,
    /// Why.
    pub relation: Relation,
    /// First documentation line.
    pub summary: Option<Text>,
}

/// The related set for one declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Related {
    /// The subject.
    pub symbol: Symbol,
    /// Rows, strongest first; empty means nothing is related, which a surface shows as no card.
    pub rows: Box<[RelatedRow]>,
    /// Whether the relation graph ran.
    pub graph: Coverage,
    /// Whether the vector lane ran.
    pub semantic: Coverage,
}

impl Related {
    /// Whether a surface should show anything at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_lines_are_bounded() {
        assert_eq!(ContextLines::clamped(255).get(), MAX_CONTEXT_LINES);
        assert_eq!(ContextLines::default().get(), DEFAULT_CONTEXT_LINES);
    }
}
