use std::collections::{BTreeMap, BTreeSet};

use super::*;

fn key(family: RowFamily, ordinal: u64) -> StableRowKey {
    let mut bytes = [0_u8; 32];
    bytes[24..].copy_from_slice(&ordinal.to_be_bytes());
    StableRowKey::new(family, bytes)
}

fn payload(bytes: &[u8]) -> RowPayload {
    RowPayload::from_bytes(bytes).expect("small fixture payload")
}

fn fixture_rows(count: usize) -> Vec<(StableRowKey, RowPayload)> {
    (0..count)
        .map(|ordinal| {
            let ordinal = u64::try_from(ordinal).expect("fixture ordinal fits");
            let row = ordinal.to_be_bytes();
            (key(RowFamily::Core, ordinal), payload(&row))
        })
        .collect()
}

fn modeled_payload(family: RowFamily, case: usize, ordinal: u64, marker: u8) -> RowPayload {
    let mut bytes = [0_u8; 11];
    bytes[0] = marker;
    bytes[1] = u8::try_from(case).expect("property case fits in one byte");
    bytes[2] = family.code();
    bytes[3..].copy_from_slice(&ordinal.to_be_bytes());
    payload(&bytes)
}

fn oracle_diff(
    before: &BTreeMap<StableRowKey, RowPayload>,
    after: &BTreeMap<StableRowKey, RowPayload>,
) -> Vec<(StableRowKey, Option<RowPayload>, Option<RowPayload>)> {
    let keys: BTreeSet<_> = before.keys().chain(after.keys()).copied().collect();
    keys.into_iter()
        .filter_map(|key| {
            let before_value = before.get(&key).copied();
            let after_value = after.get(&key).copied();
            (before_value != after_value).then_some((key, before_value, after_value))
        })
        .collect()
}

fn observed_diff(
    diff: &[StableRowIndexDiffEntry<'_, '_>],
) -> Vec<(StableRowKey, Option<RowPayload>, Option<RowPayload>)> {
    diff.iter()
        .map(|change| match change {
            StableRowIndexDiffEntry::Insert { key, after } => (*key, None, Some(**after)),
            StableRowIndexDiffEntry::Replace { key, before, after } => {
                (*key, Some(**before), Some(**after))
            }
            StableRowIndexDiffEntry::Delete { key, before } => (*key, Some(**before), None),
        })
        .collect()
}

fn replay_diff(
    rows: &mut BTreeMap<StableRowKey, RowPayload>,
    changes: &[StableRowIndexDiffEntry<'_, '_>],
) {
    for change in changes {
        match change {
            StableRowIndexDiffEntry::Insert { key, after } => {
                assert!(rows.insert(*key, **after).is_none());
            }
            StableRowIndexDiffEntry::Replace { key, before, after } => {
                assert_eq!(rows.insert(*key, **after), Some(**before));
            }
            StableRowIndexDiffEntry::Delete { key, before } => {
                assert_eq!(rows.remove(key), Some(**before));
            }
        }
    }
}

#[test]
fn payload_claim_binds_exact_length_and_preimage() {
    let admitted = payload(b"semantic row A");
    assert_eq!(admitted.verify_bytes(b"semantic row A"), Ok(()));
    assert_eq!(
        admitted.verify_bytes(b"semantic row B"),
        Err(StableRowIndexError::PayloadDigestMismatch)
    );

    let wrong_length =
        UntrustedRowPayloadIdentity::from_raw(*admitted.id().as_bytes(), admitted.byte_len() + 1);
    assert_eq!(
        wrong_length.admit(b"semantic row A"),
        Err(StableRowIndexError::PayloadLengthMismatch)
    );

    assert_ne!(
        payload(b"semantic row A").id(),
        payload(b"semantic row B").id()
    );
}

#[test]
fn tagged_payload_claim_rechecks_its_tag_and_exact_payload_after_reopen() {
    let payload = RowPayload::from_tagged_bytes(7, b"hello").expect("tagged row");
    let claim = payload.claim();
    assert_eq!(claim.admit(b"hello"), Ok(payload));
    assert_eq!(payload.verify_bytes(b"hello"), Ok(()));
    assert_eq!(
        UntrustedRowPayloadIdentity::from_tagged_raw(8, *payload.id().as_bytes(), 5)
            .admit(b"hello"),
        Err(StableRowIndexError::PayloadDigestMismatch)
    );
    assert_eq!(
        UntrustedRowPayloadIdentity::from_raw(*payload.id().as_bytes(), 5).admit(b"hello"),
        Err(StableRowIndexError::PayloadDigestMismatch)
    );
    assert_ne!(
        RowPayload::from_bytes(&[7, b'h', b'e', b'l', b'l', b'o'])
            .expect("raw row")
            .id(),
        payload.id()
    );
}

