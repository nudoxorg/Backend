use std::{cell::Cell, collections::BTreeMap};

use backend_version::{
    CANONICAL_CUT_POLICY_VERSION, CANONICAL_TREE_ABI, CheckedCanonicalRoot, IdContext,
    LazyTreeMetadataShape, LazyTreeUpdateBudget, PersistedTreeRoot, SchemaIdentity, TreeChange,
    TreeNodeLoader, UntrustedId, state_root_digest,
};
use blake3::Hasher;

use crate::{
    ir::{
        row_index::{RowFamily, RowPayload, StableRowKey, UntrustedRowPayloadIdentity},
        typed_plane_manifest_v3::{
            FAMILY_COUNT, FAMILY_DESCRIPTOR_BYTES, LazySemanticTypedPlaneFamilyIndexV3,
            MAX_TYPED_PLANE_MANIFEST_V3_BYTES, SemanticRowObjectIdV3,
            SemanticRowPayloadReferenceV3, SemanticTypedPlaneClosureLimitsV3,
            SemanticTypedPlaneIndexCatalogV3, SemanticTypedPlaneIndexV3Error,
            SemanticTypedPlaneRowRelationV3, SemanticTypedPlaneRowTreeBuilderV3,
            SemanticTypedPlaneRowTreeLimitsV3,
        },
    },
    vocabulary::{LanguageProfile, RustEdition},
};

fn reference(payload_byte: u8, byte_len: u64, object_byte: u8) -> SemanticRowPayloadReferenceV3 {
    SemanticRowPayloadReferenceV3::from_untrusted_claims(
        UntrustedRowPayloadIdentity::from_raw([payload_byte; 32], byte_len),
        SemanticRowObjectIdV3::from_raw([object_byte; 32]),
    )
}

fn ordinal_key(family: RowFamily, ordinal: u32) -> StableRowKey {
    let mut bytes = [0_u8; 32];
    bytes[..4].copy_from_slice(&ordinal.to_be_bytes());
    bytes[27] = ordinal.wrapping_mul(17) as u8;
    StableRowKey::new(family, bytes)
}

fn append_field(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    output.extend_from_slice(bytes);
}

fn build_family(
    family: RowFamily,
    profile: Option<LanguageProfile>,
    rows: &[(StableRowKey, SemanticRowPayloadReferenceV3)],
) -> super::SemanticTypedPlaneFamilyIndexV3 {
    let mut builder = SemanticTypedPlaneRowTreeBuilderV3::new(
        family,
        profile,
        SemanticTypedPlaneRowTreeLimitsV3::new(20_000, 100_000_000),
    )
    .expect("fixture family/profile is closed");
    for (key, value) in rows {
        builder
            .push(*key, *value)
            .expect("fixture rows are ordered");
    }
    builder.finish().expect("fixture tree is canonical")
}

fn independent_family_root(
    family: RowFamily,
    profile: Option<LanguageProfile>,
    row_count: u64,
    tree_root: &[u8; 32],
) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(b"backend.semantic.typed-plane-family-root.v3\0");
    hasher.update(&1_u16.to_be_bytes());
    hasher.update(&[family.code()]);
    match profile {
        Some(profile) => {
            hasher.update(&[1]);
            hasher.update(&<[u8; 2]>::from(profile));
        }
        None => {
            hasher.update(&[0, 0, 0]);
        }
    }
    hasher.update(&row_count.to_be_bytes());
    hasher.update(tree_root);
    *hasher.finalize().as_bytes()
}

fn independent_catalog_root(family_roots: &[[u8; 32]; FAMILY_COUNT]) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(b"backend.semantic.typed-plane-index-catalog.v3\0");
    hasher.update(&1_u16.to_be_bytes());
    for family_root in family_roots {
        hasher.update(family_root);
    }
    *hasher.finalize().as_bytes()
}

