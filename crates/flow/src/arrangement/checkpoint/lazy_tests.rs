#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::{ArrangementKey, ArrangementRelation, ObjectIdentity, RelationIdentity};
use backend_version::{
    CanonicalRelation, IdContext, Relation, RelationDecodeError, RelationState,
    admit_canonical_root_claim, canonical_root,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug, Eq, PartialEq)]
struct Fixture;

impl Relation for Fixture {
    const DOMAIN: u8 = 0x91;
    const TYPE: u16 = 1;
    type Key = u64;
    type Value = u64;

    fn encode_key(key: &Self::Key, out: &mut Vec<u8>) {
        out.extend_from_slice(&key.to_be_bytes());
    }

    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

impl CanonicalRelation for Fixture {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        bytes
            .try_into()
            .map(u64::from_be_bytes)
            .map_err(|_| RelationDecodeError::Malformed)
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        bytes
            .try_into()
            .map(u64::from_be_bytes)
            .map_err(|_| RelationDecodeError::Malformed)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReadError;

impl fmt::Display for ReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("poisoned or missing canonical node")
    }
}

struct Reader {
    nodes: BTreeMap<[u8; 32], Vec<u8>>,
    poisoned: BTreeSet<[u8; 32]>,
    reads: Arc<AtomicUsize>,
}

impl CasNodeReader<Fixture> for Reader {
    type Error = ReadError;

    fn read_node(
        &self,
        claim: UntrustedId<Fixture>,
    ) -> Result<backend_version::CheckedCanonicalRoot<Fixture>, Self::Error> {
        if self.poisoned.contains(claim.as_bytes()) {
            return Err(ReadError);
        }
        let bytes = self.nodes.get(claim.as_bytes()).ok_or(ReadError)?;
        self.reads.fetch_add(1, Ordering::Relaxed);
        admit_canonical_root_claim(claim, bytes).map_err(|_| ReadError)
    }
}

fn fixture_state() -> (RelationState<Fixture>, Reader) {
    let entries = (0_u64..4_096).map(|key| (key, key.saturating_mul(2)));
    let state =
        RelationState::from_entries(entries, crate::arrangement_coverage()).expect("fixture state");
    let mut nodes = BTreeMap::new();
    let mut closure = state.node_closure();
    while let Some(node) = closure.try_next().expect("node closure") {
        nodes.insert(node.id().to_bytes(), node.canonical_bytes().to_vec());
    }
    let reads = Arc::new(AtomicUsize::new(0));
    (
        state,
        Reader {
            nodes,
            poisoned: BTreeSet::new(),
            reads,
        },
    )
}

#[test]
fn opaque_arrangement_adapter_preserves_logical_root_bytes() {
    let relation = RelationIdentity::from_value(&[7_u8; 32]);
    let object = ObjectIdentity::from_value(&[9_u8; 32]);
    let rows = vec![
        (
            ArrangementKey::new(
                RowKey {
                    relation,
                    object,
                    key: 1,
                },
                11_i64,
            ),
            1_i64,
        ),
        (
            ArrangementKey::new(
                RowKey {
                    relation,
                    object,
                    key: 2,
                },
                13_i64,
            ),
            -2_i64,
        ),
    ];
    let opaque = rows.clone();
    let logical = canonical_root::<ArrangementRelation<i64>>(&rows).expect("logical");
    let admitted =
        canonical_root::<ArrangementCanonicalRelation<i64>>(&opaque).expect("wire adapter");
    assert_eq!(logical.as_bytes(), admitted.as_bytes());
    assert_eq!(
        logical.commitment().as_bytes(),
        admitted.commitment().as_bytes()
    );
}

#[test]
fn arrangement_key_encoding_is_injective_and_order_preserving() {
    let relation = RelationIdentity::from_value(&[0x11; 32]);
    let object = ObjectIdentity::from_value(&[0x22; 32]);
    let rows = (1_u64..=19)
        .map(|key| {
            (
                ArrangementKey::new(
                    RowKey {
                        relation,
                        object,
                        key,
                    },
                    if key % 2 == 0 {
                        -i64::try_from(key).expect("small key")
                    } else {
                        i64::try_from(key).expect("small key")
                    },
                ),
                1_i64,
            )
        })
        .collect::<Vec<_>>();
    let wire = rows.clone();
    let logical = canonical_root::<ArrangementRelation<i64>>(&rows).expect("logical root");
    let admitted =
        canonical_root::<ArrangementCanonicalRelation<i64>>(&wire).expect("ordered wire root");
    assert_eq!(logical.as_bytes(), admitted.as_bytes());
    let admission = backend_version::admit_canonical_root::<ArrangementCanonicalRelation<i64>>(
        admitted.as_bytes(),
    );
    assert!(admission.is_ok(), "admission={admission:?}");

    let duplicate_key = vec![(wire[0].0.clone(), 1_i64), (wire[0].0.clone(), 2_i64)];
    assert!(canonical_root::<ArrangementCanonicalRelation<i64>>(&duplicate_key).is_err());
}

