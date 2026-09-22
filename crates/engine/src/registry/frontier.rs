//! Authenticated registry facts and normalized forge-lineage mutations.
//!
//! The owner coordinates durable intent and journal commit; this module owns
//! the bounded authenticated-map update and the small prepare/apply mutation
//! used by the normalized forge association table.

use std::collections::BTreeMap;

use backend_store::{Change, OrderedMap, StoreError, StoredValue, UpdateStats};
use blake3::Hasher;

use super::PackageCoordinate;
use super::owner::{AcquisitionError, AcquisitionReceipt, PublishedPackage};

pub(super) fn validate_receipt_catalog(
    catalog: &BTreeMap<PackageCoordinate, PublishedPackage>,
    forge_associations: &BTreeMap<[u8; 32], backend_library::RegistryForgeAssociation>,
    receipt: &AcquisitionReceipt,
    maximum: usize,
) -> Result<(), AcquisitionError> {
    if receipt
        .packages
        .windows(2)
        .any(|pair| pair[0].coordinate >= pair[1].coordinate)
    {
        return Err(AcquisitionError::CorruptJournal);
    }
    let mut additional = 0usize;
    for package in &receipt.packages {
        if !valid_forge_source_ids(package, forge_associations) {
            return Err(AcquisitionError::CorruptJournal);
        }
        match catalog.get(&package.coordinate) {
            Some(existing) if existing.artifact != package.artifact => {
                return Err(AcquisitionError::CorruptJournal);
            }
            Some(_) => {}
            None => additional = additional.checked_add(1).ok_or(AcquisitionError::Bounds)?,
        }
    }
    let total = catalog
        .len()
        .checked_add(additional)
        .ok_or(AcquisitionError::Bounds)?;
    if total > maximum {
        return Err(AcquisitionError::Overrun {
            measured: u64::try_from(total).map_err(|_| AcquisitionError::Bounds)?,
            limit: u64::try_from(maximum).map_err(|_| AcquisitionError::Bounds)?,
        });
    }
    Ok(())
}

pub(super) fn prepare_receipt_facts(
    facts_map: &FactsMerkleMap,
    receipt: &AcquisitionReceipt,
) -> Result<PreparedFactsUpdate, AcquisitionError> {
    let rows = receipt
        .packages
        .iter()
        .map(|package| (package.coordinate.clone(), package_facts_digest(package)))
        .collect::<Vec<_>>();
    facts_map.prepare_rows(&rows).map_err(facts_store_error)
}

pub(super) fn apply_receipt_to_catalog(
    catalog: &mut BTreeMap<PackageCoordinate, PublishedPackage>,
    forge_associations: &mut BTreeMap<[u8; 32], backend_library::RegistryForgeAssociation>,
    forge_refcounts: &mut BTreeMap<[u8; 32], u32>,
    facts_map: &mut FactsMerkleMap,
    receipt: &AcquisitionReceipt,
    prepared_facts: PreparedFactsUpdate,
) -> Result<(), AcquisitionError> {
    prepared_facts.apply(facts_map).map_err(facts_map_error)?;
    for package in &receipt.packages {
        let previous_ids = catalog
            .get(&package.coordinate)
            .map(|existing| existing.forge_source_ids.clone())
            .unwrap_or_default();
        update_forge_refcounts(
            forge_associations,
            forge_refcounts,
            &previous_ids,
            &package.forge_source_ids,
        );
        catalog.insert(package.coordinate.clone(), package.clone());
    }
    Ok(())
}

pub(super) struct PreparedForgeLink {
    coordinate: PackageCoordinate,
    associations: Box<[backend_library::RegistryForgeAssociation]>,
    association_ids: Box<[[u8; 32]]>,
    previous_ids: Box<[[u8; 32]]>,
    facts: PreparedFactsUpdate,
}

impl PreparedForgeLink {
    pub(super) fn associations(&self) -> &[backend_library::RegistryForgeAssociation] {
        &self.associations
    }

    pub(super) fn target_root(&self) -> [u8; 32] {
        self.facts.target_root()
    }
}

