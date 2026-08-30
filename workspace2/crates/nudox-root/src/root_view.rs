//! Borrowed, checked views over the canonical root byte representation.

use crate::encode::{ROOT_ROW_RECORD_BYTES, RootHeaderRecord, RootWireRecord};
use crate::locality::LocalityCursor;
use crate::packed::RowIndex;
use crate::{
    EntryKey, GenerationEntry, LocalityError, LocalityReadError, MetadataBytes, RootEntryCount,
    ValidatedLocality,
};
use alloc::vec::Vec;
use core::{marker::PhantomData, mem::size_of, ops::Deref};
use nudox_id::GenerationId;
use nudox_object::{ObjectDescriptorDecodeError, ObjectRef};
use thiserror::Error;
use zerocopy::{FromBytes, IntoBytes, TryFromBytes};

#[derive(Debug, Error)]
pub enum RootReadError {
    #[error("root header truncated")]
    HeaderTruncated {
        required: MetadataBytes,
        available: MetadataBytes,
    },
    #[error("root count is out of range")]
    CountOutOfRange {
        declared: u64,
        source: core::num::TryFromIntError,
    },
    #[error("root layout overflow")]
    LayoutOverflow {
        count: RootEntryCount,
        record_bytes: MetadataBytes,
    },
    #[error("root extent mismatch")]
    Extent {
        count: RootEntryCount,
        required: MetadataBytes,
        available: MetadataBytes,
    },
    #[error("root descriptor is invalid")]
    Descriptor {
        ordinal: RootEntryCount,
        key: EntryKey,
        source: ObjectDescriptorDecodeError,
    },
    #[error("root keys are not ordered")]
    KeyOrder {
        ordinal: RootEntryCount,
        previous: EntryKey,
        current: EntryKey,
    },
    #[error("root parent presence is invalid")]
    ParentPresent {
        ordinal: RootEntryCount,
        key: EntryKey,
        observed: u8,
    },
    #[error("root absent parent key is invalid")]
    AbsentParentKey {
        ordinal: RootEntryCount,
        key: EntryKey,
        observed: EntryKey,
    },
    #[error("root parent is missing")]
    MissingParent { child: EntryKey, parent: EntryKey },
    #[error("root hierarchy scratch reservation failed")]
    HierarchyScratch {
        entries: RootEntryCount,
        requested_bytes: MetadataBytes,
        source: alloc::collections::TryReserveError,
    },
    #[error("root hierarchy contains a cycle")]
    HierarchyCycle { key: EntryKey },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidatedRootFacts<'bytes> {
    pub bytes: &'bytes [u8],
    pub id: GenerationId,
    pub entry_count: RootEntryCount,
}

