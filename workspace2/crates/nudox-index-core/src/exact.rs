//! Borrowed immutable exact-key segments.

use nudox_id::{ContentHasher, FixedCanonicalRecord, IndexExactSegmentDomain};
use nudox_index_vocab::ExactSegmentId;

/// Maximum number of rows admitted by one exact segment view.
pub const MAX_EXACT_ROWS: usize = 256;

/// Maximum key and value bytes admitted by one exact segment.
pub const MAX_EXACT_PAYLOAD_BYTES: usize = 65_536;

const CANONICAL_CHUNK_BYTES: usize = 32;

struct CanonicalRecord<const BYTES: usize>([u8; BYTES]);

impl<const BYTES: usize> FixedCanonicalRecord<BYTES> for CanonicalRecord<BYTES> {
    fn canonical_bytes(&self) -> &[u8; BYTES] {
        &self.0
    }
}

fn write_bytes(hasher: &mut ContentHasher<IndexExactSegmentDomain>, bytes: &[u8]) {
    hasher.write_record(&CanonicalRecord((bytes.len() as u64).to_le_bytes()));
    for chunk in bytes.chunks(CANONICAL_CHUNK_BYTES) {
        let mut record = [0_u8; CANONICAL_CHUNK_BYTES + 1];
        record[0] = chunk.len() as u8;
        record[1..=chunk.len()].copy_from_slice(chunk);
        hasher.write_record(&CanonicalRecord(record));
    }
}

fn segment_id(rows: &[ExactRow<'_>]) -> ExactSegmentId {
    let mut hasher = ContentHasher::<IndexExactSegmentDomain>::new();
    hasher.write_record(&CanonicalRecord(*b"nudox.exact.rows.v1"));
    hasher.write_record(&CanonicalRecord((rows.len() as u64).to_le_bytes()));
    for row in rows {
        write_bytes(&mut hasher, row.key);
        match row.value_bytes() {
            Some(value) => {
                hasher.write_record(&CanonicalRecord([1]));
                write_bytes(&mut hasher, value);
            }
            None => hasher.write_record(&CanonicalRecord([0])),
        }
    }
    hasher.finalize()
}

/// One borrowed exact row. A tombstone is an immutable deletion fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactRow<'bytes> {
    /// Canonical key bytes borrowed from the segment owner.
    pub key: &'bytes [u8],
    value: ExactValue<'bytes>,
}

impl<'bytes> ExactRow<'bytes> {
    /// Creates one present key/value row without copying either byte slice.
    #[must_use]
    pub const fn present(key: &'bytes [u8], value: &'bytes [u8]) -> Self {
        Self {
            key,
            value: ExactValue::Present(value),
        }
    }

    /// Creates one deletion row without copying the key.
    #[must_use]
    pub const fn tombstone(key: &'bytes [u8]) -> Self {
        Self {
            key,
            value: ExactValue::Tombstone,
        }
    }

    /// Borrows a present value, or returns `None` for a tombstone.
    #[must_use]
    pub const fn value_bytes(self) -> Option<&'bytes [u8]> {
        match self.value {
            ExactValue::Present(value) => Some(value),
            ExactValue::Tombstone => None,
        }
    }

    /// Reports whether this row is an immutable deletion fact.
    #[must_use]
    pub const fn is_tombstone(self) -> bool {
        matches!(self.value, ExactValue::Tombstone)
    }
}

/// The value state of an immutable exact row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExactValue<'bytes> {
    /// A present value borrowed from the segment owner.
    Present(&'bytes [u8]),
    /// A deletion marker for this key in this segment revision.
    Tombstone,
}

/// A bounded exact lookup over a borrowed query key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactOperation<'query> {
    /// Query key borrowed from the caller.
    pub key: &'query [u8],
}

impl<'query> ExactOperation<'query> {
    /// Creates an exact lookup without copying or hashing its key.
    #[must_use]
    pub const fn new(key: &'query [u8]) -> Self {
        Self { key }
    }
}

/// A rejected exact segment retains the offending row and bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExactSegmentError<'bytes> {
    /// The segment would exceed its fixed admission bound.
    TooManyRows {
        /// Maximum admitted rows.
        max: usize,
        /// Complete observed row count.
        observed: usize,
    },
    /// The complete key/value payload exceeds the fixed byte budget.
    PayloadBytesLimit {
        /// Maximum admitted payload bytes.
        max: usize,
        /// Complete observed payload bytes.
        observed: usize,
    },
    /// Summing hostile key/value widths overflowed the platform counter.
    PayloadBytesOverflow {
        /// Row whose value or key crossed the representable byte range.
        index: usize,
    },
    /// A key did not follow the canonical byte order.
    OutOfOrder {
        /// Zero-based offending row index.
        index: usize,
        /// Key immediately preceding the offending row.
        previous: &'bytes [u8],
        /// Complete offending key.
        observed: &'bytes [u8],
    },
    /// A key occurred more than once in one immutable segment.
    DuplicateKey {
        /// Zero-based duplicate row index.
        index: usize,
        /// Complete duplicate key.
        key: &'bytes [u8],
    },
}

/// One immutable exact segment view over caller-owned rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactSegment<'bytes> {
    id: ExactSegmentId,
    rows: &'bytes [ExactRow<'bytes>],
}

impl<'bytes> ExactSegment<'bytes> {
    /// Validates and lends a sorted exact segment without allocating or sorting input.
    ///
    /// Row and byte bounds are checked before ordering and identity work. The identity is derived
    /// from the same admitted rows this view queries, so it cannot label unrelated backing data.
    pub fn new(rows: &'bytes [ExactRow<'bytes>]) -> Result<Self, ExactSegmentError<'bytes>> {
        if rows.len() > MAX_EXACT_ROWS {
            return Err(ExactSegmentError::TooManyRows {
                max: MAX_EXACT_ROWS,
                observed: rows.len(),
            });
        }

        let mut payload_bytes = 0_usize;
        for (index, row) in rows.iter().enumerate() {
            payload_bytes = payload_bytes
                .checked_add(row.key.len())
                .and_then(|total| {
                    row.value_bytes()
                        .map_or(Some(total), |value| total.checked_add(value.len()))
                })
                .ok_or(ExactSegmentError::PayloadBytesOverflow { index })?;
        }
        if payload_bytes > MAX_EXACT_PAYLOAD_BYTES {
            return Err(ExactSegmentError::PayloadBytesLimit {
                max: MAX_EXACT_PAYLOAD_BYTES,
                observed: payload_bytes,
            });
        }

        for (index, pair) in rows.windows(2).enumerate() {
            let previous = pair[0].key;
            let observed = pair[1].key;
            match previous.cmp(observed) {
                core::cmp::Ordering::Less => {}
                core::cmp::Ordering::Equal => {
                    return Err(ExactSegmentError::DuplicateKey {
                        index: index + 1,
                        key: observed,
                    });
                }
                core::cmp::Ordering::Greater => {
                    return Err(ExactSegmentError::OutOfOrder {
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
    pub const fn id(&self) -> ExactSegmentId {
        self.id
    }

    /// Borrows the validated, sorted rows without reparsing them.
    #[must_use]
    pub const fn rows(&self) -> &'bytes [ExactRow<'bytes>] {
        self.rows
    }

    /// Finds one key with logarithmic comparisons and returns its borrowed row.
    #[must_use]
    pub fn lookup(&self, operation: ExactOperation<'_>) -> Option<&'bytes ExactRow<'bytes>> {
        self.rows
            .binary_search_by(|row| row.key.cmp(operation.key))
            .ok()
            .map(|index| &self.rows[index])
    }
}