#[test]
fn builder_and_bulk_admission_reject_unsorted_and_duplicate_keys() {
    let rows = [
        (key(RowFamily::Core, 2), payload(b"two")),
        (key(RowFamily::Core, 1), payload(b"one")),
    ];
    assert_eq!(
        StableRowIndex::from_sorted_rows(&rows).err(),
        Some(StableRowIndexError::Tree(TreeError::UnsortedOrDuplicate))
    );

    let duplicate = [
        (key(RowFamily::Core, 1), payload(b"first")),
        (key(RowFamily::Core, 1), payload(b"second")),
    ];
    assert_eq!(
        StableRowIndex::from_sorted_rows(&duplicate).err(),
        Some(StableRowIndexError::Tree(TreeError::UnsortedOrDuplicate))
    );

    let mut builder = StableRowIndex::builder();
    builder
        .push(key(RowFamily::Core, 2), payload(b"two"))
        .expect("first key is accepted");
    assert_eq!(
        builder.push(key(RowFamily::Core, 1), payload(b"one")),
        Err(StableRowIndexError::UnsortedOrDuplicate)
    );
}

#[test]
fn batch_update_handles_insert_replace_delete_and_reuses_unchanged_nodes() {
    let initial = vec![
        (key(RowFamily::Core, 1), payload(b"one")),
        (key(RowFamily::Core, 3), payload(b"old three")),
        (key(RowFamily::Core, 5), payload(b"five")),
    ];
    let base = StableRowIndex::from_sorted_rows(&initial).expect("valid index");
    let changes = [
        StableRowPayloadChange::put(key(RowFamily::Core, 2), 2, b"two"),
        StableRowPayloadChange::put(key(RowFamily::Core, 3), 2, b"new three"),
        StableRowPayloadChange::delete(key(RowFamily::Core, 5)),
    ];
    let prepared = base
        .prepare_payload_update(&changes)
        .expect("valid sorted changes");
    assert_eq!(prepared.work().changed_keys, 3);
    assert_eq!(prepared.work().row_payload_hash_bytes, 3 + 1 + 9 + 1);
    let next = prepared.commit();

    assert_eq!(next.get(&key(RowFamily::Core, 1)), Some(payload(b"one")));
    assert_eq!(
        next.get(&key(RowFamily::Core, 2)),
        Some(RowPayload::from_tagged_bytes(2, b"two").expect("tagged payload"))
    );
    assert_eq!(
        next.get(&key(RowFamily::Core, 3)),
        Some(RowPayload::from_tagged_bytes(2, b"new three").expect("tagged payload"))
    );
    assert_eq!(next.get(&key(RowFamily::Core, 5)), None);
    assert_eq!(next.row_count(), 3);
    assert_ne!(base.root(), next.root());

    let many_rows = fixture_rows(1_024);
    let wide_base = StableRowIndex::from_sorted_rows(&many_rows).expect("valid wide index");
    let edit = [StableRowIndexChange::put(
        key(RowFamily::Core, 511),
        payload(b"changed"),
    )];
    let wide_next = wide_base
        .prepare_update(&edit)
        .expect("one ordered replacement")
        .commit();
    let old_nodes: BTreeSet<_> = wide_base
        .tree
        .node_closure()
        .map(|node| node.id().to_bytes())
        .collect();
    let retained_nodes = wide_next
        .tree
        .node_closure()
        .filter(|node| old_nodes.contains(&node.id().to_bytes()))
        .count();
    assert!(retained_nodes > 0);

    let sparse_edits = [
        StableRowIndexChange::put(
            key(RowFamily::Core, 10),
            payload(b"first distant replacement"),
        ),
        StableRowIndexChange::put(
            key(RowFamily::Core, 900),
            payload(b"second distant replacement"),
        ),
    ];
    let sparse = wide_base
        .prepare_update(&sparse_edits)
        .expect("distant changes stay independently path-copied");
    assert_eq!(sparse.work().changed_keys, 2);
    assert!(sparse.work().tree.rows < many_rows.len());
}

#[test]
fn no_op_frontier_reuses_the_root_and_hashes_zero_payload_bytes() {
    let index = StableRowIndex::from_sorted_rows(&fixture_rows(16)).expect("valid index");
    let prepared = index
        .prepare_update(&[StableRowIndexChange::unchanged(key(RowFamily::Core, 4))])
        .expect("unchanged frontier entry");
    assert_eq!(prepared.index().root(), index.root());
    assert_eq!(prepared.work().changed_keys, 0);
    assert_eq!(prepared.work().row_payload_hash_bytes, 0);
    assert_eq!(prepared.work().tree, TreeWork::default());

    let empty = index
        .prepare_payload_update(&[])
        .expect("empty producer frontier");
    assert_eq!(empty.index().root(), index.root());
    assert_eq!(empty.work().row_payload_hash_bytes, 0);
    assert_eq!(empty.work().tree, TreeWork::default());

    let unchanged = index
        .prepare_payload_update(&[StableRowPayloadChange::unchanged(key(RowFamily::Core, 4))])
        .expect("explicit unchanged producer frontier");
    assert_eq!(unchanged.index().root(), index.root());
    assert_eq!(unchanged.work().changed_keys, 0);
    assert_eq!(unchanged.work().row_payload_hash_bytes, 0);
    assert_eq!(unchanged.work().tree, TreeWork::default());
}

