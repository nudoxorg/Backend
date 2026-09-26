//! Defines lexical behavior for `backend-semantic::index_core`, whose purpose is to define immutable index documents, segments, and snapshot identities.
//! This module owns the lexical invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Borrowed immutable lexical segments.

use core::{cmp::Ordering, ops::Deref};

use crate::index_vocabulary::LexicalSegmentId;
use backend_version::{ContentHasher, FixedCanonicalRecord, IndexLexicalSegmentDomain};

use crate::index_core::document::{ENTITY_DOCUMENT_ID_BYTES, EntityDocumentId};

mod segment;

/// Maximum number of rows admitted by one lexical segment view.
///
/// This is the per-segment row budget, not a manifest-wide wall: a manifest
/// may select up to [`crate::index_core::MAX_SELECTED_SEGMENTS`] segments, so
/// a fragment with more term/document rows than this budget rolls over into
/// additional segments instead of being rejected. 4,096 rows keeps one
/// segment's rank scratch and identity pass bounded while admitting real
/// multi-package fragments.
pub const MAX_LEXICAL_ROWS: usize = 4096;

/// Maximum term bytes admitted by one lexical segment.
///
/// Derived from the measured per-row term budget and the admitted row bound so
/// the byte ceiling scales with [`MAX_LEXICAL_ROWS`] instead of rejecting a
/// larger real fragment. A segment whose complete term payload exceeds this
/// still returns the typed [`LexicalSegmentError::PayloadBytesLimit`].
pub const MAX_LEXICAL_PAYLOAD_BYTES: usize = MAX_LEXICAL_ROWS * MAX_LEXICAL_ROW_PAYLOAD_BYTES;

/// Per-row term payload budget inside one lexical segment.
pub const MAX_LEXICAL_ROW_PAYLOAD_BYTES: usize = 256;

const CANONICAL_CHUNK_BYTES: usize = 32;

struct CanonicalRecord<const BYTES: usize>([u8; BYTES]);

impl<const BYTES: usize> FixedCanonicalRecord<BYTES> for CanonicalRecord<BYTES> {
    fn canonical_bytes(&self) -> &[u8; BYTES] {
        &self.0
    }
}

fn write_bytes(hasher: &mut ContentHasher<IndexLexicalSegmentDomain>, bytes: &[u8]) {
    hasher.write_record(&CanonicalRecord((bytes.len() as u64).to_le_bytes()));
    for chunk in bytes.chunks(CANONICAL_CHUNK_BYTES) {
        let mut record = [0_u8; CANONICAL_CHUNK_BYTES + 1];
        record[0] = chunk.len() as u8;
        record[1..=chunk.len()].copy_from_slice(chunk);
        hasher.write_record(&CanonicalRecord(record));
    }
}

#[allow(
    clippy::result_large_err,
    reason = "the existing exact lexical-order cause retains both borrowed order keys without allocating"
)]
fn segment_id<'bytes>(
    rows: &[LexicalRow<'bytes>],
) -> Result<LexicalSegmentId, LexicalSegmentError<'bytes>> {
    let mut verifier = LexicalSegmentVerifier::new(rows.len())?;
    for row in rows {
        verifier.admit(*row)?;
    }
    verifier.finish()
}

fn write_row(hasher: &mut ContentHasher<IndexLexicalSegmentDomain>, row: LexicalRow<'_>) {
    write_bytes(hasher, row.term);
    let document: [u8; ENTITY_DOCUMENT_ID_BYTES] = row.document.into();
    hasher.write_record(&CanonicalRecord(document));
    let value = match row.value {
        LexicalRowValue::Present(score) => {
            let mut value = [0_u8; 5];
            value[0] = 1;
            value[1..].copy_from_slice(&u32::from(score).to_le_bytes());
            value
        }
        LexicalRowValue::Tombstone => [0_u8; 5],
    };
    hasher.write_record(&CanonicalRecord(value));
}

/// Maximum number of lexical hits one operation may request.
pub const MAX_LEXICAL_TOP_K: usize = MAX_LEXICAL_ROWS;

/// A fixed-width lexical score in recipe-defined integer units.
///
/// The score deliberately has no floating-point representation.  A recipe can
/// assign the units it needs while ordering remains total and reproducible on
/// every target.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct LexicalScore(u32);

impl From<u32> for LexicalScore {
    fn from(units: u32) -> Self {
        Self(units)
    }
}

impl From<LexicalScore> for u32 {
    fn from(score: LexicalScore) -> Self {
        score.0
    }
}

