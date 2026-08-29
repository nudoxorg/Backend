use alloc::collections::TryReserveError;
use alloc::vec::Vec;
use core::{mem::size_of, num::TryFromIntError};

use thiserror::Error;

use crate::MetadataBytes;
use crate::encode::canonical_id;
use crate::entry::{EntryKey, RootEntry};
use crate::packed::{GenerationRoot, NO_PARENT, RootRow};

/// Builder-validated compact bound consumed by packed root coordinates.
struct CompactRootLength(u32);

impl CompactRootLength {
    fn from_entries(entry_count: usize) -> Result<Self, RootBuildError> {
        u32::try_from(entry_count)
            .map(Self)
            .map_err(RootBuildError::IndexConversion)
    }

    #[allow(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "this coordinate is below the builder-validated CompactRootLength"
    )]
    const fn index(position: usize) -> u32 {
        position as u32
    }
}

/// Checked mutable root builder; successful finish publishes an immutable packed root.
pub struct GenerationRootBuilder<DomainTag> {
    entries: Vec<RootEntry<DomainTag>>,
}

impl<DomainTag> GenerationRootBuilder<DomainTag> {
    /// Allocates the declared maximum input entry count before construction.
    ///
    /// # Errors
    ///
    /// Returns the allocation cause before the builder can accept any entry.
    pub fn with_capacity(entry_capacity: usize) -> Result<Self, TryReserveError> {
        let mut entries = Vec::new();
        entries.try_reserve_exact(entry_capacity)?;
        Ok(Self { entries })
    }

    /// Fallibly adds one arbitrary-order construction entry.
    ///
    /// # Errors
    ///
    /// Returns the exact unretained entry when the declared input capacity is
    /// already exhausted. This builder never grows after construction.
    pub fn try_push(
        &mut self,
        entry: RootEntry<DomainTag>,
    ) -> Result<(), RejectedRootEntry<DomainTag>> {
        if self.entries.len() == self.entries.capacity() {
            return Err(RejectedRootEntry {
                error: RootPushError::InputCapacityExceeded {
                    capacity: self.entries.capacity(),
                },
                entry,
            });
        }
        self.entries.push(entry);
        Ok(())
    }

    /// Sorts, validates hierarchy, and packs canonical semantic rows.
    ///
    /// # Errors
    ///
    /// Returns an exact duplicate/parent/cycle/depth failure, compact-index
    /// conversion failure, or retained allocation source.
    pub fn finish(mut self) -> Result<GenerationRoot<DomainTag>, RootBuildError> {
        self.entries.sort_unstable_by_key(|entry| entry.key);
        validate_unique(&self.entries)?;
        let entry_count = self.entries.len();
        let compact_length = CompactRootLength::from_entries(entry_count)?;

        let mut rows = reserve_exact(entry_count, RootBuildError::RowReservation)?;
        for (index, entry) in self.entries.iter().enumerate() {
            let _compact_index = CompactRootLength::index(index);
            let parent = resolve_parent(entry, &self.entries)?;
            rows.push(RootRow {
                object: entry.object,
                key: entry.key,
                parent,
                depth: 0_u32.into(),
            });
        }
        let peak_live_bytes = self.entries.capacity() * size_of::<RootEntry<DomainTag>>()
            + rows.capacity() * size_of::<RootRow<DomainTag>>();
        // Input entries are no longer needed after the rows have captured all
        // semantic facts. Releasing them before hierarchy scratch avoids the
        // former input + rows + marks + path peak.
        drop(self.entries);
        validate_hierarchy(&mut rows)?;
        let id = canonical_id(&rows);
        Ok(GenerationRoot {
            facts: crate::packed::GenerationRootFacts {
                id,
                entry_count: compact_length.0.into(),
                construction_peak_bytes: peak_live_bytes.into(),
            },
            rows: rows.into_boxed_slice(),
        })
    }
}

impl<DomainTag> GenerationRoot<DomainTag> {
    /// Builds a canonical root from arbitrary input order.
    ///
    /// # Errors
    ///
    /// Returns the same exact construction failures as
    /// [`GenerationRootBuilder::finish`].
    pub fn new(entries: Vec<RootEntry<DomainTag>>) -> Result<Self, RootBuildError> {
        GenerationRootBuilder { entries }.finish()
    }

    /// Returns the exact caller-buffer extent for canonical root bytes.
    #[must_use]
    pub fn canonical_len(&self) -> MetadataBytes {
        crate::encode::canonical_len(&self.rows).into()
    }

    /// Writes canonical root bytes into a caller-owned prefix.
    ///
    /// # Errors
    ///
    /// Returns [`RootWriteError`] before modifying any output
    /// byte when the exact canonical root prefix is unavailable.
    pub fn write_canonical<'output>(
        &self,
        output: &'output mut [u8],
    ) -> Result<&'output [u8], RootWriteError> {
        let required = usize::from(self.canonical_len());
        if output.len() < required {
            return Err(RootWriteError {
                required: required.into(),
                available: output.len().into(),
            });
        }
        #[allow(
            clippy::indexing_slicing,
            reason = "the immediately preceding exact canonical length preflight proves this output prefix"
        )]
        let output = &mut output[..required];
        crate::encode::write_canonical(&self.rows, output);
        Ok(output)
    }
}

/// Caller output was shorter than one canonical root encoding.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("canonical root output has {available:?} bytes but requires {required:?}")]
pub struct RootWriteError {
    /// Exact canonical root byte count.
    pub required: MetadataBytes,
    /// Caller-provided output byte count.
    pub available: MetadataBytes,
}

