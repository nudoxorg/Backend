//! Defines closure behavior for `heart-root`, whose purpose is to construct and validate immutable generation roots and locality metadata.
//! This module owns the closure invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use alloc::vec::Vec;
use core::ops::Deref;

use backend_version::observe::Probe;

use crate::entry::{EntryRange, RootEntry};
use crate::packed::{GenerationRoot, RowIndex};
use thiserror::Error;

/// Explicit caller-owned selection marks and index evidence.
///
/// Marks use generations rather than a per-plan clear. The index list contains
/// exactly selected entries, so a narrow projection touches its range and
/// ancestor chains rather than making repeated corpus-wide selection passes.
pub struct ClosureScratch {
    marks: Vec<u32>,
    selected_indices: Vec<RowIndex>,
    pub(crate) selected_count: u32,
    epoch: u32,
    facts: ClosureScratchFacts,
}

/// Immutable allocation/work evidence exposed by dereferencing selection scratch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClosureScratchFacts {
    /// Exact root-entry capacity reserved before selection begins.
    pub capacity: usize,
    /// Work performed by the latest selected closure.
    pub work: SelectionWork,
}

impl Deref for ClosureScratch {
    type Target = ClosureScratchFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

/// Exact descriptor-coordinate work performed by the latest selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SelectionWork {
    /// Rows directly selected by the requested projection.
    pub projected_rows: usize,
    /// Parent edges followed while adding ancestor closure.
    pub ancestor_edges: usize,
    /// Canonical-key comparisons spent resolving borrowed parent coordinates.
    ///
    /// Owned roots retain parent coordinates and therefore report zero. Borrowed
    /// canonical roots trade retained parent metadata for binary-search work and
    /// report that work explicitly instead of hiding it behind the edge count.
    pub parent_search_comparisons: usize,
}

/// One completed closure selection, with no key or descriptor cardinality.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RootProbeEvent {
    /// Aggregate selected descriptor count.
    pub selected_rows: usize,
    /// Aggregate range/ancestor work from that one selection.
    pub work: SelectionWork,
}

impl ClosureScratch {
    /// Allocates reusable scratch for a declared maximum root entry count.
    ///
    /// # Errors
    ///
    /// Returns the allocator cause before retaining any partially initialized
    /// scratch state.
    pub fn new(entry_capacity: usize) -> Result<Self, alloc::collections::TryReserveError> {
        let mut marks = Vec::new();
        marks.try_reserve_exact(entry_capacity)?;
        marks.resize(entry_capacity, 0);
        let mut selected_indices = Vec::new();
        selected_indices.try_reserve_exact(entry_capacity)?;
        Ok(Self {
            marks,
            selected_indices,
            selected_count: 0,
            epoch: 0,
            facts: ClosureScratchFacts {
                capacity: entry_capacity,
                work: SelectionWork {
                    projected_rows: 0,
                    ancestor_edges: 0,
                    parent_search_comparisons: 0,
                },
            },
        })
    }

    pub(crate) fn begin_selection(&mut self) {
        self.selected_indices.clear();
        self.selected_count = 0;
        self.facts.work = SelectionWork {
            projected_rows: 0,
            ancestor_edges: 0,
            parent_search_comparisons: 0,
        };
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.marks.fill(0);
            self.epoch = 1;
        }
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "select_closure checked scratch capacity against this exact root before every private RowIndex mark access"
    )]
    pub(crate) fn mark(&mut self, index: RowIndex) {
        if self.marks[index.array_index()] != self.epoch {
            self.marks[index.array_index()] = self.epoch;
            self.selected_indices.push(index);
            // A selection is a subset of the root builder's compact row bound.
            self.selected_count += 1;
        }
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "borrowed and owned selectors preflight this exact root against scratch capacity before querying a validated row coordinate"
    )]
    pub(crate) fn is_marked(&self, index: RowIndex) -> bool {
        self.marks[index.array_index()] == self.epoch
    }

    pub(crate) const fn record_projected_row(&mut self) {
        self.facts.work.projected_rows += 1;
    }

    pub(crate) const fn record_ancestor_edge(&mut self) {
        self.facts.work.ancestor_edges += 1;
    }

    pub(crate) const fn record_parent_search_comparisons(&mut self, comparisons: usize) {
        self.facts.work.parent_search_comparisons += comparisons;
    }

    pub(crate) fn selected_indices(&self) -> &[RowIndex] {
        &self.selected_indices
    }

    pub(crate) fn sort_selected_indices(&mut self) {
        self.selected_indices.sort_unstable();
    }
}