impl Deref for LexicalScore {
    type Target = u32;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl LexicalScore {
    /// Discounts a stored score toward a longer matched term.
    ///
    /// Equality is the exact match and retains the complete stored score. A longer matching term
    /// receives `stored * prefix_bytes / term_bytes` integer units, with no floating-point or
    /// platform-specific ordering.
    #[must_use]
    pub fn discounted(
        stored: LexicalScore,
        prefix_bytes: usize,
        term_bytes: usize,
    ) -> Option<LexicalScore> {
        if term_bytes <= prefix_bytes {
            return Some(stored);
        }
        let scaled = u64::from(u32::from(stored)).checked_mul(u64::try_from(prefix_bytes).ok()?)?;
        let scaled = scaled / u64::try_from(term_bytes).ok()?;
        u32::try_from(scaled).ok().map(LexicalScore::from)
    }
}

/// A checked lexical result bound.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct LexicalTopK(usize);

impl LexicalTopK {
    /// Checks and creates a result bound before ranking work begins.
    pub const fn new(value: usize) -> Result<Self, LexicalTopKError> {
        if value > MAX_LEXICAL_TOP_K {
            Err(LexicalTopKError::TooLarge {
                max: MAX_LEXICAL_TOP_K,
                observed: value,
            })
        } else {
            Ok(Self(value))
        }
    }
}

impl From<LexicalTopK> for usize {
    fn from(top_k: LexicalTopK) -> Self {
        top_k.0
    }
}

impl Deref for LexicalTopK {
    type Target = usize;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// A rejected lexical result bound retains both operands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LexicalTopKError {
    /// The requested result bound exceeds the lexical admission bound.
    TooLarge {
        /// Maximum legal result bound.
        max: usize,
        /// Complete observed result bound.
        observed: usize,
    },
}

/// The immutable fact carried by one lexical term/document row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LexicalRowValue {
    /// The document is a member of the term with this deterministic score.
    Present(LexicalScore),
    /// The document is no longer a member of the term.
    Tombstone,
}

/// One borrowed lexical term/document row.
///
/// Rows supplied to [`LexicalSegment::new`] must be ordered by term bytes and
/// then by document identity.  The score is not part of segment order; it is
/// used only when a query ranks the rows for one term.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexicalRow<'bytes> {
    /// Canonical term bytes borrowed from the segment owner.
    pub term: &'bytes [u8],
    /// Stable package document identity.
    pub document: EntityDocumentId,
    /// Immutable membership fact for this term/document pair.
    pub value: LexicalRowValue,
}

/// The order-bearing projection of a lexical row.
///
/// Scores are deliberately absent: canonical lexical order depends only on term and document.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LexicalOrderKey<'bytes> {
    /// Canonical borrowed term bytes.
    pub term: &'bytes [u8],
    /// Immutable document identity paired with the term.
    pub document: EntityDocumentId,
}

impl<'bytes> From<LexicalRow<'bytes>> for LexicalOrderKey<'bytes> {
    fn from(row: LexicalRow<'bytes>) -> Self {
        Self {
            term: row.term,
            document: row.document,
        }
    }
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<LexicalRow<'_>>() == 64);

impl<'bytes> LexicalRow<'bytes> {
    /// Creates one borrowed lexical row without copying its term bytes.
    #[must_use]
    pub const fn new(term: &'bytes [u8], document: EntityDocumentId, score: LexicalScore) -> Self {
        Self {
            term,
            document,
            value: LexicalRowValue::Present(score),
        }
    }

    /// Creates an immutable deletion fact for one term/document membership.
    #[must_use]
    pub const fn tombstone(term: &'bytes [u8], document: EntityDocumentId) -> Self {
        Self {
            term,
            document,
            value: LexicalRowValue::Tombstone,
        }
    }

    /// Returns the fixed-width score for a present membership.
    #[must_use]
    pub const fn score(self) -> Option<LexicalScore> {
        match self.value {
            LexicalRowValue::Present(score) => Some(score),
            LexicalRowValue::Tombstone => None,
        }
    }

    /// Returns whether this row deletes a prior term/document membership.
    #[must_use]
    pub const fn is_tombstone(self) -> bool {
        matches!(self.value, LexicalRowValue::Tombstone)
    }
}

/// Closed selection grammar for one lexical query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LexicalMatch {
    /// Select only rows whose term equals the query bytes.
    Exact,
    /// Select the contiguous canonical range whose terms start with the query bytes.
    Prefix,
}

