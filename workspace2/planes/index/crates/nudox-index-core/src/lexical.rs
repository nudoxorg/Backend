//! Borrowed immutable lexical segments.

use core::cmp::Ordering;

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
    hasher.write_record(&CanonicalRecord(*b"nudox.lexical.rows.v1"));
    hasher.write_record(&CanonicalRecord((rows.len() as u64).to_le_bytes()));
    for row in rows {
        write_bytes(&mut hasher, row.term());
        hasher.write_record(&CanonicalRecord(row.document().ordinal().to_le_bytes()));
        hasher.write_record(&CanonicalRecord(row.score().units().to_le_bytes()));
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

impl LexicalScore {
    /// Creates a score from its fixed-width recipe units.
    #[must_use]
    pub const fn new(units: u32) -> Self {
        Self(units)
    }

    /// Returns the fixed-width recipe units.
    #[must_use]
    pub const fn units(self) -> u32 {
        self.0
    }
}

/// A fixed-width stable document identity used across lexical segments in one snapshot.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct LexicalDocumentId(u32);

impl LexicalDocumentId {
    /// Creates a local document identity from its ordinal.
    #[must_use]
    pub const fn new(ordinal: u32) -> Self {
        Self(ordinal)
    }

    /// Returns the local document ordinal.
    #[must_use]
    pub const fn ordinal(self) -> u32 {
        self.0
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

    /// Returns the checked number of requested results.
    #[must_use]
    pub const fn limit(self) -> usize {
        self.0
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

/// One borrowed lexical term/document row.
///
/// Rows supplied to [`LexicalSegment::new`] must be ordered by term bytes and
/// then by document identity.  The score is not part of segment order; it is
/// used only when a query ranks the rows for one term.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexicalRow<'bytes> {
    term: &'bytes [u8],
    document: LexicalDocumentId,
    score: LexicalScore,
}

impl<'bytes> LexicalRow<'bytes> {
    /// Creates one borrowed lexical row without copying its term bytes.
    #[must_use]
    pub const fn new(term: &'bytes [u8], document: u32, score: LexicalScore) -> Self {
        Self {
            term,
            document: LexicalDocumentId::new(document),
            score,
        }
    }

    /// Borrows the row's term bytes.
    #[must_use]
    pub const fn term(self) -> &'bytes [u8] {
        self.term
    }

    /// Returns the row's local document identity.
    #[must_use]
    pub const fn document(self) -> LexicalDocumentId {
        self.document
    }

    /// Returns the row's fixed-width score.
    #[must_use]
    pub const fn score(self) -> LexicalScore {
        self.score
    }
}

/// A borrowed lexical term query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexicalOperation<'query> {
    term: &'query [u8],
}

impl<'query> LexicalOperation<'query> {
    /// Creates a term query without copying its bytes.
    #[must_use]
    pub const fn new(term: &'query [u8]) -> Self {
        Self { term }
    }

    /// Borrows the query term.
    #[must_use]
    pub const fn term(self) -> &'query [u8] {
        self.term
    }
}

/// One ranked lexical hit borrowed from its source row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexicalHit<'bytes> {
    term: &'bytes [u8],
    document: LexicalDocumentId,
    score: LexicalScore,
}

/// One globally ranked lexical hit with immutable segment provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexicalSnapshotHit<'bytes> {
    segment: LexicalSegmentId,
    term: &'bytes [u8],
    document: LexicalDocumentId,
    score: LexicalScore,
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

    /// Returns the immutable source segment.
    #[must_use]
    pub const fn segment(self) -> LexicalSegmentId {
        self.segment
    }

    /// Borrows the matching term bytes.
    #[must_use]
    pub const fn term(self) -> &'bytes [u8] {
        self.term
    }

    /// Returns the stable document identity.
    #[must_use]
    pub const fn document(self) -> LexicalDocumentId {
        self.document
    }

    /// Returns the fixed-width recipe score.
    #[must_use]
    pub const fn score(self) -> LexicalScore {
        self.score
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

    /// Borrows the hit's term bytes.
    #[must_use]
    pub const fn term(self) -> &'bytes [u8] {
        self.term
    }

    /// Returns the hit's local document identity.
    #[must_use]
    pub const fn document(self) -> LexicalDocumentId {
        self.document
    }

    /// Returns the hit's fixed-width score.
    #[must_use]
    pub const fn score(self) -> LexicalScore {
        self.score
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

/// One immutable lexical segment view over caller-owned rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexicalSegment<'bytes> {
    id: LexicalSegmentId,
    rows: &'bytes [LexicalRow<'bytes>],
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
                .checked_add(row.term().len())
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

        Ok(Self {
            id: segment_id(rows),
            rows,
        })
    }

    /// Returns this segment's typed identity.
    #[must_use]
    pub const fn id(&self) -> LexicalSegmentId {
        self.id
    }

    /// Borrows the validated, sorted rows without reparsing them.
    #[must_use]
    pub const fn rows(&self) -> &'bytes [LexicalRow<'bytes>] {
        self.rows
    }

    /// Finds all rows for one term as a borrowed contiguous range.
    #[must_use]
    pub fn lookup(&self, operation: LexicalOperation<'_>) -> Option<&'bytes [LexicalRow<'bytes>]> {
        let range = self.term_range(operation.term);
        if range.start == range.end {
            None
        } else {
            Some(&self.rows[range])
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
        let matches = &self.rows[range];
        let required = core::cmp::min(matches.len(), top_k.limit());
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
            let candidate = LexicalHit::new(row.term, row.document, row.score);
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