#[test]
fn sparse_tagged_update_matches_full_seven_family_rebuild() {
    let mut before_records = Vec::new();
    let mut after_records = Vec::new();
    for family in RowFamily::ALL {
        for ordinal in 0..4 {
            let stable_key = key(family, ordinal);
            let tag = family
                .code()
                .wrapping_add(u8::try_from(ordinal).expect("small ordinal"));
            let bytes = [
                family.code(),
                u8::try_from(ordinal).expect("small ordinal"),
                0xa5,
            ];
            before_records.push((
                stable_key,
                RowPayload::from_tagged_bytes(tag, &bytes).expect("valid tagged payload"),
            ));
            if !(family == RowFamily::Types && ordinal == 1)
                && !(family == RowFamily::Occurrences && ordinal == 2)
            {
                let (after_tag, after_bytes) = if family == RowFamily::Types && ordinal == 3 {
                    (0x71, [0xde, 0xad, 0x01])
                } else {
                    (tag, bytes)
                };
                after_records.push((
                    stable_key,
                    RowPayload::from_tagged_bytes(after_tag, &after_bytes)
                        .expect("valid tagged payload"),
                ));
            }
        }
    }
    let base = StableRowIndex::from_sorted_rows(&before_records).expect("seven-family base");
    let changes = [
        StableRowPayloadChange::delete(key(RowFamily::Types, 1)),
        StableRowPayloadChange::put(key(RowFamily::Types, 3), 0x71, &[0xde, 0xad, 0x01]),
        StableRowPayloadChange::delete(key(RowFamily::Occurrences, 2)),
    ];

    let sparse = base
        .prepare_payload_update(&changes)
        .expect("ordered sparse tagged update")
        .commit();
    let rebuilt = StableRowIndex::from_sorted_rows(&after_records).expect("full tagged rebuild");

    assert_eq!(sparse.root(), rebuilt.root());
    assert_eq!(sparse.row_count(), rebuilt.row_count());
    assert_eq!(
        sparse.get(&key(RowFamily::Types, 3)),
        Some(RowPayload::from_tagged_bytes(0x71, &[0xde, 0xad, 0x01]).expect("tagged payload"))
    );
    assert_eq!(
        base.get(&key(RowFamily::Types, 3)),
        Some(RowPayload::from_tagged_bytes(5, &[2, 3, 0xa5]).expect("old tagged payload"))
    );
    assert_eq!(sparse.get(&key(RowFamily::Types, 1)), None);
    assert_eq!(sparse.get(&key(RowFamily::Occurrences, 2)), None);
}

#[test]
fn payload_update_rejects_unsorted_and_duplicate_actions() {
    let index = StableRowIndex::from_sorted_rows(&fixture_rows(8)).expect("valid index");
    let unsorted = [
        StableRowPayloadChange::put(key(RowFamily::Core, 2), 1, b"two"),
        StableRowPayloadChange::delete(key(RowFamily::Core, 1)),
    ];
    assert_eq!(
        index.prepare_payload_update(&unsorted).err(),
        Some(StableRowIndexError::UnsortedOrDuplicate)
    );

    let duplicate = [
        StableRowPayloadChange::delete(key(RowFamily::Core, 2)),
        StableRowPayloadChange::put(key(RowFamily::Core, 2), 1, b"replacement"),
    ];
    assert_eq!(
        index.prepare_payload_update(&duplicate).err(),
        Some(StableRowIndexError::UnsortedOrDuplicate)
    );
}

#[test]
fn borrowed_two_root_diff_skips_no_op_and_sparse_equal_subtrees() {
    let before_rows = fixture_rows(4_096);
    let before = StableRowIndex::from_sorted_rows(&before_rows).expect("valid base index");
    let no_op = before.diff(&before).expect("same-root diff");
    assert!(no_op.entries().is_empty());
    assert_eq!(no_op.work().skipped_equal_subtrees, 1);
    assert_eq!(no_op.work().row_records_examined, 0);
    assert_eq!(no_op.work().decoded_rows, 0);

    let changes = [
        StableRowIndexChange::put(key(RowFamily::Core, 4), payload(b"sparse low replacement")),
        StableRowIndexChange::put(
            key(RowFamily::Core, 4_090),
            payload(b"sparse high replacement"),
        ),
    ];
    let after = before
        .prepare_update(&changes)
        .expect("two ordered sparse changes")
        .commit();
    let diff = before.diff(&after).expect("borrowed two-root diff");
    assert_eq!(diff.before_root(), before.root());
    assert_eq!(diff.after_root(), after.root());
    assert_eq!(diff.entries().len(), 2);
    assert!(matches!(
        diff.entries()[0],
        StableRowIndexDiffEntry::Replace { key: found, .. }
            if found == key(RowFamily::Core, 4)
    ));
    assert!(matches!(
        diff.entries()[1],
        StableRowIndexDiffEntry::Replace { key: found, .. }
            if found == key(RowFamily::Core, 4_090)
    ));
    let tree_nodes = before.tree.node_closure().count() + after.tree.node_closure().count();
    assert!(usize::try_from(diff.work().visited_nodes).expect("counter fits") < tree_nodes * 2);
    assert!(diff.work().skipped_equal_subtrees > 0);
    assert!(diff.work().row_records_examined < 4_096);
    assert_eq!(diff.work().decoded_rows, 0);
}