/// A borrowed lexical term query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexicalOperation<'query> {
    /// Query term bytes borrowed from the caller.
    pub term: &'query [u8],
    /// Closed matching policy.
    pub match_mode: LexicalMatch,
}

impl<'query> LexicalOperation<'query> {
    /// Creates an exact term query without copying its bytes.
    #[must_use]
    pub const fn new(term: &'query [u8]) -> Self {
        Self {
            term,
            match_mode: LexicalMatch::Exact,
        }
    }

    /// Creates a prefix term query without copying its bytes.
    #[must_use]
    pub const fn prefix(term: &'query [u8]) -> Self {
        Self {
            term,
            match_mode: LexicalMatch::Prefix,
        }
    }

    /// Applies this operation's deterministic relevance recipe to one stored row score.
    #[must_use]
    pub fn relevance(
        self,
        stored: LexicalScore,
        matched_term_bytes: usize,
    ) -> Option<LexicalScore> {
        match self.match_mode {
            LexicalMatch::Exact => Some(stored),
            LexicalMatch::Prefix => {
                LexicalScore::discounted(stored, self.term.len(), matched_term_bytes)
            }
        }
    }
}

/// One ranked lexical hit borrowed from its source row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexicalHit<'bytes> {
    /// Matching term bytes borrowed from the source row.
    pub term: &'bytes [u8],
    /// Stable package document identity.
    pub document: EntityDocumentId,
    /// Deterministic recipe score.
    pub score: LexicalScore,
}

/// One globally ranked lexical hit with immutable segment provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexicalSnapshotHit<'bytes> {
    /// Immutable source segment identity.
    pub segment: LexicalSegmentId,
    /// Matching term bytes borrowed from the source row.
    pub term: &'bytes [u8],
    /// Stable package document identity.
    pub document: EntityDocumentId,
    /// Deterministic recipe score.
    pub score: LexicalScore,
}

impl<'bytes> LexicalSnapshotHit<'bytes> {
    /// Creates one caller-owned placeholder or globally ranked result slot.
    #[must_use]
    pub const fn new(
        segment: LexicalSegmentId,
        term: &'bytes [u8],
        document: EntityDocumentId,
        score: LexicalScore,
    ) -> Self {
        Self {
            segment,
            term,
            document,
            score,
        }
    }
}

impl<'bytes> LexicalHit<'bytes> {
    /// Creates a caller-owned output placeholder or hit.
    #[must_use]
    pub const fn new(term: &'bytes [u8], document: EntityDocumentId, score: LexicalScore) -> Self {
        Self {
            term,
            document,
            score,
        }
    }
}

/// A lexical output capacity rejection that leaves caller output untouched.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexicalOutputError {
    /// Number of matching rows that must be returned.
    pub required: usize,
    /// Number of output slots supplied by the caller.
    pub available: usize,
}

/// A rejected lexical segment retains the offending rows and bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LexicalSegmentError<'bytes> {
    /// The segment would exceed its fixed admission bound.
    TooManyRows {
        /// Maximum admitted rows.
        max: usize,
        /// Complete observed row count.
        observed: usize,
    },
    /// The complete term payload exceeds the fixed byte budget.
    PayloadBytesLimit {
        /// Maximum admitted term bytes.
        max: usize,
        /// Complete observed term bytes.
        observed: usize,
    },
    /// Summing hostile term widths overflowed the platform counter.
    PayloadBytesOverflow {
        /// Row whose term crossed the representable byte range.
        index: usize,
    },
    /// A row did not follow term/document order.
    OutOfOrder {
        /// Zero-based offending row index.
        index: usize,
        /// Row immediately preceding the offending row.
        previous: LexicalOrderKey<'bytes>,
        /// Complete offending row.
        observed: LexicalOrderKey<'bytes>,
    },
    /// A term/document pair occurred more than once.
    DuplicateRow {
        /// Zero-based duplicate row index.
        index: usize,
        /// Repeated term bytes.
        term: &'bytes [u8],
        /// Repeated package document identity.
        document: EntityDocumentId,
    },
    /// A streaming verifier received a different count than its declared canonical lane.
    RowCount {
        /// Canonical row count declared before stream admission began.
        expected: usize,
        /// Rows admitted before completion or rejection.
        observed: usize,
    },
}

/// Incremental verifier for one canonical lexical-row lane.
///
/// It is the allocation-free semantic proof used when a durable reader must verify one packed
/// lane without first building a temporary row array.
pub struct LexicalSegmentVerifier<'bytes> {
    expected_rows: usize,
    admitted_rows: usize,
    payload_bytes: usize,
    previous: Option<LexicalRow<'bytes>>,
    hasher: ContentHasher<IndexLexicalSegmentDomain>,
}