#[test]
fn arrangement_key_encoding_orders_variable_values_without_collisions() {
    let relation = RelationIdentity::from_value(&[0x31; 32]);
    let object = ObjectIdentity::from_value(&[0x41; 32]);
    let values = ["", "a", "a\0", "aa", "\u{ff}"];
    let rows = values
        .iter()
        .map(|value| {
            (
                ArrangementKey::new(
                    RowKey {
                        relation,
                        object,
                        key: 7,
                    },
                    (*value).to_owned(),
                ),
                1_i64,
            )
        })
        .collect::<Vec<_>>();
    let wire = rows.clone();
    let logical = canonical_root::<ArrangementRelation<String>>(&rows).expect("logical root");
    let admitted =
        canonical_root::<ArrangementCanonicalRelation<String>>(&wire).expect("ordered wire root");
    assert_eq!(logical.as_bytes(), admitted.as_bytes());
    assert!(
        backend_version::admit_canonical_root::<ArrangementCanonicalRelation<String>>(
            admitted.as_bytes()
        )
        .is_ok()
    );
}

#[test]
fn admitted_lazy_root_opens_without_reads_and_lookup_is_path_bounded() {
    let (state, reader) = fixture_state();
    let root_bytes = state.materialize().canonical_bytes().to_vec();
    let claim = UntrustedId::<Fixture>::from_wire(
        state.root().as_bytes(),
        IdContext::relation::<Fixture>(),
    )
    .expect("root claim");
    let root_level = state.root_handle().summary().level;
    let reads = Arc::clone(&reader.reads);
    let loader = CasNodeLoader::new(reader);
    let lazy = LazyCheckpointRoot::open(&loader, claim, &root_bytes).expect("open");
    assert_eq!(reads.load(Ordering::Relaxed), 0);
    assert_eq!(lazy.root(), state.root());
    assert_eq!(lazy.lookup(&3_001).expect("lookup"), Some(6_002));
    assert!(reads.load(Ordering::Relaxed) <= usize::from(root_level));
}

#[test]
fn poisoned_unselected_descendants_are_not_read_by_lazy_lookup_or_update() {
    let (state, mut reader) = fixture_state();
    let root_bytes = state.materialize().canonical_bytes().to_vec();
    let claim = UntrustedId::<Fixture>::from_wire(
        state.root().as_bytes(),
        IdContext::relation::<Fixture>(),
    )
    .expect("root claim");
    let target = 3_001_u64;
    let mut closure = state.node_closure();
    while let Some(node) = closure.try_next().expect("node closure") {
        if node.first_key().is_some_and(|first| *first > target)
            && node.id().to_bytes() != *state.root().as_bytes()
        {
            reader.poisoned.insert(node.id().to_bytes());
        }
    }
    let root_level = state.root_handle().summary().level;
    let reads = Arc::clone(&reader.reads);
    let loader = CasNodeLoader::new(reader);
    let lazy = LazyCheckpointRoot::open(&loader, claim, &root_bytes).expect("open");
    assert_eq!(lazy.lookup(&target).expect("lookup"), Some(6_002));
    assert!(reads.load(Ordering::Relaxed) <= usize::from(root_level));
    let update = lazy
        .prepare_replace(&target, 9_999)
        .expect("path-copy update");
    assert!(update.work().loaded_nodes <= usize::from(root_level));
    assert!(update.work().rebuilt_nodes > 0);
}

#[test]
fn owned_store_root_reopens_and_path_copies_without_row_materialization() {
    let path = std::env::temp_dir().join(format!(
        "backend-flow-lazy-root-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&path);
    let registry = backend_store::RelationAdmissionRegistry::new()
        .with_relation::<Fixture>()
        .expect("fixture relation registration");
    let store = FileStore::open_with_registry(&path, 8 * 1024 * 1024, registry).expect("store");
    let entries = (0_u64..100_000).map(|key| (key, key.saturating_mul(2)));
    let state = RelationState::<Fixture>::from_entries(entries, crate::arrangement_coverage())
        .expect("fixture state");
    let write = store.write_relation_state(&state).expect("write relation");
    assert!(write.nodes_written > 1);
    let claim = UntrustedId::<Fixture>::from_wire(
        state.root().as_bytes(),
        IdContext::relation::<Fixture>(),
    )
    .expect("root claim");
    let mut lazy = OwnedLazyCheckpointRoot::open(&store, claim).expect("lazy reopen");
    assert_eq!(lazy.lookup(&99_999).expect("lookup"), Some(199_998));
    let update = lazy
        .prepare_replace(&99_999, 777_777)
        .expect("prepare path copy");
    let root_level = state.root_handle().summary().level;
    assert!(update.work().loaded_nodes <= usize::from(root_level));
    let work = lazy.apply(&update).expect("publish path copy");
    assert!(work.rebuilt_nodes > 0);
    assert_eq!(lazy.lookup(&99_999).expect("updated lookup"), Some(777_777));
    let _ = std::fs::remove_dir_all(path);
}
