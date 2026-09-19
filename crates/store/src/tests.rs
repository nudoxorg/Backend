use super::*;
use std::io::Write as _;

#[track_caller]
fn must<T, E: fmt::Debug>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => {
            std::panic::resume_unwind(Box::new(format!("test operation failed: {error:?}")))
        }
    }
}

fn present<T>(value: Option<T>) -> T {
    if let Some(value) = value {
        value
    } else {
        std::panic::resume_unwind(Box::new("test value was absent"));
    }
}

fn test_hex(bytes: &Hash) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    output
}

struct TestCoverageSchema;

impl backend_version::Schema for TestCoverageSchema {
    const DOMAIN: u8 = 0x7f;
    const TYPE: u16 = 1;
    type Value = u64;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

struct AuxiliaryRelation;

impl Relation for AuxiliaryRelation {
    const DOMAIN: u8 = 0x10;
    const TYPE: u16 = 1;
    type Key = u64;
    type Value = u64;

    fn encode_key(value: &Self::Key, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

impl backend_version::CanonicalRelation for AuxiliaryRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, backend_version::RelationDecodeError> {
        bytes
            .try_into()
            .map(u64::from_be_bytes)
            .map_err(|_| backend_version::RelationDecodeError::Malformed)
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, backend_version::RelationDecodeError> {
        bytes
            .try_into()
            .map(u64::from_be_bytes)
            .map_err(|_| backend_version::RelationDecodeError::Malformed)
    }
}

struct FixtureCoverageVerifier;

impl backend_version::ProducerObservationVerifier for FixtureCoverageVerifier {
    type Error = ();

    fn verify(
        &self,
        _observation: &backend_version::UntrustedProducerObservation,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

fn coverage() -> CoverageWitness {
    let scope = backend_version::ObjectVersion::<TestCoverageSchema>::from_value(&1);
    let declaration = backend_version::AuthorityScopeClaim::from_object_version(scope);
    let observation = backend_version::UntrustedProducerObservation::new(
        [7; 32],
        declaration.scope_root(),
        [3; 32],
        vec![4, 5, 6],
    );
    let admitted = must(backend_version::admit_producer_observation(
        observation,
        &FixtureCoverageVerifier,
    ));
    CoverageWitness::Complete(must(backend_version::admit_complete_scope(
        declaration,
        admitted,
    )))
}

fn index_byte(index: usize) -> u8 {
    u8::try_from(index % 251).unwrap_or_default()
}

fn state_byte(state: u64) -> u8 {
    u8::try_from(state & u64::from(u8::MAX)).unwrap_or_default()
}

fn checked_map<I>(items: I) -> OrderedMap
where
    I: IntoIterator<Item = (Vec<u8>, StoredValue)>,
{
    must(OrderedMap::try_from_iter_with_coverage(items, coverage()))
}

fn v(n: u8) -> StoredValue {
    StoredValue::new(vec![n], 1, vec![])
}
#[test]
fn roots_history_independent() {
    let a = checked_map([(b"b".to_vec(), v(2)), (b"a".to_vec(), v(1))]);
    let e = must(must(OrderedMap::try_empty()).apply(&[
        Change {
            key: b"a".to_vec(),
            before: None,
            after: Some(v(1)),
        },
        Change {
            key: b"b".to_vec(),
            before: None,
            after: Some(v(2)),
        },
    ]));
    assert_eq!(a.state_root(), e.state_root());
}
#[test]
fn stale() {
    let a = checked_map([(b"a".to_vec(), v(1))]);
    let b = must(a.apply(&[Change {
        key: b"a".to_vec(),
        before: Some(v(1)),
        after: Some(v(2)),
    }]));
    assert!(must(b.delta_to(&a)).apply(&b).is_ok());
    assert!(must(b.delta_to(&a)).apply(&b).is_ok());
    assert!(must(b.delta_to(&a)).apply(&a).is_err());
}
#[test]
fn pack_gc() {
    let a = checked_map([(b"a".to_vec(), v(1))]);
    let p = must(encode_pack(&a, LayoutId::from_bytes([1; 32]), 100));
    let id = p.id();
    let mut r = Residency::default();
    r.admit(p);
    let pin = present(r.pin(id));
    assert_eq!(r.collect(), 0);
    r.unpin(pin);
    assert_eq!(r.collect(), 1);
}

#[test]
fn wire_pack_admission_checks_directory_and_round_trips() {
    let map = checked_map([(b"a".to_vec(), v(1)), (b"b".to_vec(), v(2))]);
    let pack = must(encode_pack(&map, LayoutId::from_bytes([3; 32]), 100));
    let wire = pack.to_wire();
    assert_eq!(
        must(decode_wire_pack(wire.clone(), 100)).state_root(),
        map.state_root()
    );

    let mut malformed = wire;
    malformed.locations.insert(b"forged".to_vec(), (0, 0));
    assert!(matches!(
        admit_pack(malformed, 100),
        Err(StoreError::Corrupt)
    ));
    let moved = must(encode_pack(&map, LayoutId::derive(b"repacked"), 100));
    assert_eq!(must(decode_pack(&moved)).state_root(), map.state_root());
}
#[test]
fn single_edit_reuses_unchanged_cas_subtrees() {
    let base =
        checked_map((0usize..10_000).map(|i| (format!("k{i:05}").into_bytes(), v(index_byte(i)))));
    let (edited, stats) = must(base.apply_with_stats(&[Change {
        key: b"k05000".to_vec(),
        before: Some(v(index_byte(5000))),
        after: Some(v(9)),
    }]));
    assert!(stats.visited_nodes < 128);
    assert!(stats.copied_nodes <= stats.visited_nodes);
    let before = base.root_node().children().collect::<Vec<_>>();
    let after = edited.root_node().children().collect::<Vec<_>>();
    assert!(!before.is_empty() && !after.is_empty());
    assert!(
        before
            .iter()
            .zip(after.iter())
            .any(|(left, right)| left.id() == right.id())
    );
}

#[test]
fn budget_failure_does_not_publish_a_partial_root() {
    let base =
        checked_map((0usize..2_000).map(|i| (format!("k{i:05}").into_bytes(), v(index_byte(i)))));
    let root = base.state_root();
    let result = base.apply_with_budget(
        &[Change {
            key: b"k01000".to_vec(),
            before: Some(v(index_byte(1_000))),
            after: Some(v(8)),
        }],
        WorkBudget::new(0),
    );
    assert!(matches!(result, Err(StoreError::NeedsScopedRebuild)));
    assert_eq!(base.state_root(), root);
    assert_eq!(base.get(b"k01000"), Some(&v(index_byte(1_000))));
}

#[test]
fn randomized_histories_match_canonical_root() {
    let mut map = must(OrderedMap::try_empty());
    let mut expected = BTreeMap::new();
    let mut state = 0x9e37_79b9_u64;
    for _ in 0..600 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        let key = format!("k{:04}", (state % 240) as usize).into_bytes();
        state ^= state >> 17;
        let change = match (state % 3, expected.get(&key).cloned()) {
            (0, Some(before)) => Change {
                key: key.clone(),
                before: Some(before),
                after: Some(v(state_byte(state >> 8))),
            },
            (1, Some(before)) => Change {
                key: key.clone(),
                before: Some(before),
                after: None,
            },
            (_, None) => Change {
                key: key.clone(),
                before: None,
                after: Some(v(state_byte(state >> 8))),
            },
            (_, Some(before)) => Change {
                key: key.clone(),
                before: Some(before.clone()),
                after: Some(v(state_byte(state >> 8))),
            },
        };
        if let Some(after) = &change.after {
            expected.insert(key, after.clone());
        } else {
            expected.remove(&key);
        }
        map = must(map.apply(&[change]));
        let items = expected
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    RawValue {
                        value: value.value.clone(),
                        availability: value.availability,
                        references: value.references.clone(),
                    },
                )
            })
            .collect::<Vec<_>>();
        let expected_root =
            must(backend_version::canonical_root::<RawRelation>(&items)).commitment();
        assert_eq!(map.state_root(), expected_root);
        assert_eq!(
            map.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn hundred_thousand_value_update_is_sublinear() {
    let base =
        checked_map((0usize..100_000).map(|i| (format!("k{i:06}").into_bytes(), v(index_byte(i)))));
    let (edited, stats) = must(base.apply_with_stats(&[Change {
        key: b"k050000".to_vec(),
        before: Some(v(index_byte(50_000))),
        after: Some(v(7)),
    }]));
    assert!(stats.visited_nodes < 128);
    assert!(stats.copied_nodes < 128);
    assert!(stats.reused_nodes > stats.copied_nodes);
    assert_eq!(edited.get(b"k050000"), Some(&v(7)));
    let expected = (0usize..100_000)
        .map(|i| {
            let value = if i == 50_000 { v(7) } else { v(index_byte(i)) };
            (format!("k{i:06}").into_bytes(), raw_value(&value))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        edited.state_root(),
        must(backend_version::canonical_root::<RawRelation>(&expected)).commitment()
    );

    let (inserted, insert_stats) = must(edited.apply_with_stats(&[Change {
        key: b"k050000x".to_vec(),
        before: None,
        after: Some(v(8)),
    }]));
    assert!(insert_stats.visited_nodes < 1_000);
    assert!(insert_stats.reused_nodes > insert_stats.copied_nodes);
    let (deleted, delete_stats) = must(inserted.apply_with_stats(&[Change {
        key: b"k050000x".to_vec(),
        before: Some(v(8)),
        after: None,
    }]));
    assert!(delete_stats.visited_nodes < 1_000);
    assert!(delete_stats.reused_nodes > delete_stats.copied_nodes);
    assert_eq!(deleted.state_root(), edited.state_root());
}

#[test]
fn cas_reaps_dropped_history_and_keeps_live_deduplication() {
    let entries = (0..1_024usize).map(|index| {
        (
            format!("key-{index:04}").into_bytes(),
            StoredValue::new(vec![index_byte(index)], 1, Vec::new()),
        )
    });
    let historical = must(OrderedMap::try_from_iter(entries));
    let first = must(historical.apply(&[Change {
        key: b"key-0512".to_vec(),
        before: Some(StoredValue::new(vec![index_byte(512)], 1, Vec::new())),
        after: Some(StoredValue::new(vec![9], 1, Vec::new())),
    }]));
    drop(historical);
    let (with_stale, live_before_reap) = first.cas_counts();
    assert!(with_stale > live_before_reap);

    let (second, stats) = must(first.apply_with_stats(&[Change {
        key: b"key-0512".to_vec(),
        before: Some(StoredValue::new(vec![9], 1, Vec::new())),
        after: Some(StoredValue::new(vec![10], 1, Vec::new())),
    }]));
    let (after_reap, live_after_reap) = second.cas_counts();
    assert_eq!(after_reap, live_after_reap);
    assert!(stats.reused_nodes > 0);
}

#[test]
fn no_op_update_reuses_the_exact_shared_root_handle() {
    let map = checked_map(
        (0usize..10_000).map(|index| (format!("k{index:05}").into_bytes(), v(index_byte(index)))),
    );
    let before = map.root_node();
    let (same, stats) = must(map.apply_with_stats(&[Change {
        key: b"k05000".to_vec(),
        before: Some(v(index_byte(5_000))),
        after: Some(v(index_byte(5_000))),
    }]));
    let after = same.root_node();
    assert_eq!(before.id(), after.id());
    assert!(std::ptr::eq(before.canonical(), after.canonical()));
    assert_eq!(stats.copied_nodes, 0);
    assert_eq!(stats.reused_nodes, 0);
}

#[test]
fn weak_root_handle_expires_with_its_map_generation() {
    let weak = {
        let map = checked_map([(b"a".to_vec(), v(1))]);
        map.root_node().downgrade()
    };
    assert!(weak.upgrade().is_none());
}

#[test]
fn lazy_closure_open_lookup_and_pages_are_bounded() {
    const OBJECT_COUNT: u64 = 1_024;
    let object_count = usize::try_from(OBJECT_COUNT).unwrap_or_default();
    let mut objects = (0u64..OBJECT_COUNT)
        .map(|value| {
            let key = backend_version::ObjectKey::<TestCoverageSchema>::from_value(&value);
            TypedObject::from_value(&key, &value)
        })
        .collect::<Vec<_>>();
    objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
    let manifest = must(ClosureManifest::new(objects));
    let path =
        std::env::temp_dir().join(format!("backend-store-lazy-closure-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    let store = must(FileStore::open(&path, 16 * 1024 * 1024));
    let id = must(store.write_closure(&manifest));

    let lazy = must(store.open_closure(id));
    assert_eq!(lazy.id(), id);
    assert_eq!(lazy.entry_count(), Some(object_count));
    let selected = manifest.objects()[object_count / 2].clone();
    let selected_claim = UntrustedObjectId::from_bytes(*selected.id().as_bytes());
    assert_eq!(must(lazy.admit_claim(selected_claim)), Some(selected.id()));
    assert_eq!(
        must(lazy.admit_claim(UntrustedObjectId::from_bytes([0xff; 32]))),
        None
    );
    let (found, stats) = must(lazy.get_with_stats(selected.id()));
    assert_eq!(found, Some(selected.clone()));
    assert_eq!(stats.objects_read, 1);
    assert!(stats.nodes_read <= 8);

    let first = must(lazy.page(None, 17));
    assert_eq!(first.objects().len(), 17);
    assert!(first.next().is_some());
    assert!(first.stats().nodes_read <= 8);
    let second = must(lazy.page(first.next(), 17));
    assert_eq!(second.objects().len(), 17);
    assert!(second.objects()[0].id() > first.objects()[16].id());
    assert!(matches!(
        lazy.page_with_budget(None, 1, 1),
        Err(StoreError::Bounds)
    ));

    let missing = manifest.objects()[0].clone();
    let object_path = path
        .join("objects")
        .join(format!("{}.object", test_hex(missing.id().as_bytes())));
    must(std::fs::remove_file(object_path));
    assert_eq!(
        must(lazy.admit_claim(UntrustedObjectId::from_bytes(*missing.id().as_bytes()))),
        Some(missing.id())
    );
    assert_eq!(must(lazy.get(selected.id())), Some(selected));
    assert!(matches!(lazy.get(missing.id()), Err(StoreError::Corrupt)));
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn diff_prunes_equal_subtrees_after_a_shared_kernel_update() {
    let map = checked_map(
        (0usize..100_000).map(|index| (format!("k{index:06}").into_bytes(), v(index_byte(index)))),
    );
    let edited = must(map.apply(&[Change {
        key: b"k050000".to_vec(),
        before: Some(v(index_byte(50_000))),
        after: Some(v(11)),
    }]));
    let stats = map.diff_stats(&edited);
    assert!(stats.visited_nodes < 512);
    assert_eq!(stats.changed_leaves, 1);
}

#[test]
fn structural_edits_reuse_outside_boundary() {
    let base =
        checked_map((0usize..10_000).map(|i| (format!("k{i:05}").into_bytes(), v(index_byte(i)))));
    let (inserted, stats) = must(base.apply_with_stats(&[Change {
        key: b"k05000x".to_vec(),
        before: None,
        after: Some(v(99)),
    }]));
    assert!(stats.reused_nodes > stats.copied_nodes);
    assert!(stats.visited_nodes < 500);
    let expected = (0usize..10_000)
        .map(|i| {
            (
                format!("k{i:05}").into_bytes(),
                raw_value(&v(index_byte(i))),
            )
        })
        .chain(std::iter::once((b"k05000x".to_vec(), raw_value(&v(99)))))
        .collect::<BTreeMap<_, _>>();
    let items = expected.into_iter().collect::<Vec<_>>();
    assert_eq!(
        inserted.state_root(),
        must(backend_version::canonical_root::<RawRelation>(&items)).commitment()
    );
}

#[test]
fn randomized_large_structural_histories_match_canonical_root() {
    let mut expected = (0usize..5_000)
        .map(|i| (format!("k{i:06}").into_bytes(), v(index_byte(i))))
        .collect::<BTreeMap<_, _>>();
    let mut map = checked_map(
        expected
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    let mut state = 0x1234_5678_9abc_def0_u64;
    for _ in 0..240 {
        state = state
            .wrapping_mul(2_862_933_555_777_941_757)
            .wrapping_add(3_039_700_493);
        let key = format!("k{:06}", (state % 7_000) as usize).into_bytes();
        let before = expected.get(&key).cloned();
        let operation = (state >> 24) % 3;
        let change = if operation == 1 {
            Change {
                key: key.clone(),
                before: before.clone(),
                after: None,
            }
        } else {
            Change {
                key: key.clone(),
                before: before.clone(),
                after: Some(v(state_byte(state >> 32))),
            }
        };
        if let Some(after) = &change.after {
            expected.insert(key, after.clone());
        } else {
            expected.remove(&key);
        }
        map = must(map.apply(&[change]));
        let items = expected
            .iter()
            .map(|(key, value)| (key.clone(), raw_value(value)))
            .collect::<Vec<_>>();
        assert_eq!(
            map.state_root(),
            must(backend_version::canonical_root::<RawRelation>(&items)).commitment()
        );
    }
}

#[test]
fn structural_delta_merges_unequal_leaf_ranges() {
    let base =
        checked_map((0usize..20_000).map(|i| (format!("k{i:06}").into_bytes(), v(index_byte(i)))));
    let mut target_items = (0usize..20_000)
        .filter(|i| *i != 4_000 && *i != 12_000)
        .map(|i| {
            let value = if i == 8_000 { v(201) } else { v(index_byte(i)) };
            (format!("k{i:06}").into_bytes(), value)
        })
        .collect::<Vec<_>>();
    target_items.push((b"k04000x".to_vec(), v(202)));
    target_items.sort_by(|left, right| left.0.cmp(&right.0));
    let target = must(OrderedMap::try_from_iter_with_coverage(
        target_items,
        coverage(),
    ));
    assert_eq!(base.get(b"k012036"), Some(&v(index_byte(12_036))));
    assert_eq!(target.get(b"k012036"), Some(&v(index_byte(12_036))));
    let delta = must(base.delta_to(&target));
    let changes = delta.changes().collect::<Vec<_>>();
    assert_eq!(changes.len(), 4);
    assert!(
        changes
            .windows(2)
            .all(|window| window[0].key < window[1].key)
    );
    assert_eq!(must(delta.apply(&base)).state_root(), target.state_root());
    assert_eq!(
        base.diff_stats(&base),
        DiffStats {
            visited_nodes: 1,
            changed_leaves: 0,
        }
    );
    assert!(base.diff_stats(&target).changed_leaves > 0);
}

#[test]
fn inverse_and_compose_recompute_canonical_delta_ids() {
    let a = checked_map([(b"a".to_vec(), v(1)), (b"b".to_vec(), v(2))]);
    let b = must(a.apply(&[Change {
        key: b"b".to_vec(),
        before: Some(v(2)),
        after: Some(v(4)),
    }]));
    let c = must(b.apply(&[Change {
        key: b"c".to_vec(),
        before: None,
        after: Some(v(7)),
    }]));
    let ab = must(a.delta_to(&b));
    let bc = must(b.delta_to(&c));
    let inverse = must(ab.inverse());
    assert_eq!(
        inverse.id(),
        must(delta_id_for(
            b.state_root(),
            inverse.target(),
            inverse.changes_slice(),
            coverage(),
        ))
    );
    let composed = must(ab.compose(&bc));
    assert_eq!(
        composed.id(),
        must(delta_id_for(
            a.state_root(),
            composed.target(),
            composed.changes_slice(),
            coverage(),
        ))
    );
    assert_eq!(must(inverse.apply(&b)).state_root(), a.state_root());
    assert_eq!(must(composed.apply(&a)).state_root(), c.state_root());
}

#[test]
fn root_height_changes_keep_canonical_identity() {
    let mut map = must(OrderedMap::try_empty());
    let mut expected = BTreeMap::new();
    for i in 0usize..280 {
        let key = format!("k{i:05}").into_bytes();
        let value = v(index_byte(i));
        map = must(map.apply(&[Change {
            key: key.clone(),
            before: None,
            after: Some(value.clone()),
        }]));
        expected.insert(key, value);
    }
    for i in (0usize..280).rev() {
        let key = format!("k{i:05}").into_bytes();
        let before = present(expected.remove(&key));
        map = must(map.apply(&[Change {
            key: key.clone(),
            before: Some(before),
            after: None,
        }]));
        let items = expected
            .iter()
            .map(|(key, value)| (key.clone(), raw_value(value)))
            .collect::<Vec<_>>();
        assert_eq!(
            map.state_root(),
            must(backend_version::canonical_root::<RawRelation>(&items)).commitment()
        );
    }
}
#[test]
fn proof_and_corrupt_pack() {
    let a = checked_map([(b"a".to_vec(), v(1)), (b"b".to_vec(), v(2))]);
    let proof = present(a.prove(b"a"));
    assert!(proof.verify(&a));
    let b = must(a.apply(&[Change {
        key: b"b".to_vec(),
        before: Some(v(2)),
        after: Some(v(3)),
    }]));
    assert!(!proof.verify(&b));
    let pack = must(encode_pack(&a, LayoutId::from_bytes([1; 32]), 100));
    let mut wire = pack.into_wire();
    wire.bytes[0] ^= 1;
    assert!(matches!(admit_pack(wire, 100), Err(StoreError::Corrupt)));
}
#[test]
fn large_proof_verifies_independently_and_rejects_leaf_tampering() {
    let map =
        checked_map((0usize..12_000).map(|i| (format!("k{i:06}").into_bytes(), v(index_byte(i)))));
    let proof = present(map.prove(b"k006000"));
    assert!(proof.leaf_entries.len() >= OrderedMap::LEAF_MIN);
    assert!(!proof.path.is_empty());
    assert!(proof.verify_root(map.state_root()));

    let mut tampered = proof.clone();
    tampered.leaf_entries[0].1.value[0] ^= 1;
    assert!(!tampered.verify_root(map.state_root()));
}

#[test]
fn nonmembership_proof_authenticates_empty_key_slot() {
    let map = checked_map([
        (b"a".to_vec(), v(1)),
        (b"c".to_vec(), v(3)),
        (b"e".to_vec(), v(5)),
    ]);
    let proof = present(map.prove_nonmembership(b"d"));
    assert!(proof.is_nonmembership());
    assert!(proof.verify(&map));
    assert!(present(map.prove_nonmembership(b"0")).verify(&map));
    assert!(present(map.prove_nonmembership(b"z")).verify(&map));
    let large =
        checked_map((0usize..300).map(|i| (format!("k{i:03}").into_bytes(), v(index_byte(i)))));
    assert!(present(large.prove_nonmembership(b"a")).verify(&large));
    assert!(present(large.prove_nonmembership(b"z")).verify(&large));
    let mut tampered = proof.clone();
    tampered.key = b"c".to_vec();
    assert!(!tampered.verify_root(map.state_root()));
}

#[test]
fn range_proof_authenticates_all_selected_leaves() {
    let map =
        checked_map((0usize..2_000).map(|i| (format!("k{i:05}").into_bytes(), v(index_byte(i)))));
    let proof = must(map.prove_range(Some(b"k00500"), Some(b"k01500")));
    assert!(proof.verify(&map));
    assert_eq!(proof.entries.len(), 1_000);
    let mut tampered = proof.clone();
    tampered.entries[0].1.value[0] ^= 1;
    assert!(!tampered.verify_root(map.state_root()));
    let mut omitted = proof.clone();
    omitted.leaf_proofs.truncate(1);
    omitted.entries = omitted.leaf_proofs[0]
        .leaf_entries
        .iter()
        .filter(|(key, _)| key.as_slice() >= b"k00500" && key.as_slice() < b"k01500")
        .cloned()
        .collect();
    assert!(!omitted.verify_root(map.state_root()));
    assert!(must(map.prove_range(None, Some(b"k00000"))).verify(&map));
    assert!(must(map.prove_range(Some(b"k02000"), None)).verify(&map));
    assert!(must(map.prove_range(Some(b"k00500"), None)).verify(&map));
    assert!(must(map.prove_range(None, Some(b"k01500"))).verify(&map));
    assert!(must(map.prove_range(None, None)).verify(&map));
}
#[test]
fn try_from_iter_rejects_duplicate_keys() {
    assert!(matches!(
        OrderedMap::try_from_iter(vec![(b"a".to_vec(), v(1)), (b"a".to_vec(), v(2))]),
        Err(StoreError::MalformedDelta)
    ));
}

#[test]
fn canonical_admission_rejects_oversized_keys_and_nodes() {
    let oversized_key = vec![0; backend_version::CutPolicy::MAX_ENCODED_BYTES];
    assert!(matches!(
        OrderedMap::try_from_iter([(oversized_key, v(1))]),
        Err(StoreError::OversizedKey)
    ));
    let oversized_value = StoredValue::new(
        vec![0; backend_version::CutPolicy::MAX_ENCODED_BYTES],
        1,
        vec![],
    );
    assert!(matches!(
        OrderedMap::try_from_iter([(b"a".to_vec(), oversized_value)]),
        Err(StoreError::Bounds)
    ));
}

#[test]
fn untrusted_maps_reject_delta_preparation() {
    let base = must(OrderedMap::try_from_iter([(b"a".to_vec(), v(1))]));
    let target = must(base.apply(&[Change {
        key: b"a".to_vec(),
        before: Some(v(1)),
        after: Some(v(2)),
    }]));
    assert!(matches!(
        base.delta_to(&target),
        Err(StoreError::IncompleteCoverage)
    ));
}

#[test]
fn malformed_delta_rejects_duplicate_or_unsorted_keys() {
    let map = checked_map([(b"a".to_vec(), v(1))]);
    let delta = MapDelta::from_parts(
        map.state_root(),
        map.state_root(),
        vec![
            Change {
                key: b"b".to_vec(),
                before: None,
                after: Some(v(2)),
            },
            Change {
                key: b"a".to_vec(),
                before: Some(v(1)),
                after: None,
            },
        ],
        must(delta_id_for(
            map.state_root(),
            map.state_root(),
            &[],
            coverage(),
        )),
        coverage(),
    );
    assert!(matches!(delta.apply(&map), Err(StoreError::MalformedDelta)));

    let target = must(map.apply(&[Change {
        key: b"a".to_vec(),
        before: Some(v(1)),
        after: Some(v(2)),
    }]));
    let valid = must(map.delta_to(&target));
    let forged = MapDelta::from_parts(
        valid.base(),
        valid.target(),
        valid.changes().cloned().collect(),
        must(delta_id_for(
            map.state_root(),
            valid.target(),
            &[],
            coverage(),
        )),
        coverage(),
    );
    assert!(matches!(
        forged.apply(&map),
        Err(StoreError::MalformedDelta)
    ));
}
#[test]
fn filesystem_publication_reopens_and_rejects_torn_or_corrupt_objects() {
    let path = std::env::temp_dir().join(format!(
        "backend-store-{}-{}",
        std::process::id(),
        state_byte(17)
    ));
    let _ = std::fs::remove_dir_all(&path);
    let store = must(FileStore::open(&path, 8_192));
    let map = checked_map([
        (b"a".to_vec(), v(1)),
        (b"b".to_vec(), v(2)),
        (b"c".to_vec(), v(3)),
    ]);
    let published = must(store.publish(&map, LayoutId::from_bytes([9; 32])));

    let journal = std::fs::OpenOptions::new()
        .append(true)
        .open(path.join("journal"))
        .map_err(|_| StoreError::Corrupt);
    let mut journal = must(journal);
    must(journal.write_all(b"LJNL"));
    must(journal.sync_all());
    let reopened = must(FileStore::open(&path, 8_192));
    let recovered = present(must(reopened.recover()));
    assert_eq!(recovered.state_root(), published);

    let pack_path = must(std::fs::read_dir(path.join("packs")))
        .next()
        .and_then(Result::ok)
        .map(|entry| entry.path());
    let pack_path = present(pack_path);
    let mut bytes = must(std::fs::read(&pack_path));
    let last = present(bytes.len().checked_sub(1));
    bytes[last] ^= 1;
    must(std::fs::write(pack_path, bytes));
    assert!(matches!(reopened.recover(), Err(StoreError::Corrupt)));
    must(std::fs::remove_dir_all(path));
}

#[test]
fn typed_closure_round_trips_and_binds_workspace_root() {
    struct BytesSchema;

    impl backend_version::Schema for BytesSchema {
        const DOMAIN: u8 = 0x79;
        const TYPE: u16 = 4;
        type Value = [u8];

        fn encode(value: &Self::Value, out: &mut Vec<u8>) {
            out.extend_from_slice(value);
        }
    }

    let key = backend_version::ObjectKey::<BytesSchema>::from_value(b"object-key");
    let object = TypedObject::from_value(&key, b"object-bytes".as_slice());
    let manifest = must(ClosureManifest::new(vec![object]));
    let encoded = must(manifest.encode(4_096));
    let reopened = must(ClosureManifest::decode(&encoded, 4_096));
    assert_eq!(reopened, manifest);

    let authority = backend_version::ObjectVersion::<TestCoverageSchema>::from_value(&7);
    let workspace = must(backend_version::CheckedWorkspaceManifest::from_versions(
        1,
        Vec::new(),
        Vec::new(),
        authority,
        coverage(),
    ));
    let authority_key = backend_version::ObjectKey::<TestCoverageSchema>::from_value(&7u64);
    let authority_object = TypedObject::from_value(&authority_key, &7u64);
    let bound_manifest = must(ClosureManifest::new(vec![authority_object]));
    let bound = must(WorkspaceClosure::from_checked_manifest(
        &workspace,
        bound_manifest,
    ));
    assert_eq!(bound.manifest().id(), bound.binding().closure());
    assert_eq!(bound.binding().root(), workspace.root().as_bytes());
}

#[test]
fn relation_state_object_closes_checked_workspace_root() {
    let state = must(backend_version::RelationState::<RawRelation>::from_entries(
        [(
            b"relation-key".to_vec(),
            RawValue {
                value: b"relation-value".to_vec(),
                availability: 1,
                references: Vec::new(),
            },
        )],
        coverage(),
    ));
    let relation_object = must(TypedObject::from_relation_state(&state));
    let materialized = state.materialize();
    let root_object = must(TypedObject::from_checked_state_object_ref(materialized));
    assert_eq!(root_object, relation_object);
    assert_eq!(relation_object.version(), state.root().as_bytes());

    let authority_value = 7u64;
    let authority_key =
        backend_version::ObjectKey::<TestCoverageSchema>::from_value(&authority_value);
    let authority_version =
        backend_version::ObjectVersion::<TestCoverageSchema>::from_value(&authority_value);
    let workspace = must(backend_version::CheckedWorkspaceManifest::from_versions(
        1,
        vec![backend_version::RelationBinding::from_state(&state)],
        Vec::new(),
        authority_version,
        coverage(),
    ));
    let authority_object = TypedObject::from_value(&authority_key, &authority_value);
    let mut objects = vec![relation_object, authority_object];
    objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
    let closure = must(ClosureManifest::new(objects));
    let bound = must(WorkspaceClosure::from_checked_manifest(&workspace, closure));
    assert_eq!(bound.root(), workspace.root());
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "keeps the checked target regression scenario self-contained"
)]
fn closure_extension_binds_checked_target_and_replaces_root_only_frontier() {
    let old_state = must(backend_version::RelationState::<RawRelation>::from_entries(
        [(
            b"relation-key".to_vec(),
            RawValue {
                value: b"old".to_vec(),
                availability: 1,
                references: Vec::new(),
            },
        )],
        coverage(),
    ));
    let target_state = must(backend_version::RelationState::<RawRelation>::from_entries(
        [(
            b"relation-key".to_vec(),
            RawValue {
                value: b"new".to_vec(),
                availability: 1,
                references: Vec::new(),
            },
        )],
        coverage(),
    ));
    let authority_value = 7u64;
    let authority_key =
        backend_version::ObjectKey::<TestCoverageSchema>::from_value(&authority_value);
    let authority_version =
        backend_version::ObjectVersion::<TestCoverageSchema>::from_value(&authority_value);
    let authority_object = TypedObject::from_value(&authority_key, &authority_value);
    let old_workspace = must(backend_version::CheckedWorkspaceManifest::from_versions(
        1,
        vec![backend_version::RelationBinding::from_state(&old_state)],
        Vec::new(),
        authority_version,
        coverage(),
    ));
    let target_workspace = must(backend_version::CheckedWorkspaceManifest::from_versions(
        1,
        vec![backend_version::RelationBinding::from_state(&target_state)],
        Vec::new(),
        authority_version,
        coverage(),
    ));
    let old_object = must(TypedObject::from_relation_state(&old_state));
    let target_object = must(TypedObject::from_relation_state(&target_state));
    let mut old_objects = vec![old_object.clone(), authority_object.clone()];
    old_objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
    let base = must(
        WorkspaceClosure::from_checked_manifest_root_only_with_registry(
            &old_workspace,
            must(ClosureManifest::new(old_objects)),
            &RelationAdmissionRegistry::default(),
        ),
    );

    let extended = must(WorkspaceClosure::extend_checked_nodes(
        &base,
        &target_workspace,
        target_state.root(),
        [target_state.root_handle()],
        [authority_object.clone()],
    ));
    assert_eq!(extended.root(), target_workspace.root());
    assert_ne!(extended.root(), base.root());
    assert!(extended.manifest().contains_object_id(target_object.id()));
    assert!(!extended.manifest().contains_object_id(old_object.id()));
    assert_eq!(extended.manifest().objects().len(), 2);

    assert!(
        WorkspaceClosure::extend_checked_nodes(
            &base,
            &old_workspace,
            target_state.root(),
            [target_state.root_handle()],
            [authority_object.clone()],
        )
        .is_err()
    );

    // Repeated one-key path copies replace the selected root in place. This
    // guards the root-only handoff against retaining a 10k-generation history.
    let mut current = extended;
    let mut previous = target_object;
    for value in 0..10_000u64 {
        let state = must(backend_version::RelationState::<RawRelation>::from_entries(
            [(
                b"relation-key".to_vec(),
                RawValue {
                    value: value.to_be_bytes().to_vec(),
                    availability: 1,
                    references: Vec::new(),
                },
            )],
            coverage(),
        ));
        let workspace = must(backend_version::CheckedWorkspaceManifest::from_versions(
            1,
            vec![backend_version::RelationBinding::from_state(&state)],
            Vec::new(),
            authority_version,
            coverage(),
        ));
        let object = must(TypedObject::from_relation_state(&state));
        current = must(WorkspaceClosure::extend_checked_nodes(
            &current,
            &workspace,
            state.root(),
            [state.root_handle()],
            [authority_object.clone()],
        ));
        assert_eq!(current.manifest().objects().len(), 2);
        assert!(!current.manifest().contains_object_id(previous.id()));
        previous = object;
    }
}

#[test]
fn root_only_extension_tracks_all_checked_relation_roots_by_binding() {
    let old_raw = must(backend_version::RelationState::<RawRelation>::from_entries(
        [(
            b"raw".to_vec(),
            RawValue {
                value: b"old".to_vec(),
                availability: 1,
                references: Vec::new(),
            },
        )],
        coverage(),
    ));
    let target_raw = must(backend_version::RelationState::<RawRelation>::from_entries(
        [(
            b"raw".to_vec(),
            RawValue {
                value: b"new".to_vec(),
                availability: 1,
                references: Vec::new(),
            },
        )],
        coverage(),
    ));
    let auxiliary = must(
        backend_version::RelationState::<AuxiliaryRelation>::from_entries([(1, 2)], coverage()),
    );
    let authority_value = 7u64;
    let authority_key =
        backend_version::ObjectKey::<TestCoverageSchema>::from_value(&authority_value);
    let authority_version =
        backend_version::ObjectVersion::<TestCoverageSchema>::from_value(&authority_value);
    let authority_object = TypedObject::from_value(&authority_key, &authority_value);
    let old_workspace = must(backend_version::CheckedWorkspaceManifest::from_versions(
        1,
        vec![
            backend_version::RelationBinding::from_state(&auxiliary),
            backend_version::RelationBinding::from_state(&old_raw),
        ],
        Vec::new(),
        authority_version,
        coverage(),
    ));
    let target_workspace = must(backend_version::CheckedWorkspaceManifest::from_versions(
        1,
        vec![
            backend_version::RelationBinding::from_state(&auxiliary),
            backend_version::RelationBinding::from_state(&target_raw),
        ],
        Vec::new(),
        authority_version,
        coverage(),
    ));
    let registry = must(RelationAdmissionRegistry::default().with_relation::<AuxiliaryRelation>());
    let mut objects = vec![
        must(TypedObject::from_relation_state(&old_raw)),
        must(TypedObject::from_relation_state(&auxiliary)),
        authority_object.clone(),
    ];
    objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
    let base = must(
        WorkspaceClosure::from_checked_manifest_root_only_with_registry(
            &old_workspace,
            must(ClosureManifest::new(objects)),
            &registry,
        ),
    );
    let old_raw_object = must(TypedObject::from_relation_state(&old_raw));
    let target_raw_object = must(TypedObject::from_relation_state(&target_raw));
    let extended = must(WorkspaceClosure::extend_checked_nodes_with_registry(
        &base,
        &target_workspace,
        target_raw.root(),
        [target_raw.root_handle()],
        [
            must(TypedObject::from_relation_state(&auxiliary)),
            authority_object,
        ],
        &registry,
    ));
    assert_eq!(extended.root(), target_workspace.root());
    assert!(
        extended
            .manifest()
            .contains_object_id(target_raw_object.id())
    );
    assert!(!extended.manifest().contains_object_id(old_raw_object.id()));
    assert_eq!(extended.manifest().objects().len(), 3);
}

#[test]
fn closure_admission_rejects_forged_object_version() {
    let value = 17u64;
    let key = backend_version::ObjectKey::<TestCoverageSchema>::from_value(&value);
    let typed = TypedObject::from_value(&key, &value);
    let schema = typed.schema();
    let bytes = typed.bytes().to_vec();
    let valid = TypedObject::from_wire_parts(
        schema,
        *typed.key(),
        *typed.version(),
        bytes.clone().into_boxed_slice(),
    );
    assert!(ClosureManifest::new(vec![valid]).is_ok());

    let forged = TypedObject::from_wire_parts(
        schema,
        *typed.key(),
        [2; 32],
        bytes.clone().into_boxed_slice(),
    );
    assert!(matches!(
        ClosureManifest::new(vec![forged]),
        Err(StoreError::Corrupt)
    ));
    assert!(!verify_backend_object_version(schema, &bytes, &[2; 32]));
}

#[test]
fn lazy_bulk_frontier_publishes_every_branch_child() {
    struct EmptyLoader;
    impl backend_version::TreeNodeLoader<AuxiliaryRelation> for EmptyLoader {
        type Error = StoreError;

        fn load(
            &self,
            _claim: backend_version::UntrustedId<AuxiliaryRelation>,
        ) -> Result<backend_version::CheckedCanonicalRoot<AuxiliaryRelation>, Self::Error> {
            Err(StoreError::Corrupt)
        }
    }

    let empty = canonical_empty::<AuxiliaryRelation>();
    let admitted = must(backend_version::admit_canonical_root::<AuxiliaryRelation>(
        empty.as_bytes(),
    ));
    let persisted = backend_version::PersistedTreeRoot::from_checked(admitted);
    let lazy = backend_version::LazyTree::from_admitted(&EmptyLoader, persisted);
    let changes = (0..512u64)
        .map(|key| backend_version::TreeChange {
            key,
            after: Some(key * 2),
        })
        .collect::<Vec<_>>();
    let update = must(lazy.prepare_update(&changes));
    assert!(update.target().node().level() > 0);

    let path = std::env::temp_dir().join(format!(
        "backend-store-lazy-bulk-{}-{}",
        std::process::id(),
        state_byte(41)
    ));
    let _ = std::fs::remove_dir_all(&path);
    let registry = must(RelationAdmissionRegistry::default().with_relation::<AuxiliaryRelation>());
    let store = must(FileStore::open_with_registry(&path, 8_192, registry));
    let stats = must(store.write_lazy_relation_update(&update));
    assert_eq!(stats.nodes_written, update.changed_nodes().len());
    let reopened = must(store.read_relation_node(update.target().root()));
    assert_eq!(reopened.version(), update.target().root().as_bytes());
    must(std::fs::remove_dir_all(path));
}

#[test]
fn filesystem_typestate_reopens_and_rejects_interleaved_stale_publish() {
    let path = std::env::temp_dir().join(format!(
        "backend-store-typestate-{}-{}",
        std::process::id(),
        state_byte(23)
    ));
    let _ = std::fs::remove_dir_all(&path);
    let store = must(FileStore::open(&path, 8_192));
    let first = checked_map([(b"a".to_vec(), v(1))]);
    let second = checked_map([(b"b".to_vec(), v(2))]);
    let third = checked_map([(b"c".to_vec(), v(3))]);
    let first_durable =
        must(must(store.prepare_map(&first, LayoutId::derive(b"layout"))).durable());
    let second_durable =
        must(must(store.prepare_map(&second, LayoutId::derive(b"layout"))).durable());
    let third_durable =
        must(must(store.prepare_map(&third, LayoutId::derive(b"layout"))).durable());
    let first_published = must(first_durable.publish());
    assert_eq!(first_published.root(), first.state_root());
    assert!(matches!(
        second_durable.publish(),
        Err(StoreError::StaleHead)
    ));
    assert!(matches!(
        third_durable.publish(),
        Err(StoreError::StaleHead)
    ));
    let fourth = checked_map([(b"d".to_vec(), v(4))]);
    let fourth_published =
        must(must(store.prepare_map(&fourth, LayoutId::derive(b"layout"))).durable()).publish();
    let fourth_published = must(fourth_published);
    assert_eq!(fourth_published.root(), fourth.state_root());
    let reopened = must(FileStore::open(&path, 8_192));
    assert_eq!(
        present(must(reopened.recover())).state_root(),
        fourth.state_root()
    );
    assert!(
        must(std::fs::read_dir(path.join("packs")))
            .filter_map(Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".pack.tmp"))
    );
    must(std::fs::remove_dir_all(path));
}

#[test]
fn filesystem_complete_journal_corruption_is_rejected() {
    let path = std::env::temp_dir().join(format!(
        "backend-store-journal-{}-{}",
        std::process::id(),
        state_byte(29)
    ));
    let _ = std::fs::remove_dir_all(&path);
    let store = must(FileStore::open(&path, 8_192));
    let map = checked_map([(b"a".to_vec(), v(1))]);
    must(store.publish(&map, LayoutId::derive(b"journal-layout")));
    let mut bytes = must(std::fs::read(path.join("journal")));
    let index = present(bytes.len().checked_sub(1));
    bytes[index] ^= 1;
    must(std::fs::write(path.join("journal"), bytes));
    assert!(matches!(
        FileStore::open(&path, 8_192),
        Err(StoreError::Corrupt)
    ));
    must(std::fs::remove_dir_all(path));
}

#[test]
fn corrupted_head_checkpoint_falls_back_to_journal_and_repairs() {
    let path = std::env::temp_dir().join(format!(
        "backend-store-head-checkpoint-{}-{}",
        std::process::id(),
        state_byte(29)
    ));
    let _ = std::fs::remove_dir_all(&path);
    let store = must(FileStore::open(&path, 8_192));
    let map = checked_map([(b"checkpoint".to_vec(), v(1))]);
    must(store.publish(&map, LayoutId::derive(b"checkpoint-layout")));
    let mut head = must(std::fs::read(path.join("HEAD")));
    head[0] ^= 1;
    must(std::fs::write(path.join("HEAD"), head));
    let reopened = must(FileStore::open(&path, 8_192));
    assert_eq!(
        present(must(reopened.recover())).state_root(),
        map.state_root()
    );
    assert!(std::fs::read(path.join("HEAD")).is_ok());
    must(std::fs::remove_dir_all(path));
}

#[test]
fn completed_journal_faults_report_typed_sync_statuses() {
    let path = std::env::temp_dir().join(format!(
        "backend-store-journal-faults-{}-{}",
        std::process::id(),
        state_byte(37)
    ));
    let _ = std::fs::remove_dir_all(&path);
    let store = must(FileStore::open(&path, 8_192));
    let layout = LayoutId::derive(b"fault-layout");
    let first = checked_map([(b"a".to_vec(), v(1))]);

    let prepared = must(store.prepare_map(&first, layout));
    durable::set_test_fault(1);
    let prepared_error = prepared.durable();
    let prepared_sequence = match prepared_error {
        Err(StoreError::PreparedWithSyncPending {
            sequence,
            transaction,
        }) => {
            assert!(sequence > 0);
            assert_ne!(transaction.as_bytes(), &[0; 32]);
            sequence
        }
        other => {
            eprintln!("unexpected prepared fault result: {other:?}");
            std::panic::resume_unwind(Box::new("test operation failed"));
        }
    };

    // Re-admission of the same descriptor discovers the complete frame
    // and does not append a duplicate prepare record.
    let retried = must(store.prepare_map(&first, layout));
    let durable = must(retried.durable());
    assert_eq!(durable.descriptor().base_generation(), 0);
    assert_eq!(prepared_sequence, 1);

    durable::set_test_fault(2);
    let publish_error = durable.publish();
    match publish_error {
        Err(StoreError::PublishedWithSyncPending(head)) => {
            assert_eq!(head.descriptor().target(), *first.state_root().as_bytes());
        }
        other => {
            eprintln!("unexpected publish fault result: {other:?}");
            std::panic::resume_unwind(Box::new("test operation failed"));
        }
    }
    assert_eq!(
        present(must(store.recover())).state_root(),
        first.state_root()
    );

    let second = checked_map([(b"b".to_vec(), v(2))]);
    let durable = must(must(store.prepare_map(&second, layout)).durable());
    durable::set_test_fault(4);
    let publish_error = durable.publish();
    assert!(matches!(
        publish_error,
        Err(StoreError::PublishedWithSyncPending(_))
    ));
    assert_eq!(
        present(must(store.head())).descriptor().target(),
        *second.state_root().as_bytes()
    );

    let third = checked_map([(b"c".to_vec(), v(3))]);
    let durable = must(must(store.prepare_map(&third, layout)).durable());
    durable::set_test_fault(3);
    let publish_error = durable.publish();
    assert!(matches!(
        publish_error,
        Err(StoreError::PublishedWithSyncPending(_))
    ));
    assert_eq!(
        present(must(store.recover())).state_root(),
        third.state_root()
    );
    must(std::fs::remove_dir_all(path));
}

#[test]
fn publication_authority_serializes_head_writes_and_reports_pre_head_abort() {
    let path = std::env::temp_dir().join(format!(
        "backend-store-publication-authority-{}-{}",
        std::process::id(),
        state_byte(41)
    ));
    let _ = std::fs::remove_dir_all(&path);
    let store = must(FileStore::open(&path, 8_192));
    let authority = must(store.acquire_publication_authority());
    assert!(matches!(
        store.acquire_publication_authority(),
        Err(PublicationAuthorityError::Busy)
    ));
    let map = checked_map([(b"authority".to_vec(), v(1))]);
    let durable = must(must(store.prepare_map(&map, LayoutId::derive(b"authority"))).durable());
    let aborted = must(
        durable.publish_with_authority_before_head(&authority, || Err::<(), _>("stop before HEAD")),
    );
    assert!(matches!(aborted, Err("stop before HEAD")));
    assert!(!path.join("HEAD").exists());
    drop(authority);
    let reopened = must(FileStore::open(&path, 8_192));
    assert_eq!(
        present(must(reopened.head())).descriptor().target(),
        *map.state_root().as_bytes()
    );
    must(std::fs::remove_dir_all(path));
}

#[test]
fn authorityless_publish_cannot_bypass_held_owner_lease() {
    let path = std::env::temp_dir().join(format!(
        "backend-store-publication-authority-bypass-{}-{}",
        std::process::id(),
        state_byte(43)
    ));
    let _ = std::fs::remove_dir_all(&path);
    let store = must(FileStore::open(&path, 8_192));
    let authority = must(store.acquire_publication_authority());
    let reopened = must(FileStore::open(&path, 8_192));
    let map = checked_map([(b"authority-bypass".to_vec(), v(1))]);
    let head_before = std::fs::read(path.join("HEAD")).ok();
    let state_before = std::fs::read(path.join("state")).ok();
    assert!(matches!(
        reopened.publish(&map, LayoutId::derive(b"authority-bypass")),
        Err(StoreError::PublicationAuthorityBusy)
    ));
    assert_eq!(std::fs::read(path.join("HEAD")).ok(), head_before);
    assert_eq!(std::fs::read(path.join("state")).ok(), state_before);
    drop(authority);
    assert_eq!(
        must(reopened.publish(&map, LayoutId::derive(b"authority-bypass"))),
        map.state_root()
    );
    must(std::fs::remove_dir_all(path));
}

#[test]
fn tree_cas_reopens_transitively_and_writes_one_edit_boundary() {
    let path = std::env::temp_dir().join(format!(
        "backend-store-tree-cas-{}-{}",
        std::process::id(),
        state_byte(47)
    ));
    let _ = std::fs::remove_dir_all(&path);
    let store = must(FileStore::open(&path, 8_192));
    let layout = LayoutId::derive(b"tree-cas-layout");
    let base = checked_map(
        (0usize..20_000).map(|index| (format!("k{index:06}").into_bytes(), v(index_byte(index)))),
    );
    let prepared = must(store.prepare_map(&base, layout));
    let durable = must(prepared.durable());
    must(durable.publish());
    let initial = store.last_tree_write_stats();
    assert!(initial.nodes_written > 1);
    assert!(initial.bytes_written > 0);

    let edited = must(base.apply(&[Change {
        key: b"k010000".to_vec(),
        before: Some(v(index_byte(10_000))),
        after: Some(v(201)),
    }]));
    let prepared = must(store.prepare_map(&edited, layout));
    let durable = must(prepared.durable());
    must(durable.publish());
    let update = store.last_tree_write_stats();
    assert!(update.nodes_written > 0);
    assert!(update.nodes_written < initial.nodes_written);
    assert!(update.bytes_written < initial.bytes_written);
    assert!(update.nodes_visited < 64);

    let reopened = must(FileStore::open(&path, 8_192));
    let recovered = present(must(reopened.recover()));
    assert_eq!(recovered, edited);
    let warm_edit = must(recovered.apply(&[Change {
        key: b"k010001".to_vec(),
        before: Some(v(index_byte(10_001))),
        after: Some(v(202)),
    }]));
    let durable = must(must(reopened.prepare_map(&warm_edit, layout)).durable());
    must(durable.publish());
    let warm = reopened.last_tree_write_stats();
    assert!(warm.nodes_written > 0);
    assert!(warm.nodes_visited < 64);
    assert!(warm.nodes_written < initial.nodes_written);

    let lazy = present(must(reopened.open_tree()));
    let (lazy_value, read_stats) = must(lazy.get_with_stats(b"k010001"));
    assert_eq!(lazy_value, Some(v(202)));
    assert!(read_stats.nodes_read <= 16);
    assert!(read_stats.bytes_read > 0);

    let child_version = present(edited.root_node().children().next())
        .id()
        .to_bytes();
    let child = path
        .join("nodes")
        .join(format!("rel-73-0001-01-{}.ref", test_hex(&child_version)));
    assert!(child.is_file());
    must(std::fs::remove_file(child));
    let reopened = must(FileStore::open(&path, 8_192));
    assert!(matches!(reopened.recover(), Err(StoreError::Corrupt)));
    must(std::fs::remove_dir_all(path));
}

#[test]
fn checked_workspace_publication_binds_manifest_refs_and_typed_root() {
    let authority_value = 7u64;
    let authority_key =
        backend_version::ObjectKey::<TestCoverageSchema>::from_value(&authority_value);
    let authority_version =
        backend_version::ObjectVersion::<TestCoverageSchema>::from_value(&authority_value);
    let workspace_manifest = must(backend_version::CheckedWorkspaceManifest::from_versions(
        1,
        Vec::new(),
        Vec::new(),
        authority_version,
        coverage(),
    ));
    let object = TypedObject::from_value(&authority_key, &authority_value);
    let closure = must(ClosureManifest::new(vec![object]));

    let path = std::env::temp_dir().join(format!(
        "backend-store-workspace-{}-{}",
        std::process::id(),
        state_byte(31)
    ));
    let _ = std::fs::remove_dir_all(&path);
    let store = must(FileStore::open(&path, 8_192));
    let first_receipt = must(store.write_object_with_receipt(&closure.objects()[0]));
    assert!(first_receipt.created());
    assert_eq!(first_receipt.id(), closure.objects()[0].id());
    assert!(first_receipt.bytes() > 0);
    let second_receipt = must(store.write_object_with_receipt(&closure.objects()[0]));
    assert!(!second_receipt.created());
    assert_eq!(second_receipt.id(), first_receipt.id());
    assert_eq!(second_receipt.bytes(), first_receipt.bytes());
    assert!(must(store.contains_object(first_receipt.id())));
    assert_eq!(
        must(store.read_object(first_receipt.id())),
        closure.objects()[0]
    );
    let claim = UntrustedObjectId::from_bytes(*first_receipt.id().as_bytes());
    assert_eq!(must(store.read_object_claim(claim)), closure.objects()[0]);
    let map = checked_map([(b"pack".to_vec(), v(1))]);
    let layout = LayoutId::derive(b"workspace-layout");
    let pack = must(encode_pack(&map, layout, 8_192));
    let pack_id = must(store.write_pack(&pack));
    let publication =
        CheckedWorkspacePublication::new(&workspace_manifest, closure, layout, pack_id, None);
    let authority = must(store.acquire_publication_authority());
    let published = must(must(store.prepare_checked_workspace_publication(publication)).durable())
        .publish_with_authority(&authority);
    let published = must(published);
    assert_eq!(published.root(), workspace_manifest.root());
    let reopened = must(FileStore::open(&path, 8_192));
    let head = present(must(reopened.head()));
    assert_eq!(
        head.descriptor().target(),
        workspace_manifest.root().to_bytes()
    );
    assert!(
        must(std::fs::read_dir(path.join("objects")))
            .find_map(Result::ok)
            .is_some()
    );
    must(std::fs::remove_dir_all(path));
}
