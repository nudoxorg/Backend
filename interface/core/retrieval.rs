//! Typed, monomorphized retrieval capability facts for the application service.
//!
//! This module deliberately contains no index implementation.  Concrete consumption of
//! `server-index-core` belongs to adapter or test code so the portable application service
//! never manufactures snapshot authority, rows, or unload history.

use crate::InputText;

pub(crate) const MAX_REPLY_ROW_SLOTS: usize = 4;
#[allow(
    clippy::as_conversions,
    reason = "the public `u8` limit and fixed array length are kept equal by this compile-time assertion"
)]
const _: [(); MAX_REPLY_ROW_SLOTS] = [(); crate::MAX_REPLY_ROWS as usize];

/// Maximum inline signature tokens carried by one bounded retrieval row.
pub const MAX_SIGNATURE_TOKENS: usize = 4;

/// Closed role for one rendered signature token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenKind {
    /// Declaration or control keyword.
    Keyword,
    /// Identifier spelling.
    Name,
    /// Type spelling.
    Type,
    /// Structural punctuation.
    Punctuation,
    /// Other rendered text.
    Text,
}

/// One signature token and its optional resolved documentation target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignatureToken {
    /// Closed render role.
    pub kind: TokenKind,
    /// Original spelling.
    pub text: InputText,
    /// Resolved target when the capability can prove one.
    pub target: Option<InputText>,
}

/// Fixed, admission-ordered signature facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignatureTokens {
    slots: [Option<SignatureToken>; MAX_SIGNATURE_TOKENS],
}

impl SignatureTokens {
    /// Creates an empty token prefix.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: [None; MAX_SIGNATURE_TOKENS],
        }
    }

    /// Adds one token or returns the exact rejected token when full.
    pub const fn push(mut self, token: SignatureToken) -> Result<Self, SignatureToken> {
        let [first, second, third, fourth] = &mut self.slots;
        if first.is_none() {
            *first = Some(token);
        } else if second.is_none() {
            *second = Some(token);
        } else if third.is_none() {
            *third = Some(token);
        } else if fourth.is_none() {
            *fourth = Some(token);
        } else {
            return Err(token);
        }
        Ok(self)
    }

    /// Iterates the admitted token prefix in its capability-owned order.
    pub fn iter(&self) -> impl Iterator<Item = &SignatureToken> {
        self.slots.iter().flatten()
    }
}

impl Default for SignatureTokens {
    fn default() -> Self {
        Self::new()
    }
}

/// Closed documentation section that owns a row's placement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocSection {
    /// Overview or exact identity placement.
    Summary,
    /// Member or lexical placement.
    Members,
    /// Field or variant placement.
    Fields,
    /// Source placement.
    Source,
}

/// Whether the configured retrieval capability can answer questions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetrievalReadiness {
    /// A local capability owns snapshot facts.
    Ready,
    /// No accepted capability is configured.
    Unavailable,
}

/// Closed mode that produced a retrieval row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetrievalMode {
    /// Deterministic exact-key lookup.
    Exact,
    /// Deterministic lexical prefix/ranking lookup.
    Lexical,
}

/// A checked half-open source byte span.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetrievalSpan {
    /// Inclusive start byte offset.
    pub start: u32,
    /// Exclusive end byte offset.
    pub end: u32,
}

impl RetrievalSpan {
    /// Creates a non-inverted source span.
    pub const fn new(start: u32, end: u32) -> Result<Self, (u32, u32)> {
        if start > end {
            Err((start, end))
        } else {
            Ok(Self { start, end })
        }
    }
}

/// One bounded retrieval result, complete with authority-owned rank facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetrievalRow {
    /// Capability-owned document rendering.
    pub document: InputText,
    /// Matching key or term rendering.
    pub term: InputText,
    /// Byte anchor when the capability proves one.
    pub span: Option<RetrievalSpan>,
    /// Capability-defined deterministic score.
    pub score: u32,
    /// Retrieval mode.
    pub mode: RetrievalMode,
    /// Optional signature facts supplied by the authority.
    pub signature: Option<SignatureTokens>,
    /// Documentation section.
    pub section: DocSection,
}

/// A packed, fixed-capacity retrieval result table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetrievalRows {
    slots: [Option<RetrievalRow>; MAX_REPLY_ROW_SLOTS],
}

impl RetrievalRows {
    /// Creates an empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: [None; MAX_REPLY_ROW_SLOTS],
        }
    }

    /// Appends a row or returns the exact rejected row if the table is full.
    #[allow(
        clippy::result_large_err,
        reason = "the rejected bounded row is an exact capability terminal and needs no allocation"
    )]
    pub const fn push(mut self, row: RetrievalRow) -> Result<Self, RetrievalRow> {
        let [first, second, third, fourth] = &mut self.slots;
        if first.is_none() {
            *first = Some(row);
        } else if second.is_none() {
            *second = Some(row);
        } else if third.is_none() {
            *third = Some(row);
        } else if fourth.is_none() {
            *fourth = Some(row);
        } else {
            return Err(row);
        }
        Ok(self)
    }

    /// Number of admitted rows.
    #[must_use]
    pub const fn len(&self) -> u8 {
        let [first, second, third, fourth] = self.slots;
        let first = if first.is_some() { 1 } else { 0 };
        let second = if second.is_some() { 1 } else { 0 };
        let third = if third.is_some() { 1 } else { 0 };
        let fourth = if fourth.is_some() { 1 } else { 0 };
        first + second + third + fourth
    }

    /// Whether the table has no rows.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterates rows in authority-owned rank order.
    pub fn iter(&self) -> impl Iterator<Item = &RetrievalRow> {
        self.slots.iter().flatten()
    }
}

