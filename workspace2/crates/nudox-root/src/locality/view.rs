//! Coherent root/locality views and their allocation-free iterators.

use core::{mem::size_of_val, ops::Deref};

use nudox_id::Domain;
use nudox_observe::Probe;

use crate::closure::{ClosureError, ClosureScratch, RootProbeEvent, SelectedClosure};
use crate::entry::{EntryKey, EntryRange};
use crate::packed::{CanonicalRows, GenerationRoot, RowIndex};

use super::{
    GenerationEntry, LocalityCursor, LocalityError, LocalityLookupWork, LocalityReadError,
    LocalityScanWork, SelectedCount, SelectedOrdinal, SelectedOrdinalBuffer, SelectedOrdinals,
    ValidatedLocality,
};

/// Borrowed coherent composition of immutable semantic root and locality map.
pub struct GenerationView<'root, 'locality, DomainTag> {
    root: &'root GenerationRoot<DomainTag>,
    locality: &'locality ValidatedLocality<'locality, DomainTag>,
}

/// Optional composed lookup result paired with its exact sparse-route work.
pub type MeasuredGenerationLookup<DomainTag> =
    (Option<GenerationEntry<DomainTag>>, LocalityLookupWork);

/// A coherent view exposes immutable root facts and operations through deref;
/// no mutable dereference can decouple locality evidence from its root.
impl<DomainTag: Domain> Deref for GenerationView<'_, '_, DomainTag> {
    type Target = GenerationRoot<DomainTag>;

    fn deref(&self) -> &Self::Target {
        self.root
    }
}

/// Borrowed selected semantic closure composed with sparse locality evidence.
pub struct SelectedGeneration<'root, DomainTag> {
    selected: SelectedClosure<'root, DomainTag>,
    locality: &'root ValidatedLocality<'root, DomainTag>,
}

impl<DomainTag: Domain> Copy for SelectedGeneration<'_, DomainTag> {}

impl<DomainTag: Domain> Clone for SelectedGeneration<'_, DomainTag> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'selection, DomainTag: Domain> SelectedGeneration<'selection, DomainTag> {
    /// Iterates selected composed entries in canonical key order.
    pub fn iter(
        &self,
    ) -> impl Iterator<Item = Result<GenerationEntry<DomainTag>, LocalityReadError>> + '_ {
        self.selected
            .rows()
            .scan(self.locality.scan(), |cursor, (index, entry)| {
                Some(
                    cursor
                        .locality_without_work(index)
                        .map(|locality| GenerationEntry {
                            key: entry.key,
                            parent: entry.parent,
                            object: entry.object,
                            locality,
                        }),
                )
            })
    }

    /// Retains only predicate-matching selected positions in caller-owned
    /// compact storage. The opaque result owns this exact selection copy, so
    /// foreign positions can never drive its infallible reconstruction.
    ///
    /// # Errors
    ///
    /// Returns [`super::SelectedOrdinalBufferError::TooSmall`] before calling
    /// the predicate or mutating the buffer when its declared capacity cannot
    /// cover this selected closure.
    pub fn retain_ordinals_where<'storage, IsAbsent>(
        &self,
        buffer: &'storage mut SelectedOrdinalBuffer,
        mut is_absent: IsAbsent,
    ) -> Result<SelectedOrdinals<'selection, 'storage, DomainTag>, super::SelectedOrdinalBufferError>
    where
        IsAbsent: FnMut(GenerationEntry<DomainTag>) -> bool,
    {
        let count = self.count();
        if count > buffer.capacity {
            return Err(super::SelectedOrdinalBufferError::TooSmall {
                required: count,
                available: buffer.capacity,
            });
        }
        buffer.clear();
        for item in self.enumerated() {
            let (ordinal, entry) = item?;
            if is_absent(entry) {
                buffer.push(ordinal);
            }
        }
        Ok(SelectedOrdinals {
            selected: *self,
            positions: buffer.positions.as_slice(),
        })
    }

    fn enumerated(
        &self,
    ) -> impl Iterator<
        Item = Result<(SelectedOrdinal, GenerationEntry<DomainTag>), LocalityReadError>,
    > + '_ {
        self.selected.rows().zip(0_u32..).scan(
            self.locality.scan(),
            |cursor, ((index, entry), position)| {
                Some(cursor.locality_without_work(index).map(|locality| {
                    (
                        SelectedOrdinal::from_compact(position),
                        GenerationEntry {
                            key: entry.key,
                            parent: entry.parent,
                            object: entry.object,
                            locality,
                        },
                    )
                }))
            },
        )
    }

    fn entry_at_compact(
        &self,
        position: u32,
    ) -> Result<GenerationEntry<DomainTag>, LocalityReadError> {
        let ordinal = SelectedOrdinal::from_compact(position);
        let (index, entry) = self.selected.row_at_position(ordinal.array_index());
        self.locality
            .locality_for(index)
            .map(|locality| GenerationEntry {
                key: entry.key,
                parent: entry.parent,
                object: entry.object,
                locality,
            })
    }

    /// Returns the root-proven selected count as a compact canonical field.
    #[must_use]
    pub const fn count(&self) -> SelectedCount {
        SelectedCount(self.selected.compact_count())
    }

    /// Returns selected descriptor count without rescanning root state.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.selected.len()
    }

    /// Returns whether this selected composition has no entries.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    /// Iterates selected locality while recording sparse-cursor work for a
    /// test or benchmark. The ordinary [`Self::iter`] path has no diagnostic
    /// writes.
    pub fn measured_iter<'scan>(
        &'scan self,
        work: &'scan mut LocalityScanWork,
    ) -> impl Iterator<Item = Result<GenerationEntry<DomainTag>, LocalityReadError>> + 'scan {
        self.selected.rows().scan(
            (self.locality.scan(), work),
            |(cursor, work), (index, entry)| {
                work.rows += 1;
                Some(
                    cursor
                        .locality_at(index, work)
                        .map(|locality| GenerationEntry {
                            key: entry.key,
                            parent: entry.parent,
                            object: entry.object,
                            locality,
                        }),
                )
            },
        )
    }
}