/// Checked root construction failure with retained allocation/conversion sources.
#[derive(Debug, Error)]
pub enum RootBuildError {
    /// Duplicate stable semantic key.
    #[error("duplicate root key {key:?}")]
    DuplicateKey {
        /// Duplicate key.
        key: EntryKey,
    },
    /// A parent key is absent.
    #[error("root entry {child:?} has absent parent {parent:?}")]
    MissingParent {
        /// Child key.
        child: EntryKey,
        /// Absent parent.
        parent: EntryKey,
    },
    /// Parent links form a cycle.
    #[error("root hierarchy has a parent cycle at {key:?}")]
    HierarchyCycle {
        /// Key seen twice on a parent chain.
        key: EntryKey,
    },
    /// A compact index conversion cannot represent this root.
    #[error("root exceeds compact index limits")]
    IndexConversion(#[source] TryFromIntError),
    /// Packed root row reservation failed.
    #[error("could not reserve packed root rows")]
    RowReservation(#[source] TryReserveError),
    /// Hierarchy validation mark reservation failed.
    #[error("could not reserve hierarchy validation marks")]
    HierarchyReservation(#[source] TryReserveError),
    /// Hierarchy validation path reservation failed.
    #[error("could not reserve hierarchy validation path")]
    PathReservation(#[source] TryReserveError),
    /// A hierarchy path exceeds the compact depth representation.
    #[error("root hierarchy depth exceeds compact representation")]
    DepthOverflow,
}

/// Closed non-allocation rejection while appending to a bounded root builder.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RootPushError {
    /// Declared builder input capacity was exhausted before finishing.
    #[error("root builder input capacity {capacity} is exhausted")]
    InputCapacityExceeded {
        /// Declared maximum input entries.
        capacity: usize,
    },
}

/// An entry returned intact when a bounded root builder rejects an append.
pub struct RejectedRootEntry<DomainTag> {
    /// Exact closed push rejection.
    pub error: RootPushError,
    /// Unretained caller-owned construction entry.
    pub entry: RootEntry<DomainTag>,
}

fn reserve_exact<Element>(
    capacity: usize,
    map_error: impl FnOnce(TryReserveError) -> RootBuildError,
) -> Result<Vec<Element>, RootBuildError> {
    let mut output = Vec::new();
    output.try_reserve_exact(capacity).map_err(map_error)?;
    Ok(output)
}

fn validate_unique<DomainTag>(entries: &[RootEntry<DomainTag>]) -> Result<(), RootBuildError> {
    let mut previous = None;
    for entry in entries {
        if previous == Some(entry.key) {
            return Err(RootBuildError::DuplicateKey { key: entry.key });
        }
        previous = Some(entry.key);
    }
    Ok(())
}

fn resolve_parent<DomainTag>(
    entry: &RootEntry<DomainTag>,
    entries: &[RootEntry<DomainTag>],
) -> Result<u32, RootBuildError> {
    match entry.parent {
        None => Ok(NO_PARENT),
        Some(parent_key) => {
            let Ok(index) = entries.binary_search_by_key(&parent_key, |candidate| candidate.key)
            else {
                return Err(RootBuildError::MissingParent {
                    child: entry.key,
                    parent: parent_key,
                });
            };
            Ok(CompactRootLength::index(index))
        }
    }
}

/// Validates a just-built dense hierarchy before rows can become immutable.
///
/// The builder created every parent by a binary search over this same `rows`
/// slice. `visits` and `path` were allocated to exactly `rows.len()`, and all
/// cursors originate in `0..rows.len()` or those resolved parents. The direct
/// dense accesses below therefore consume construction proof rather than
/// exposing an impossible `Internal` result to root consumers.
#[allow(
    clippy::indexing_slicing,
    reason = "all coordinates originate in this exact rows range or builder-resolved parent links; dense marks/path have the same length"
)]
fn validate_hierarchy<DomainTag>(rows: &mut [RootRow<DomainTag>]) -> Result<(), RootBuildError> {
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum Visit {
        Unseen,
        Visiting,
        Done,
    }

    let mut visits = reserve_exact(rows.len(), RootBuildError::HierarchyReservation)?;
    visits.resize(rows.len(), Visit::Unseen);
    let mut path = reserve_exact(rows.len(), RootBuildError::PathReservation)?;
    for start in 0..rows.len() {
        if visits[start] == Visit::Done {
            continue;
        }
        path.clear();
        let mut cursor = Some(start);
        while let Some(index) = cursor {
            match visits[index] {
                Visit::Unseen => {
                    visits[index] = Visit::Visiting;
                    path.push(index);
                    cursor = parent_index(rows[index].parent)?;
                }
                Visit::Visiting => {
                    return Err(RootBuildError::HierarchyCycle {
                        key: rows[index].key,
                    });
                }
                Visit::Done => break,
            }
        }
        let mut depth = match cursor {
            Some(index) => rows[index].depth,
            None => 0_u32.into(),
        };
        while let Some(index) = path.pop() {
            depth = depth.checked_child().ok_or(RootBuildError::DepthOverflow)?;
            rows[index].depth = depth;
            visits[index] = Visit::Done;
        }
    }
    Ok(())
}

fn parent_index(parent: u32) -> Result<Option<usize>, RootBuildError> {
    if parent == NO_PARENT {
        Ok(None)
    } else {
        usize::try_from(parent)
            .map(Some)
            .map_err(RootBuildError::IndexConversion)
    }
}
