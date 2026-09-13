//! Defines exact behavior for `server-index-core`, whose purpose is to define immutable index documents, segments, and snapshot identities.
//! This module owns the exact invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Borrowed immutable exact-key segments.

use core::ops::Deref;

use backend_version::{ContentHasher, FixedCanonicalRecord, IndexExactSegmentDomain};
use server_index_vocabulary::ExactSegmentId;

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

fn segment_id<'bytes>(
    rows: &[ExactRow<'bytes>],
) -> Result<ExactSegmentId, ExactSegmentError<'bytes>> {
    let mut verifier = ExactSegmentVerifier::new(rows.len())?;
    for row in rows {
        verifier.admit(*row)?;
    }
    verifier.finish()
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
    /// A streaming verifier received a different count than its declared canonical lane.
    RowCount {
        /// Canonical row count declared before stream admission began.
        expected: usize,
        /// Rows admitted before completion or rejection.
        observed: usize,
    },
}

/// Incremental verifier for one canonical exact-row lane.
///
/// It reuses the exact segment's production identity grammar while holding only counters, a
/// borrowed predecessor, and the hasher. This is for durable streaming readers that must prove
/// semantic identity without materializing a row array.
pub struct ExactSegmentVerifier<'bytes> {
    expected_rows: usize,
    admitted_rows: usize,
    payload_bytes: usize,
    previous: Option<&'bytes [u8]>,
    hasher: ContentHasher<IndexExactSegmentDomain>,
}

impl<'bytes> ExactSegmentVerifier<'bytes> {
    /// Starts a verifier for one declared canonical row count.
    pub fn new(expected_rows: usize) -> Result<Self, ExactSegmentError<'bytes>> {
        if expected_rows > MAX_EXACT_ROWS {
            return Err(ExactSegmentError::TooManyRows {
                max: MAX_EXACT_ROWS,
                observed: expected_rows,
            });
        }
        let mut hasher = ContentHasher::<IndexExactSegmentDomain>::new();
        hasher.write_record(&CanonicalRecord(*b"heart.exact.rows.v1"));
        hasher.write_record(&CanonicalRecord((expected_rows as u64).to_le_bytes()));
        Ok(Self {
            expected_rows,
            admitted_rows: 0,
            payload_bytes: 0,
            previous: None,
            hasher,
        })
    }

    /// Admits one row in canonical order and incorporates its existing segment grammar.
    pub fn admit(&mut self, row: ExactRow<'bytes>) -> Result<(), ExactSegmentError<'bytes>> {
        let observed_rows =
            self.admitted_rows
                .checked_add(1)
                .ok_or(ExactSegmentError::RowCount {
                    expected: self.expected_rows,
                    observed: usize::MAX,
                })?;
        if observed_rows > self.expected_rows {
            return Err(ExactSegmentError::RowCount {
                expected: self.expected_rows,
                observed: observed_rows,
            });
        }
        let row_payload = row
            .key
            .len()
            .checked_add(row.value_bytes().map_or(0, <[u8]>::len))
            .ok_or(ExactSegmentError::PayloadBytesOverflow {
                index: self.admitted_rows,
            })?;
        let payload_bytes = self.payload_bytes.checked_add(row_payload).ok_or(
            ExactSegmentError::PayloadBytesOverflow {
                index: self.admitted_rows,
            },
        )?;
        if payload_bytes > MAX_EXACT_PAYLOAD_BYTES {
            return Err(ExactSegmentError::PayloadBytesLimit {
                max: MAX_EXACT_PAYLOAD_BYTES,
                observed: payload_bytes,
            });
        }
        if let Some(previous) = self.previous {
            match previous.cmp(row.key) {
                core::cmp::Ordering::Less => {}
                core::cmp::Ordering::Equal => {
                    return Err(ExactSegmentError::DuplicateKey {
                        index: self.admitted_rows,
                        key: row.key,
                    });
                }
                core::cmp::Ordering::Greater => {
                    return Err(ExactSegmentError::OutOfOrder {
                        index: self.admitted_rows,
                        previous,
                        observed: row.key,
                    });
                }
            }
        }
        write_bytes(&mut self.hasher, row.key);
        match row.value_bytes() {
            Some(value) => {
                self.hasher.write_record(&CanonicalRecord([1]));
                write_bytes(&mut self.hasher, value);
            }
            None => self.hasher.write_record(&CanonicalRecord([0])),
        }
        self.admitted_rows = observed_rows;
        self.payload_bytes = payload_bytes;
        self.previous = Some(row.key);
        Ok(())
    }

    /// Finishes only after exactly the declared number of rows was admitted.
    pub fn finish(self) -> Result<ExactSegmentId, ExactSegmentError<'bytes>> {
        if self.admitted_rows != self.expected_rows {
            return Err(ExactSegmentError::RowCount {
                expected: self.expected_rows,
                observed: self.admitted_rows,
            });
        }
        Ok(self.hasher.finalize())
    }
}

/// Immutable public facts of one validated exact segment.
///
/// This view is read-only when reached through [`ExactSegment`].  Constructing a view directly
/// does not create an [`ExactSegment`] proof; only [`ExactSegment::new`] can do that.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactSegmentView<'bytes> {
    /// Content identity derived from every admitted row.
    pub id: ExactSegmentId,
    /// Validated sorted rows borrowed from the segment owner.
    pub rows: &'bytes [ExactRow<'bytes>],
}

/// One immutable exact segment view over caller-owned rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct ExactSegment<'bytes>(ExactSegmentView<'bytes>);

impl<'bytes> Deref for ExactSegment<'bytes> {
    type Target = ExactSegmentView<'bytes>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
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

        Ok(Self(ExactSegmentView {
            id: segment_id(rows)?,
            rows,
        }))
    }

    /// Finds one key with logarithmic comparisons and returns its borrowed row.
    #[must_use]
    pub fn lookup(&self, operation: ExactOperation<'_>) -> Option<&'bytes ExactRow<'bytes>> {
        self.0
            .rows
            .binary_search_by(|row| row.key.cmp(operation.key))
            .ok()
            .map(|index| &self.0.rows[index])
    }
}