impl<'bytes> LexicalSegmentVerifier<'bytes> {
    /// Starts a verifier for one declared canonical row count.
    #[allow(
        clippy::result_large_err,
        reason = "the existing lexical-order cause retains borrowed facts without allocating"
    )]
    pub fn new(expected_rows: usize) -> Result<Self, LexicalSegmentError<'bytes>> {
        if expected_rows > MAX_LEXICAL_ROWS {
            return Err(LexicalSegmentError::TooManyRows {
                max: MAX_LEXICAL_ROWS,
                observed: expected_rows,
            });
        }
        let mut hasher = ContentHasher::<IndexLexicalSegmentDomain>::new();
        hasher.write_record(&CanonicalRecord(*b"heart.lexical.rows.v2"));
        hasher.write_record(&CanonicalRecord((expected_rows as u64).to_le_bytes()));
        Ok(Self {
            expected_rows,
            admitted_rows: 0,
            payload_bytes: 0,
            previous: None,
            hasher,
        })
    }

    /// Admits one term/document row in canonical order and incorporates its existing identity grammar.
    #[allow(
        clippy::result_large_err,
        reason = "the closed lexical error retains two fixed-width document identities rather than allocating"
    )]
    pub fn admit(&mut self, row: LexicalRow<'bytes>) -> Result<(), LexicalSegmentError<'bytes>> {
        let observed_rows =
            self.admitted_rows
                .checked_add(1)
                .ok_or(LexicalSegmentError::RowCount {
                    expected: self.expected_rows,
                    observed: usize::MAX,
                })?;
        if observed_rows > self.expected_rows {
            return Err(LexicalSegmentError::RowCount {
                expected: self.expected_rows,
                observed: observed_rows,
            });
        }
        let payload_bytes = self.payload_bytes.checked_add(row.term.len()).ok_or(
            LexicalSegmentError::PayloadBytesOverflow {
                index: self.admitted_rows,
            },
        )?;
        if payload_bytes > MAX_LEXICAL_PAYLOAD_BYTES {
            return Err(LexicalSegmentError::PayloadBytesLimit {
                max: MAX_LEXICAL_PAYLOAD_BYTES,
                observed: payload_bytes,
            });
        }
        if let Some(previous) = self.previous {
            match row_order(previous, row) {
                Ordering::Less => {}
                Ordering::Equal => {
                    return Err(LexicalSegmentError::DuplicateRow {
                        index: self.admitted_rows,
                        term: row.term,
                        document: row.document,
                    });
                }
                Ordering::Greater => {
                    return Err(LexicalSegmentError::OutOfOrder {
                        index: self.admitted_rows,
                        previous: previous.into(),
                        observed: row.into(),
                    });
                }
            }
        }
        write_row(&mut self.hasher, row);
        self.admitted_rows = observed_rows;
        self.payload_bytes = payload_bytes;
        self.previous = Some(row);
        Ok(())
    }

    /// Finishes only after exactly the declared number of rows was admitted.
    #[allow(
        clippy::result_large_err,
        reason = "the existing lexical-order cause retains borrowed facts without allocating"
    )]
    pub fn finish(self) -> Result<LexicalSegmentId, LexicalSegmentError<'bytes>> {
        if self.admitted_rows != self.expected_rows {
            return Err(LexicalSegmentError::RowCount {
                expected: self.expected_rows,
                observed: self.admitted_rows,
            });
        }
        Ok(self.hasher.finalize())
    }
}

/// Immutable public facts of one validated lexical segment.
///
/// This view is read-only when reached through [`LexicalSegment`].  Constructing a view directly
/// does not create a [`LexicalSegment`] proof; only [`LexicalSegment::new`] can do that.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexicalSegmentView<'bytes> {
    /// Content identity derived from every admitted row.
    pub id: LexicalSegmentId,
    /// Validated rows borrowed from the segment owner.
    pub rows: &'bytes [LexicalRow<'bytes>],
}

/// One immutable lexical segment view over caller-owned rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct LexicalSegment<'bytes>(LexicalSegmentView<'bytes>);

fn row_order(left: LexicalRow<'_>, right: LexicalRow<'_>) -> Ordering {
    match left.term.cmp(right.term) {
        Ordering::Equal => left.document.cmp(&right.document),
        order => order,
    }
}
