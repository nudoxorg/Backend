//! Borrowed root/locality projection, selection, and allocation-free scans.

use core::{ops::Deref, slice};

use nudox_id::{ContentAuthority, Domain};
use nudox_object::{ObjectKind, ObjectLength, ObjectRef};

use super::{BorrowedRootFacts, BorrowedRootRows, ValidatedRoot};
use crate::{
    ClosureError, ClosureScratch, EntryKey, EntryRange, GenerationEntry, LocalityError, RootEntry,
    ValidatedLocality,
    encode::{ParentWire, RootWireRecord},
    locality::LocalityCursor,
    packed::RowIndex,
};

impl<'bytes, DomainTag> Deref for ValidatedRoot<'bytes, DomainTag> {
    type Target = BorrowedRootFacts<'bytes>;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl<DomainTag: Domain> ValidatedRoot<'_, DomainTag> {
    /// Returns the validated row count as a native slice length.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.rows.rows().len()
    }

    /// Returns whether this root has no rows.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rows.rows().is_empty()
    }

    /// Finds one borrowed-root entry through canonical-key binary search.
    #[must_use]
    pub fn get(&self, key: EntryKey) -> Option<RootEntry<DomainTag>> {
        match self.rows {
            BorrowedRootRows::Empty(_) => None,
            BorrowedRootRows::Populated { rows, authority } => {
                let position = rows.binary_search_by_key(&*key, |row| row.key.get()).ok()?;
                #[allow(
                    clippy::indexing_slicing,
                    reason = "binary search returned a coordinate in this exact immutable slice"
                )]
                Some(project_root_entry(&rows[position], authority))
            }
        }
    }
}

/// Borrowed coherent composition of canonical root bytes and validated locality.
pub struct BorrowedGenerationView<'bytes, 'locality, DomainTag> {
    facts: BorrowedRootFacts<'bytes>,
    rows: BorrowedRootRows<'bytes, DomainTag>,
    locality: &'locality ValidatedLocality<'locality, DomainTag>,
}

impl<'bytes, DomainTag> Deref for BorrowedGenerationView<'bytes, '_, DomainTag> {
    type Target = BorrowedRootFacts<'bytes>;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl<'bytes, 'locality, DomainTag: Domain> BorrowedGenerationView<'bytes, 'locality, DomainTag> {
    /// Pairs root and locality witnesses after checking their generation and cardinality.
    ///
    /// # Errors
    ///
    /// Returns the existing exact locality mismatch with both complete operands.
    pub fn new(
        root: &ValidatedRoot<'bytes, DomainTag>,
        locality: &'locality ValidatedLocality<'locality, DomainTag>,
    ) -> Result<Self, LocalityError> {
        if locality.generation != root.id {
            return Err(LocalityError::GenerationMismatch {
                locality_generation: locality.generation,
                root_generation: root.id,
            });
        }
        if locality.root_count != root.entry_count {
            return Err(LocalityError::RootCountMismatch {
                locality_count: locality.root_count,
                root_count: root.entry_count,
            });
        }
        Ok(Self {
            facts: root.facts,
            rows: root.rows,
            locality,
        })
    }

    /// Returns the canonical composed row count.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.rows.rows().len()
    }

    /// Returns whether this composed view has no rows.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rows.rows().is_empty()
    }

    /// Iterates composed entries in canonical key order without allocation.
    #[must_use]
    pub fn closure(&self) -> BorrowedGenerationScan<'_, 'locality, DomainTag> {
        BorrowedGenerationScan::new(self.rows, self.locality)
    }

    /// Finds one composed entry by semantic key.
    #[must_use]
    pub fn get(&self, key: EntryKey) -> Option<GenerationEntry<DomainTag>> {
        match self.rows {
            BorrowedRootRows::Empty(_) => None,
            BorrowedRootRows::Populated { rows, authority } => {
                let position = rows.binary_search_by_key(&*key, |row| row.key.get()).ok()?;
                let index = RowIndex::from_validated_borrowed_root_position(position);
                #[allow(
                    clippy::indexing_slicing,
                    reason = "binary search returned a coordinate in this exact immutable slice"
                )]
                let root = project_root_entry(&rows[position], authority);
                Some(GenerationEntry {
                    key: root.key,
                    parent: root.parent,
                    object: root.object,
                    locality: self.locality.locality_for(index),
                })
            }
        }
    }

    /// Selects a projection and all ancestors into reusable caller scratch.
    ///
    /// # Errors
    ///
    /// Returns [`ClosureError::ScratchTooSmall`] before changing scratch when
    /// the caller's declared capacity cannot cover this root.
    pub fn select_closure<'selected>(
        &'selected self,
        range: Option<EntryRange>,
        scratch: &'selected mut ClosureScratch,
    ) -> Result<BorrowedSelectedGeneration<'selected, 'bytes, 'locality, DomainTag>, ClosureError>
    {
        let rows = self.rows.rows();
        if scratch.capacity < rows.len() {
            return Err(ClosureError::ScratchTooSmall {
                required: rows.len(),
                available: scratch.capacity,
            });
        }
        scratch.begin_selection();
        match range {
            None => {
                for position in 0..rows.len() {
                    scratch.record_projected_row();
                    scratch.mark(RowIndex::from_validated_borrowed_root_position(position));
                }
            }
            Some(range) => {
                let start = rows.partition_point(|row| row.key.get() < *range.start());
                let end = rows.partition_point(|row| row.key.get() <= *range.end());
                for position in start..end {
                    scratch.record_projected_row();
                    mark_borrowed_ancestors(rows, scratch, position);
                }
                scratch.sort_selected_indices();
            }
        }
        Ok(BorrowedSelectedGeneration {
            rows: self.rows,
            selected: scratch.selected_indices(),
            compact_count: scratch.selected_count(),
            locality: self.locality,
        })
    }
}