#[test]
fn diff_equal_subtree_certificates_are_root_bound_and_cover_exact_rows() {
    let empty = StableRowIndex::from_sorted_rows(&[]).expect("canonical empty root");
    let empty_diff = empty.diff(&empty).expect("empty same-root diff");
    let [empty_certificate] = empty_diff.unchanged_subtrees() else {
        panic!("equal empty roots produce one borrowed root certificate");
    };
    assert_eq!(empty_certificate.first_key(), None);
    assert_eq!(empty_certificate.last_key(), None);
    assert_eq!(empty_certificate.row_count(), 0);
    assert!(empty_certificate.is_scoped_to(&empty, &empty));

    let before_rows = fixture_rows(4_096);
    let before = StableRowIndex::from_sorted_rows(&before_rows).expect("valid base index");
    let no_op = before.diff(&before).expect("same-root diff");
    let [root_certificate] = no_op.unchanged_subtrees() else {
        panic!("equal roots produce one borrowed root certificate");
    };
    assert_eq!(root_certificate.before_root(), before.root());
    assert_eq!(root_certificate.after_root(), before.root());
    assert_eq!(root_certificate.first_key(), Some(key(RowFamily::Core, 0)));
    assert_eq!(
        root_certificate.last_key(),
        Some(key(RowFamily::Core, 4_095))
    );
    assert_eq!(root_certificate.row_count(), 4_096);
    assert_eq!(
        root_certificate.subtree_commitment(),
        before.tree.root_view().id().to_bytes()
    );
    assert!(root_certificate.is_scoped_to(&before, &before));

    let changes = [
        StableRowIndexChange::put(key(RowFamily::Core, 4), payload(b"sparse low replacement")),
        StableRowIndexChange::put(
            key(RowFamily::Core, 4_090),
            payload(b"sparse high replacement"),
        ),
    ];
    let after = before
        .prepare_update(&changes)
        .expect("two ordered sparse changes")
        .commit();
    let diff = before.diff(&after).expect("borrowed two-root diff");
    assert!(!diff.unchanged_subtrees().is_empty());
    assert_eq!(
        u64::try_from(diff.unchanged_subtrees().len()).expect("certificate count fits"),
        diff.work().skipped_equal_subtrees
    );

    let before_node_ids: BTreeSet<_> = before
        .tree
        .node_closure()
        .map(|node| node.id().to_bytes())
        .collect();
    let after_node_ids: BTreeSet<_> = after
        .tree
        .node_closure()
        .map(|node| node.id().to_bytes())
        .collect();
    let changed_keys = [key(RowFamily::Core, 4), key(RowFamily::Core, 4_090)];
    let mut previous_last_key = None;
    for certificate in diff.unchanged_subtrees() {
        let first_key = certificate
            .first_key()
            .expect("nonempty fixture subtree has a first key");
        let last_key = certificate
            .last_key()
            .expect("nonempty fixture subtree has a last key");
        assert_eq!(certificate.before_root(), before.root());
        assert_eq!(certificate.after_root(), after.root());
        assert!(certificate.is_scoped_to(&before, &after));
        assert!(!certificate.is_scoped_to(&after, &before));
        assert!(certificate.row_count() > 0);
        assert!(first_key <= last_key);
        assert!(previous_last_key.is_none_or(|previous| previous < first_key));
        previous_last_key = Some(last_key);
        assert!(before_node_ids.contains(&certificate.subtree_commitment()));
        assert!(after_node_ids.contains(&certificate.subtree_commitment()));
        assert!(
            changed_keys
                .iter()
                .all(|changed| { *changed < first_key || *changed > last_key })
        );

        let before_rows_in_range: Vec<_> = before
            .range(StableRowRange::new(None, None).expect("unbounded range"))
            .expect("valid prior range")
            .filter(|entry| *entry.key >= first_key && *entry.key <= last_key)
            .map(|entry| (*entry.key, *entry.payload))
            .collect();
        let after_rows_in_range: Vec<_> = after
            .range(StableRowRange::new(None, None).expect("unbounded range"))
            .expect("valid target range")
            .filter(|entry| *entry.key >= first_key && *entry.key <= last_key)
            .map(|entry| (*entry.key, *entry.payload))
            .collect();
        assert_eq!(before_rows_in_range, after_rows_in_range);
        assert_eq!(
            u64::try_from(before_rows_in_range.len()).expect("row count fits"),
            certificate.row_count()
        );
    }
}

#[test]
fn divergent_run_cursor_borrows_large_row_runs_with_tree_depth_scratch() {
    let rows = fixture_rows(16_384);
    let index = StableRowIndex::from_sorted_rows(&rows).expect("valid base index");
    let root = index.tree.root_view();
    let roots = [root];
    let mut cursor = RunRowIter::new(&roots).expect("bounded cursor");
    assert_eq!(cursor.len(), rows.len());
    for (key, payload) in &rows {
        assert_eq!(cursor.next(), Some((key, payload)));
    }
    assert_eq!(cursor.next(), None);
    assert_eq!(cursor.len(), 0);
    assert!(cursor.visited_nodes > 1);
    assert!(cursor.frames.capacity() < 1_024);
}

#[test]
fn fresh_repartition_after_prefix_insert_does_not_buffer_equal_row_runs() {
    let after_rows = fixture_rows(16_385);
    let before = StableRowIndex::from_sorted_rows(&after_rows[1..]).expect("base index");
    let after = StableRowIndex::from_sorted_rows(&after_rows).expect("repartitioned index");
    let diff = before.diff(&after).expect("borrowed repartition diff");
    assert!(matches!(
        diff.entries(),
        [StableRowIndexDiffEntry::Insert { key: inserted, .. }]
            if *inserted == key(RowFamily::Core, 0)
    ));
    assert!(diff.work().peak_run_cursor_scratch_bytes < 4_096);
    assert_eq!(diff.work().decoded_rows, 0);
}

