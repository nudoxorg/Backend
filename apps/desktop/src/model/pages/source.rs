//! The Source board's read model: the deepest depth of one declaration.

use super::common::{ByteSpan, DeclRef, Known, LineSpan};
use super::symbol::{FileSpan, SymbolLink};
use std::fmt;
use std::sync::Arc;

/// Where the source text in a [`SourceView`] came from.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub enum SourceOrigin {
    /// The engine's bounded declaration excerpt: the declaration only, at
    /// most 4096 bytes. These are producer-captured bytes.
    Excerpt,
    /// The current whole local file, read from a held directory capability.
    /// This does not imply that bytes outside a verified declaration excerpt
    /// still match the version the engine indexed.
    LocalFile,
}

/// Evidence describing which source bytes are tied to the indexed producer.
///
/// This value is not serialized: it is rebuilt by the live read worker, and a
/// decoded claim must not become proof of a current source match.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SourceCoverage {
    /// No live producer/source-byte relationship has been verified.
    #[default]
    Unverified,
    /// The source view contains only the producer-captured excerpt.
    CapturedExcerpt,
    /// Only this exact range of the current live file matched the producer's
    /// declaration excerpt; bytes before and after it remain live-only.
    LiveFileExcerptVerified {
        /// Exact byte range found at the producer's declared source line.
        bytes: ByteSpan,
    },
}

/// Why a source byte buffer cannot be assigned unique one-based line IDs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceTextError {
    /// A source line starts at one, never zero.
    FirstLineZero,
    /// The buffer's byte offsets exceed the source-span representation.
    ByteRangeOverflow,
    /// The final line cannot be represented as a `u32`.
    LineRangeOverflow,
}

impl fmt::Display for SourceTextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::FirstLineZero => "source first line must be one or greater",
            Self::ByteRangeOverflow => "source byte offsets exceed the supported range",
            Self::LineRangeOverflow => "source line numbers exceed the supported range",
        })
    }
}

impl std::error::Error for SourceTextError {}

/// Source text with the line its first byte sits on.
#[derive(Clone, Debug, serde::Serialize)]
pub struct SourceText {
    /// The text.
    text: Arc<str>,
    /// Sparse immutable line checkpoints built on the read worker. The
    /// bounded index supports binary seeking plus one monotone scan of the
    /// visible window, without retaining one offset pair per source line.
    #[serde(skip)]
    line_index: SourceLineIndex,
    /// Source-match evidence established by a live read worker.
    #[serde(skip)]
    coverage: SourceCoverage,
    /// One-based line of the text's first byte.
    first_line: u32,
    /// Where the text came from.
    pub origin: SourceOrigin,
    /// Whether the producer's declaration excerpt is complete.
    pub complete: bool,
}

// The sparse index is derived/runtime-only state. Coverage is also omitted
// from persistence, but it affects whether links are enabled and therefore
// participates in semantic equality so a fresh revalidation redraws the page.
impl PartialEq for SourceText {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
            && self.coverage == other.coverage
            && self.first_line == other.first_line
            && self.origin == other.origin
            && self.complete == other.complete
    }
}

impl Eq for SourceText {}

