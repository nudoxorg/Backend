//! Result lists: search, names, relations, and any other bounded row page.
//!
//! One record is two lines, never one. The first line is the readable trail;
//! the second is the exact coordinate an agent passes back, on its own so it
//! can be copied without picking it out of prose, and never truncated by the
//! width logic. Everything else — kind glyph, language tag, lane provenance,
//! score — rides on the trail line where it costs nothing.

use crate::coverage::CoverageLine;
use crate::identity::Identity;
use crate::language::Language;
use crate::signature::Signature;
use backend_library::{DeclarationKind, PageContinuation, Row, RowState, SourceLocation};

/// The lifecycle of one result row.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RecordState {
    /// Row content is available.
    Ready,
    /// Row is waiting for a demanded lane.
    Loading,
    /// Row failed with a typed lane result.
    Failed,
}

impl RecordState {
    /// Lowers one engine row state.
    #[must_use]
    pub const fn from_row(state: RowState) -> Self {
        match state {
            RowState::Ready => Self::Ready,
            RowState::Loading => Self::Loading,
            RowState::Failed => Self::Failed,
        }
    }

    /// Returns the stable lowercase name shared by every surface.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Loading => "loading",
            Self::Failed => "failed",
        }
    }

    /// Returns the glyph that precedes a record.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Ready => "●",
            Self::Loading => "◐",
            Self::Failed => "✗",
        }
    }
}

/// A bounded relevance score.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Score(u32);

impl Score {
    /// Retains one reported score.
    #[must_use]
    pub const fn new(score: u32) -> Self {
        Self(score)
    }

    /// Returns the reported score.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// One result row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Record {
    identity: Identity,
    kind: Option<DeclarationKind>,
    language: Language,
    state: RecordState,
    signature: Option<Signature>,
    summary: Option<String>,
    score: Option<Score>,
}

impl Record {
    /// Lowers one engine row into a result record.
    #[must_use]
    pub fn from_row(row: &Row) -> Self {
        let captured = row.source.captured();
        let identity = Identity::parse_with_key(&row.label, row.id.into()).with_captured_source(
            captured.map(SourceLocation::path),
            captured.map(SourceLocation::start_line),
        );
        let language = identity.language();
        let signature = row
            .signature
            .as_ref()
            .map(|text| Signature::tokenize(text, language));
        Self {
            identity,
            kind: row.kind,
            language,
            state: RecordState::from_row(row.state),
            signature: signature.filter(|signature| !signature.is_empty()),
            summary: None,
            score: row.score.map(Score::new),
        }
    }

    /// Attaches a one-line summary drawn from the row's documentation.
    #[must_use]
    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        let summary = summary.into();
        let trimmed = summary.trim();
        self.summary = (!trimmed.is_empty()).then(|| trimmed.to_owned());
        self
    }

    /// Returns the record identity.
    #[must_use]
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Returns the declaration kind.
    #[must_use]
    pub const fn kind(&self) -> Option<DeclarationKind> {
        self.kind
    }

    /// Returns the source language.
    #[must_use]
    pub const fn language(&self) -> Language {
        self.language
    }

    /// Returns the row lifecycle.
    #[must_use]
    pub const fn state(&self) -> RecordState {
        self.state
    }

    /// Returns the tokenized signature.
    #[must_use]
    pub const fn signature(&self) -> Option<&Signature> {
        self.signature.as_ref()
    }

    /// Returns the one-line summary.
    #[must_use]
    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    /// Returns the reported score.
    #[must_use]
    pub const fn score(&self) -> Option<Score> {
        self.score
    }
}

/// One bounded page of result records with its honest lane coverage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordList {
    query: String,
    coverage: CoverageLine,
    records: Box<[Record]>,
    more: bool,
    continuation: Option<PageContinuation>,
    /// Why this page has no records, when the caller proved a reason other
    /// than a missed declaration. Search leaves this empty.
    empty_reason: Option<String>,
}

impl RecordList {
    /// Records one bounded result page.
    #[must_use]
    pub fn new(
        query: impl Into<String>,
        coverage: CoverageLine,
        records: impl Into<Box<[Record]>>,
    ) -> Self {
        Self {
            query: query.into(),
            coverage,
            records: records.into(),
            more: false,
            continuation: None,
            empty_reason: None,
        }
    }

    /// Names why an empty page is empty. A graph or related probe with no
    /// edges must not reuse the search sentence.
    #[must_use]
    pub fn with_empty_reason(mut self, reason: impl Into<String>) -> Self {
        self.empty_reason = Some(reason.into());
        self
    }

    /// The sentence a renderer prints when [`Self::is_empty`] is true.
    #[must_use]
    pub fn empty_explanation(&self) -> String {
        if let Some(reason) = &self.empty_reason {
            return reason.clone();
        }
        if self.coverage.has_unavailable() {
            return "no rows, and at least one lane answered nothing — read the marks above"
                .to_owned();
        }
        if self.coverage.readiness() == "indexing" {
            return "no rows yet; indexing has not finished for this revision".to_owned();
        }
        format!("no declaration matches {:?} at this revision", self.query)
    }

    /// Marks that the owner holds another page after this one.
    #[must_use]
    pub const fn with_more(mut self, more: bool) -> Self {
        self.more = more;
        self
    }

    /// Retains the owner-issued continuation for a following page.
    #[must_use]
    pub const fn with_continuation(mut self, continuation: Option<PageContinuation>) -> Self {
        self.continuation = continuation;
        self.more = self.continuation.is_some();
        self
    }

    /// Returns the query text this page answers.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Returns the honest lane coverage behind this page.
    #[must_use]
    pub const fn coverage(&self) -> CoverageLine {
        self.coverage
    }

    /// Returns the records in rank order.
    #[must_use]
    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// Returns whether another page follows.
    #[must_use]
    pub const fn has_more(&self) -> bool {
        self.more
    }

    /// Returns the owner-issued continuation, when another page follows.
    #[must_use]
    pub const fn continuation(&self) -> Option<PageContinuation> {
        self.continuation
    }

    /// Returns whether this page carried no record.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The proven empty-page reason, when the caller set one.
    #[must_use]
    pub fn empty_reason(&self) -> Option<&str> {
        self.empty_reason.as_deref()
    }
}