impl Default for RetrievalRows {
    fn default() -> Self {
        Self::new()
    }
}

impl<'rows> IntoIterator for &'rows RetrievalRows {
    type Item = &'rows RetrievalRow;
    type IntoIter = core::iter::Flatten<core::slice::Iter<'rows, Option<RetrievalRow>>>;

    fn into_iter(self) -> Self::IntoIter {
        self.slots.iter().flatten()
    }
}

/// Residency facts for exactly one immutable snapshot selector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotFacts {
    /// Named snapshot.
    pub snapshot: InputText,
    /// Whether the capability currently owns it.
    pub resident: bool,
}

/// Idempotent unload receipt for exactly one snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnloadReceipt {
    /// Named snapshot.
    pub snapshot: InputText,
    /// `true` only when this invocation removed a resident snapshot.
    pub removed: bool,
}

/// Exact retrieval capability terminal.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(
    clippy::large_enum_variant,
    reason = "the rejected fixed row is retained exactly rather than reified through an allocation"
)]
pub enum RetrievalCause {
    /// The selector is not known to the configured authority.
    SnapshotUnknown {
        /// Exact selector.
        snapshot: InputText,
    },
    /// Query admission failed with a typed reason.
    QueryRejected {
        /// Exact query cause.
        reason: RetrievalQueryCause,
    },
    /// The capability failed during one named phase.
    Backend {
        /// Failing phase.
        phase: RetrievalPhase,
    },
    /// The fixed response table could not retain this row.
    RowTableFull {
        /// Exact rejected row.
        rejected: RetrievalRow,
    },
    /// The bounded receipt journal could not retain this selector.
    JournalFull {
        /// Exact rejected selector.
        rejected: InputText,
    },
}

/// Exact query admission cause.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetrievalQueryCause {
    /// Empty lexical input is rejected by this capability.
    Empty,
    /// Unsupported query syntax is retained unchanged.
    Unsupported {
        /// Exact query.
        observed: InputText,
    },
}

/// Named retrieval phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetrievalPhase {
    /// Candidate scan phase.
    Scan,
    /// Ranking/projection phase.
    Rank,
}

/// Borrowed retrieval request after service admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetrievalRequest<'input> {
    /// Snapshot selector.
    pub snapshot: &'input InputText,
    /// Lexical query.
    pub query: &'input InputText,
    /// Fixed service-admitted result bound.
    pub limit: u8,
}

/// Monomorphized retrieval capability paired with the compiler capability.
pub trait RetrievalCapability {
    /// Whether this capability is configured.
    fn readiness(&self) -> RetrievalReadiness;

    /// Returns exact snapshot residency facts or a retained capability cause.
    #[allow(
        clippy::result_large_err,
        reason = "the fixed terminal must retain the exact rejected retrieval fact"
    )]
    fn snapshot_status(&self, snapshot: &InputText) -> Result<SnapshotFacts, RetrievalCause>;

    /// Returns typed bounded results or a retained capability cause.
    #[allow(
        clippy::result_large_err,
        reason = "the fixed terminal must retain the exact rejected retrieval fact"
    )]
    fn search(&self, request: RetrievalRequest<'_>) -> Result<RetrievalRows, RetrievalCause>;

    /// Produces an idempotent receipt or a retained capability cause.
    #[allow(
        clippy::result_large_err,
        reason = "the fixed terminal must retain the exact rejected retrieval fact"
    )]
    fn unload(&mut self, snapshot: &InputText) -> Result<UnloadReceipt, RetrievalCause>;
}

/// Portable default that reports unavailable before any retrieval operation is attempted.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UnavailableRetrieval;

impl RetrievalCapability for UnavailableRetrieval {
    fn readiness(&self) -> RetrievalReadiness {
        RetrievalReadiness::Unavailable
    }

    fn snapshot_status(&self, _snapshot: &InputText) -> Result<SnapshotFacts, RetrievalCause> {
        Err(RetrievalCause::Backend {
            phase: RetrievalPhase::Scan,
        })
    }

    fn search(&self, _request: RetrievalRequest<'_>) -> Result<RetrievalRows, RetrievalCause> {
        Err(RetrievalCause::Backend {
            phase: RetrievalPhase::Scan,
        })
    }

    fn unload(&mut self, _snapshot: &InputText) -> Result<UnloadReceipt, RetrievalCause> {
        Err(RetrievalCause::Backend {
            phase: RetrievalPhase::Scan,
        })
    }
}