#[allow(
    clippy::indexing_slicing,
    reason = "the start and every binary-resolved parent position belong to this exact validated typed row slice"
)]
fn mark_borrowed_ancestors(rows: &[RootWireRecord], scratch: &mut ClosureScratch, start: usize) {
    let mut position = start;
    loop {
        let index = RowIndex::from_validated_borrowed_root_position(position);
        if scratch.is_marked(index) {
            return;
        }
        scratch.mark(index);
        let row = &rows[position];
        if matches!(row.parent_present, ParentWire::Absent) {
            return;
        }
        let mut comparisons = 0;
        let Ok(parent) = rows.binary_search_by(|candidate| {
            comparisons += 1;
            candidate.key.get().cmp(&row.parent_key.get())
        }) else {
            return;
        };
        scratch.record_parent_search_comparisons(comparisons);
        scratch.record_ancestor_edge();
        position = parent;
    }
}

/// Allocation-free scan over a borrowed root/locality composition.
pub struct BorrowedGenerationScan<'scan, 'locality, DomainTag> {
    state: BorrowedGenerationScanState<'scan, 'locality, DomainTag>,
}

enum BorrowedGenerationScanState<'scan, 'locality, DomainTag> {
    Empty,
    Populated {
        rows: core::iter::Enumerate<slice::Iter<'scan, RootWireRecord>>,
        authority: ContentAuthority<DomainTag>,
        locality: LocalityCursor<'locality, DomainTag>,
    },
}

impl<'scan, 'locality, DomainTag: Domain> BorrowedGenerationScan<'scan, 'locality, DomainTag> {
    fn new(
        rows: BorrowedRootRows<'scan, DomainTag>,
        locality: &'locality ValidatedLocality<'locality, DomainTag>,
    ) -> Self {
        let state = match rows {
            BorrowedRootRows::Empty(_) => BorrowedGenerationScanState::Empty,
            BorrowedRootRows::Populated { rows, authority } => {
                BorrowedGenerationScanState::Populated {
                    rows: rows.iter().enumerate(),
                    authority,
                    locality: LocalityCursor::new(locality),
                }
            }
        };
        Self { state }
    }
}

impl<DomainTag: Domain> Iterator for BorrowedGenerationScan<'_, '_, DomainTag> {
    type Item = GenerationEntry<DomainTag>;

