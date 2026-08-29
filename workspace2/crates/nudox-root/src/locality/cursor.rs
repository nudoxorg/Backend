//! Forward-only sparse locality traversal over canonical root rows.

use super::{Locality, LocalityReadError, RowIndex, ValidatedLocality, artifact::LaneTable};

/// Work evidence emitted by one canonical locality scan.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LocalityScanWork {
    /// Canonical rows composed by this scan.
    pub rows: usize,
    /// Comparisons against the next sparse exception row.
    pub sparse_comparisons: usize,
}

/// Work evidence for one random composed locality lookup.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LocalityLookupWork {
    /// Number of sparse exception-set binary searches issued by the lookup.
    pub route_binary_searches: usize,
    /// Sparse-row comparisons made by those binary searches.
    pub route_comparisons: usize,
    /// Fixed membership words popcounted by random rank projections.
    pub rank_word_popcounts: u32,
}

/// Monotone sequential cursor. Its four payload ordinals advance alongside
/// exception rows, so scans perform no random rank query.
pub(crate) struct LocalityCursor<'locality, DomainTag> {
    locality: &'locality ValidatedLocality<'locality, DomainTag>,
    lanes: LaneTable,
    next_exception: u32,
    next_promise: u32,
    next_overlay: u32,
    next_present: u32,
}

impl<'locality, DomainTag> LocalityCursor<'locality, DomainTag> {
    pub(super) const fn new(locality: &'locality ValidatedLocality<'locality, DomainTag>) -> Self {
        Self {
            locality,
            lanes: locality.lane_table(),
            next_exception: 0,
            next_promise: 0,
            next_overlay: 0,
            next_present: 0,
        }
    }

    pub(super) fn locality_without_work(
        &mut self,
        row: RowIndex,
    ) -> Result<Locality<DomainTag>, LocalityReadError> {
        while self.next_exception < self.exception_count() {
            match self
                .locality
                .exception_row(self.lanes, self.next_exception)
                .cmp(&row.compact())
            {
                core::cmp::Ordering::Less => self.skip_exception(),
                core::cmp::Ordering::Equal => return self.take_exception(),
                core::cmp::Ordering::Greater => return Ok(Locality::Resident),
            }
        }
        Ok(Locality::Resident)
    }

    pub(super) fn locality_at(
        &mut self,
        row: RowIndex,
        work: &mut LocalityScanWork,
    ) -> Result<Locality<DomainTag>, LocalityReadError> {
        while self.next_exception < self.exception_count() {
            work.sparse_comparisons += 1;
            match self
                .locality
                .exception_row(self.lanes, self.next_exception)
                .cmp(&row.compact())
            {
                core::cmp::Ordering::Less => self.skip_exception(),
                core::cmp::Ordering::Equal => return self.take_exception(),
                core::cmp::Ordering::Greater => return Ok(Locality::Resident),
            }
        }
        Ok(Locality::Resident)
    }

    const fn exception_count(&self) -> u32 {
        self.locality.exception_count()
    }

    fn skip_exception(&mut self) {
        if self
            .locality
            .cursor_is_promise(self.lanes, self.next_exception)
        {
            self.next_promise += 1;
        } else {
            if self
                .locality
                .cursor_overlay_is_present(self.lanes, self.next_overlay)
            {
                self.next_present += 1;
            }
            self.next_overlay += 1;
        }
        self.next_exception += 1;
    }

    fn take_exception(&mut self) -> Result<Locality<DomainTag>, LocalityReadError> {
        let locality = if self
            .locality
            .cursor_is_promise(self.lanes, self.next_exception)
        {
            let locality = self
                .locality
                .cursor_promise(self.lanes, self.next_promise)
                .map(Locality::Promised);
            self.next_promise += 1;
            locality
        } else if self
            .locality
            .cursor_overlay_is_present(self.lanes, self.next_overlay)
        {
            let locality = self
                .locality
                .cursor_overlay_present(self.lanes, self.next_present);
            self.next_present += 1;
            self.next_overlay += 1;
            locality
        } else {
            self.next_overlay += 1;
            Ok(self.locality.cursor_overlay_absent(self.lanes))
        };
        self.next_exception += 1;
        locality
    }
}
