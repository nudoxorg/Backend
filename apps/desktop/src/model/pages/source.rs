//! The Source board's read model: the deepest depth of one declaration.

use super::common::{ByteSpan, DeclRef, Known, LineSpan};
use super::symbol::{FileSpan, SymbolLink};
use std::sync::Arc;

/// Where the source text in a [`SourceView`] came from.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub enum SourceOrigin {
    /// The engine's bounded declaration excerpt: the declaration only, at
    /// most 4096 bytes.
    Excerpt,
    /// The whole local file, read from disk and checked against the engine's
    /// excerpt at the declared line, so it is the text the index saw.
    LocalFile,
}

/// Source text with the line its first byte sits on.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SourceText {
    /// The text.
    pub text: Arc<str>,
    /// Sparse immutable line checkpoints built on the read worker. The
    /// bounded index supports binary seeking plus one monotone scan of the
    /// visible window, without retaining one offset pair per source line.
    #[serde(default)]
    line_index: SourceLineIndex,
    /// One-based line of the text's first byte.
    pub first_line: u32,
    /// Where the text came from.
    pub origin: SourceOrigin,
    /// Whether the producer retained the complete declaration.
    pub complete: bool,
}

const LINE_CHECKPOINT_STRIDE: usize = 64;

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct SourceLineIndex {
    line_count: u32,
    checkpoints: Arc<[SourceLineCheckpoint]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct SourceLineCheckpoint {
    line_index: u32,
    byte_offset: u32,
}

impl SourceText {
    /// Builds source text and its reusable line-offset index.
    #[must_use]
    pub fn new(text: Arc<str>, first_line: u32, origin: SourceOrigin, complete: bool) -> Self {
        let line_index = SourceLineIndex::build(&text);
        Self {
            text,
            line_index,
            first_line,
            origin,
            complete,
        }
    }

    /// Returns the byte span of one one-based line inside `text`.
    #[must_use]
    pub fn line_span(&self, line: u32) -> Option<ByteSpan> {
        let index = line.checked_sub(self.first_line)? as usize;
        self.line_spans_in(index, 1).into_iter().next()
    }

    /// Returns the exact byte spans for a bounded one-based line window.
    /// The sparse checkpoint lookup is logarithmic; only the gap to the
    /// nearest checkpoint and the requested rows are scanned.
    #[must_use]
    pub fn line_spans_in(&self, first_index: usize, maximum: usize) -> Vec<ByteSpan> {
        self.line_index.spans_in(&self.text, first_index, maximum)
    }

    /// Returns the number of lines in the text.
    #[must_use]
    pub fn line_count(&self) -> usize {
        if self.line_index.line_count == 0 && !self.text.is_empty() {
            SourceLineIndex::build(&self.text).line_count as usize
        } else {
            self.line_index.line_count as usize
        }
    }
}

impl SourceLineIndex {
    fn build(text: &str) -> Self {
        if text.is_empty() {
            return Self::default();
        }
        let mut checkpoints = Vec::new();
        let mut line_index = 0_usize;
        let mut byte_offset = 0_usize;
        for segment in text.split_inclusive('\n') {
            if line_index.is_multiple_of(LINE_CHECKPOINT_STRIDE)
                && let (Ok(line_index), Ok(byte_offset)) =
                    (u32::try_from(line_index), u32::try_from(byte_offset))
            {
                checkpoints.push(SourceLineCheckpoint {
                    line_index,
                    byte_offset,
                });
            }
            line_index = line_index.saturating_add(1);
            byte_offset = byte_offset.saturating_add(segment.len());
        }
        Self {
            line_count: u32::try_from(line_index).unwrap_or(u32::MAX),
            checkpoints: checkpoints.into(),
        }
    }

    fn spans_in(&self, text: &str, first_index: usize, maximum: usize) -> Vec<ByteSpan> {
        if maximum == 0 || text.is_empty() {
            return Vec::new();
        }
        let index = if self.line_count == 0 {
            Self::build(text)
        } else {
            self.clone()
        };
        if first_index >= index.line_count as usize {
            return Vec::new();
        }
        let checkpoint_index = first_index / LINE_CHECKPOINT_STRIDE;
        let Some(checkpoint) = index.checkpoints.get(checkpoint_index).copied() else {
            return Vec::new();
        };
        let checkpoint_line = checkpoint.line_index as usize;
        let checkpoint_offset = checkpoint.byte_offset as usize;
        let skip = first_index.saturating_sub(checkpoint_line);
        let last_index = first_index
            .saturating_add(maximum)
            .min(index.line_count as usize);
        let mut spans = Vec::with_capacity(last_index.saturating_sub(first_index));
        let mut byte_offset = checkpoint_offset;
        let mut segments = text[checkpoint_offset..].split_inclusive('\n');
        for _ in 0..skip {
            let segment = segments.next()?;
            byte_offset = byte_offset.saturating_add(segment.len());
        }
        for segment in segments.take(last_index.saturating_sub(first_index)) {
            let start = byte_offset;
            let content_end = start + segment.trim_end_matches(['\n', '\r']).len();
            if let (Ok(start), Ok(content_end)) = (u32::try_from(start), u32::try_from(content_end))
            {
                spans.push(ByteSpan {
                    start,
                    end: content_end,
                });
            }
            byte_offset = byte_offset.saturating_add(segment.len());
        }
        spans
    }
}

/// One identifier inside the text that links to a declaration.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IdentifierSpan {
    /// Byte span inside [`SourceText::text`].
    pub span: ByteSpan,
    /// Where it links, with the evidence.
    pub link: SymbolLink,
}

