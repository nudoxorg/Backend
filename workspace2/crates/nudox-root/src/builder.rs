use alloc::collections::TryReserveError;
use alloc::vec::Vec;
use core::{mem::size_of, num::TryFromIntError};

use thiserror::Error;

use crate::MetadataBytes;
use crate::encode::canonical_id;
use crate::entry::{EntryKey, RootEntry};
use crate::packed::{GenerationRoot, HierarchyDepth, HierarchyState, NO_PARENT, RootRow};

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
    rows: Vec<RootRow<DomainTag>>,
}

impl<DomainTag> GenerationRootBuilder<DomainTag> {
    /// Allocates the declared maximum input entry count before construction.
    ///
    /// # Errors
    ///
    /// Returns the allocation cause before the builder can accept any entry.
    pub fn with_capacity(entry_capacity: usize) -> Result<Self, TryReserveError> {
        let mut rows = Vec::new();
        rows.try_reserve_exact(entry_capacity)?;
        Ok(Self { rows })
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
        if self.rows.len() == self.rows.capacity() {
            return Err(RejectedRootEntry {
                error: RootPushError::InputCapacityExceeded {
                    capacity: self.rows.capacity(),
                },
                entry,
            });
        }
        self.rows.push(RootRow::collected(entry));
        Ok(())
    }

    /// Sorts, validates hierarchy, and packs canonical semantic rows.
    ///
    /// # Errors
    ///
    /// Returns an exact duplicate/parent/cycle/depth or compact-index failure.
    /// Finishing performs no allocation: the row's private phase niche carries
    /// hierarchy traversal state in the already reserved arena.
    #[allow(
        clippy::indexing_slicing,
        reason = "the loop coordinate is constructed directly from this exact row length"
    )]
    pub fn finish(mut self) -> Result<GenerationRoot<DomainTag>, RootBuildError> {
        self.rows.sort_unstable_by_key(|row| row.key);
        validate_unique_rows(&self.rows)?;
        let entry_count = self.rows.len();
        let compact_length = CompactRootLength::from_entries(entry_count)?;
        for index in 0..self.rows.len() {
            let _compact_index = CompactRootLength::index(index);
            let key = self.rows[index].key;
            let parent = match self.rows[index].collected_parent() {
                None => NO_PARENT,
                Some(parent_key) => resolve_parent_key(key, parent_key, &self.rows)?,
            };
            self.rows[index].set_unseen(parent);
        }
        let peak_live_bytes = self.rows.capacity() * size_of::<RootRow<DomainTag>>();
        validate_hierarchy(&mut self.rows)?;
        let id = canonical_id(&self.rows);
        Ok(GenerationRoot {
            facts: crate::packed::GenerationRootFacts {
                id,
                entry_count: compact_length.0.into(),
                construction_peak_bytes: peak_live_bytes.into(),
            },
            rows: self.rows.into_boxed_slice(),
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
        let input_bytes = entries.capacity() * size_of::<RootEntry<DomainTag>>();
        let mut builder = GenerationRootBuilder::with_capacity(entries.len())
            .map_err(RootBuildError::RowReservation)?;
        let row_bytes = builder.rows.capacity() * size_of::<RootRow<DomainTag>>();
        for entry in entries {
            builder.rows.push(RootRow::collected(entry));
        }
        let mut root = builder.finish()?;
        root.facts.construction_peak_bytes = (input_bytes + row_bytes).into();
        Ok(root)
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

fn validate_unique_rows<DomainTag>(rows: &[RootRow<DomainTag>]) -> Result<(), RootBuildError> {
    let mut previous = None;
    for row in rows {
        if previous == Some(row.key) {
            return Err(RootBuildError::DuplicateKey { key: row.key });
        }
        previous = Some(row.key);
    }
    Ok(())
}

fn resolve_parent_key<DomainTag>(
    child: EntryKey,
    parent: EntryKey,
    rows: &[RootRow<DomainTag>],
) -> Result<u32, RootBuildError> {
    rows.binary_search_by_key(&parent, |row| row.key)
        .map(CompactRootLength::index)
        .map_err(|_| RootBuildError::MissingParent { child, parent })
}

/// Validates a just-built dense hierarchy before rows can become immutable.
///
/// The builder created every parent by a binary search over this same `rows`
/// slice. The explicit two-byte state occupies descriptor padding and the
/// parent remains in the low payload word while rows are marked. Thus cycle
/// detection and depth assignment need no side allocation. All cursors
/// originate in `0..rows.len()` or those resolved parents.
#[allow(
    clippy::indexing_slicing,
    reason = "all coordinates originate in this exact rows range or builder-resolved parent links"
)]
fn validate_hierarchy<DomainTag>(rows: &mut [RootRow<DomainTag>]) -> Result<(), RootBuildError> {
    for start in 0..rows.len() {
        if rows[start].hierarchy_state() == HierarchyState::Published {
            continue;
        }
        let mut cursor = Some(start);
        let mut chain_length = 0_u32;
        let base_depth = loop {
            let Some(index) = cursor else {
                break 0_u32.into();
            };
            match rows[index].hierarchy_state() {
                HierarchyState::Unseen => {
                    rows[index].mark_visiting();
                    chain_length = chain_length
                        .checked_add(1)
                        .ok_or(RootBuildError::DepthOverflow)?;
                    cursor = parent_index(rows[index].parent())?;
                }
                HierarchyState::Visiting => {
                    return Err(RootBuildError::HierarchyCycle {
                        key: rows[index].key,
                    });
                }
                HierarchyState::Published => break rows[index].depth(),
            }
        };

        cursor = Some(start);
        let mut remaining = chain_length;
        while remaining != 0 {
            let Some(index) = cursor else {
                unreachable!("resolved hierarchy chain ended before its measured length");
            };
            let depth = HierarchyDepth::from(
                (*base_depth)
                    .checked_add(remaining)
                    .ok_or(RootBuildError::DepthOverflow)?,
            );
            cursor = parent_index(rows[index].parent())?;
            rows[index].publish(depth);
            remaining -= 1;
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
