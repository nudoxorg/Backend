//! Implementation of [`SparseCoverage`] interval operations.

use super::*;

impl SparseCoverage {
    /// Creates empty coverage with a fixed interval budget.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::CoverageLimit`] for a zero budget.
    pub fn new(max_ranges: usize) -> Result<Self, ReplicationError> {
        if max_ranges == 0 {
            return Err(ReplicationError::CoverageLimit);
        }
        Ok(Self {
            ranges: Vec::new(),
            max_ranges,
        })
    }
    /// Creates coverage from intervals, sorting and coalescing them.
    ///
    /// # Errors
    ///
    /// Returns a coverage or range error when an interval overflows or the
    /// interval budget is exceeded.
    pub fn from_ranges(
        ranges: impl IntoIterator<Item = ByteRange>,
        max_ranges: usize,
    ) -> Result<Self, ReplicationError> {
        let mut coverage = Self::new(max_ranges)?;
        for range in ranges {
            coverage.insert(range)?;
        }
        Ok(coverage)
    }
    /// Returns a complete interval covering an object of `len` bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Range`] for a zero-length object or
    /// [`ReplicationError::Overflow`] when the requested length cannot be
    /// represented as an interval.
    pub fn complete(len: u64) -> Result<Self, ReplicationError> {
        if len == 0 {
            return Err(ReplicationError::Range);
        }
        let mut coverage = Self::new(1)?;
        coverage.insert(ByteRange::new(0, len)?)?;
        Ok(coverage)
    }
    /// Adds an interval and coalesces touching neighbors.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::CoverageLimit`] when another sparse
    /// interval cannot fit in the budget.
    pub fn insert(&mut self, range: ByteRange) -> Result<(), ReplicationError> {
        if range.len == 0 {
            return Ok(());
        }
        range.end()?;
        // Locate and validate the complete splice before mutating the compact
        // interval vector.  Earlier versions cloned every retained range for
        // each received chunk merely to obtain rollback.  All fallible work
        // now happens during preparation, so commit is one infallible splice.
        let start = self.ranges.partition_point(|covered| {
            covered
                .start
                .checked_add(covered.len)
                .is_some_and(|end| end < range.start)
        });
        let mut merged = range;
        let mut end = start;
        while end < self.ranges.len() {
            if self.ranges[end].touches(merged) {
                merged = self.ranges[end].merge(merged)?;
                end = end.checked_add(1).ok_or(ReplicationError::Overflow)?;
            } else if self.ranges[end].start > merged.end()? {
                break;
            } else {
                end = end.checked_add(1).ok_or(ReplicationError::Overflow)?;
            }
        }
        let removed = end.checked_sub(start).ok_or(ReplicationError::Overflow)?;
        let retained = self
            .ranges
            .len()
            .checked_sub(removed)
            .ok_or(ReplicationError::Overflow)?;
        if retained >= self.max_ranges {
            return Err(ReplicationError::CoverageLimit);
        }
        self.ranges.splice(start..end, core::iter::once(merged));
        Ok(())
    }
    /// Returns the coalesced intervals.
    #[must_use]
    pub fn ranges(&self) -> &[ByteRange] {
        &self.ranges
    }

    /// Returns the checked logical bytes retained by this coverage value.
    /// The count includes the inline record and the vector's allocated range
    /// storage, so transport owners can charge retained capacity instead of
    /// a range-count estimate.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Overflow`] when the allocation-size
    /// calculation cannot be represented by the platform `usize`.
    pub fn retained_size(&self) -> Result<usize, ReplicationError> {
        size_of::<Self>()
            .checked_add(self.retained_payload_size()?)
            .ok_or(ReplicationError::Overflow)
    }

    /// Returns only heap bytes allocated for the range vector.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Overflow`] when the allocation-size
    /// calculation cannot be represented by the platform `usize`.
    pub fn retained_payload_size(&self) -> Result<usize, ReplicationError> {
        self.ranges
            .capacity()
            .checked_mul(size_of::<ByteRange>())
            .ok_or(ReplicationError::Overflow)
    }
    /// Returns the number of covered bytes.
    #[must_use]
    pub fn covered_bytes(&self) -> u64 {
        self.checked_covered_bytes().unwrap_or(u64::MAX)
    }
    /// Returns the covered-byte sum with checked arithmetic.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Overflow`] if disjoint valid intervals
    /// would exceed the width of the public byte-count type.
    pub fn checked_covered_bytes(&self) -> Result<u64, ReplicationError> {
        self.ranges.iter().try_fold(0u64, |total, range| {
            total
                .checked_add(range.len)
                .ok_or(ReplicationError::Overflow)
        })
    }
    /// Returns whether the interval is fully covered.
    #[must_use]
    pub fn covers(&self, range: ByteRange) -> bool {
        let Some(range_end) = range.start.checked_add(range.len) else {
            return false;
        };
        self.ranges.iter().any(|covered| {
            covered.start <= range.start
                && covered
                    .start
                    .checked_add(covered.len)
                    .is_some_and(|covered_end| covered_end >= range_end)
        })
    }
    /// Returns whether any covered byte overlaps the supplied interval.
    #[must_use]
    pub fn overlaps(&self, range: ByteRange) -> bool {
        let Some(range_end) = range.start.checked_add(range.len) else {
            return false;
        };
        self.ranges.iter().any(|covered| {
            covered.start < range_end
                && covered
                    .start
                    .checked_add(covered.len)
                    .is_some_and(|covered_end| range.start < covered_end)
        })
    }
    /// Returns whether every byte in a nonempty object is covered. Empty
    /// coverage never claims completeness.
    #[must_use]
    pub fn is_complete(&self, object_len: u64) -> bool {
        object_len != 0
            && self.covers(ByteRange {
                start: 0,
                len: object_len,
            })
    }
    /// Computes missing intervals up to a caller-supplied range budget.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::CoverageLimit`] when the result needs more
    /// intervals than allowed, or [`ReplicationError::Overflow`] for a bad
    /// interval end.
    pub fn missing(
        &self,
        object_len: u64,
        max_ranges: usize,
    ) -> Result<Vec<ByteRange>, ReplicationError> {
        if max_ranges == 0 {
            return Err(ReplicationError::CoverageLimit);
        }
        for range in &self.ranges {
            if range.start > object_len || range.end()? > object_len {
                return Err(ReplicationError::Range);
            }
        }
        let mut result = Vec::new();
        let mut cursor = 0;
        for range in &self.ranges {
            if range.start > cursor {
                result.push(ByteRange::new(cursor, range.start - cursor)?);
            }
            cursor = cursor.max(range.end()?);
        }
        if cursor < object_len {
            result.push(ByteRange::new(cursor, object_len - cursor)?);
        }
        if result.len() > max_ranges {
            return Err(ReplicationError::CoverageLimit);
        }
        Ok(result)
    }
    /// Returns the interval budget used by this coverage value.
    #[must_use]
    pub const fn max_ranges(&self) -> usize {
        self.max_ranges
    }
}
