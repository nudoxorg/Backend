//! Authenticated registry facts map.
//!
//! Receipt catalogs and forge-link mutations stay with the frontier module.
//! This module prepares and applies one content-addressed facts root.

use super::super::owner::AcquisitionError;
use super::PackageCoordinate;
use backend_store::{Change, OrderedMap, StoreError, StoredValue, UpdateStats};
use blake3::Hasher;

/// Errors raised while preparing or applying one authenticated facts root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum FactsMapError {
    /// The shared canonical ordered-map kernel rejected the transition.
    Store(StoreError),
    /// A prepared transition was based on a root that is no longer current.
    StaleBasis {
        /// Root observed while preparing the transition.
        expected: [u8; 32],
        /// Root observed while applying the transition.
        actual: [u8; 32],
    },
}

impl From<StoreError> for FactsMapError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

/// Maps storage-kernel failures into the registry owner's typed journal
/// failure. A malformed/corrupt kernel transition is history corruption;
/// explicit storage bounds remain an operator-visible acquisition bound.
pub(in crate::registry) fn facts_store_error(error: StoreError) -> AcquisitionError {
    match error {
        StoreError::Bounds | StoreError::OversizedKey => AcquisitionError::Bounds,
        StoreError::NeedsScopedRebuild => AcquisitionError::Bounds,
        _ => AcquisitionError::CorruptJournal,
    }
}

pub(super) fn facts_map_error(error: FactsMapError) -> AcquisitionError {
    match error {
        FactsMapError::Store(error) => facts_store_error(error),
        FactsMapError::StaleBasis { .. } => AcquisitionError::CorruptJournal,
    }
}

/// One immutable, prepared facts-root transition.
///
/// The transition retains the shared store tree returned by `OrderedMap`'s
/// path-copy update. Applying it only swaps one already-admitted root after a
/// stale-basis check; it does not clone the catalog, rehash unrelated rows, or
/// allocate on the journal commit path.
#[derive(Clone, Debug)]
pub(in crate::registry) struct PreparedFactsUpdate {
    base_root: [u8; 32],
    target_root: [u8; 32],
    map: OrderedMap,
    stats: UpdateStats,
}

impl PreparedFactsUpdate {
    pub(in crate::registry) fn target_root(&self) -> [u8; 32] {
        self.target_root
    }

    /// Returns the exact canonical node/byte work measured by preparation.
    #[must_use]
    pub(super) fn stats(&self) -> UpdateStats {
        self.stats
    }

    /// Swaps in the prepared tree when the live root still matches the basis.
    pub(super) fn apply(self, target: &mut FactsMerkleMap) -> Result<(), FactsMapError> {
        let actual = target.root();
        if actual != self.base_root {
            return Err(FactsMapError::StaleBasis {
                expected: self.base_root,
                actual,
            });
        }
        target.map = self.map;
        Ok(())
    }
}

/// Authenticated release-facts map backed by the repository's persistent
/// canonical ordered tree.
///
/// `OrderedMap` owns immutable canonical B-tree nodes, a weak content
/// interner, path-copy updates, exact stale-before checks, bounded proofs, and
/// changed-frontier statistics. This wrapper domain-separates its root for
/// registry facts while retaining those nodes by reference across generations.
/// An update therefore costs O(log_B N) canonical nodes and O(delta) values;
/// untouched pages/children retain their exact shared handles. Proof paths are
/// bounded by the store's fixed canonical tree height and leaf cut policy.
#[derive(Clone, Debug)]
pub(in crate::registry) struct FactsMerkleMap {
    map: OrderedMap,
}

/// A canonical B-tree proof never exceeds this path budget on the supported
/// store geometry. Keeping the check at the registry seam prevents a future
/// store-policy change from turning a GUI proof request into an unbounded
/// allocation.
const MAX_FACTS_PROOF_LEVELS: usize = usize::BITS as usize;

impl FactsMerkleMap {
    pub(in crate::registry) fn try_new() -> Result<Self, StoreError> {
        Ok(Self {
            map: OrderedMap::try_empty()?,
        })
    }

    pub(in crate::registry) fn root(&self) -> [u8; 32] {
        facts_root(self.map.state_root().as_bytes())
    }

    /// Prepares a strictly bounded batch of coordinate rows against this
    /// exact root. The input may be in arbitrary coordinate order; hashed keys
    /// are sorted before the shared ordered-map delta is admitted.
    pub(super) fn prepare_rows(
        &self,
        rows: &[(PackageCoordinate, [u8; 32])],
    ) -> Result<PreparedFactsUpdate, StoreError> {
        let mut changes = rows
            .iter()
            .map(|(coordinate, row)| {
                let key = facts_key(coordinate).to_vec();
                let before = self.map.get(&key).cloned();
                let after = StoredValue::new(row.to_vec(), 1, Vec::new());
                Change {
                    key,
                    before,
                    after: Some(after),
                }
            })
            .collect::<Vec<_>>();
        changes.sort_by(|left, right| left.key.cmp(&right.key));
        if changes.windows(2).any(|pair| pair[0].key == pair[1].key) {
            return Err(StoreError::MalformedDelta);
        }
        let base_root = self.root();
        let (map, stats) = self.map.apply_with_stats(&changes)?;
        let target_root = facts_root(map.state_root().as_bytes());
        Ok(PreparedFactsUpdate {
            base_root,
            target_root,
            map,
            stats,
        })
    }

    /// Builds a bounded proof for one facts row. The returned proof verifies
    /// directly against its underlying store root, so it does not borrow the
    /// owner or retain a catalog reference.
    pub(super) fn proof(&self, coordinate: &PackageCoordinate) -> Option<backend_store::KeyProof> {
        let proof = self.map.prove_key(&facts_key(coordinate));
        (proof.path.len() <= MAX_FACTS_PROOF_LEVELS).then_some(proof)
    }
}

fn facts_root(state_root: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(b"backend.registry.facts-root.v3\0");
    hasher.update(state_root);
    *hasher.finalize().as_bytes()
}

fn facts_key(coordinate: &PackageCoordinate) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(b"backend.registry.facts-key.v1\0");
    hasher.update(coordinate.as_str().as_bytes());
    *hasher.finalize().as_bytes()
}