const LINE_CHECKPOINT_STRIDE: usize = 64;
const MAX_DESERIALIZED_SOURCE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SourceLineIndex {
    line_count: u32,
    checkpoints: Arc<[SourceLineCheckpoint]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceLineCheckpoint {
    line_index: u32,
    byte_offset: u32,
}

impl SourceText {
    /// Builds source text and its reusable line-offset index.
    ///
    /// # Errors
    /// Rejects zero-based or overflowing line numbers and byte offsets before
    /// the text can enter a page's sparse line index.
    pub fn new(
        text: Arc<str>,
        first_line: u32,
        origin: SourceOrigin,
        complete: bool,
    ) -> Result<Self, SourceTextError> {
        if first_line == 0 {
            return Err(SourceTextError::FirstLineZero);
        }
        u32::try_from(text.len()).map_err(|_| SourceTextError::ByteRangeOverflow)?;
        let line_index = SourceLineIndex::build(&text);
        checked_line_range(first_line, line_index.line_count)?;
        Ok(Self {
            text,
            line_index,
            coverage: match origin {
                SourceOrigin::Excerpt => SourceCoverage::CapturedExcerpt,
                SourceOrigin::LocalFile => SourceCoverage::Unverified,
            },
            first_line,
            origin,
            complete,
        })
    }

    /// Records the exact excerpt range independently verified in this live
    /// file by the bounded source worker.
    #[must_use]
    pub(crate) fn with_verified_local_excerpt(mut self, bytes: ByteSpan) -> Self {
        if self.origin == SourceOrigin::LocalFile && self.text.get(bytes.range()).is_some() {
            self.coverage = SourceCoverage::LiveFileExcerptVerified { bytes };
        }
        self
    }

    /// Returns the immutable source text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the validated one-based line of the text's first byte.
    #[must_use]
    pub const fn first_line(&self) -> u32 {
        self.first_line
    }

    /// Returns the source-match evidence established by the read worker.
    #[must_use]
    pub const fn coverage(&self) -> SourceCoverage {
        self.coverage
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
        self.line_index.spans_in(self.text(), first_index, maximum)
    }

    /// Returns the number of lines in the text.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.line_index.line_count as usize
    }

    /// The checked one-based range represented by this text, if nonempty.
    /// The constructor and deserializer establish this invariant before
    /// admitting the immutable line origin.
    #[must_use]
    pub fn line_range(&self) -> Option<LineSpan> {
        checked_line_range(self.first_line, self.line_index.line_count)
            .ok()
            .flatten()
    }
}

fn checked_line_range(
    first_line: u32,
    line_count: u32,
) -> Result<Option<LineSpan>, SourceTextError> {
    if first_line == 0 {
        return Err(SourceTextError::FirstLineZero);
    }
    if line_count == 0 {
        return Ok(None);
    }
    let last = first_line
        .checked_add(line_count - 1)
        .ok_or(SourceTextError::LineRangeOverflow)?;
    Ok(Some(LineSpan {
        first: first_line,
        last,
    }))
}

#[derive(serde::Deserialize)]
struct SourceTextWire {
    text: Arc<str>,
    first_line: u32,
    origin: SourceOrigin,
    complete: bool,
}