impl<DomainTag: Domain> SelectedOrdinals<'_, '_, DomainTag> {
    /// Returns the exact logical compact bytes retained for this list.
    #[must_use]
    pub const fn state_bytes(&self) -> usize {
        size_of_val(self.positions)
    }

    /// Iterates every entry from the exact selected closure that emitted these
    /// sparse positions.
    pub fn required_entries(
        &self,
    ) -> impl Iterator<Item = Result<GenerationEntry<DomainTag>, LocalityReadError>> + '_ {
        self.selected.iter()
    }

    /// Iterates selected entries not retained in this sparse ordinal list.
    pub fn present_entries(
        &self,
    ) -> impl Iterator<Item = Result<GenerationEntry<DomainTag>, LocalityReadError>> + '_ {
        let mut absent = self.positions.iter().copied().peekable();
        self.selected
            .enumerated()
            .filter_map(move |item| match item {
                Ok((ordinal, entry)) => {
                    if absent.peek().is_some_and(|position| *position == ordinal.0) {
                        absent.next();
                        None
                    } else {
                        Some(Ok(entry))
                    }
                }
                Err(error) => Some(Err(error)),
            })
    }

    /// Iterates entries retained in this sparse ordinal list without any
    /// per-item checked selected-array access.
    pub fn absent_entries(
        &self,
    ) -> impl Iterator<Item = Result<GenerationEntry<DomainTag>, LocalityReadError>> + '_ {
        self.positions
            .iter()
            .copied()
            .map(move |position| self.selected.entry_at_compact(position))
    }
}

/// Canonical composed scan with no diagnostic writes in the production path.
pub struct GenerationScan<'root, 'locality, DomainTag> {
    rows: CanonicalRows<'root, DomainTag>,
    locality: LocalityCursor<'locality, DomainTag>,
}

impl<DomainTag: Domain> Iterator for GenerationScan<'_, '_, DomainTag> {
    type Item = Result<GenerationEntry<DomainTag>, LocalityReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        let row = self.rows.next()?;
        Some(
            self.locality
                .locality_without_work(row.index)
                .map(|locality| GenerationEntry {
                    key: row.entry.key,
                    parent: row.entry.parent,
                    object: row.entry.object,
                    locality,
                }),
        )
    }
}

/// Test/benchmark-only canonical scan that records exact sparse-route work.
pub struct MeasuredGenerationScan<'root, 'locality, DomainTag> {
    rows: CanonicalRows<'root, DomainTag>,
    locality: LocalityCursor<'locality, DomainTag>,
    work: LocalityScanWork,
}

impl<DomainTag: Domain> Deref for MeasuredGenerationScan<'_, '_, DomainTag> {
    type Target = LocalityScanWork;

    fn deref(&self) -> &Self::Target {
        &self.work
    }
}

impl<DomainTag: Domain> Iterator for MeasuredGenerationScan<'_, '_, DomainTag> {
    type Item = Result<GenerationEntry<DomainTag>, LocalityReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        let row = self.rows.next()?;
        self.work.rows += 1;
        Some(
            self.locality
                .locality_at(row.index, &mut self.work)
                .map(|locality| GenerationEntry {
                    key: row.entry.key,
                    parent: row.entry.parent,
                    object: row.entry.object,
                    locality,
                }),
        )
    }
}

/// Private canonical scan retaining the root-issued coordinate needed by
/// direct overlay ancestry propagation.
pub(crate) struct GenerationRows<'root, 'locality, DomainTag> {
    rows: CanonicalRows<'root, DomainTag>,
    locality: LocalityCursor<'locality, DomainTag>,
}

impl<DomainTag: Domain> Iterator for GenerationRows<'_, '_, DomainTag> {
    type Item = Result<(RowIndex, GenerationEntry<DomainTag>), LocalityReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        let row = self.rows.next()?;
        Some(
            self.locality
                .locality_without_work(row.index)
                .map(|locality| {
                    (
                        row.index,
                        GenerationEntry {
                            key: row.entry.key,
                            parent: row.entry.parent,
                            object: row.entry.object,
                            locality,
                        },
                    )
                }),
        )
    }
}

