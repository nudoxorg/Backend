//! Validates and searches one sealed lexical segment.

use core::cmp::Ordering;
use core::ops::Deref;

use super::{
    LexicalHit, LexicalMatch, LexicalOperation, LexicalOutputError, LexicalRow, LexicalSegment,
    LexicalSegmentError, LexicalSegmentView, LexicalTopK, MAX_LEXICAL_PAYLOAD_BYTES,
    MAX_LEXICAL_ROWS, row_order, segment_id,
};

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
    #[allow(
        clippy::result_large_err,
        reason = "cold canonical-order rejection retains both complete fixed-width global document addresses without heap allocation"
    )]
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
                        previous: previous.into(),
                        observed: observed.into(),
                    });
                }
            }
        }

        Ok(Self(LexicalSegmentView {
            id: segment_id(rows)?,
            rows,
        }))
    }

    /// Finds all rows selected by one operation as a borrowed contiguous range.
    #[must_use]
    pub fn lookup(&self, operation: LexicalOperation<'_>) -> Option<&'bytes [LexicalRow<'bytes>]> {
        let range = self.query_range(operation);
        if range.start == range.end {
            None
        } else {
            Some(&self.0.rows[range])
        }
    }

    /// Ranks up to the checked TopK rows selected by one operation into caller-owned output.
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
        let range = self.query_range(operation);
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
            let Some(stored) = row.score() else {
                continue;
            };
            let Some(score) = operation.relevance(stored, row.term.len()) else {
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

    fn query_range(&self, operation: LexicalOperation<'_>) -> core::ops::Range<usize> {
        match operation.match_mode {
            LexicalMatch::Exact => self.term_range(operation.term),
            LexicalMatch::Prefix => {
                let first = lower_bound(self.rows, operation.term);
                let mut last = first;
                while last < self.rows.len() && self.rows[last].term.starts_with(operation.term) {
                    last += 1;
                }
                first..last
            }
        }
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
