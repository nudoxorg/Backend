//! Exact graph replacement and tombstone overlay.

use crate::contracts::GraphRow;
use crate::delta::GraphDelta;
use crate::{Binding, Error};
use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;
use std::sync::Arc;

/// Exact replacement/delete overlay for one graph transition range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphOverlay {
    binding: Binding,
    // The tombstone list is the only base-hiding arrangement. Replacement
    // rows are stored separately because their values are returned directly;
    // neither structure duplicates the immutable base or becomes a second
    // graph index.
    tombstones: Arc<[u64]>,
    replacements: Arc<BTreeMap<u64, Arc<GraphRow>>>,
    bytes: usize,
}

impl GraphOverlay {
    pub(super) fn from_delta(delta: &GraphDelta) -> Result<Self, Error> {
        let (tombstones, replacements, bytes) = overlay_parts(delta)?;
        Ok(Self {
            binding: delta.binding,
            tombstones: Arc::from(tombstones.into_iter().collect::<Vec<_>>()),
            replacements: Arc::new(replacements),
            bytes,
        })
    }

    pub(super) fn merge_delta(&self, delta: &GraphDelta) -> Result<Self, Error> {
        if delta.delta.base() != self.binding.root {
            return Err(Error::StaleRoot);
        }
        let mut tombstones = self.tombstones.iter().copied().collect::<BTreeSet<_>>();
        let mut replacements = self
            .replacements
            .iter()
            .map(|(key, row)| (*key, Arc::clone(row)))
            .collect::<BTreeMap<_, _>>();
        for change in delta.delta.changes() {
            tombstones.insert(change.key);
            replacements.remove(&change.key);
            if let Some(values) = &change.after {
                replacements.insert(
                    change.key,
                    Arc::new(GraphRow {
                        key: change.key,
                        values: values.clone(),
                    }),
                );
            }
        }
        let bytes = overlay_bytes(&tombstones, &replacements)?;
        Ok(Self {
            binding: delta.binding,
            tombstones: Arc::from(tombstones.into_iter().collect::<Vec<_>>()),
            replacements: Arc::new(replacements),
            bytes,
        })
    }

    /// Returns the exact target binding.
    #[must_use]
    pub fn binding(&self) -> Binding {
        self.binding
    }

    /// Returns hidden base keys in canonical order.
    #[must_use]
    pub fn tombstones(&self) -> &[u64] {
        &self.tombstones
    }

    /// Returns replacement rows without allocating.
    #[must_use]
    pub fn replacements(&self) -> &BTreeMap<u64, Arc<GraphRow>> {
        &self.replacements
    }

    /// Returns retained replacement bytes.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }
}

type OverlayParts = (BTreeSet<u64>, BTreeMap<u64, Arc<GraphRow>>, usize);

fn overlay_parts(delta: &GraphDelta) -> Result<OverlayParts, Error> {
    let mut tombstones = BTreeSet::new();
    let mut replacements = BTreeMap::new();
    for change in delta.delta.changes() {
        tombstones.insert(change.key);
        if let Some(values) = &change.after {
            replacements.insert(
                change.key,
                Arc::new(GraphRow {
                    key: change.key,
                    values: values.clone(),
                }),
            );
        }
    }
    let bytes = overlay_bytes(&tombstones, &replacements)?;
    Ok((tombstones, replacements, bytes))
}

fn overlay_bytes(
    tombstones: &BTreeSet<u64>,
    replacements: &BTreeMap<u64, Arc<GraphRow>>,
) -> Result<usize, Error> {
    let tombstone_bytes = tombstones
        .len()
        .checked_mul(size_of::<u64>())
        .ok_or(Error::SizeLimit)?;
    replacements
        .iter()
        .try_fold(tombstone_bytes, |bytes, (_, row)| {
            let bytes = bytes
                .checked_add(size_of::<u64>())
                .and_then(|bytes| bytes.checked_add(size_of::<u64>()))
                .ok_or(Error::SizeLimit)?;
            row.values.iter().try_fold(bytes, |bytes, value| {
                bytes.checked_add(value.len()).ok_or(Error::SizeLimit)
            })
        })
}