impl<'root, 'locality, DomainTag: Domain> GenerationView<'root, 'locality, DomainTag> {
    /// Validates the only cross-axis invariant once before any iteration.
    ///
    /// # Errors
    ///
    /// Returns [`LocalityError::GenerationMismatch`] when the map belongs to
    /// a different immutable semantic root.
    pub fn new(
        root: &'root GenerationRoot<DomainTag>,
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
        Ok(Self { root, locality })
    }

    /// Iterates composed entries in canonical semantic order with direct work evidence.
    #[must_use]
    pub fn closure(&self) -> GenerationScan<'root, 'locality, DomainTag> {
        GenerationScan {
            rows: self.root.canonical_rows(),
            locality: self.locality.scan(),
        }
    }

    /// Iterates canonical locality with explicit benchmark/test work evidence.
    #[must_use]
    pub fn measured_closure(&self) -> MeasuredGenerationScan<'root, 'locality, DomainTag> {
        MeasuredGenerationScan {
            rows: self.root.canonical_rows(),
            locality: self.locality.scan(),
            work: LocalityScanWork::default(),
        }
    }

    /// Selects a projection and composes sparse locality without a key lookup.
    ///
    /// # Errors
    ///
    /// Returns [`ClosureError::ScratchTooSmall`] before composing locality
    /// when caller-owned selection scratch cannot cover the root.
    pub fn select_closure<'selected>(
        &'selected self,
        range: Option<EntryRange>,
        scratch: &'selected mut ClosureScratch,
    ) -> Result<SelectedGeneration<'selected, DomainTag>, ClosureError> {
        let selected = self.root.select_closure(range, scratch)?;
        Ok(SelectedGeneration {
            selected,
            locality: self.locality,
        })
    }

    /// Selects a locality-composed closure while lazily recording the root's
    /// one aggregate selection event.
    ///
    /// # Errors
    ///
    /// Returns the exact scratch-capacity rejection before locality composition.
    pub fn select_closure_with_probe<'selected, Observation>(
        &'selected self,
        range: Option<EntryRange>,
        scratch: &'selected mut ClosureScratch,
        probe: &mut Observation,
    ) -> Result<SelectedGeneration<'selected, DomainTag>, ClosureError>
    where
        Observation: Probe<RootProbeEvent>,
    {
        let selected = self.root.select_closure_with_probe(range, scratch, probe)?;
        Ok(SelectedGeneration {
            selected,
            locality: self.locality,
        })
    }

    /// Finds a composed entry by semantic key with one sparse-route lookup.
    ///
    /// # Errors
    ///
    /// Returns the exact post-validation provider or descriptor reconstruction
    /// fault without manufacturing a placement state.
    pub fn get(
        &self,
        key: EntryKey,
    ) -> Result<Option<GenerationEntry<DomainTag>>, LocalityReadError> {
        self.get_without_work(key)
    }

    /// Finds one composed entry while exposing the one sparse binary search
    /// needed to resolve its locality.
    ///
    /// # Errors
    ///
    /// Returns the exact post-validation provider or descriptor reconstruction
    /// fault without manufacturing a placement state.
    pub fn measured_get(
        &self,
        key: EntryKey,
    ) -> Result<MeasuredGenerationLookup<DomainTag>, LocalityReadError> {
        let mut work = LocalityLookupWork::default();
        let entry = self.get_with_work(key, &mut work)?;
        Ok((entry, work))
    }

    fn get_without_work(
        &self,
        key: EntryKey,
    ) -> Result<Option<GenerationEntry<DomainTag>>, LocalityReadError> {
        let Ok(position) = self.root.rows.binary_search_by_key(&key, |row| row.key) else {
            return Ok(None);
        };
        let index = RowIndex::from_arena_position(position);
        let entry = self.root.entry_at(index);
        self.locality.locality_for(index).map(|locality| {
            Some(GenerationEntry {
                key: entry.key,
                parent: entry.parent,
                object: entry.object,
                locality,
            })
        })
    }

    fn get_with_work(
        &self,
        key: EntryKey,
        work: &mut LocalityLookupWork,
    ) -> Result<Option<GenerationEntry<DomainTag>>, LocalityReadError> {
        let Ok(position) = self.root.rows.binary_search_by_key(&key, |row| row.key) else {
            return Ok(None);
        };
        let index = RowIndex::from_arena_position(position);
        let entry = self.root.entry_at(index);
        self.locality
            .measured_locality_for(index, work)
            .map(|locality| {
                Some(GenerationEntry {
                    key: entry.key,
                    parent: entry.parent,
                    object: entry.object,
                    locality,
                })
            })
    }

    pub(crate) fn root_rows(&self) -> GenerationRows<'root, 'locality, DomainTag> {
        GenerationRows {
            rows: self.root.canonical_rows(),
            locality: self.locality.scan(),
        }
    }
}
