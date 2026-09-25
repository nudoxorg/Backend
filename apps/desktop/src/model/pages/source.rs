//! The Source board's read model: the deepest depth of one declaration.

use super::common::{ByteSpan, DeclRef, Known, LineSpan};
use super::symbol::{FileSpan, SymbolLink};
use std::sync::Arc;

/// Where the source text in a [`SourceView`] came from.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SourceOrigin {
    /// The engine's bounded declaration excerpt: the declaration only, at
    /// most 4096 bytes.
    Excerpt,
    /// The whole local file, read from disk and checked against the engine's
    /// excerpt at the declared line, so it is the text the index saw.
    LocalFile,
}

/// Source text with the line its first byte sits on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceText {
    /// The text.
    pub text: Arc<str>,
    /// One-based line of the text's first byte.
    pub first_line: u32,
    /// Where the text came from.
    pub origin: SourceOrigin,
    /// Whether the producer retained the complete declaration.
    pub complete: bool,
}

impl SourceText {
    /// Returns the byte span of one one-based line inside `text`.
    #[must_use]
    pub fn line_span(&self, line: u32) -> Option<ByteSpan> {
        let index = line.checked_sub(self.first_line)? as usize;
        let mut start = 0_usize;
        for (current, segment) in self.text.split_inclusive('\n').enumerate() {
            let end = start + segment.len();
            if current == index {
                let trimmed = segment.trim_end_matches(['\n', '\r']).len();
                return ByteSpan::new(u32::try_from(start).ok()?, u32::try_from(start + trimmed).ok()?);
            }
            start = end;
        }
        None
    }

    /// Returns the number of lines in the text.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.text.lines().count()
    }
}

/// One identifier inside the text that links to a declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentifierSpan {
    /// Byte span inside [`SourceText::text`].
    pub span: ByteSpan,
    /// Where it links, with the evidence.
    pub link: SymbolLink,
}

/// Everything the Source board renders for one declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceView {
    /// The declaration being read.
    pub symbol: DeclRef,
    /// Package-relative file path.
    pub file: Known<Arc<str>>,
    /// Source text.
    pub text: Known<SourceText>,
    /// Lines the declaration spans.
    pub declaration: Known<LineSpan>,
    /// Identifiers in `text` that link elsewhere.
    pub identifiers: Known<Arc<[IdentifierSpan]>>,
    /// Uses of this declaration located in `text`, as spans in `text`.
    pub uses: Known<Arc<[ByteSpan]>>,
    /// Uses of this declaration in other files, as the producer spelled them.
    pub uses_elsewhere: Arc<[FileSpan]>,
}