/// Explicit projected-closure selection failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ClosureError {
    /// Caller scratch cannot cover root length.
    #[error("closure scratch has {available} entries but needs {required}")]
    ScratchTooSmall {
        /// Required entries.
        required: usize,
        /// Scratch capacity.
        available: usize,
    },
}

/// Borrowing selected-closure view; iteration does not allocate.
pub struct SelectedClosure<'root, DomainTag> {
    root: &'root GenerationRoot<DomainTag>,
    selected_indices: &'root [RowIndex],
    pub(crate) compact_count: u32,
    work: SelectionWork,
}

impl<DomainTag> Copy for SelectedClosure<'_, DomainTag> {}

impl<DomainTag> Clone for SelectedClosure<'_, DomainTag> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<DomainTag> Deref for SelectedClosure<'_, DomainTag> {
    type Target = SelectionWork;

    fn deref(&self) -> &Self::Target {
        &self.work
    }
}

impl<DomainTag> SelectedClosure<'_, DomainTag> {
    /// Iterates reconstructed selected entries in canonical key order.
    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = RootEntry<DomainTag>> + '_ {
        self.rows().map(|(_, entry)| entry)
    }

    pub(crate) fn rows(
        &self,
    ) -> impl ExactSizeIterator<Item = (RowIndex, RootEntry<DomainTag>)> + '_ {
        self.selected_indices
            .iter()
            .map(|index| (*index, self.root.entry_at(*index)))
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "only SelectedOrdinals constructed by this exact SelectedGeneration call this private selected-array access"
    )]
    pub(crate) fn row_at_position(&self, position: usize) -> (RowIndex, RootEntry<DomainTag>) {
        let index = self.selected_indices[position];
        (index, self.root.entry_at(index))
    }

    /// Returns selected descriptor count without rescanning root state.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.selected_indices.len()
    }

    /// Returns whether no entry is selected.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.selected_indices.is_empty()
    }
}

impl<DomainTag> GenerationRoot<DomainTag> {
    /// Selects a range and all ancestors using caller-owned reusable scratch.
    ///
    /// # Errors
    ///
    /// Returns [`ClosureError::ScratchTooSmall`] before changing selection
    /// marks when the caller did not provide capacity for the whole root.
    pub fn select_closure<'root>(
        &'root self,
        range: Option<EntryRange>,
        scratch: &'root mut ClosureScratch,
    ) -> Result<SelectedClosure<'root, DomainTag>, ClosureError> {
        if scratch.capacity < self.len() {
            return Err(ClosureError::ScratchTooSmall {
                required: self.len(),
                available: scratch.capacity,
            });
        }
        scratch.begin_selection();
        match range {
            None => {
                for row in self.canonical_rows() {
                    scratch.facts.work.projected_rows += 1;
                    scratch.mark(row.index);
                }
            }
            Some(range) => {
                for row in self.projected_rows(range) {
                    scratch.facts.work.projected_rows += 1;
                    mark_ancestors(self, scratch, row.index);
                }
                scratch.selected_indices.sort_unstable();
            }
        }
        Ok(SelectedClosure {
            root: self,
            selected_indices: &scratch.selected_indices,
            compact_count: scratch.selected_count,
            work: scratch.work,
        })
    }

    /// Selects a closure and lazily records its one aggregate operation event.
    ///
    /// A no-op probe does not construct the event, so ordinary selection has
    /// no observation allocation or per-row writes.
    ///
    /// # Errors
    ///
    /// Returns the exact scratch-capacity rejection from [`Self::select_closure`].
    pub fn select_closure_with_probe<'root, Observation>(
        &'root self,
        range: Option<EntryRange>,
        scratch: &'root mut ClosureScratch,
        probe: &mut Observation,
    ) -> Result<SelectedClosure<'root, DomainTag>, ClosureError>
    where
        Observation: Probe<RootProbeEvent>,
    {
        let selected = self.select_closure(range, scratch)?;
        probe.record_with(|| RootProbeEvent {
            selected_rows: selected.len(),
            work: selected.work,
        });
        Ok(selected)
    }
}

#[allow(
    clippy::indexing_slicing,
    reason = "select_closure checked scratch capacity against this exact root before every private RowIndex mark access"
)]
fn mark_ancestors<DomainTag>(
    root: &GenerationRoot<DomainTag>,
    scratch: &mut ClosureScratch,
    start: RowIndex,
) {
    let mut cursor = Some(start);
    while let Some(index) = cursor {
        if scratch.marks[index.array_index()] == scratch.epoch {
            break;
        }
        scratch.mark(index);
        cursor = root.parent_index(index);
        if cursor.is_some() {
            scratch.facts.work.ancestor_edges += 1;
        }
    }
}
