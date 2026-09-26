//! Authenticated registry facts and normalized forge-lineage mutations.
//!
//! The owner coordinates durable intent and journal commit; this module owns
//! the bounded authenticated-map update and the small prepare/apply mutation
//! used by the normalized forge association table.

use std::collections::BTreeMap;

use super::PackageCoordinate;
use super::owner::{AcquisitionError, AcquisitionReceipt, PublishedPackage};

mod facts;
#[cfg(test)]
use facts::FactsMapError;
pub(super) use facts::{FactsMerkleMap, facts_store_error};
use facts::{PreparedFactsUpdate, facts_map_error};

fn validate_coordinates_increasing<'a>(
    coordinates: impl IntoIterator<Item = &'a PackageCoordinate>,
) -> Result<(), AcquisitionError> {
    let mut previous: Option<&PackageCoordinate> = None;
    for coordinate in coordinates {
        if previous.is_some_and(|prev| prev >= coordinate) {
            return Err(AcquisitionError::CorruptJournal);
        }
        previous = Some(coordinate);
    }
    Ok(())
}

fn validate_catalog_overrun(
    catalog: &BTreeMap<PackageCoordinate, PublishedPackage>,
    additional: usize,
    maximum: usize,
) -> Result<(), AcquisitionError> {
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

pub(super) fn validate_receipt_catalog(
    catalog: &BTreeMap<PackageCoordinate, PublishedPackage>,
    forge_associations: &BTreeMap<[u8; 32], backend_library::RegistryForgeAssociation>,
    receipt: &AcquisitionReceipt,
    maximum: usize,
) -> Result<(), AcquisitionError> {
    validate_coordinates_increasing(receipt.packages.iter().map(|package| &package.coordinate))?;
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
    validate_catalog_overrun(catalog, additional, maximum)
}

/// Checks the catalog capacity and ordering of a feed page before archive
/// staging begins. This keeps a measured catalog overrun ahead of archive I/O,
/// so an otherwise valid page cannot fail with an unrelated archive result.
pub(super) fn validate_page_catalog(
    catalog: &BTreeMap<PackageCoordinate, PublishedPackage>,
    packages: &[super::RemotePackage],
    maximum: usize,
) -> Result<(), AcquisitionError> {
    validate_coordinates_increasing(packages.iter().map(|package| &package.coordinate))?;
    let mut additional = 0usize;
    for package in packages {
        if !catalog.contains_key(&package.coordinate) {
            additional = additional.checked_add(1).ok_or(AcquisitionError::Bounds)?;
        }
    }
    validate_catalog_overrun(catalog, additional, maximum)
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
    use std::sync::Arc;

    use crate::acquisition::RawArchiveObjectId;
    use crate::effects::effect_key;
    use backend_advisory::AdvisoryPackageDto;

    use crate::registry::owner::AcquisitionReceipt;
    use crate::registry::transport::ArchiveIntegrity;
    use crate::registry::{
        CanonicalFeedV1, FeedCursor, ProvenanceDigest, PublishedArtifactClaim, RegistryEcosystem,
        RegistryId, ReleaseFacts, RemotePackage, RemoteRegistry, admit_registry_coordinate,
    };

    use super::*;

    fn test_coordinate(index: u8) -> PackageCoordinate {
        PackageCoordinate::parse(format!("pkg:cargo/catalog-{index}@1.0.0")).expect("coordinate")
    }

    fn test_native_metadata() -> backend_library::RegistryNativeMetadata {
        backend_library::RegistryNativeMetadata::unavailable(
            RegistryEcosystem::Cargo,
            "test fixture",
        )
    }

    fn test_facts() -> ReleaseFacts {
        let metadata = test_native_metadata();
        ReleaseFacts::default().with_native_metadata(metadata.identity().expect("test metadata"))
    }

    fn unavailable_dependency_facts()
    -> backend_library::DependencyFacts<Box<[backend_library::PackageDependencyRecord]>> {
        backend_library::DependencyFacts::Unavailable(
            backend_library::ProductText::new("test dependency metadata unavailable")
                .expect("bounded test dependency reason"),
        )
    }

    fn test_artifact(seed: u8) -> PublishedArtifactClaim {
        PublishedArtifactClaim::from_journal([seed; 32])
    }

    fn minimal_published_package(
        coordinate: PackageCoordinate,
        artifact: PublishedArtifactClaim,
    ) -> PublishedPackage {
        let registry = admit_registry_coordinate(&coordinate).expect("registry");
        PublishedPackage {
            coordinate,
            registry,
            artifact,
            raw_object: RawArchiveObjectId::from_bytes(b"archive"),
            bytes: 7,
            provenance: ProvenanceDigest::from_authenticated_feed([3; 32]),
            upstream_integrity: [4; 32],
            facts: test_facts(),
            native_metadata: test_native_metadata(),
            forge_source_ids: Box::new([]),
            advisory: AdvisoryPackageDto::unknown(),
            dependency_facts: unavailable_dependency_facts(),
        }
    }

    fn minimal_remote_package(coordinate: PackageCoordinate) -> RemotePackage {
        RemotePackage {
            coordinate,
            integrity: ArchiveIntegrity::Canonical([5; 32]),
            provenance: ProvenanceDigest::from_authenticated_feed([6; 32]),
            facts: test_facts(),
            native_metadata: test_native_metadata(),
            advisory: None,
            dependency_facts: unavailable_dependency_facts(),
            archive_url: Arc::from("http://127.0.0.1:9/archive"),
        }
    }

    fn minimal_receipt(packages: Vec<PublishedPackage>) -> AcquisitionReceipt {
        let registry = RegistryId::from_bytes([1; 32]);
        let cursor = FeedCursor::<RemoteRegistry, CanonicalFeedV1>::genesis(registry);
        AcquisitionReceipt {
            effect: effect_key(b"catalog-test"),
            base: cursor,
            target: cursor,
            packages,
            facts_root: [0; 32],
        }
    }

    fn catalog_with(coordinates: &[u8]) -> BTreeMap<PackageCoordinate, PublishedPackage> {
        coordinates
            .iter()
            .map(|index| {
                let coordinate = test_coordinate(*index);
                let package = minimal_published_package(coordinate.clone(), test_artifact(*index));
                (coordinate, package)
            })
            .collect()
    }

    #[test]
    fn catalog_capacity_rejects_unsorted_coordinates() {
        let catalog = BTreeMap::new();
        let first = test_coordinate(1);
        let second = test_coordinate(2);
        let coordinates = [&second, &first];
        assert!(matches!(
            validate_coordinates_increasing(coordinates.iter().copied()),
            Err(AcquisitionError::CorruptJournal)
        ));

        let receipt = minimal_receipt(vec![
            minimal_published_package(second.clone(), test_artifact(2)),
            minimal_published_package(first.clone(), test_artifact(1)),
        ]);
        assert!(matches!(
            validate_receipt_catalog(&catalog, &BTreeMap::new(), &receipt, 8),
            Err(AcquisitionError::CorruptJournal)
        ));

        let packages = [
            minimal_remote_package(second.clone()),
            minimal_remote_package(first.clone()),
        ];
        assert!(matches!(
            validate_page_catalog(&catalog, &packages, 8),
            Err(AcquisitionError::CorruptJournal)
        ));
    }

    #[test]
    fn catalog_capacity_accepts_existing_and_rejects_overrun() {
        const MAXIMUM: usize = 2;
        let existing = test_coordinate(1);
        let newcomer = test_coordinate(3);
        let catalog = catalog_with(&[1, 2]);
        let artifact = catalog.get(&existing).expect("existing").artifact;

        let receipt_existing =
            minimal_receipt(vec![minimal_published_package(existing.clone(), artifact)]);
        assert!(
            validate_receipt_catalog(&catalog, &BTreeMap::new(), &receipt_existing, MAXIMUM)
                .is_ok()
        );

        let receipt_new = minimal_receipt(vec![minimal_published_package(
            newcomer.clone(),
            test_artifact(3),
        )]);
        let receipt_overrun =
            validate_receipt_catalog(&catalog, &BTreeMap::new(), &receipt_new, MAXIMUM);
        assert!(matches!(
            receipt_overrun,
            Err(AcquisitionError::Overrun {
                measured: 3,
                limit: 2,
            })
        ));

        let page_existing = [minimal_remote_package(existing.clone())];
        assert!(validate_page_catalog(&catalog, &page_existing, MAXIMUM).is_ok());

        let page_new = [minimal_remote_package(newcomer.clone())];
        let page_overrun = validate_page_catalog(&catalog, &page_new, MAXIMUM);
        assert!(matches!(
            page_overrun,
            Err(AcquisitionError::Overrun {
                measured: 3,
                limit: 2,
            })
        ));
    }

    #[test]
    fn receipt_artifact_mismatch_precedes_catalog_overrun() {
        const MAXIMUM: usize = 2;
        let catalog = catalog_with(&[1, 2]);
        let existing = test_coordinate(1);
        let newcomer = test_coordinate(3);
        let receipt = minimal_receipt(vec![
            minimal_published_package(existing.clone(), test_artifact(99)),
            minimal_published_package(newcomer, test_artifact(3)),
        ]);
        assert!(matches!(
            validate_receipt_catalog(&catalog, &BTreeMap::new(), &receipt, MAXIMUM),
            Err(AcquisitionError::CorruptJournal)
        ));
    }

    #[test]
    fn catalog_capacity_accepts_first_coordinate_on_empty_catalog() {
        let catalog = BTreeMap::new();
        let coordinate = test_coordinate(1);

        let receipt = minimal_receipt(vec![minimal_published_package(
            coordinate.clone(),
            test_artifact(1),
        )]);
        assert!(validate_receipt_catalog(&catalog, &BTreeMap::new(), &receipt, 4).is_ok());

        let packages = [minimal_remote_package(coordinate.clone())];
        assert!(validate_page_catalog(&catalog, &packages, 4).is_ok());
    }

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
        let page_count = map.map.leaf_cuts().len();
        assert!(
            page_count > 1,
            "the batched workload must span persistent pages: rows={}, pages={page_count}",
            rows.len()
        );
        assert!(
            stats.copied_nodes <= page_count + height_bound,
            "batch node growth must be pages plus tree height: rows={}, pages={page_count}, stats={stats:?}, bound={height_bound}",
            rows.len()
        );

        let same = map
            .prepare_rows(&[(rows[rows.len() / 2].0.clone(), rows[rows.len() / 2].1)])
            .expect("no-op facts update");
        assert_eq!(same.stats().copied_nodes, 0);
        assert_eq!(same.target_root(), map.root());

        let edited = map
            .prepare_rows(&[(rows[rows.len() / 2].0.clone(), [7; 32])])
            .expect("single-row facts update");
        let edit_stats = edited.stats();
        // `reused_nodes` counts unchanged child handles retained by rebuilt
        // branches, while `copied_nodes` counts the rebuilt path itself. They
        // are different units, so structural reuse is proven by a retained
        // child plus a path-sized copy bound rather than by comparing totals.
        assert!(
            edit_stats.reused_nodes > 0,
            "single-row update must retain an untouched child: stats={edit_stats:?}"
        );
        assert!(
            edit_stats.copied_nodes <= height_bound,
            "single-row update rebuilt beyond derived height bound: stats={edit_stats:?}, bound={height_bound}"
        );
        let mut edited_map = map.clone();
        edited
            .apply(&mut edited_map)
            .expect("apply single-row update");
        let diff = map.map.diff_stats(&edited_map.map);
        assert_eq!(
            diff.changed_leaves, 1,
            "single-row edit should change exactly one leaf: diff={diff:?}"
        );
        assert!(
            diff.visited_nodes <= height_bound.saturating_add(1),
            "single-row diff should stay on a bounded path: diff={diff:?}, bound={height_bound}"
        );

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
