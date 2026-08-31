//! Borrowed immutable lexical segments.

use core::{cmp::Ordering, ops::Deref};

use nudox_id::{ContentHasher, FixedCanonicalRecord, IndexLexicalSegmentDomain};
use nudox_index_vocab::LexicalSegmentId;

/// Maximum number of rows admitted by one lexical segment view.
pub const MAX_LEXICAL_ROWS: usize = 256;

/// Maximum term bytes admitted by one lexical segment.
pub const MAX_LEXICAL_PAYLOAD_BYTES: usize = 65_536;

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

fn segment_id(rows: &[LexicalRow<'_>]) -> LexicalSegmentId {
    let mut hasher = ContentHasher::<IndexLexicalSegmentDomain>::new();
    hasher.write_record(&CanonicalRecord(*b"nudox.lexical.rows.v2"));
    hasher.write_record(&CanonicalRecord((rows.len() as u64).to_le_bytes()));
    for row in rows {
        write_bytes(&mut hasher, row.term);
        hasher.write_record(&CanonicalRecord(u32::from(row.document).to_le_bytes()));
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
    hasher.finalize()
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

/// A fixed-width stable document identity used across lexical segments in one snapshot.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct LexicalDocumentId(u32);

impl From<u32> for LexicalDocumentId {
    fn from(ordinal: u32) -> Self {
        Self(ordinal)
    }
}

impl From<LexicalDocumentId> for u32 {
    fn from(document: LexicalDocumentId) -> Self {
        document.0
    }
}

impl Deref for LexicalDocumentId {
    type Target = u32;

    fn deref(&self) -> &Self::Target {
        &self.0
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
    /// Stable local document identity.
    pub document: LexicalDocumentId,
    /// Immutable membership fact for this term/document pair.
    pub value: LexicalRowValue,
}

impl<'bytes> LexicalRow<'bytes> {
    /// Creates one borrowed lexical row without copying its term bytes.
    #[must_use]
    pub const fn new(term: &'bytes [u8], document: u32, score: LexicalScore) -> Self {
        Self {
            term,
            document: LexicalDocumentId(document),
            value: LexicalRowValue::Present(score),
        }
    }

    /// Creates an immutable deletion fact for one term/document membership.
    #[must_use]
    pub const fn tombstone(term: &'bytes [u8], document: u32) -> Self {
        Self {
            term,
            document: LexicalDocumentId(document),
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

/// A borrowed lexical term query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexicalOperation<'query> {
    /// Query term bytes borrowed from the caller.
    pub term: &'query [u8],
}

impl<'query> LexicalOperation<'query> {
    /// Creates a term query without copying its bytes.
    #[must_use]
    pub const fn new(term: &'query [u8]) -> Self {
        Self { term }
    }
}

/// One ranked lexical hit borrowed from its source row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexicalHit<'bytes> {
    /// Matching term bytes borrowed from the source row.
    pub term: &'bytes [u8],
    /// Stable local document identity.
    pub document: LexicalDocumentId,
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
    /// Stable local document identity.
    pub document: LexicalDocumentId,
    /// Deterministic recipe score.
    pub score: LexicalScore,
}

impl<'bytes> LexicalSnapshotHit<'bytes> {
    /// Creates one caller-owned placeholder or globally ranked result slot.
    #[must_use]
    pub const fn new(
        segment: LexicalSegmentId,
        term: &'bytes [u8],
        document: LexicalDocumentId,
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
    pub const fn new(term: &'bytes [u8], document: LexicalDocumentId, score: LexicalScore) -> Self {
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
        previous: LexicalRow<'bytes>,
        /// Complete offending row.
        observed: LexicalRow<'bytes>,
    },
    /// A term/document pair occurred more than once.
    DuplicateRow {
        /// Zero-based duplicate row index.
        index: usize,
        /// Repeated term bytes.
        term: &'bytes [u8],
        /// Repeated local document identity.
        document: LexicalDocumentId,
    },
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

impl<'bytes> Deref for LexicalSegment<'bytes> {
    type Target = LexicalSegmentView<'bytes>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'bytes> LexicalSegment<'bytes> {
    /// Validates and lends a sorted lexical segment without allocating or
    /// sorting input.
    ///
    /// Row and byte bounds are checked before ordering, duplicate, and identity work.
    /// This keeps an attacker-controlled row count from amplifying validation
    /// or canonical identity hashing.
    pub fn new(rows: &'bytes [LexicalRow<'bytes>]) -> Result<Self, LexicalSegmentError<'bytes>> {
        if rows.len() > MAX_LEXICAL_ROWS {
            return Err(LexicalSegmentError::TooManyRows {
                max: MAX_LEXICAL_ROWS,
                observed: rows.len(),
            });
        }

        let mut payload_bytes = 0_usize;
        for (index, row) in rows.iter().enumerate() {
            payload_bytes = payload_bytes
                .checked_add(row.term.len())
                .ok_or(LexicalSegmentError::PayloadBytesOverflow { index })?;
        }
        if payload_bytes > MAX_LEXICAL_PAYLOAD_BYTES {
            return Err(LexicalSegmentError::PayloadBytesLimit {
                max: MAX_LEXICAL_PAYLOAD_BYTES,
                observed: payload_bytes,
            });
        }

        for (index, pair) in rows.windows(2).enumerate() {
            let previous = pair[0];
            let observed = pair[1];
            match row_order(previous, observed) {
                Ordering::Less => {}
                Ordering::Equal => {
                    return Err(LexicalSegmentError::DuplicateRow {
                        index: index + 1,
                        term: observed.term,
                        document: observed.document,
                    });
                }
                Ordering::Greater => {
                    return Err(LexicalSegmentError::OutOfOrder {
                        index: index + 1,
                        previous,
                        observed,
                    });
                }
            }
        }

        Ok(Self(LexicalSegmentView {
            id: segment_id(rows),
            rows,
        }))
    }

    /// Finds all rows for one term as a borrowed contiguous range.
    #[must_use]
    pub fn lookup(&self, operation: LexicalOperation<'_>) -> Option<&'bytes [LexicalRow<'bytes>]> {
        let range = self.term_range(operation.term);
        if range.start == range.end {
            None
        } else {
            Some(&self.0.rows[range])
        }
    }

    /// Ranks up to the checked TopK rows for one term into caller-owned output.
    ///
    /// The capacity check happens before the first output write.  A short
    /// output therefore returns its complete requirement and remains byte-for-
    /// byte unchanged.  Hits are ordered by score descending and document
    /// identity ascending.
    pub fn rank(
        &self,
        operation: LexicalOperation<'_>,
        top_k: LexicalTopK,
        output: &mut [LexicalHit<'bytes>],
    ) -> Result<usize, LexicalOutputError> {
        let range = self.term_range(operation.term);
        let matches = &self.0.rows[range];
        let required = core::cmp::min(
            matches.iter().filter(|row| !row.is_tombstone()).count(),
            usize::from(top_k),
        );
        if output.len() < required {
            return Err(LexicalOutputError {
                required,
                available: output.len(),
            });
        }

        if required == 0 {
            return Ok(0);
        }

        let selected = &mut output[..required];
        let mut selected_len = 0;
        for row in matches.iter().copied() {
            let Some(score) = row.score() else {
                continue;
            };
            let candidate = LexicalHit::new(row.term, row.document, score);
            if selected_len < required {
                selected[selected_len] = candidate;
                selected_len += 1;
                selected[..selected_len].sort_unstable_by(rank_order);
            } else if rank_order(&candidate, &selected[required - 1]) == Ordering::Less {
                selected[required - 1] = candidate;
                selected.sort_unstable_by(rank_order);
            }
        }
        Ok(selected_len)
    }

    fn term_range(&self, term: &[u8]) -> core::ops::Range<usize> {
        let first = lower_bound(self.rows, term);
        let last = upper_bound(self.rows, term);
        first..last
    }
}

fn row_order(left: LexicalRow<'_>, right: LexicalRow<'_>) -> Ordering {
    match left.term.cmp(right.term) {
        Ordering::Equal => left.document.cmp(&right.document),
        order => order,
    }
}

fn rank_order(left: &LexicalHit<'_>, right: &LexicalHit<'_>) -> Ordering {
    right
        .score
        .cmp(&left.score)
        .then_with(|| left.document.cmp(&right.document))
}

fn lower_bound(rows: &[LexicalRow<'_>], term: &[u8]) -> usize {
    let mut left = 0;
    let mut right = rows.len();
    while left < right {
        let middle = left + (right - left) / 2;
        if rows[middle].term < term {
            left = middle + 1;
        } else {
            right = middle;
        }
    }
    left
}

fn upper_bound(rows: &[LexicalRow<'_>], term: &[u8]) -> usize {
    let mut left = 0;
    let mut right = rows.len();
    while left < right {
        let middle = left + (right - left) / 2;
        if rows[middle].term <= term {
            left = middle + 1;
        } else {
            right = middle;
        }
    }
    left
}