    fn next(&mut self) -> Option<Self::Item> {
        let BorrowedGenerationScanState::Populated {
            rows,
            authority,
            locality,
        } = &mut self.state
        else {
            return None;
        };
        let (position, row) = rows.next()?;
        let root = project_root_entry(row, *authority);
        Some(GenerationEntry {
            key: root.key,
            parent: root.parent,
            object: root.object,
            locality: locality
                .locality_without_work(RowIndex::from_validated_borrowed_root_position(position)),
        })
    }
}

/// Borrowed selected closure backed by root bytes and caller-owned marks.
pub struct BorrowedSelectedGeneration<'selected, 'bytes, 'locality, DomainTag> {
    rows: BorrowedRootRows<'bytes, DomainTag>,
    selected: &'selected [RowIndex],
    compact_count: u32,
    locality: &'locality ValidatedLocality<'locality, DomainTag>,
}

impl<'bytes, 'locality, DomainTag: Domain>
    BorrowedSelectedGeneration<'_, 'bytes, 'locality, DomainTag>
{
    /// Iterates selected entries in canonical key order.
    #[must_use]
    pub fn iter(&self) -> BorrowedSelectedScan<'_, 'bytes, 'locality, DomainTag> {
        let state = match self.rows {
            BorrowedRootRows::Empty(_) => BorrowedSelectedScanState::Empty,
            BorrowedRootRows::Populated { rows, authority } => {
                BorrowedSelectedScanState::Populated {
                    selected: self.selected.iter(),
                    rows,
                    authority,
                    locality: LocalityCursor::new(self.locality),
                }
            }
        };
        BorrowedSelectedScan { state }
    }

    /// Returns the root-proven compact selected count.
    #[must_use]
    pub fn count(&self) -> crate::SelectedCount {
        crate::SelectedCount::from(self.compact_count)
    }

    /// Returns selected descriptor count.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.selected.len()
    }

    /// Returns whether this selection is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }
}

impl<'scan, 'bytes, 'locality, DomainTag: Domain> IntoIterator
    for &'scan BorrowedSelectedGeneration<'_, 'bytes, 'locality, DomainTag>
{
    type Item = GenerationEntry<DomainTag>;
    type IntoIter = BorrowedSelectedScan<'scan, 'bytes, 'locality, DomainTag>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Allocation-free canonical scan over one borrowed selected closure.
pub struct BorrowedSelectedScan<'scan, 'bytes, 'locality, DomainTag> {
    state: BorrowedSelectedScanState<'scan, 'bytes, 'locality, DomainTag>,
}

enum BorrowedSelectedScanState<'scan, 'bytes, 'locality, DomainTag> {
    Empty,
    Populated {
        selected: slice::Iter<'scan, RowIndex>,
        rows: &'bytes [RootWireRecord],
        authority: ContentAuthority<DomainTag>,
        locality: LocalityCursor<'locality, DomainTag>,
    },
}

impl<DomainTag: Domain> Iterator for BorrowedSelectedScan<'_, '_, '_, DomainTag> {
    type Item = GenerationEntry<DomainTag>;

    #[allow(
        clippy::indexing_slicing,
        reason = "selected coordinates were issued only after scratch capacity was checked against this exact validated root"
    )]
    fn next(&mut self) -> Option<Self::Item> {
        let BorrowedSelectedScanState::Populated {
            selected,
            rows,
            authority,
            locality,
        } = &mut self.state
        else {
            return None;
        };
        let index = *selected.next()?;
        let root = project_root_entry(&rows[index.array_index()], *authority);
        Some(GenerationEntry {
            key: root.key,
            parent: root.parent,
            object: root.object,
            locality: locality.locality_without_work(index),
        })
    }
}

fn project_root_entry<DomainTag: Domain>(
    row: &RootWireRecord,
    authority: ContentAuthority<DomainTag>,
) -> RootEntry<DomainTag> {
    let [_, payload @ ..] = row.descriptor.content;
    RootEntry {
        key: EntryKey::from(row.key.get()),
        parent: match row.parent_present {
            ParentWire::Absent => None,
            ParentWire::Present => Some(EntryKey::from(row.parent_key.get())),
        },
        object: ObjectRef {
            content: authority.bind(payload),
            length: ObjectLength::from(row.descriptor.length.get()),
            schema: row.descriptor.schema.get(),
            kind: ObjectKind::from(row.descriptor.kind.get()),
        },
    }
}
