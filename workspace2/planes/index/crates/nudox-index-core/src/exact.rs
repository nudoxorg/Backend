//! Borrowed immutable exact-key segments.

use nudox_index_vocab::ExactSegmentId;

/// Maximum number of rows admitted by one exact segment view.
pub const MAX_EXACT_ROWS: usize = 256;

/// One borrowed exact row. A tombstone is an immutable deletion fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactRow<'bytes> {
    key: &'bytes [u8],
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

    /// Borrows the canonical key bytes.
    #[must_use]
    pub const fn key(self) -> &'bytes [u8] {
        self.key
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
    key: &'query [u8],
}

impl<'query> ExactOperation<'query> {
    /// Creates an exact lookup without copying or hashing its key.
    #[must_use]
    pub const fn new(key: &'query [u8]) -> Self {
        Self { key }
    }

    /// Borrows the query key.
    #[must_use]
    pub const fn key(self) -> &'query [u8] {
        self.key
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
    /// The row bound is checked before ordering validation and before the segment bytes are
    /// hashed into the disposable typed identity. This keeps attacker-controlled row counts from
    /// amplifying duplicate, ordering, or identity work.
    pub fn new(
        segment_bytes: &[u8],
        rows: &'bytes [ExactRow<'bytes>],
    ) -> Result<Self, ExactSegmentError<'bytes>> {
        if rows.len() > MAX_EXACT_ROWS {
            return Err(ExactSegmentError::TooManyRows {
                max: MAX_EXACT_ROWS,
                observed: rows.len(),
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
            id: ExactSegmentId::from_canonical_bytes(segment_bytes),
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