pub(super) fn prepare_forge_link(
    catalog: &BTreeMap<PackageCoordinate, PublishedPackage>,
    forge_associations: &BTreeMap<[u8; 32], backend_library::RegistryForgeAssociation>,
    facts_map: &FactsMerkleMap,
    coordinate: &PackageCoordinate,
    associations: Box<[backend_library::RegistryForgeAssociation]>,
) -> Result<PreparedForgeLink, AcquisitionError> {
    if associations.len() > backend_library::MAX_REGISTRY_FORGE_ASSOCIATIONS {
        return Err(AcquisitionError::Bounds);
    }
    let package = catalog
        .get(coordinate)
        .ok_or(AcquisitionError::CorruptJournal)?;
    let mut associations = associations.into_vec();
    associations.sort_by_key(|association| association.facts_version);
    let associations = associations.into_boxed_slice();
    let registry = backend_library::PackageReference::Purl(coordinate.clone());
    let mut association_ids = associations
        .iter()
        .map(|association| {
            association
                .admit_for_registry(&registry)
                .map(|()| association.facts_version)
                .map_err(|_| AcquisitionError::CorruptJournal)
        })
        .collect::<Result<Vec<_>, _>>()?;
    association_ids.sort_unstable();
    if association_ids
        .windows(2)
        .any(|window| window[0] == window[1])
    {
        return Err(AcquisitionError::CorruptJournal);
    }
    for association in &associations {
        match forge_associations.get(&association.facts_version) {
            Some(existing) if existing != association => {
                return Err(AcquisitionError::CorruptJournal);
            }
            Some(_) | None => {}
        }
    }
    if !valid_forge_source_ids(package, forge_associations) {
        return Err(AcquisitionError::CorruptJournal);
    }
    let mut next_package = package.clone();
    next_package.forge_source_ids = association_ids.clone().into();
    let facts = facts_map
        .prepare_rows(&[(coordinate.clone(), package_facts_digest(&next_package))])
        .map_err(facts_store_error)?;
    Ok(PreparedForgeLink {
        coordinate: coordinate.clone(),
        associations,
        association_ids: association_ids.into_boxed_slice(),
        previous_ids: package.forge_source_ids.clone(),
        facts,
    })
}

pub(super) fn apply_prepared_forge_link(
    catalog: &mut BTreeMap<PackageCoordinate, PublishedPackage>,
    forge_associations: &mut BTreeMap<[u8; 32], backend_library::RegistryForgeAssociation>,
    forge_refcounts: &mut BTreeMap<[u8; 32], u32>,
    facts_map: &mut FactsMerkleMap,
    prepared: PreparedForgeLink,
) -> Result<(), AcquisitionError> {
    let mut next_package = catalog
        .get(&prepared.coordinate)
        .cloned()
        .ok_or(AcquisitionError::CorruptJournal)?;
    next_package.forge_source_ids = prepared.association_ids.clone();
    let target_root = prepared.facts.target_root();
    prepared.facts.apply(facts_map).map_err(facts_map_error)?;
    let row_digest = package_facts_digest(&next_package);
    catalog.insert(prepared.coordinate.clone(), next_package);
    for association in &prepared.associations {
        forge_associations
            .entry(association.facts_version)
            .or_insert_with(|| association.clone());
    }
    update_forge_refcounts(
        forge_associations,
        forge_refcounts,
        &prepared.previous_ids,
        &prepared.association_ids,
    );
    debug_assert_eq!(facts_map.root(), target_root);
    debug_assert!(
        catalog
            .get(&prepared.coordinate)
            .is_some_and(|package| row_digest == package_facts_digest(package))
    );
    Ok(())
}

fn update_forge_refcounts(
    forge_associations: &mut BTreeMap<[u8; 32], backend_library::RegistryForgeAssociation>,
    forge_refcounts: &mut BTreeMap<[u8; 32], u32>,
    previous_ids: &[[u8; 32]],
    next_ids: &[[u8; 32]],
) {
    for association_id in previous_ids {
        if next_ids.binary_search(association_id).is_ok() {
            continue;
        }
        match forge_refcounts.get_mut(association_id) {
            Some(count) if *count > 1 => *count -= 1,
            Some(_) => {
                forge_refcounts.remove(association_id);
                forge_associations.remove(association_id);
            }
            None => {
                debug_assert!(false, "missing forge association refcount during apply");
                forge_associations.remove(association_id);
            }
        }
    }
    for association_id in next_ids {
        if previous_ids.binary_search(association_id).is_ok() {
            continue;
        }
        let count = forge_refcounts.entry(*association_id).or_insert(0);
        *count = count.saturating_add(1);
    }
}