impl<'de> serde::Deserialize<'de> for SourceText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = <SourceTextWire as serde::Deserialize>::deserialize(deserializer)?;
        if wire.text.len() > MAX_DESERIALIZED_SOURCE_BYTES {
            return Err(<D::Error as serde::de::Error>::custom(
                "saved source exceeds the bounded local-source limit",
            ));
        }
        let mut source = Self::new(wire.text, wire.first_line, wire.origin, wire.complete)
            .map_err(<D::Error as serde::de::Error>::custom)?;
        source.coverage = SourceCoverage::Unverified;
        Ok(source)
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
        if maximum == 0 || text.is_empty() || self.line_count == 0 {
            return Vec::new();
        }
        if first_index >= self.line_count as usize {
            return Vec::new();
        }
        let checkpoint_index = first_index / LINE_CHECKPOINT_STRIDE;
        let Some(checkpoint) = self.checkpoints.get(checkpoint_index).copied() else {
            return Vec::new();
        };
        let checkpoint_line = checkpoint.line_index as usize;
        let checkpoint_offset = checkpoint.byte_offset as usize;
        let skip = first_index.saturating_sub(checkpoint_line);
        let last_index = first_index
            .saturating_add(maximum)
            .min(self.line_count as usize);
        let mut spans = Vec::with_capacity(last_index.saturating_sub(first_index));
        let mut byte_offset = checkpoint_offset;
        let mut segments = text[checkpoint_offset..].split_inclusive('\n');
        for _ in 0..skip {
            let Some(segment) = segments.next() else {
                return spans;
            };
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
    /// Byte span inside the immutable [`SourceText::text`] view.
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
    /// Uses whose spans are proven to be in the displayed source bytes.
    pub uses: Known<Arc<[ByteSpan]>>,
    /// Use sites not proven to be inside the displayed source bytes, as the
    /// producer spelled them. This may include this file outside a verified
    /// excerpt range.
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
    use super::{SourceCoverage, SourceOrigin, SourceText, SourceTextError};
    use crate::model::pages::ByteSpan;
    use std::sync::Arc;

    #[test]
    fn sparse_line_index_seeks_and_borrows_only_the_requested_window() {
        let text: Arc<str> = (0..200)
            .map(|line| format!("line-{line:03}\n"))
            .collect::<String>()
            .into();
        let source = SourceText::new(Arc::clone(&text), 1, SourceOrigin::LocalFile, true)
            .expect("valid line range");
        assert_eq!(source.line_count(), 200);

        for line_index in [0, 63, 64, 65, 127, 128, 199] {
            let span = source
                .line_spans_in(line_index, 1)
                .into_iter()
                .next()
                .expect("indexed line exists");
            let expected = format!("line-{line_index:03}");
            assert_eq!(source.text().get(span.range()), Some(expected.as_str()));
        }

        let window = source.line_spans_in(62, 5);
        assert_eq!(window.len(), 5);
        assert_eq!(source.text().get(window[0].range()), Some("line-062"));
        assert_eq!(source.text().get(window[4].range()), Some("line-066"));
        assert!(source.line_spans_in(200, 1).is_empty());
    }

    #[test]
    fn sparse_line_index_handles_crlf_and_final_newline() {
        let source = SourceText::new(
            Arc::from("first\r\nsecond\r\n"),
            10,
            SourceOrigin::Excerpt,
            true,
        )
        .expect("valid line range");
        assert_eq!(source.line_count(), 2);
        assert_eq!(
            source
                .line_span(10)
                .and_then(|span| source.text().get(span.range())),
            Some("first")
        );
        assert_eq!(
            source
                .line_span(11)
                .and_then(|span| source.text().get(span.range())),
            Some("second")
        );
        assert!(source.line_span(12).is_none());
    }

    #[test]
    fn constructor_and_saved_source_reject_non_unique_line_numbers() {
        let zero = SourceText::new(Arc::from("one"), 0, SourceOrigin::Excerpt, true);
        assert_eq!(zero.unwrap_err(), SourceTextError::FirstLineZero);
        let overflow =
            SourceText::new(Arc::from("one\ntwo"), u32::MAX, SourceOrigin::Excerpt, true);
        assert_eq!(overflow.unwrap_err(), SourceTextError::LineRangeOverflow);
        let last = SourceText::new(Arc::from("last"), u32::MAX, SourceOrigin::Excerpt, true)
            .expect("one final line is representable");
        assert_eq!(
            last.line_range(),
            Some(crate::model::pages::LineSpan {
                first: u32::MAX,
                last: u32::MAX
            })
        );

        for first_line in [0, u32::MAX] {
            let saved = serde_json::json!({
                "text": "one\ntwo",
                "first_line": first_line,
                "origin": "Excerpt",
                "complete": true,
            });
            assert!(
                serde_json::from_value::<SourceText>(saved).is_err(),
                "saved range {first_line} was admitted"
            );
        }
    }

    #[test]
    fn deserialization_rebuilds_offsets_and_does_not_restore_coverage_proof() {
        let source = SourceText::new(Arc::from("one\ntwo\n"), 1, SourceOrigin::LocalFile, true)
            .expect("valid line range")
            .with_verified_local_excerpt(ByteSpan::new(0, 3).expect("excerpt range"));
        assert!(matches!(
            source.coverage(),
            SourceCoverage::LiveFileExcerptVerified { .. }
        ));

        let mut wire = serde_json::to_value(&source).expect("serialize source view");
        wire["line_index"] = serde_json::json!({
            "line_count": 80_000,
            "checkpoints": [{ "line_index": 0, "byte_offset": u32::MAX }],
        });
        let restored: SourceText = serde_json::from_value(wire).expect("rebuild source view");
        assert_eq!(restored.line_count(), 2);
        assert_eq!(
            serde_json::to_value(&restored).expect("serialize restored source"),
            serde_json::to_value(&source).expect("serialize original source"),
            "the persisted source projection round-trips without runtime proof"
        );
        assert_ne!(
            restored, source,
            "runtime coverage is part of semantic equality"
        );
        assert_eq!(
            restored
                .line_span(2)
                .and_then(|span| restored.text().get(span.range())),
            Some("two")
        );
        assert_eq!(restored.coverage(), SourceCoverage::Unverified);
    }

    #[test]
    fn deserialization_rejects_source_larger_than_the_worker_limit() {
        let wire = serde_json::json!({
            "text": "x".repeat(super::MAX_DESERIALIZED_SOURCE_BYTES + 1),
            "first_line": 1,
            "origin": "LocalFile",
            "complete": true,
        });
        assert!(serde_json::from_value::<SourceText>(wire).is_err());
    }
}