pub struct ValidatedRoot<'bytes, DomainTag> {
    facts: ValidatedRootFacts<'bytes>,
    rows: &'bytes [RootWireRecord],
    domain: PhantomData<fn() -> DomainTag>,
}
impl<'bytes, DomainTag> Deref for ValidatedRoot<'bytes, DomainTag> {
    type Target = ValidatedRootFacts<'bytes>;
    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl<'bytes, DomainTag> TryFrom<&'bytes [u8]> for ValidatedRoot<'bytes, DomainTag> {
    type Error = RootReadError;
    fn try_from(bytes: &'bytes [u8]) -> Result<Self, Self::Error> {
        let header_bytes = size_of::<RootHeaderRecord>();
        if bytes.len() < header_bytes {
            return Err(RootReadError::HeaderTruncated {
                required: header_bytes.into(),
                available: bytes.len().into(),
            });
        }
        let Ok(header) = RootHeaderRecord::read_from_bytes(&bytes[..header_bytes]) else {
            return Err(RootReadError::HeaderTruncated {
                required: header_bytes.into(),
                available: bytes.len().into(),
            });
        };
        let declared = header.count.get();
        let count_u32 = u32::try_from(declared)
            .map_err(|source| RootReadError::CountOutOfRange { declared, source })?;
        let count = RootEntryCount::from(count_u32);
        let required = header_bytes
            .checked_add(
                (count_u32 as usize)
                    .checked_mul(ROOT_ROW_RECORD_BYTES)
                    .ok_or(RootReadError::LayoutOverflow {
                        count,
                        record_bytes: ROOT_ROW_RECORD_BYTES.into(),
                    })?,
            )
            .ok_or(RootReadError::LayoutOverflow {
                count,
                record_bytes: ROOT_ROW_RECORD_BYTES.into(),
            })?;
        if bytes.len() != required {
            return Err(RootReadError::Extent {
                count,
                required: required.into(),
                available: bytes.len().into(),
            });
        }
        // Schema provenance is checked from each raw fixed-width descriptor
        // before any typed slice is retained.
        for i in 0..count_u32 as usize {
            let start = header_bytes + i * ROOT_ROW_RECORD_BYTES;
            let key = EntryKey::from(raw_u64(&bytes[start..start + 8]));
            let descriptor_start = start + 17;
            ObjectRef::<DomainTag>::try_from(
                &bytes[descriptor_start
                    ..descriptor_start + nudox_object::OBJECT_DESCRIPTOR_RECORD_BYTES],
            )
            .map_err(|source| RootReadError::Descriptor {
                ordinal: RootEntryCount::from(i as u32),
                key,
                source,
            })?;
        }
        let row_bytes = &bytes[header_bytes..];
        let Ok(rows) =
            <[RootWireRecord]>::try_ref_from_bytes_with_elems(row_bytes, count_u32 as usize)
        else {
            return Err(RootReadError::Extent {
                count,
                required: required.into(),
                available: bytes.len().into(),
            });
        };
        let mut previous = None;
        // Descriptor/schema phase is intentionally complete before ordering and parents.
        for (i, row) in rows.iter().enumerate() {
            let ordinal = RootEntryCount::from(i as u32);
            let key = EntryKey::from(row.key.get());
            let _ = (ordinal, key);
        }
        for (i, row) in rows.iter().enumerate() {
            let ordinal = RootEntryCount::from(i as u32);
            let key = EntryKey::from(row.key.get());
            if let Some(prev) = previous {
                if key <= prev {
                    return Err(RootReadError::KeyOrder {
                        ordinal,
                        previous: prev,
                        current: key,
                    });
                }
            }
            previous = Some(key);
        }
        for (i, row) in rows.iter().enumerate() {
            let ordinal = RootEntryCount::from(i as u32);
            let key = EntryKey::from(row.key.get());
            if row.parent_present > 1 {
                return Err(RootReadError::ParentPresent {
                    ordinal,
                    key,
                    observed: row.parent_present,
                });
            }
            if row.parent_present == 0 && row.parent_key.get() != 0 {
                return Err(RootReadError::AbsentParentKey {
                    ordinal,
                    key,
                    observed: EntryKey::from(row.parent_key.get()),
                });
            }
            if row.parent_present == 1 {
                let parent = EntryKey::from(row.parent_key.get());
                if rows
                    .binary_search_by_key(&parent, |r| EntryKey::from(r.key.get()))
                    .is_err()
                {
                    return Err(RootReadError::MissingParent { child: key, parent });
                }
            }
        }
        let n = count_u32 as usize;
        let requested_bytes = n.checked_mul(size_of::<u32>()).ok_or(RootReadError::LayoutOverflow {
            count,
            record_bytes: size_of::<u32>().into(),
        })?;
        let mut scratch = Vec::new();
        if n != 0 {
            scratch.try_reserve_exact(n).map_err(|source| {
                RootReadError::HierarchyScratch {
                    entries: count,
                    requested_bytes: requested_bytes.into(),
                    source,
                }
            })?;
            scratch.resize(n, u32::MAX);
        }
        for (i, row) in rows.iter().enumerate() {
            scratch[i] = if row.parent_present == 0 {
                u32::MAX
            } else {
                match rows.binary_search_by_key(&row.parent_key.get(), |r| r.key.get()) {
                    Ok(parent) => parent as u32,
                    Err(_) => {
                        return Err(RootReadError::MissingParent {
                            child: EntryKey::from(row.key.get()),
                            parent: EntryKey::from(row.parent_key.get()),
                        });
                    }
                }
            };
        }
        for start in 0..n {
            validate_component(start, &mut scratch, rows)?;
        }
        drop(scratch);
        Ok(Self {
            facts: ValidatedRootFacts {
                bytes,
                id: GenerationId::from_canonical_bytes(bytes),
                entry_count: count,
            },
            rows,
            domain: PhantomData,
        })
    }
}
fn validate_component<DomainTag>(
    start: usize,
    parents: &mut [u32],
    rows: &[RootWireRecord],
) -> Result<(), RootReadError> {
    if parents[start] == u32::MAX {
        return Ok(());
    }
    let mut slow = start;
    let mut fast = start;
    loop {
        let slow_next = parents[slow];
        if slow_next == u32::MAX { break; }
        slow = slow_next as usize;
        let fast_next = parents[fast];
        if fast_next == u32::MAX { break; }
        fast = fast_next as usize;
        let fast_next = parents[fast];
        if fast_next == u32::MAX { break; }
        fast = fast_next as usize;
        if slow == fast {
            return Err(RootReadError::HierarchyCycle { key: EntryKey::from(rows[slow].key.get()) });
        }
    }
    let mut current = start;
    loop {
        let next = parents[current];
        parents[current] = u32::MAX;
        if next == u32::MAX { break; }
        current = next as usize;
    }
    Ok(())
}
fn raw_u64(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0_u64, |value, byte| (value << 8) | u64::from(*byte))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BorrowedGenerationViewFacts {
    pub id: GenerationId,
    pub entry_count: RootEntryCount,
}
pub struct BorrowedGenerationView<'root, 'locality, DomainTag> {
    facts: BorrowedGenerationViewFacts,
    root: &'root ValidatedRoot<'root, DomainTag>,
    locality: &'locality ValidatedLocality<'locality, DomainTag>,
}
impl<DomainTag> Deref for BorrowedGenerationView<'_, '_, DomainTag> {
    type Target = BorrowedGenerationViewFacts;
    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}