fn forge_source_ids_sorted_unique(ids: &[[u8; 32]]) -> bool {
    ids.windows(2).all(|window| window[0] < window[1])
}

fn package_facts_digest(package: &PublishedPackage) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.registry.facts-row.v2\0");
    let coordinate = package.coordinate.as_str().as_bytes();
    hasher.update(&(coordinate.len() as u64).to_be_bytes());
    hasher.update(coordinate);
    hasher.update(&package.artifact.as_bytes());
    hasher.update(&package.bytes.to_be_bytes());
    hasher.update(&package.facts.version());
    let native_metadata = package.native_metadata.encode_canonical();
    if let Ok(identity) = package.native_metadata.identity() {
        hasher.update(&identity);
    } else {
        hasher.update(&(native_metadata.len() as u64).to_be_bytes());
        hasher.update(&native_metadata);
    }
    if let Ok(bytes) = serde_json::to_vec(&package.advisory) {
        hasher.update(&(bytes.len() as u64).to_be_bytes());
        hasher.update(&bytes);
    } else {
        hasher.update(&0_u64.to_be_bytes());
    }
    hasher.update(b"forge-lineage\0");
    hasher.update(
        &u64::try_from(package.forge_source_ids.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for association_id in &package.forge_source_ids {
        hasher.update(association_id);
    }
    *hasher.finalize().as_bytes()
}

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
pub(super) fn facts_store_error(error: StoreError) -> AcquisitionError {
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
pub(super) struct PreparedFactsUpdate {
    base_root: [u8; 32],
    target_root: [u8; 32],
    map: OrderedMap,
    stats: UpdateStats,
}

impl PreparedFactsUpdate {
    pub(super) fn target_root(&self) -> [u8; 32] {
        self.target_root
    }

    /// Returns the exact canonical node/byte work measured by preparation.
    #[must_use]
    pub(super) fn stats(&self) -> UpdateStats {
        self.stats
    }

    fn apply(self, target: &mut FactsMerkleMap) -> Result<(), FactsMapError> {
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
pub(super) struct FactsMerkleMap {
    map: OrderedMap,
}

/// A canonical B-tree proof never exceeds this path budget on the supported
/// store geometry. Keeping the check at the registry seam prevents a future
/// store-policy change from turning a GUI proof request into an unbounded
/// allocation.
const MAX_FACTS_PROOF_LEVELS: usize = usize::BITS as usize;

impl FactsMerkleMap {
    pub(super) fn try_new() -> Result<Self, StoreError> {
        Ok(Self {
            map: OrderedMap::try_empty()?,
        })
    }

    pub(super) fn root(&self) -> [u8; 32] {
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
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.registry.facts-key.v1\0");
    hasher.update(coordinate.as_str().as_bytes());
    *hasher.finalize().as_bytes()
}

pub(super) fn valid_forge_source_ids(
    package: &PublishedPackage,
    forge_associations: &BTreeMap<[u8; 32], backend_library::RegistryForgeAssociation>,
) -> bool {
    if package.forge_source_ids.len() > backend_library::MAX_REGISTRY_FORGE_ASSOCIATIONS {
        return false;
    }
    if !forge_source_ids_sorted_unique(&package.forge_source_ids) {
        return false;
    }
    package.forge_source_ids.iter().all(|association_id| {
        forge_associations
            .get(association_id)
            .is_some_and(|association| {
                association
                    .admit_for_registry(&backend_library::PackageReference::Purl(
                        package.coordinate.clone(),
                    ))
                    .is_ok()
            })
    })
}

#[cfg(test)]
mod forge_frontier_tests {
    use super::*;

    #[test]
    fn facts_root_is_order_independent_and_updates_one_persistent_delta() {
        let first = PackageCoordinate::parse("pkg:cargo/first@1.0.0").expect("first");
        let second = PackageCoordinate::parse("pkg:cargo/second@1.0.0").expect("second");
        let mut left = FactsMerkleMap::try_new().expect("empty facts map");
        apply_facts(&mut left, &first, [1; 32]);
        let before = left.root();
        apply_facts(&mut left, &second, [2; 32]);
        let after = left.root();
        assert_ne!(before, after);

        let mut right = FactsMerkleMap::try_new().expect("empty facts map");
        apply_facts(&mut right, &second, [2; 32]);
        apply_facts(&mut right, &first, [1; 32]);
        assert_eq!(right.root(), after);

        apply_facts(&mut left, &first, [3; 32]);
        assert_ne!(left.root(), after);
    }

    #[test]
    fn facts_updates_retain_shared_nodes_and_bound_proofs() {
        let mut rows = Vec::new();
        for index in 0..1_024_u32 {
            let coordinate = PackageCoordinate::parse(&format!("pkg:cargo/facts-{index}@1.0.0"))
                .expect("coordinate");
            rows.push((coordinate, [u8::try_from(index % 251).expect("byte"); 32]));
        }
        let base = FactsMerkleMap::try_new().expect("empty facts map");
        let prepared = base.prepare_rows(&rows).expect("batch facts update");
        let stats = prepared.stats();
        let height_bound = usize::BITS as usize - rows.len().max(1).leading_zeros() as usize + 1;
        assert!(stats.copied_nodes <= rows.len() + height_bound);
        assert!(stats.encoded_bytes >= rows.len());
        let mut map = base.clone();
        prepared.apply(&mut map).expect("apply facts update");
        assert_eq!(map.map.len(), rows.len());

        let same = map
            .prepare_rows(&[(rows[rows.len() / 2].0.clone(), rows[rows.len() / 2].1)])
            .expect("no-op facts update");
        assert_eq!(same.stats().copied_nodes, 0);
        assert_eq!(same.target_root(), map.root());

        let edited = map
            .prepare_rows(&[(rows[rows.len() / 2].0.clone(), [7; 32])])
            .expect("single-row facts update");
        let edit_stats = edited.stats();
        assert!(edit_stats.reused_nodes > edit_stats.copied_nodes);
        assert!(edit_stats.copied_nodes <= height_bound);
        let mut edited_map = map.clone();
        edited
            .apply(&mut edited_map)
            .expect("apply single-row update");
        let diff = map.map.diff_stats(&edited_map.map);
        assert!(diff.visited_nodes < rows.len());

        let proof = map.proof(&rows[rows.len() / 2].0).expect("bounded proof");
        assert!(proof.path.len() <= 64);
        assert!(proof.verify(&map.map));
        assert!(proof.verify_root(proof.root));

        let stale = base
            .prepare_rows(&[(rows[0].0.clone(), [9; 32])])
            .expect("stale preparation");
        let mut advanced = map.clone();
        assert!(matches!(
            stale.apply(&mut advanced),
            Err(FactsMapError::StaleBasis { .. })
        ));
    }

    fn apply_facts(map: &mut FactsMerkleMap, coordinate: &PackageCoordinate, row: [u8; 32]) {
        let prepared = map
            .prepare_rows(&[(coordinate.clone(), row)])
            .expect("facts update");
        prepared.apply(map).expect("apply facts update");
    }

    #[test]
    fn forge_reference_counts_gc_without_scanning_catalog_rows() {
        let association = backend_library::RegistryForgeAssociation::new(
            backend_library::PackageReference::parse("pkg:cargo/demo@1.0.0").expect("package"),
            Box::new([]),
            backend_library::RegistryForgeAssociationState::Unavailable {
                reason: backend_library::ForgeUnavailableReason::Offline,
                blobs: Box::new([]),
            },
            backend_library::RegistryForgeProvenance::RegistryMetadata,
        )
        .expect("association");
        let id = association.facts_version;
        let mut associations = BTreeMap::from([(id, association)]);
        let mut counts = BTreeMap::from([(id, 2_u32)]);
        update_forge_refcounts(&mut associations, &mut counts, &[id], &[]);
        assert_eq!(counts.get(&id), Some(&1));
        assert!(associations.contains_key(&id));
        update_forge_refcounts(&mut associations, &mut counts, &[id], &[]);
        assert!(counts.is_empty());
        assert!(associations.is_empty());
    }
}