fn independent_catalog_wire(
    root: [u8; 32],
    claims: &[(RowFamily, Option<LanguageProfile>, u64, [u8; 32], [u8; 32]); FAMILY_COUNT],
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(MAX_TYPED_PLANE_MANIFEST_V3_BYTES);
    bytes.extend_from_slice(b"STPI");
    bytes.extend_from_slice(&1_u16.to_be_bytes());
    bytes.extend_from_slice(&root);
    bytes.push(FAMILY_COUNT as u8);
    for (family, profile, row_count, tree_root, family_root) in claims {
        bytes.push(family.code());
        match profile {
            Some(profile) => {
                bytes.push(1);
                bytes.extend_from_slice(&<[u8; 2]>::from(*profile));
            }
            None => {
                bytes.push(0);
                bytes.extend_from_slice(&[0, 0]);
            }
        }
        bytes.extend_from_slice(&row_count.to_be_bytes());
        bytes.extend_from_slice(tree_root);
        bytes.extend_from_slice(family_root);
    }
    bytes
}

#[test]
fn one_row_page_matches_independently_framed_canonical_bytes_and_root() {
    let row_key = StableRowKey::new(RowFamily::Core, [0x2a; 32]);
    let reference = reference(0x31, 9, 0x45);
    let index = build_family(RowFamily::Core, None, &[(row_key, reference)]);

    // This is a literal grammar oracle: construct V2NODE framing, relation
    // key/value frames, and the V3 fixed value without calling either tree
    // encoder or the V3 relation encoder.
    let mut key_bytes = Vec::with_capacity(33);
    key_bytes.push(RowFamily::Core.code());
    key_bytes.extend_from_slice(&[0x2a; 32]);
    let mut value_bytes = Vec::with_capacity(74);
    value_bytes.extend_from_slice(&[0, 0]);
    value_bytes.extend_from_slice(&[0x31; 32]);
    value_bytes.extend_from_slice(&9_u64.to_be_bytes());
    value_bytes.extend_from_slice(&[0x45; 32]);
    let mut body = Vec::new();
    append_field(&mut body, &key_bytes);
    append_field(&mut body, &value_bytes);
    let mut expected_node = b"V2NODE\0".to_vec();
    expected_node.extend_from_slice(&[
        CANONICAL_TREE_ABI,
        CANONICAL_CUT_POLICY_VERSION,
        0x01,
        1,
        0x53,
    ]);
    expected_node.extend_from_slice(&0xf204_u16.to_be_bytes());
    expected_node.extend_from_slice(&0_u16.to_be_bytes());
    append_field(&mut expected_node, &body);
    let expected_tree_root =
        state_root_digest(SchemaIdentity::new(0x53, 0xf204, 1), &expected_node)
            .expect("small independent oracle bytes fit the identity frame");

    let node = index.node_closure().next().expect("single leaf root");
    assert_eq!(node.bytes, expected_node);
    assert_eq!(node.id, expected_tree_root);
    assert_eq!(index.tree_root(), expected_tree_root);
}