impl<'root, 'locality, DomainTag> BorrowedGenerationView<'root, 'locality, DomainTag> {
    pub fn new(
        root: &'root ValidatedRoot<'root, DomainTag>,
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
            facts: BorrowedGenerationViewFacts {
                id: root.id,
                entry_count: root.entry_count,
            },
            root,
            locality,
        })
    }
    pub const fn len(&self) -> usize {
        self.root.rows.len()
    }
    pub fn get(
        &self,
        key: EntryKey,
    ) -> Result<Option<GenerationEntry<DomainTag>>, LocalityReadError> {
        let Ok(i) = self
            .root
            .rows
            .binary_search_by_key(&key, |r| EntryKey::from(r.key.get()))
        else {
            return Ok(None);
        };
        self.entry(i)
    }
    fn entry(&self, i: usize) -> Result<Option<GenerationEntry<DomainTag>>, LocalityReadError> {
        let row = &self.root.rows[i];
        let idx = RowIndex::from_validated_borrowed_root_position(i);
        self.locality.locality_for(idx).map(|locality| {
            Some(GenerationEntry {
                key: EntryKey::from(row.key.get()),
                parent: (row.parent_present == 1).then(|| EntryKey::from(row.parent_key.get())),
                object: ObjectRef::from(&row.descriptor),
                locality,
            })
        })
    }
    pub fn closure(&self) -> BorrowedGenerationScan<'_, 'root, 'locality, DomainTag> {
        BorrowedGenerationScan {
            view: self,
            locality: self.locality.scan(),
            next: 0,
        }
    }
}
pub struct BorrowedGenerationScan<'view, 'root, 'locality, DomainTag> {
    view: &'view BorrowedGenerationView<'root, 'locality, DomainTag>,
    locality: LocalityCursor<'locality, DomainTag>,
    next: usize,
}
impl<DomainTag> Iterator for BorrowedGenerationScan<'_, '_, '_, DomainTag> {
    type Item = Result<GenerationEntry<DomainTag>, LocalityReadError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.view.len() {
            return None;
        }
        let i = self.next;
        self.next += 1;
        let row = &self.view.root.rows[i];
        let index = RowIndex::from_validated_borrowed_root_position(i);
        Some(
            self.locality
                .locality_without_work(index)
                .map(|locality| GenerationEntry {
                    key: EntryKey::from(row.key.get()),
                    parent: (row.parent_present == 1).then(|| EntryKey::from(row.parent_key.get())),
                    object: ObjectRef::from(&row.descriptor),
                    locality,
                }),
        )
    }
}