#[test]
fn two_root_diff_matches_btree_map_oracle_for_insert_delete_replace() {
    let before_rows = fixture_rows(4_096);
    let before = StableRowIndex::from_sorted_rows(&before_rows).expect("valid base index");
    let mut oracle: BTreeMap<_, _> = before_rows.iter().copied().collect();
    let changes = [
        StableRowIndexChange::delete(key(RowFamily::Core, 0)),
        StableRowIndexChange::put(key(RowFamily::Core, 7), payload(b"replacement")),
        StableRowIndexChange::put(key(RowFamily::Core, 4_096), payload(b"inserted")),
    ];
    let after = before
        .prepare_update(&changes)
        .expect("ordered insert delete and replacement")
        .commit();
    oracle.remove(&key(RowFamily::Core, 0));
    oracle.insert(key(RowFamily::Core, 7), payload(b"replacement"));
    oracle.insert(key(RowFamily::Core, 4_096), payload(b"inserted"));

    let before_map: BTreeMap<_, _> = before
        .range(StableRowRange::new(None, None).expect("unbounded range"))
        .expect("valid full range")
        .map(|entry| (*entry.key, *entry.payload))
        .collect();
    let mut expected = Vec::new();
    for (key, before_value) in &before_map {
        match oracle.get(key) {
            Some(after_value) if after_value != before_value => {
                expected.push((*key, Some(*before_value), Some(*after_value)));
            }
            Some(_) => {}
            None => expected.push((*key, Some(*before_value), None)),
        }
    }
    for (key, after_value) in &oracle {
        if !before_map.contains_key(key) {
            expected.push((*key, None, Some(*after_value)));
        }
    }
    expected.sort_by_key(|(key, _, _)| *key);

    let after_map: BTreeMap<_, _> = after
        .range(StableRowRange::new(None, None).expect("unbounded range"))
        .expect("valid full range")
        .map(|entry| (*entry.key, *entry.payload))
        .collect();
    assert_eq!(after_map, oracle);

    let diff = before.diff(&after).expect("admitted roots diff");
    let actual: Vec<_> = diff
        .entries()
        .iter()
        .map(|change| match change {
            StableRowIndexDiffEntry::Insert { key, after } => (*key, None, Some(**after)),
            StableRowIndexDiffEntry::Replace { key, before, after } => {
                (*key, Some(**before), Some(**after))
            }
            StableRowIndexDiffEntry::Delete { key, before } => (*key, Some(**before), None),
        })
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn two_root_diff_handles_empty_and_different_height_roots() {
    let empty = StableRowIndex::from_sorted_rows(&[]).expect("canonical empty root");
    let short = StableRowIndex::from_sorted_rows(&fixture_rows(16)).expect("short leaf root");
    let tall = StableRowIndex::from_sorted_rows(&fixture_rows(1_024)).expect("branch root");

    let inserted = empty.diff(&short).expect("empty to populated root");
    assert_eq!(inserted.entries().len(), 16);
    assert!(
        inserted
            .entries()
            .iter()
            .all(|entry| matches!(entry, StableRowIndexDiffEntry::Insert { .. }))
    );

    let height_change = short.diff(&tall).expect("leaf to branch root");
    assert_eq!(height_change.entries().len(), 1_008);
    assert!(
        height_change
            .entries()
            .iter()
            .all(|entry| matches!(entry, StableRowIndexDiffEntry::Insert { .. }))
    );
    assert_eq!(height_change.work().decoded_rows, 0);
}

#[test]
fn structured_multifamily_diffs_match_oracle_and_replay_across_boundary_edits() {
    // Seventy-two deterministic cardinality/edit combinations exercise
    // family boundaries and leaf repartitioning without fuzz-test runtime.
    const CARDINALITIES: [usize; 24] = [
        0, 1, 2, 3, 31, 63, 64, 65, 127, 255, 256, 257, 383, 511, 512, 513, 700, 736, 737, 768,
        1_000, 1_023, 17, 129,
    ];

    for case in 0..72 {
        let cardinality = CARDINALITIES[case % CARDINALITIES.len()];
        let mut before_model = BTreeMap::new();
        let mut family_bounds = [(0_u64, 0_usize); 7];
        for (family_index, family) in RowFamily::ALL.into_iter().enumerate() {
            let family_start = 2_000_u64
                .checked_add(
                    u64::try_from(family_index)
                        .expect("seven family indexes fit")
                        .checked_mul(5_000)
                        .expect("family key offset fits"),
                )
                .expect("family key offset fits");
            let variation = (case * 11 + family_index * 7) % 15;
            let count = cardinality.saturating_sub(variation);
            family_bounds[family_index] = (family_start, count);
            for offset in 0..count {
                let ordinal = family_start
                    .checked_add(u64::try_from(offset).expect("row offset fits"))
                    .expect("row key fits");
                before_model.insert(
                    key(family, ordinal),
                    modeled_payload(family, case, ordinal, 0),
                );
            }
        }

        let mut pending = BTreeMap::<StableRowKey, Option<RowPayload>>::new();
        if case % 12 != 0 {
            for (family_index, family) in RowFamily::ALL.into_iter().enumerate() {
                let (family_start, count) = family_bounds[family_index];
                if count != 0 {
                    match (case + family_index) % 4 {
                        0 => {
                            pending.insert(key(family, family_start), None);
                        }
                        1 => {
                            let ordinal = family_start
                                + u64::try_from(count / 2).expect("middle row offset fits");
                            pending.insert(
                                key(family, ordinal),
                                Some(modeled_payload(family, case, ordinal, 1)),
                            );
                        }
                        2 => {
                            let ordinal = family_start
                                + u64::try_from(count - 1).expect("last row offset fits");
                            pending.insert(key(family, ordinal), None);
                        }
                        _ => {
                            let ordinal = family_start
                                + u64::try_from(count / 3).expect("replacement row offset fits");
                            pending.insert(
                                key(family, ordinal),
                                Some(modeled_payload(family, case, ordinal, 2)),
                            );
                        }
                    }
                }

                match (case * 3 + family_index) % 6 {
                    0 => {
                        let ordinal = family_start - 1;
                        pending.insert(
                            key(family, ordinal),
                            Some(modeled_payload(family, case, ordinal, 3)),
                        );
                    }
                    1 => {
                        let ordinal =
                            family_start + u64::try_from(count).expect("end boundary offset fits");
                        pending.insert(
                            key(family, ordinal),
                            Some(modeled_payload(family, case, ordinal, 4)),
                        );
                    }
                    2 => {
                        let end_ordinal =
                            family_start + u64::try_from(count).expect("end boundary offset fits");
                        for (ordinal, marker) in [(family_start - 1, 3), (end_ordinal, 4)] {
                            pending.insert(
                                key(family, ordinal),
                                Some(modeled_payload(family, case, ordinal, marker)),
                            );
                        }
                    }
                    _ => {}
                }
            }
        }

        let mut after_model = before_model.clone();
        for (key, after) in &pending {
            if let Some(payload) = after {
                after_model.insert(*key, *payload);
            } else {
                assert!(after_model.remove(key).is_some());
            }
        }

        let before_rows: Vec<_> = before_model
            .iter()
            .map(|(key, value)| (*key, *value))
            .collect();
        let before = StableRowIndex::from_sorted_rows(&before_rows).expect("valid model root");
        let mut changes: Vec<_> = pending
            .iter()
            .map(|(key, after)| match after {
                Some(payload) => StableRowIndexChange::put(*key, *payload),
                None => StableRowIndexChange::delete(*key),
            })
            .collect();
        if case % 9 == 0 {
            if let Some(key) = before_model.keys().find(|key| !pending.contains_key(*key)) {
                changes.push(StableRowIndexChange::unchanged(*key));
            }
        }
        changes.sort_by_key(|change| change.key);
        let after = before
            .prepare_update(&changes)
            .expect("strictly ordered model frontier")
            .commit();

        let target_rows: Vec<_> = after_model
            .iter()
            .map(|(key, value)| (*key, *value))
            .collect();
        let rebuilt = StableRowIndex::from_sorted_rows(&target_rows).expect("valid oracle root");
        assert_eq!(after.root(), rebuilt.root(), "case {case}: update root");

        let forward = before.diff(&after).expect("forward root diff");
        assert_eq!(
            forward.before_root(),
            before.root(),
            "case {case}: source root"
        );
        assert_eq!(
            forward.after_root(),
            rebuilt.root(),
            "case {case}: target root"
        );
        assert_eq!(forward.work().decoded_rows, 0, "case {case}: decoded rows");
        let expected = oracle_diff(&before_model, &after_model);
        let observed = observed_diff(forward.entries());
        assert_eq!(observed, expected, "case {case}: diff");
        assert!(
            observed.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "case {case}: ordering"
        );

        let mut replayed = before_model.clone();
        replay_diff(&mut replayed, forward.entries());
        assert_eq!(replayed, after_model, "case {case}: forward replay");
        assert_eq!(
            replayed,
            oracle_from_index(&after),
            "case {case}: indexed replay"
        );

        let reverse = after.diff(&before).expect("reverse root diff");
        assert_eq!(
            observed_diff(reverse.entries()),
            oracle_diff(&after_model, &before_model)
        );
        replay_diff(&mut replayed, reverse.entries());
        assert_eq!(replayed, before_model, "case {case}: reverse replay");
        assert_eq!(reverse.before_root(), after.root());
        assert_eq!(reverse.after_root(), before.root());
    }
}

fn oracle_from_index(index: &StableRowIndex) -> BTreeMap<StableRowKey, RowPayload> {
    index
        .range(StableRowRange::new(None, None).expect("unbounded range"))
        .expect("valid full range")
        .map(|entry| (*entry.key, *entry.payload))
        .collect()
}

#[test]
fn family_census_is_exact_and_rejects_omitted_rows() {
    let mut rows = Vec::new();
    for family in RowFamily::ALL {
        rows.push((key(family, 1), payload(&[family.code(), 1])));
        rows.push((key(family, 2), payload(&[family.code(), 2])));
    }
    let index = StableRowIndex::from_sorted_rows(&rows).expect("family order is canonical");
    let census = index.census().expect("exact seven-family census");
    assert_eq!(census.counts(), [2; 7]);
    assert_eq!(census.root(), index.root());
    assert_eq!(index.row_count(), 14);

    let mut omitted = index.census_proof().expect("proof emitted");
    omitted.counts[RowFamily::Types.index()] -= 1;
    assert_eq!(
        index.admit_census(omitted),
        Err(StableRowIndexError::CensusMismatch)
    );

    let wrong_root = UntrustedStableRowIndexCensus::from_raw([0; 32], [2; 7]);
    assert_eq!(
        index.admit_census(wrong_root),
        Err(StableRowIndexError::RootMismatch)
    );
}

#[test]
fn ordered_range_cursor_and_root_bound_proof_reject_omissions() {
    let index = StableRowIndex::from_sorted_rows(&fixture_rows(12)).expect("valid index");
    let range = StableRowRange::new(Some(key(RowFamily::Core, 3)), Some(key(RowFamily::Core, 9)))
        .expect("valid half-open range");
    let cursor = index.range(range).expect("range seeks successfully");
    assert_eq!(cursor.len(), 6);
    assert_eq!(cursor.root(), index.root());
    assert_eq!(
        cursor.map(|entry| entry.key.key()[31]).collect::<Vec<_>>(),
        vec![3, 4, 5, 6, 7, 8]
    );

    let mut proof = index.range_proof(range).expect("range proof emitted");
    proof.entries = proof.entries[1..].to_vec().into_boxed_slice();
    assert_eq!(
        index.admit_range_proof(&proof).err(),
        Some(StableRowIndexError::RangeProofMismatch)
    );

    let wrong_root = StableRowRangeProof::from_raw([0; 32], range, Vec::new());
    assert_eq!(
        index.admit_range_proof(&wrong_root).err(),
        Some(StableRowIndexError::RootMismatch)
    );

    let mut unsorted = index.range_proof(range).expect("range proof emitted");
    unsorted.entries.swap(0, 1);
    assert_eq!(
        index.admit_range_proof(&unsorted).err(),
        Some(StableRowIndexError::UnsortedOrDuplicate)
    );

    let out_of_range = StableRowRangeProof::from_raw(
        index.root().as_bytes(),
        range,
        vec![StableRowRangeProofEntry::from_raw(
            key(RowFamily::Core, 10),
            payload(&10_u64.to_be_bytes()).claim(),
        )],
    );
    assert_eq!(
        index.admit_range_proof(&out_of_range).err(),
        Some(StableRowIndexError::RangeProofMismatch)
    );
}

#[test]
fn untrusted_root_requires_exact_complete_records() {
    let rows = fixture_rows(32);
    let index = StableRowIndex::from_sorted_rows(&rows).expect("valid index");
    let admitted = UntrustedStableRowIndexRoot::from_raw(index.root().as_bytes())
        .admit_records(&rows)
        .expect("matching complete record set");
    assert_eq!(admitted.root(), index.root());

    assert_eq!(
        UntrustedStableRowIndexRoot::from_raw([0; 32])
            .admit_records(&rows)
            .err(),
        Some(StableRowIndexError::RootMismatch)
    );

    assert_eq!(
        UntrustedStableRowIndexRoot::from_raw(index.root().as_bytes())
            .admit_records(&rows[..rows.len() - 1])
            .err(),
        Some(StableRowIndexError::RootMismatch)
    );
}

#[test]
fn tagged_payload_identity_commits_the_row_tag_without_copying() {
    let typed = RowPayload::from_tagged_bytes(2, b"value").expect("valid tagged payload");
    let documentation = RowPayload::from_tagged_bytes(5, b"value").expect("valid tagged payload");
    let materialized = RowPayload::from_bytes(&[2, b'v', b'a', b'l', b'u', b'e'])
        .expect("materialized comparison payload");

    assert_eq!(typed.byte_len(), 5);
    assert_ne!(typed.id(), documentation.id());
    assert_ne!(typed.id(), materialized.id());
    assert_eq!(typed.claim().admit(b"value"), Ok(typed));
    assert_eq!(
        materialized
            .claim()
            .admit(&[2, b'v', b'a', b'l', b'u', b'e']),
        Ok(materialized)
    );
}

#[test]
fn rename_is_delete_plus_insert_and_survives_cold_reopen() {
    let old_key = key(RowFamily::Core, 17);
    let new_key = key(RowFamily::Core, 29);
    let unchanged_payload = payload(b"same declaration bytes after rename");
    let before = StableRowIndex::from_sorted_rows(&[(old_key, unchanged_payload)])
        .expect("valid old generation");

    // Stable identity follows the declaration key. Keeping its payload digest
    // while changing the key still means a tombstone and an insertion.
    let changes = [
        StableRowIndexChange::new(old_key, None),
        StableRowIndexChange::new(new_key, Some(unchanged_payload)),
    ];
    let prepared = before
        .prepare_update(&changes)
        .expect("ordered rename frontier");
    assert_eq!(prepared.work().changed_keys, 2);
    assert_eq!(prepared.work().row_payload_hash_bytes, 0);
    let after = prepared.commit();
    let target = BTreeMap::from([(new_key, unchanged_payload)]);
    let reopened = index_from_oracle(&target);
    assert_eq!(after.root(), reopened.root());

    let diff = before.diff(&after).expect("rename diff");
    assert!(matches!(
        diff.entries(),
        [
            StableRowIndexDiffEntry::Delete { key: deleted, .. },
            StableRowIndexDiffEntry::Insert { key: inserted, .. }
        ] if *deleted == old_key && *inserted == new_key
    ));

    let cold = UntrustedStableRowIndexRoot::from_raw(after.root().as_bytes())
        .admit_records(
            &target
                .iter()
                .map(|(key, value)| (*key, *value))
                .collect::<Vec<_>>(),
        )
        .expect("cold reconstruction admits the complete renamed row set");
    assert_eq!(cold.root(), after.root());
}

#[test]
fn complete_scan_oracle_catches_omitted_transitive_and_plane_changes() {
    let mut before_model = BTreeMap::new();
    for family in RowFamily::ALL {
        before_model.insert(key(family, 41), payload(&[family.code(), 1]));
    }
    let before = index_from_oracle(&before_model);

    // This modeled edit changes a referenced type and dependent declaration,
    // relation, occurrence, and extension rows; docs and source provenance
    // also change independently in the same generation. Embedding text has a
    // separate recipe/input identity and is intentionally outside these seven
    // IR families.
    let mut after_model = before_model.clone();
    for family in RowFamily::ALL {
        after_model.insert(key(family, 41), payload(&[family.code(), 2]));
    }
    let rebuilt = index_from_oracle(&after_model);

    // A producer that reports only the directly changed type row would leave
    // derived rows stale. The complete-reader oracle must reject that target.
    let incomplete = [StableRowIndexChange::new(
        key(RowFamily::Types, 41),
        after_model.get(&key(RowFamily::Types, 41)).copied(),
    )];
    let incomplete = before
        .prepare_update(&incomplete)
        .expect("sorted but incomplete producer claim");
    assert_eq!(incomplete.work().changed_keys, 1);
    let candidate = incomplete.commit();
    assert_ne!(candidate.root(), rebuilt.root());

    let expected = oracle_diff(&before_model, &after_model);
    assert_eq!(expected.len(), RowFamily::ALL.len());
    assert_eq!(
        expected
            .iter()
            .map(|(key, _, _)| key.family())
            .collect::<Vec<_>>(),
        RowFamily::ALL.to_vec(),
    );
    let complete_changes = expected
        .iter()
        .map(|(key, _, after)| StableRowIndexChange::new(*key, *after))
        .collect::<Vec<_>>();
    let complete = before
        .prepare_update(&complete_changes)
        .expect("complete modeled frontier");
    assert_eq!(complete.work().changed_keys, 7);
    assert_eq!(complete.work().row_payload_hash_bytes, 0);
    let complete = complete.commit();
    assert_eq!(complete.root(), rebuilt.root());
    assert_eq!(
        before
            .diff(&complete)
            .expect("frontier diff")
            .entries()
            .len(),
        7
    );
}

#[test]
fn empty_frontier_is_not_evidence_of_a_semantic_no_op() {
    let key = key(RowFamily::Documentation, 83);
    let before_rows = [(key, payload(b"old documentation"))];
    let before = StableRowIndex::from_sorted_rows(&before_rows).expect("valid old rows");
    let target_rows = [(key, payload(b"new documentation"))];
    let target = StableRowIndex::from_sorted_rows(&target_rows).expect("full-scan target");

    // The low-level update primitive can produce a zero-work root reuse from
    // an empty claim. Comparing against the independently rebuilt target is
    // the admission oracle that prevents that claim from being mistaken for
    // a complete no-op proof.
    let claimed_no_op = before.prepare_update(&[]).expect("empty sparse claim");
    assert_eq!(claimed_no_op.work().changed_keys, 0);
    assert_eq!(claimed_no_op.work().row_payload_hash_bytes, 0);
    let claimed_no_op = claimed_no_op.commit();
    assert_eq!(claimed_no_op.root(), before.root());
    assert_ne!(claimed_no_op.root(), target.root());
    assert_eq!(
        before.diff(&target).expect("oracle diff").entries().len(),
        1
    );
}

fn index_from_oracle(rows: &BTreeMap<StableRowKey, RowPayload>) -> StableRowIndex {
    let rows = rows
        .iter()
        .map(|(key, value)| (*key, *value))
        .collect::<Vec<_>>();
    StableRowIndex::from_sorted_rows(&rows).expect("oracle rows are strictly ordered")
}

#[test]
fn memory_accounting_separates_referenced_payloads_from_resident_index() {
    let index = StableRowIndex::from_sorted_rows(&fixture_rows(20)).expect("valid index");
    let memory = index.memory_usage().expect("representable accounting");
    assert_eq!(memory.rows, 20);
    assert!(memory.nodes > 0);
    assert!(memory.canonical_node_bytes > 0);
    assert!(memory.row_slot_storage_bytes >= 20 * size_of::<(StableRowKey, RowPayload)>() as u64);
    assert_eq!(memory.referenced_payload_bytes, 20 * 8);
    assert!(memory.estimated_resident_bytes >= memory.canonical_node_bytes);
}