#[test]
fn catalog_wire_and_root_match_independent_asymmetric_oracle() {
    let counts = [0, 2, 19, 3, 41, 7, 5];
    let claims: [(RowFamily, Option<LanguageProfile>, u64, [u8; 32], [u8; 32]); FAMILY_COUNT] =
        std::array::from_fn(|index| {
            let family = RowFamily::ALL[index];
            let profile = (family == RowFamily::LanguageExtensions)
                .then_some(LanguageProfile::Rust(RustEdition::Rust2021));
            let tree_root = [0x80 + index as u8; 32];
            let family_root = independent_family_root(family, profile, counts[index], &tree_root);
            (family, profile, counts[index], tree_root, family_root)
        });
    let family_roots = std::array::from_fn(|index| claims[index].4);
    let expected_root = independent_catalog_root(&family_roots);
    let families = std::array::from_fn(|index| {
        let (family, profile, row_count, tree_root, family_root) = claims[index];
        super::SemanticTypedPlaneFamilyRootV3::from_untrusted_claims(
            family,
            profile,
            row_count,
            tree_root,
            family_root,
        )
        .expect("independent root claims are internally consistent")
    });
    let catalog = SemanticTypedPlaneIndexCatalogV3::from_untrusted_claims(families)
        .expect("seven ordered asymmetric family roots");
    let expected_wire = independent_catalog_wire(expected_root, &claims);

    assert_eq!(catalog.root(), expected_root);
    assert_eq!(catalog.canonical_bytes(), expected_wire);
    assert!(SemanticTypedPlaneIndexCatalogV3::decode(&expected_wire).is_ok());
    assert_eq!(expected_wire.len(), MAX_TYPED_PLANE_MANIFEST_V3_BYTES);
    assert_eq!(FAMILY_DESCRIPTOR_BYTES, 76);

    // Dropping a complete family descriptor and swapping two family slots
    // must not be interpreted as another valid complete catalog.
    let mut omitted = expected_wire.clone();
    omitted.truncate(omitted.len() - FAMILY_DESCRIPTOR_BYTES);
    assert!(SemanticTypedPlaneIndexCatalogV3::decode(&omitted).is_err());
    let mut swapped = expected_wire;
    let first = 4 + 2 + 32 + 1;
    let second = first + FAMILY_DESCRIPTOR_BYTES;
    let first_bytes = swapped[first..first + FAMILY_DESCRIPTOR_BYTES].to_vec();
    let second_bytes = swapped[second..second + FAMILY_DESCRIPTOR_BYTES].to_vec();
    swapped[first..first + FAMILY_DESCRIPTOR_BYTES].copy_from_slice(&second_bytes);
    swapped[second..second + FAMILY_DESCRIPTOR_BYTES].copy_from_slice(&first_bytes);
    assert!(SemanticTypedPlaneIndexCatalogV3::decode(&swapped).is_err());
}

#[derive(Default)]
struct MemoryNodes {
    bytes: BTreeMap<[u8; 32], Vec<u8>>,
    loads: Cell<usize>,
}

impl TreeNodeLoader<SemanticTypedPlaneRowRelationV3> for MemoryNodes {
    type Error = String;

    fn load(
        &self,
        claim: UntrustedId<SemanticTypedPlaneRowRelationV3>,
    ) -> Result<CheckedCanonicalRoot<SemanticTypedPlaneRowRelationV3>, Self::Error> {
        self.loads.set(self.loads.get() + 1);
        let bytes = self
            .bytes
            .get(claim.as_bytes())
            .ok_or_else(|| "descriptor page missing".to_owned())?;
        PersistedTreeRoot::admit(claim, bytes)
            .map(|root| root.evidence().clone())
            .map_err(|error| error.to_string())
    }
}

fn index_with_core_rows(count: u32) -> super::SemanticTypedPlaneIndexV3 {
    let mut rows = Vec::with_capacity(count as usize);
    for ordinal in 0..count {
        let mut key_bytes = [0_u8; 32];
        key_bytes[..4].copy_from_slice(&ordinal.to_be_bytes());
        key_bytes[27] = ordinal.wrapping_mul(17) as u8;
        rows.push((
            StableRowKey::new(RowFamily::Core, key_bytes),
            fixture_reference(ordinal),
        ));
    }
    let mut families = Vec::with_capacity(FAMILY_COUNT);
    for family in RowFamily::ALL {
        if family == RowFamily::Core {
            families.push(build_family(family, None, &rows));
        } else {
            let profile = (family == RowFamily::LanguageExtensions)
                .then_some(LanguageProfile::Rust(RustEdition::Rust2021));
            families.push(build_family(family, profile, &[]));
        }
    }
    let families: [_; FAMILY_COUNT] = families
        .try_into()
        .unwrap_or_else(|_| panic!("exactly seven families"));
    super::SemanticTypedPlaneIndexV3::from_families(families)
        .expect("fixture family roots are ordered")
}