/// Everything the Source board renders for one declaration.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SourceView {
    /// The declaration being read.
    pub symbol: DeclRef,
    /// Package-relative file path.
    pub file: Known<Arc<str>>,
    /// Canonical local editor/display path hint retained separately from the
    /// held-descriptor authority used to read source text.
    #[serde(default = "editor_path_unavailable")]
    pub editor_path: Known<Arc<str>>,
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

fn editor_path_unavailable() -> Known<Arc<str>> {
    Known::unknown(
        super::common::GapReason::NotRecorded,
        "no local editor path hint was recorded",
    )
}

#[cfg(test)]
mod tests {
    use super::{SourceOrigin, SourceText};
    use std::sync::Arc;

    #[test]
    fn sparse_line_index_seeks_and_borrows_only_the_requested_window() {
        let text: Arc<str> = (0..200)
            .map(|line| format!("line-{line:03}\n"))
            .collect::<String>()
            .into();
        let source = SourceText::new(Arc::clone(&text), 1, SourceOrigin::LocalFile, true);
        assert_eq!(source.line_count(), 200);

        for line_index in [0, 63, 64, 65, 127, 128, 199] {
            let span = source
                .line_spans_in(line_index, 1)
                .into_iter()
                .next()
                .expect("indexed line exists");
            let expected = format!("line-{line_index:03}");
            assert_eq!(source.text.get(span.range()), Some(expected.as_str()));
        }

        let window = source.line_spans_in(62, 5);
        assert_eq!(window.len(), 5);
        assert_eq!(source.text.get(window[0].range()), Some("line-062"));
        assert_eq!(source.text.get(window[4].range()), Some("line-066"));
        assert!(source.line_spans_in(200, 1).is_empty());
    }

    #[test]
    fn sparse_line_index_handles_crlf_and_final_newline() {
        let source = SourceText::new(
            Arc::from("first\r\nsecond\r\n"),
            10,
            SourceOrigin::Excerpt,
            true,
        );
        assert_eq!(source.line_count(), 2);
        assert_eq!(
            source
                .line_span(10)
                .and_then(|span| source.text.get(span.range())),
            Some("first")
        );
        assert_eq!(
            source
                .line_span(11)
                .and_then(|span| source.text.get(span.range())),
            Some("second")
        );
        assert!(source.line_span(12).is_none());
    }
}