fn fixture_reference(ordinal: u32) -> SemanticRowPayloadReferenceV3 {
    reference(
        (ordinal.wrapping_mul(29) & 0xff) as u8,
        u64::from(ordinal % 37),
        (ordinal.wrapping_mul(7) & 0xff) as u8,
    )
}

fn memory_nodes(index: &super::SemanticTypedPlaneFamilyIndexV3) -> MemoryNodes {
    let mut nodes = BTreeMap::new();
    for node in index.node_closure() {
        nodes.insert(node.id, node.bytes.to_vec());
    }
    MemoryNodes {
        bytes: nodes,
        loads: Cell::new(0),
    }
}

fn open_core<'a>(
    loader: &'a MemoryNodes,
    index: &super::SemanticTypedPlaneIndexV3,
) -> LazySemanticTypedPlaneFamilyIndexV3<'a, MemoryNodes> {
    let family = index.family(RowFamily::Core);
    let root_id = family.tree_root();
    let root_bytes = loader.bytes.get(&root_id).expect("root page stored");
    let descriptor = index.catalog().family(RowFamily::Core);
    LazySemanticTypedPlaneFamilyIndexV3::open(loader, descriptor, root_bytes)
        .expect("retained page closure root admits")
}

#[test]
fn cold_seek_is_path_bounded_and_closure_counts_exact_pages_and_rows() {
    let index = index_with_core_rows(2_048);
    let family = index.family(RowFamily::Core);
    let loader = memory_nodes(family);
    let cold = open_core(&loader, &index);
    assert_eq!(loader.loads.get(), 0, "opening uses supplied root evidence");

    let start = ordinal_key(RowFamily::Core, 1_500);
    let end = ordinal_key(RowFamily::Core, 1_700);
    let page = cold
        .page(Some(start), Some(end), 13)
        .expect("bounded cold seek");
    assert_eq!(page.entries().len(), 13);
    assert!(page.entries().windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert!(
        page.entries()
            .iter()
            .all(|(key, _)| *key >= start && *key < end)
    );
    let touched_for_seek = loader.loads.get();
    assert!(touched_for_seek < family.node_closure().count());

    let mut seen_nodes = Vec::new();
    let mut seen_rows = Vec::new();
    let proof = cold
        .visit_closure(
            SemanticTypedPlaneClosureLimitsV3::new(100_000, 3_000, 100_000_000, 2_000),
            |id, _| seen_nodes.push(id),
            |key, _| seen_rows.push(key),
        )
        .expect("every descriptor page and row reference closes");
    assert_eq!(proof.node_count() as usize, family.node_closure().count());
    assert_eq!(proof.row_reference_count(), 2_048);
    assert_eq!(seen_nodes.len(), proof.node_count() as usize);
    assert_eq!(seen_rows.len(), proof.row_count() as usize);
}

#[test]
fn omitted_and_swapped_child_pages_fail_closure_admission() {
    let index = index_with_core_rows(2_048);
    let family = index.family(RowFamily::Core);
    let root_id = family.tree_root();
    let mut loader = memory_nodes(family);
    assert!(loader.bytes.len() > 2);
    let child_ids: Vec<_> = loader
        .bytes
        .keys()
        .copied()
        .filter(|id| *id != root_id)
        .collect();

    loader.bytes.remove(&child_ids[0]);
    let cold = open_core(&loader, &index);
    assert!(matches!(
        cold.visit_closure(
            SemanticTypedPlaneClosureLimitsV3::new(100_000, 3_000, 100_000_000, 2_000),
            |_, _| {},
            |_, _| {},
        ),
        Err(SemanticTypedPlaneIndexV3Error::LazyTree(_))
    ));

    let mut loader = memory_nodes(family);
    let left = child_ids[0];
    let right = child_ids[1];
    let left_bytes = loader.bytes[&left].clone();
    let right_bytes = loader.bytes[&right].clone();
    loader.bytes.insert(left, right_bytes);
    loader.bytes.insert(right, left_bytes);
    let cold = open_core(&loader, &index);
    assert!(
        cold.visit_closure(
            SemanticTypedPlaneClosureLimitsV3::new(100_000, 3_000, 100_000_000, 2_000),
            |_, _| {},
            |_, _| {},
        )
        .is_err()
    );
}

#[test]
fn cold_batched_edit_emits_a_checked_new_root_and_changed_pages() {
    let index = index_with_core_rows(32);
    let family = index.family(RowFamily::Core);
    let loader = memory_nodes(family);
    let cold = open_core(&loader, &index);
    let change_key = ordinal_key(RowFamily::Core, 11);
    let replacement = reference(0xee, 91, 0xab);
    let changes = [
        TreeChange {
            key: ordinal_key(RowFamily::Core, 2),
            after: None,
        },
        TreeChange {
            key: change_key,
            after: Some(replacement),
        },
        TreeChange {
            key: ordinal_key(RowFamily::Core, 35),
            after: Some(reference(0xf4, 27, 0x1b)),
        },
    ];
    let shape = LazyTreeMetadataShape::new(33, 74, 64, 1_024, 64, 8);
    let budget = LazyTreeUpdateBudget::new(8, 2_000_000, shape);
    let update = cold
        .prepare_update_bounded(&changes, budget)
        .expect("one sorted replacement fits explicit bounds");
    assert_eq!(update.target_descriptor().row_count(), 32);
    assert_ne!(update.target_descriptor().tree_root(), family.tree_root());
    let changed: Vec<_> = update.changed_pages().collect();
    assert!(!changed.is_empty());
    assert!(changed.iter().all(|page| !page.bytes.is_empty()));
    assert!(update.work().peak_metadata_bytes <= budget.max_metadata_bytes);

    // Reopen the target from the union of retained base pages and emitted
    // changed pages; compare its complete ordered contents with an explicit
    // independently stated logical mutation history.
    let target_descriptor = update.target_descriptor();
    let mut target_nodes = loader.bytes.clone();
    for page in update.changed_pages() {
        target_nodes.insert(page.id, page.bytes.to_vec());
    }
    let target_loader = MemoryNodes {
        bytes: target_nodes,
        loads: Cell::new(0),
    };
    let target_root = target_descriptor.tree_root();
    let target_root_bytes = target_loader
        .bytes
        .get(&target_root)
        .expect("target root page is in emitted pages");
    let reopened = LazySemanticTypedPlaneFamilyIndexV3::open(
        &target_loader,
        target_descriptor,
        target_root_bytes,
    )
    .expect("target changed-page closure root admits");
    let target_page = reopened
        .page(None, None, 64)
        .expect("target range can be read");
    let expected_keys: Vec<_> = (0..32)
        .filter(|ordinal| *ordinal != 2)
        .map(|ordinal| ordinal_key(RowFamily::Core, ordinal))
        .chain([ordinal_key(RowFamily::Core, 35)])
        .collect();
    assert_eq!(target_page.entries().len(), expected_keys.len());
    assert_eq!(
        target_page
            .entries()
            .iter()
            .map(|(key, _)| *key)
            .collect::<Vec<_>>(),
        expected_keys
    );
    assert_eq!(
        target_page
            .entries()
            .iter()
            .find(|(key, _)| *key == change_key)
            .map(|(_, value)| *value),
        Some(replacement)
    );
}

#[test]
fn v3_update_work_counters_cover_noop_clustered_and_scattered_batches() {
    let index = index_with_core_rows(2_048);
    let family = index.family(RowFamily::Core);
    let flat_c005_bytes = 426 + 108 * 2_048;
    let flat_bridge_bytes = 72 * 2_048;
    let shape = LazyTreeMetadataShape::new(33, 74, 64, 1_024, 64, 8);
    let budget = LazyTreeUpdateBudget::new(100, 32_000_000, shape);

    let mut scenarios: Vec<(&str, Vec<TreeChange<SemanticTypedPlaneRowRelationV3>>)> = Vec::new();
    scenarios.push(("empty-noop", Vec::new()));
    scenarios.push((
        "same-value-noop",
        vec![TreeChange {
            key: ordinal_key(RowFamily::Core, 500),
            after: Some(fixture_reference(500)),
        }],
    ));
    for count in [1_u32, 10, 100] {
        let clustered: Vec<_> = (900..900 + count)
            .map(|ordinal| TreeChange {
                key: ordinal_key(RowFamily::Core, ordinal),
                after: Some(reference(0xd1, u64::from(ordinal + count), 0xc7)),
            })
            .collect();
        let mut scattered: Vec<_> = (0..count)
            .map(|offset| {
                let ordinal = (offset * 997 + 11) % 2_048;
                TreeChange {
                    key: ordinal_key(RowFamily::Core, ordinal),
                    after: Some(reference(0xe3, u64::from(ordinal + count), 0xb5)),
                }
            })
            .collect();
        scattered.sort_by_key(|change| change.key);
        scenarios.push(("clustered", clustered));
        scenarios.push(("scattered", scattered));
    }

    for (label, changes) in scenarios {
        let loader = memory_nodes(family);
        let cold = open_core(&loader, &index);
        let update = cold
            .prepare_update_bounded(&changes, budget)
            .expect("sorted batch fits the explicit update budget");
        let work = update.work();
        if changes.iter().all(|change| {
            family
                .range(Some(change.key), None)
                .expect("key remains inside one family")
                .next()
                .is_some_and(|entry| *entry.reference == *change.after.as_ref().unwrap())
        }) {
            assert_eq!(update.target_descriptor().tree_root(), family.tree_root());
            assert!(update.changed_pages().next().is_none());
        }
        assert!(work.peak_metadata_bytes <= budget.max_metadata_bytes);
        eprintln!(
            "V3-work {label}: edits={} loaded={} rebuilt={} split={} bytes={} peak_meta={} flat_c005={} flat_bridge={}",
            changes.len(),
            work.loaded_nodes,
            work.rebuilt_nodes,
            work.split_nodes,
            work.emitted_bytes,
            work.peak_metadata_bytes,
            flat_c005_bytes,
            flat_bridge_bytes,
        );
    }
}

#[test]
fn payload_claim_fixed_wire_round_trips_distinct_encodings() {
    let raw = UntrustedRowPayloadIdentity::from_raw([0x12; 32], 0x0102);
    let tagged = UntrustedRowPayloadIdentity::from_tagged_raw(0xa7, [0x34; 32], 0x0304);
    let raw_wire = raw.to_fixed_wire();
    let tagged_wire = tagged.to_fixed_wire();
    assert_eq!(raw_wire[0..2], [0, 0]);
    assert_eq!(tagged_wire[0..2], [1, 0xa7]);
    assert_eq!(
        UntrustedRowPayloadIdentity::from_fixed_wire(&raw_wire),
        Ok(raw)
    );
    assert_eq!(
        UntrustedRowPayloadIdentity::from_fixed_wire(&tagged_wire),
        Ok(tagged)
    );

    let verified = RowPayload::from_tagged_bytes(0xa7, b"typed bytes").unwrap();
    let claim = verified.claim();
    assert_eq!(claim.tag(), Some(0xa7));
    assert_eq!(
        UntrustedRowPayloadIdentity::from_fixed_wire(&claim.to_fixed_wire()),
        Ok(claim)
    );
}

#[test]
fn persisted_root_rejects_a_wrong_relation_claim_context() {
    let index = index_with_core_rows(0);
    let family = index.family(RowFamily::Core);
    let loader = memory_nodes(family);
    let root = family.tree_root();
    let wrong_context = UntrustedId::<SemanticTypedPlaneRowRelationV3>::from_wire(
        &root,
        IdContext::delta::<SemanticTypedPlaneRowRelationV3>(),
    )
    .expect("digest width is valid even with the wrong context");
    let bytes = loader.bytes.get(&root).unwrap();
    assert!(PersistedTreeRoot::admit(wrong_context, bytes).is_err());
}
