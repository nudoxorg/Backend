use super::*;
use std::borrow::Borrow;
use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

#[derive(Debug, Eq, PartialEq)]
struct RelationFixture;

impl Relation for RelationFixture {
    const DOMAIN: u8 = 1;
    const TYPE: u16 = 7;
    type Key = u64;
    type Value = u64;

    fn encode_key(value: &u64, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn encode_value(value: &u64, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

impl CanonicalRelation for RelationFixture {
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

#[derive(Debug, Eq, PartialEq)]
struct VariableRelation;

impl Relation for VariableRelation {
    const DOMAIN: u8 = 11;
    const TYPE: u16 = 7;
    type Key = u64;
    type Value = Vec<u8>;

    fn encode_key(value: &u64, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn encode_value(value: &Vec<u8>, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

#[derive(Debug, Eq, PartialEq)]
struct OtherRelation;

impl Relation for OtherRelation {
    const DOMAIN: u8 = 2;
    const TYPE: u16 = 7;
    type Key = u64;
    type Value = u64;

    fn encode_key(value: &u64, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn encode_value(value: &u64, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

struct MarkerRelation;

impl Relation for MarkerRelation {
    const DOMAIN: u8 = 3;
    const TYPE: u16 = 7;
    type Key = u64;
    type Value = u64;

    fn encode_key(value: &u64, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn encode_value(value: &u64, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

static BORROWED_KEY_CLONES: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Eq, PartialEq)]
struct BorrowedKey(Vec<u8>);

impl Clone for BorrowedKey {
    fn clone(&self) -> Self {
        BORROWED_KEY_CLONES.fetch_add(1, AtomicOrdering::Relaxed);
        Self(self.0.clone())
    }
}

impl Ord for BorrowedKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl PartialOrd for BorrowedKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Borrow<[u8]> for BorrowedKey {
    fn borrow(&self) -> &[u8] {
        &self.0
    }
}

struct BorrowedKeyRelation;

impl Relation for BorrowedKeyRelation {
    const DOMAIN: u8 = 4;
    const TYPE: u16 = 7;
    type Key = BorrowedKey;
    type Value = u64;

    fn encode_key(value: &BorrowedKey, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.0);
    }

    fn encode_value(value: &u64, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

#[derive(Debug, Eq, PartialEq)]
struct SchemaFixture;

impl Schema for SchemaFixture {
    const DOMAIN: u8 = 1;
    const TYPE: u16 = 9;
    type Value = u64;

    fn encode(value: &u64, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

struct FixtureVerifier;

impl ProducerObservationVerifier for FixtureVerifier {
    type Error = CoverageAdmissionError;

    fn verify(&self, _: &UntrustedProducerObservation) -> Result<(), Self::Error> {
        Ok(())
    }
}

struct FixtureProducer {
    scope: ScopeRoot,
    producer: [u8; ID_BYTES],
}

struct OtherFixtureProducer {
    scope: ScopeRoot,
}

fn admitted_observation(
    scope: ScopeRoot,
    producer: [u8; ID_BYTES],
) -> Result<AdmittedProducerObservation, CoverageAdmissionError> {
    admit_producer_observation(
        UntrustedProducerObservation::new(producer, scope, [3; ID_BYTES], vec![4, 5, 6]),
        &FixtureVerifier,
    )
    .map_err(|error| match error {
        ProducerObservationAdmissionError::Rejected(error) => error,
    })
}

fn complete(scope: u64) -> Result<CoverageWitness, CoverageAdmissionError> {
    let version = ObjectVersion::<SchemaFixture>::from_value(&scope);
    let declared = AuthorityScopeClaim::from_object_version(version);
    let admitted = admitted_observation(declared.scope_root(), [7; ID_BYTES])?;
    Ok(CoverageWitness::Complete(admit_complete_scope(
        declared, admitted,
    )?))
}

fn state(
    entries: impl IntoIterator<Item = (u64, u64)>,
) -> Result<RelationState<RelationFixture>, StateError> {
    RelationState::from_entries(
        entries,
        complete(1).map_err(|_| StateError::Canonical(NodeError::InvalidBranch))?,
    )
}

fn manifest(
    state: &RelationState<RelationFixture>,
    coverage: CoverageWitness,
) -> Result<CheckedWorkspaceManifest, WorkspaceError> {
    WorkspaceManifest::new_checked(
        1,
        vec![RelationBinding::from_state(state)],
        vec![],
        ObjectClosure::from_version(ObjectVersion::<SchemaFixture>::from_value(&3)),
        coverage,
    )
}

#[test]
fn canonical_state_is_independent_of_input_order() -> Result<(), StateError> {
    let first = state([(2, 3), (1, 4)])?;
    let second = state([(1, 4), (2, 3)])?;
    assert_eq!(first.root(), second.root());
    Ok(())
}

#[test]
fn variable_value_sizes_drive_canonical_leaf_boundaries() -> Result<(), TreeError> {
    let items: Vec<_> = (0..300u64)
        .map(|key| {
            let size = match key % 9 {
                0 => 128,
                1 => 1_165,
                2 => 2_202,
                3 => 3_239,
                4 => 4_276,
                5 => 5_313,
                6 => 6_350,
                7 => 7_387,
                _ => 8_424,
            };
            (key, vec![key.to_le_bytes()[0]; size])
        })
        .collect();
    let tree = PersistentTree::<VariableRelation>::from_sorted_items(&items)?;
    let canonical = canonical_root::<VariableRelation>(&items).map_err(TreeError::Canonical)?;
    assert_eq!(tree.root().commitment(), canonical.commitment());
    assert!(
        tree.node_closure()
            .all(|node| node.canonical_bytes().len() <= CutPolicy::MAX_ENCODED_BYTES)
    );
    assert!(tree.root().level() > 0);
    Ok(())
}

#[test]
fn one_value_that_exceeds_the_wire_budget_is_rejected() {
    let oversized = vec![0u8; CutPolicy::MAX_ENCODED_BYTES];
    assert!(matches!(
        canonical_leaf::<VariableRelation>(&[(1, oversized)]),
        Err(NodeError::OversizedNode)
    ));
}

#[test]
fn bound_delta_keeps_application_on_its_checked_base() -> Result<(), Box<dyn std::error::Error>> {
    let base = state([(1, 10), (2, 20)])?;
    let (prepared, _) = prepare_delta_with_state(
        &base,
        vec![MapChange {
            key: 2,
            before: Some(20),
            after: Some(21),
        }],
    )?;
    let delta = prepared.into_delta();
    let view = delta.view();
    assert_eq!(view.base(), base.root());
    assert_eq!(view.changes().count(), 1);
    let next = delta.bind(&base)?.apply()?;
    assert_eq!(next.get(&2), Some(&21));
    assert_eq!(next.root(), delta.target());
    Ok(())
}

struct CountingLoader {
    nodes: BTreeMap<[u8; ID_BYTES], Vec<u8>>,
    calls: Cell<usize>,
}

impl TreeNodeLoader<RelationFixture> for CountingLoader {
    type Error = NodeError;

    fn load(
        &self,
        claim: UntrustedId<RelationFixture>,
    ) -> Result<CheckedCanonicalRoot<RelationFixture>, Self::Error> {
        self.calls.set(self.calls.get() + 1);
        let bytes = self
            .nodes
            .get(claim.as_bytes())
            .ok_or(NodeError::MalformedEncoding)?;
        admit_canonical_root_claim(claim, bytes).map_err(|error| match error {
            CanonicalRootAdmissionError::Node(error) => error,
            CanonicalRootAdmissionError::Identity(error) => {
                let _ = error;
                NodeError::MalformedEncoding
            }
        })
    }
}

#[test]
fn lazy_path_copy_loads_only_the_authenticated_update_path()
-> Result<(), Box<dyn std::error::Error>> {
    let items: Vec<_> = (0..3_000u64).map(|key| (key, key * 2)).collect();
    let tree = PersistentTree::<RelationFixture>::from_sorted_items(&items)?;
    let nodes: BTreeMap<_, _> = tree
        .node_closure()
        .map(|node| (node.id().to_bytes(), node.canonical_bytes().to_vec()))
        .collect();
    let mut nodes = nodes;
    let admitted_root = admit_canonical_root::<RelationFixture>(tree.root().as_bytes())?;
    if let Some(unrelated) = admitted_root.child_summaries()?.first() {
        nodes.remove(unrelated.commitment.as_bytes());
    }
    let loader = CountingLoader {
        nodes,
        calls: Cell::new(0),
    };
    let claim = UntrustedId::<RelationFixture>::from_wire(
        tree.root().commitment().as_bytes(),
        IdContext::relation::<RelationFixture>(),
    )?;
    let wrong_context = UntrustedId::<RelationFixture>::from_wire(
        tree.root().commitment().as_bytes(),
        IdContext::new(
            0x56,
            RelationFixture::DOMAIN,
            RelationFixture::TYPE,
            RelationFixture::VERSION,
        ),
    )?;
    assert!(matches!(
        LazyTree::open(&loader, wrong_context),
        Err(LazyTreeError::Node(NodeError::SchemaMismatch))
    ));
    assert_eq!(loader.calls.get(), 0);
    let lazy = LazyTree::open(&loader, claim)?;
    let open_calls = loader.calls.get();
    assert_eq!(lazy.lookup(&2_999)?, Some(5_998));
    assert!(loader.calls.get() - open_calls <= usize::from(tree.root().level()));
    let handoff = lazy.root_handle();
    let reopened = LazyTree::from_admitted(&loader, handoff);
    let handoff_calls = loader.calls.get();
    assert_eq!(reopened.lookup(&2_999)?, Some(5_998));
    assert!(loader.calls.get() > handoff_calls);
    let update_calls = loader.calls.get();
    let update = lazy.prepare_replace(&2_999, 99_999)?;
    assert_eq!(update.base(), tree.root().commitment());
    assert_ne!(update.target().root(), tree.root().commitment());
    assert_eq!(
        update.changed_nodes().len(),
        usize::from(tree.root().level()) + 1
    );
    assert!(loader.calls.get() - update_calls <= usize::from(tree.root().level()) + 1);
    let inserted = lazy.prepare_insert(&3_001, 6_002)?;
    let mut expected = items;
    expected.push((3_001, 6_002));
    assert_eq!(
        inserted.target().root(),
        canonical_root::<RelationFixture>(&expected)?.commitment()
    );
    Ok(())
}

#[test]
fn lazy_batch_reuses_its_frontier_and_emits_one_exact_delta()
-> Result<(), Box<dyn std::error::Error>> {
    let items: Vec<_> = (0..8_000u64).map(|key| (key * 2, key * 4)).collect();
    let tree = PersistentTree::<RelationFixture>::from_sorted_items(&items)?;
    let nodes = tree
        .node_closure()
        .map(|node| (node.id().to_bytes(), node.canonical_bytes().to_vec()))
        .collect();
    let loader = CountingLoader {
        nodes,
        calls: Cell::new(0),
    };
    let claim = UntrustedId::from_wire(
        tree.root().commitment().as_bytes(),
        IdContext::relation::<RelationFixture>(),
    )?;
    let lazy = LazyTree::open(&loader, claim)?;
    let changes = vec![
        TreeChange {
            key: 3,
            after: Some(33),
        },
        TreeChange {
            key: 4_000,
            after: Some(44),
        },
        TreeChange {
            key: 9_998,
            after: None,
        },
        TreeChange {
            key: 16_001,
            after: Some(55),
        },
    ];
    let update = lazy.prepare_update(&changes)?;
    let mut expected = items;
    expected.insert(2, (3, 33));
    let replacement = expected
        .binary_search_by_key(&4_000, |(key, _)| *key)
        .map_err(|_| "replacement fixture key missing")?;
    expected[replacement].1 = 44;
    let removed = expected
        .binary_search_by_key(&9_998, |(key, _)| *key)
        .map_err(|_| "removal fixture key missing")?;
    expected.remove(removed);
    expected.push((16_001, 55));
    assert_eq!(
        update.target().root(),
        canonical_root::<RelationFixture>(&expected)?.commitment()
    );
    let delta = update.delta();
    assert_eq!(delta.base(), tree.root().commitment());
    assert_eq!(delta.target(), update.target().root());
    assert_eq!(delta.changes().count(), changes.len());
    assert_eq!(update.target().row_count(), expected.len() as u64);
    assert!(update.work().rebuilt_nodes < tree.node_closure().count());
    assert!(matches!(
        lazy.prepare_update(&[
            TreeChange {
                key: 9,
                after: Some(1),
            },
            TreeChange {
                key: 9,
                after: Some(2),
            },
        ]),
        Err(LazyTreeError::Node(NodeError::UnsortedOrDuplicate))
    ));
    Ok(())
}

#[test]
fn lazy_first_ingest_bulk_builds_each_canonical_node_once() -> Result<(), Box<dyn std::error::Error>>
{
    let empty = canonical_empty::<RelationFixture>();
    let admitted = admit_canonical_root::<RelationFixture>(empty.as_bytes())?;
    let loader = CountingLoader {
        nodes: BTreeMap::new(),
        calls: Cell::new(0),
    };
    let lazy = LazyTree::from_admitted(&loader, PersistedTreeRoot::from_checked(admitted));
    let changes = (0..16_384u64)
        .map(|key| TreeChange {
            key,
            after: Some(key * 2),
        })
        .collect::<Vec<_>>();
    let update = lazy.prepare_update(&changes)?;
    let expected = changes
        .iter()
        .map(|change| (change.key, change.after.unwrap_or_default()))
        .collect::<Vec<_>>();
    let rebuilt = PersistentTree::<RelationFixture>::from_sorted_items(&expected)?;

    assert_eq!(update.target().root(), rebuilt.root().commitment());
    assert_eq!(update.delta().changes().count(), changes.len());
    assert_eq!(update.work().loaded_nodes, 0);
    assert_eq!(update.work().rebuilt_nodes, update.changed_nodes().len());
    assert!(update.work().rebuilt_nodes < changes.len() / 8);
    Ok(())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the scenario covers the complete lazy update matrix"
)]
fn lazy_split_shrink_and_duplicate_updates_keep_canonical_roots()
-> Result<(), Box<dyn std::error::Error>> {
    // A two-leaf root exercises leaf split propagation and root growth.  The
    // untouched sibling is deliberately removed from the loader: inserting in
    // the first leaf must use its authenticated summary without fetching it.
    let items: Vec<_> = (0..2_048u64).map(|key| (key, key * 2)).collect();
    let tree = PersistentTree::<RelationFixture>::from_sorted_items(&items)?;
    let root = admit_canonical_root::<RelationFixture>(tree.root().as_bytes())?;
    let sibling = root
        .child_summaries()?
        .first()
        .map(|child| child.commitment);
    let mut nodes: BTreeMap<_, _> = tree
        .node_closure()
        .map(|node| (node.id().to_bytes(), node.canonical_bytes().to_vec()))
        .collect();
    if let Some(sibling) = sibling {
        nodes.remove(sibling.as_bytes());
    }
    let loader = CountingLoader {
        nodes,
        calls: Cell::new(0),
    };
    let claim = UntrustedId::from_wire(
        tree.root().commitment().as_bytes(),
        IdContext::relation::<RelationFixture>(),
    )?;
    let lazy = LazyTree::open(&loader, claim)?;
    let inserted = lazy.prepare_insert(&2_048, 7_777)?;
    let mut expected = items.clone();
    expected.push((2_048, 7_777));
    assert_eq!(
        inserted.target().root(),
        canonical_root::<RelationFixture>(&expected)?.commitment()
    );
    assert_eq!(inserted.target().row_count(), 2_049);
    assert!(inserted.work().split_nodes > 0 || inserted.work().rebuilt_nodes > 0);
    assert!(matches!(
        lazy.prepare_insert(&2_047, 8_888),
        Err(LazyTreeError::DuplicateKey)
    ));

    // A hard boundary is crossed when a missing key is inserted inside the
    // first full leaf. The distant final leaf is poisoned to ensure the
    // boundary spill stops at the first stable region.
    let items: Vec<_> = (0..2_048u64).map(|key| (key * 2, key * 4)).collect();
    let tree = PersistentTree::<RelationFixture>::from_sorted_items(&items)?;
    let root = admit_canonical_root::<RelationFixture>(tree.root().as_bytes())?;
    let distant = root.child_summaries()?.last().map(|child| child.commitment);
    let mut nodes: BTreeMap<_, _> = tree
        .node_closure()
        .map(|node| (node.id().to_bytes(), node.canonical_bytes().to_vec()))
        .collect();
    if let Some(distant) = distant {
        nodes.remove(distant.as_bytes());
    }
    let loader = CountingLoader {
        nodes,
        calls: Cell::new(0),
    };
    let claim = UntrustedId::from_wire(
        tree.root().commitment().as_bytes(),
        IdContext::relation::<RelationFixture>(),
    )?;
    let lazy = LazyTree::open(&loader, claim)?;
    let inserted = lazy.prepare_insert(&1, 2)?;
    let mut expected = items;
    expected.insert(1, (1, 2));
    assert_eq!(
        inserted.target().root(),
        canonical_root::<RelationFixture>(&expected)?.commitment()
    );
    assert_eq!(inserted.target().row_count(), 2_049);

    // Removing the first key from the 1,025-row tree must merge the two leaf
    // region and shrink the root back to the canonical single leaf.
    let items: Vec<_> = (0..1_025u64).map(|key| (key, key * 2)).collect();
    let tree = PersistentTree::<RelationFixture>::from_sorted_items(&items)?;
    let nodes = tree
        .node_closure()
        .map(|node| (node.id().to_bytes(), node.canonical_bytes().to_vec()))
        .collect();
    let loader = CountingLoader {
        nodes,
        calls: Cell::new(0),
    };
    let claim = UntrustedId::from_wire(
        tree.root().commitment().as_bytes(),
        IdContext::relation::<RelationFixture>(),
    )?;
    let lazy = LazyTree::open(&loader, claim)?;
    let removed = lazy.prepare_remove(&0)?;
    let expected: Vec<_> = items.into_iter().skip(1).collect();
    assert_eq!(
        removed.target().root(),
        canonical_root::<RelationFixture>(&expected)?.commitment()
    );
    assert_eq!(removed.target().row_count(), 1_024);
    assert_eq!(removed.work().removed_entries, 1);

    // A checked one-child branch is a valid authenticated input even though
    // bulk construction normally collapses it. Removing its final entry must
    // return the canonical empty leaf and therefore shrink the root level.
    let leaf = canonical_leaf::<RelationFixture>(&[(9, 18)])?;
    let branch = canonical_branch_from_commitments::<RelationFixture>(
        1,
        &[CommittedChild {
            first_key: 9,
            commitment: leaf.commitment().into(),
            level: 0,
            row_count: 1,
        }],
    )?;
    let nodes = BTreeMap::from([
        (leaf.commitment().to_bytes(), leaf.as_bytes().to_vec()),
        (branch.commitment().to_bytes(), branch.as_bytes().to_vec()),
    ]);
    let loader = CountingLoader {
        nodes,
        calls: Cell::new(0),
    };
    let claim = UntrustedId::from_wire(
        branch.commitment().as_bytes(),
        IdContext::relation::<RelationFixture>(),
    )?;
    let lazy = LazyTree::open(&loader, claim)?;
    let removed = lazy.prepare_remove(&9)?;
    assert_eq!(
        removed.target().root(),
        canonical_empty::<RelationFixture>().commitment()
    );
    assert_eq!(removed.target().row_count(), 0);
    assert_eq!(removed.target().node().level(), 0);
    Ok(())
}

#[test]
fn lazy_multilevel_insert_spills_only_adjacent_authenticated_branches()
-> Result<(), Box<dyn std::error::Error>> {
    let items: Vec<_> = (0..70_000u64).map(|key| (key * 2, key * 4)).collect();
    let tree = PersistentTree::<RelationFixture>::from_sorted_items(&items)?;
    assert!(tree.root().level() >= 2);
    let root = admit_canonical_root::<RelationFixture>(tree.root().as_bytes())?;
    assert_eq!(root.row_count(), items.len() as u64);
    assert_eq!(
        root.child_summaries()?
            .iter()
            .try_fold(0u64, |sum, child| sum.checked_add(child.row_count)),
        Some(items.len() as u64)
    );
    let distant = root.child_summaries()?.last().map(|child| child.commitment);
    let mut nodes: BTreeMap<_, _> = tree
        .node_closure()
        .map(|node| (node.id().to_bytes(), node.canonical_bytes().to_vec()))
        .collect();
    if let Some(distant) = distant {
        nodes.remove(distant.as_bytes());
    }
    let loader = CountingLoader {
        nodes,
        calls: Cell::new(0),
    };
    let claim = UntrustedId::from_wire(
        tree.root().commitment().as_bytes(),
        IdContext::relation::<RelationFixture>(),
    )?;
    let lazy = LazyTree::open(&loader, claim)?;
    let inserted = lazy.prepare_insert(&1, 2)?;
    let mut expected = items;
    expected.insert(1, (1, 2));
    assert_eq!(
        inserted.target().root(),
        canonical_root::<RelationFixture>(&expected)?.commitment()
    );
    assert_eq!(inserted.target().row_count(), 70_001);
    assert!(inserted.work().loaded_nodes < 20);
    Ok(())
}

#[test]
fn canonical_root_admission_rechecks_bounded_wire_grammar() -> Result<(), Box<dyn std::error::Error>>
{
    let node = canonical_leaf::<RelationFixture>(&[(1, 4), (2, 8)])?;
    let admitted = admit_canonical_root::<RelationFixture>(node.as_bytes())?;
    assert_eq!(admitted.root(), node.commitment());
    assert_eq!(admitted.node().first_key(), Some(&1));
    assert_eq!(admitted.bytes(), node.as_bytes());
    let claim = UntrustedId::<RelationFixture>::from_wire(
        node.commitment().as_bytes(),
        IdContext::relation::<RelationFixture>(),
    )?;
    assert_eq!(
        admit_canonical_root_claim::<RelationFixture>(claim, node.as_bytes())?.root(),
        node.commitment()
    );
    let forged = UntrustedId::<RelationFixture>::from_wire(
        &[0; ID_BYTES],
        IdContext::relation::<RelationFixture>(),
    )?;
    assert_eq!(
        admit_canonical_root_claim::<RelationFixture>(forged, node.as_bytes()),
        Err(CanonicalRootAdmissionError::Identity(
            IdAdmissionError::DigestMismatch
        ))
    );

    let mut wrong_abi = node.as_bytes().to_vec();
    wrong_abi[NODE_MAGIC_BYTES] = 0xff;
    assert_eq!(
        admit_canonical_root::<RelationFixture>(&wrong_abi),
        Err(NodeError::SchemaMismatch)
    );

    let mut trailing = node.as_bytes().to_vec();
    trailing.push(0);
    assert_eq!(
        admit_canonical_root::<RelationFixture>(&trailing),
        Err(NodeError::MalformedEncoding)
    );
    Ok(())
}

#[test]
fn runtime_identity_and_borrowed_node_view_share_checked_wire_grammar()
-> Result<(), Box<dyn std::error::Error>> {
    let leaf = canonical_leaf::<RelationFixture>(&[(1, 4), (2, 8)])?;
    let schema = SchemaIdentity::of_relation::<RelationFixture>();
    let root = state_root_digest(schema, leaf.as_bytes())?;
    assert_eq!(
        admit_state_root_bytes(schema, leaf.as_bytes(), &root)?,
        root
    );
    let object = object_version_digest(schema, leaf.as_bytes())?;
    assert_eq!(
        admit_object_version_bytes(schema, leaf.as_bytes(), &object)?,
        object
    );
    assert_eq!(
        admit_state_root_bytes(schema, leaf.as_bytes(), &object),
        Err(RuntimeIdentityError::DigestMismatch)
    );
    assert!(matches!(
        CanonicalNodeView::parse(leaf.as_bytes(), SchemaIdentity::new(0xff, 1, 1)),
        Err(NodeError::SchemaMismatch)
    ));
    let branch = canonical_branch::<RelationFixture>(
        1,
        &[Child {
            first_key: 1,
            node: leaf.clone(),
        }],
    )?;
    let view = CanonicalNodeView::parse(branch.as_bytes(), schema)?;
    assert_eq!(view.row_count(), 2);
    let children: Vec<_> = view.branch_children()?.collect::<Result<_, _>>()?;
    assert_eq!(children[0].first_key, &1u64.to_be_bytes());
    assert_eq!(children[0].commitment, leaf.commitment().as_bytes());
    assert_eq!(children[0].row_count, 2);
    let mut trailing = branch.as_bytes().to_vec();
    trailing.push(0);
    assert!(matches!(
        CanonicalNodeView::parse(&trailing, schema),
        Err(NodeError::MalformedEncoding)
    ));
    assert!(matches!(
        CanonicalNodeView::parse(&branch.as_bytes()[..branch.as_bytes().len() - 1], schema),
        Err(NodeError::MalformedEncoding)
    ));
    assert!(matches!(
        CanonicalNodeView::parse(&vec![0; 65_537], schema),
        Err(NodeError::OversizedNode)
    ));
    Ok(())
}

#[test]
fn object_version_hasher_matches_typed_digest_across_chunk_partitions()
-> Result<(), Box<dyn std::error::Error>> {
    let schema = SchemaIdentity::new(
        SchemaFixture::DOMAIN,
        SchemaFixture::TYPE,
        SchemaFixture::VERSION,
    );
    let value = 0x0102_0304_0506_0708u64;
    let mut payload = Vec::new();
    SchemaFixture::encode(&value, &mut payload);
    let expected = ObjectVersion::<SchemaFixture>::from_value(&value).to_bytes();
    for cuts in [
        vec![payload.len()],
        vec![1, 3, payload.len()],
        vec![0, 0, payload.len()],
    ] {
        let mut hasher = ObjectVersionHasher::new(schema, payload.len())?;
        let mut start = 0;
        for end in cuts {
            let end = end.min(payload.len());
            if end >= start {
                hasher.update(&payload[start..end])?;
                start = end;
            }
        }
        if start < payload.len() {
            hasher.update(&payload[start..])?;
        }
        assert_eq!(hasher.finish()?, expected);
    }
    let mut empty = ObjectVersionHasher::new(schema, 0)?;
    empty.update(&[])?;
    assert_eq!(empty.finish()?, object_version_digest(schema, &[])?);
    let mut over = ObjectVersionHasher::new(schema, 1)?;
    assert_eq!(
        over.update(&[1, 2]),
        Err(RuntimeIdentityError::LengthMismatch {
            expected: 1,
            actual: 2
        })
    );
    assert_eq!(
        over.finish(),
        Err(RuntimeIdentityError::LengthMismatch {
            expected: 1,
            actual: 2
        })
    );
    let under = ObjectVersionHasher::new(schema, 1)?.finish();
    assert_eq!(
        under,
        Err(RuntimeIdentityError::LengthMismatch {
            expected: 1,
            actual: 0
        })
    );
    Ok(())
}

#[test]
fn canonical_branch_admission_checks_child_commitment_width() -> Result<(), NodeError> {
    let first = canonical_leaf::<RelationFixture>(&[(1, 1)])?;
    let second = canonical_leaf::<RelationFixture>(&[(2, 2)])?;
    let first_commitment = first.commitment();
    let branch = canonical_branch(
        1,
        &[
            Child {
                first_key: 1,
                node: first,
            },
            Child {
                first_key: 2,
                node: second,
            },
        ],
    )?;
    assert_eq!(
        admit_canonical_root::<RelationFixture>(branch.as_bytes())?.root(),
        branch.commitment()
    );
    let proof = admit_canonical_root::<RelationFixture>(branch.as_bytes())?;
    assert_eq!(proof.row_count(), 2);
    let children = proof.child_summaries()?;
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].first_key, 1);
    assert_eq!(
        children[0].commitment,
        ChildCommitment::from(first_commitment)
    );
    assert_eq!(children[0].level, 0);
    assert_eq!(children[0].row_count, 1);
    let borrowed_children: Vec<_> = proof.child_iter()?.collect::<Result<_, _>>()?;
    assert_eq!(borrowed_children, children);

    let mut malformed = branch.as_bytes().to_vec();
    let length_at = malformed
        .len()
        .checked_sub(ID_BYTES + 16)
        .ok_or(NodeError::MalformedEncoding)?;
    malformed[length_at + 7] = 31;
    assert_eq!(
        admit_canonical_root::<RelationFixture>(&malformed),
        Err(NodeError::MalformedEncoding)
    );
    Ok(())
}

#[test]
fn authenticated_row_counts_reject_tampering_and_overflow() -> Result<(), Box<dyn std::error::Error>>
{
    let empty = canonical_empty::<RelationFixture>();
    let leaf = canonical_leaf::<RelationFixture>(&[(1, 1), (2, 2)])?;
    assert_eq!(empty.row_count(), 0);
    assert_eq!(
        admit_canonical_root::<RelationFixture>(empty.as_bytes())?.row_count(),
        0
    );
    assert_eq!(leaf.row_count(), 2);
    assert_eq!(
        admit_canonical_root::<RelationFixture>(leaf.as_bytes())?.row_count(),
        2
    );

    let first = canonical_leaf::<RelationFixture>(&[(1, 1)])?;
    let second = canonical_leaf::<RelationFixture>(&[(2, 2)])?;
    assert_eq!(
        canonical_branch_from_commitments::<RelationFixture>(
            1,
            &[CommittedChild {
                first_key: 1,
                commitment: first.commitment().into(),
                level: 0,
                row_count: 0,
            }],
        ),
        Err(NodeError::InvalidBranch)
    );
    let branch = canonical_branch::<RelationFixture>(
        1,
        &[
            Child {
                first_key: 1,
                node: first,
            },
            Child {
                first_key: 2,
                node: second,
            },
        ],
    )?;
    // Header (including the body length) is 24 bytes. Each branch entry is
    // 56 bytes before its authenticated eight-byte row count.
    let row_count_offset = 24 + 56;
    let second_count_offset = row_count_offset + 64;
    let mut zero = branch.as_bytes().to_vec();
    zero[row_count_offset..row_count_offset + 8].fill(0);
    assert_eq!(
        admit_canonical_root::<RelationFixture>(&zero),
        Err(NodeError::MalformedEncoding)
    );

    let claim = UntrustedId::<RelationFixture>::from_wire(
        branch.commitment().as_bytes(),
        IdContext::relation::<RelationFixture>(),
    )?;
    let mut tampered = branch.as_bytes().to_vec();
    tampered[row_count_offset..row_count_offset + 8].copy_from_slice(&2u64.to_be_bytes());
    assert_eq!(
        admit_canonical_root_claim::<RelationFixture>(claim, &tampered),
        Err(CanonicalRootAdmissionError::Identity(
            IdAdmissionError::DigestMismatch
        ))
    );

    let mut overflow = branch.as_bytes().to_vec();
    overflow[row_count_offset..row_count_offset + 8].copy_from_slice(&u64::MAX.to_be_bytes());
    overflow[second_count_offset..second_count_offset + 8].copy_from_slice(&1u64.to_be_bytes());
    assert_eq!(
        admit_canonical_root::<RelationFixture>(&overflow),
        Err(NodeError::OversizedNode)
    );
    Ok(())
}

const NODE_MAGIC_BYTES: usize = b"V2NODE\0".len();

#[test]
fn checked_state_object_materialization_rechecks_root_identity() -> Result<(), StateError> {
    let first = state([(1, 4), (2, 3)])?;
    let second = state([(1, 8), (2, 3)])?;
    let materialized = first.materialize();

    assert_eq!(materialized.root(), first.root());
    assert_eq!(
        materialized.context(),
        IdContext::relation::<RelationFixture>()
    );
    assert_eq!(
        materialized.schema(),
        SchemaIdentity::of_relation::<RelationFixture>()
    );
    assert_eq!(
        materialized.canonical_bytes(),
        first.materialize().canonical_bytes()
    );
    assert_eq!(
        materialized.canonical_bytes().as_ptr(),
        first.clone().materialize().canonical_bytes().as_ptr()
    );
    assert_eq!(
        StateRoot::admit_canonical_node(first.root(), materialized.canonical_node().clone())
            .map(|object| object.root()),
        Ok(first.root())
    );
    assert_eq!(
        StateRoot::admit_canonical_node(second.root(), materialized.canonical_node().clone()),
        Err(IdAdmissionError::DigestMismatch)
    );
    Ok(())
}

#[test]
fn persistent_updates_match_independent_canonical_rebuilds()
-> Result<(), Box<dyn std::error::Error>> {
    let base_entries: Vec<_> = (0..10_000u64).map(|key| (key, key * 3)).collect();
    let base = state(base_entries.clone())?;
    assert_eq!(base.clone().root(), base.root());

    let (replacement, replacement_work) = prepare_delta_with_state(
        &base,
        vec![MapChange {
            key: 5_000,
            before: Some(15_000),
            after: Some(99_999),
        }],
    )?;
    assert!(replacement_work.nodes < 10);
    assert!(replacement_work.reused_nodes > 0);
    let (replaced, replacement_commit_work) = replacement.commit_with_work(&base)?;
    assert_eq!(replacement_commit_work, DeltaWork::default());
    let expected_replaced = state(
        base_entries
            .iter()
            .map(|(key, value)| (*key, if *key == 5_000 { 99_999 } else { *value })),
    )?;
    assert_eq!(replaced.root(), expected_replaced.root());
    assert_eq!(
        replaced.iter().collect::<Vec<_>>(),
        expected_replaced.iter().collect::<Vec<_>>()
    );

    let (structural, structural_prepare_work) = prepare_delta_with_state(
        &replaced,
        vec![
            MapChange {
                key: 10,
                before: Some(30),
                after: Some(30),
            },
            MapChange {
                key: 10_001,
                before: None,
                after: Some(30_003),
            },
        ],
    )?;
    assert!(structural_prepare_work.nodes < 100);
    let (structurally_changed, structural_commit_work) = structural.commit_with_work(&replaced)?;
    assert_eq!(structural_commit_work, DeltaWork::default());
    let expected_structural = state(
        expected_replaced
            .iter()
            .map(|(key, value)| (*key, *value))
            .chain([(10_001, 30_003)]),
    )?;
    assert_eq!(
        structurally_changed.iter().collect::<Vec<_>>(),
        expected_structural.iter().collect::<Vec<_>>()
    );
    assert_eq!(structurally_changed.root(), expected_structural.root());
    Ok(())
}

#[test]
fn public_persistent_kernel_preserves_empty_work_and_shared_nodes() -> Result<(), TreeError> {
    let tree = PersistentTree::<RelationFixture>::from_sorted_items(&[(1, 2), (2, 3)])?;
    let empty = tree.prepare_update(&[])?;
    assert_eq!(empty.work(), TreeWork::default());
    assert_eq!(
        tree.root().as_bytes().as_ptr(),
        empty.tree().root().as_bytes().as_ptr()
    );

    let repeated = tree.prepare_update(&[TreeChange {
        key: 2,
        after: Some(3),
    }])?;
    assert_eq!(repeated.work(), TreeWork::default());
    assert_eq!(
        tree.root().as_bytes().as_ptr(),
        repeated.tree().root().as_bytes().as_ptr()
    );
    let absent_delete = tree.prepare_update(&[TreeChange {
        key: 9,
        after: None,
    }])?;
    assert_eq!(absent_delete.work(), TreeWork::default());
    assert_eq!(
        tree.root().as_bytes().as_ptr(),
        absent_delete.tree().root().as_bytes().as_ptr()
    );

    let replacement = tree.prepare_update(&[TreeChange {
        key: 2,
        after: Some(8),
    }])?;
    assert!(replacement.work().nodes < 4);
    let updated = replacement.commit();
    assert_eq!(updated.get(&2), Some(&8));
    assert_eq!(updated.get(&1), Some(&2));
    Ok(())
}

#[test]
fn borrowed_lookup_and_range_never_clone_owned_keys() -> Result<(), TreeError> {
    let tree = PersistentTree::<BorrowedKeyRelation>::from_sorted_items(&[
        (BorrowedKey(b"alpha".to_vec()), 1),
        (BorrowedKey(b"beta".to_vec()), 2),
        (BorrowedKey(b"delta".to_vec()), 4),
        (BorrowedKey(b"gamma".to_vec()), 7),
    ])?;
    BORROWED_KEY_CLONES.store(0, AtomicOrdering::Relaxed);

    assert_eq!(tree.get(b"beta".as_slice()), Some(&2));
    let lower: &[u8] = b"beta";
    let upper: &[u8] = b"gamma";
    assert_eq!(
        tree.range::<[u8], _>((
            std::ops::Bound::Included(lower),
            std::ops::Bound::Excluded(upper)
        ))
        .map(|(key, value)| (key.0.as_slice(), *value))
        .collect::<Vec<_>>(),
        vec![(b"beta".as_slice(), 2), (b"delta".as_slice(), 4)]
    );
    assert_eq!(BORROWED_KEY_CLONES.load(AtomicOrdering::Relaxed), 0);
    Ok(())
}

#[test]
fn borrowed_node_closure_is_canonical_transitive_and_payload_free()
-> Result<(), Box<dyn std::error::Error>> {
    let items: Vec<_> = (0..8_193u64)
        .map(|key| (BorrowedKey(key.to_be_bytes().to_vec()), key * 11))
        .collect();
    let tree = PersistentTree::<BorrowedKeyRelation>::from_sorted_items(&items)?;
    let root_bytes = tree.root().as_bytes().as_ptr();
    BORROWED_KEY_CLONES.store(0, AtomicOrdering::Relaxed);

    let mut closure = tree.node_closure();
    let mut nodes = Vec::new();
    while let Some(node) = closure.try_next()? {
        // These are all borrowed views.  In particular, neither identity
        // calculation nor child expansion clones a key or value payload.
        assert!(!node.canonical_bytes().is_empty());
        if nodes.is_empty() {
            assert_eq!(node.canonical_bytes().as_ptr(), root_bytes);
            assert_eq!(node.state_root(), tree.root().commitment());
        }
        let object = node.state_object();
        assert_eq!(object.root(), node.state_root());
        assert_eq!(
            object.canonical_bytes().as_ptr(),
            node.canonical_bytes().as_ptr()
        );
        let mut previous = None;
        for child in node.children() {
            if let (Some(previous), Some(current)) = (previous, child.first_key()) {
                assert!(previous < current);
            }
            previous = child.first_key();
        }
        nodes.push(node);
    }

    let work = closure.work();
    assert!(closure.is_complete());
    assert!(nodes.len() > 8);
    assert_eq!(work.visited_nodes, nodes.len());
    assert_eq!(work.visited_edges, nodes.len() - 1);
    assert_eq!(work.shared_edges, 0);
    assert_eq!(
        work.canonical_bytes,
        nodes
            .iter()
            .map(|node| node.canonical_bytes().len())
            .sum::<usize>()
    );
    let mut ids = nodes.iter().map(|node| node.id()).collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), nodes.len());
    assert_eq!(BORROWED_KEY_CLONES.load(AtomicOrdering::Relaxed), 0);
    Ok(())
}

#[test]
fn zipper_keeps_only_a_borrowed_logarithmic_path() -> Result<(), TreeError> {
    let tree = PersistentTree::<BorrowedKeyRelation>::from_sorted_items(&[
        (BorrowedKey(b"alpha".to_vec()), 1),
        (BorrowedKey(b"beta".to_vec()), 2),
        (BorrowedKey(b"delta".to_vec()), 4),
        (BorrowedKey(b"gamma".to_vec()), 7),
    ])?;
    let path = tree.zipper().seek(b"delta".as_slice());
    assert!(!path.is_empty());
    assert_eq!(
        path.root().map(TreeNodeView::state_root),
        Some(tree.root().commitment())
    );
    assert_eq!(
        path.leaf().and_then(TreeNodeView::entries).map(<[_]>::len),
        Some(4)
    );

    let mut work = TreeWork::default();
    let checked = tree
        .zipper()
        .seek_with_work(b"delta".as_slice(), &mut work)?;
    assert_eq!(checked.len(), path.len());
    assert_eq!(work.visited_nodes, path.len());
    Ok(())
}

#[test]
fn one_key_edit_keeps_node_work_logarithmic_at_four_k_plus_rows() -> Result<(), TreeError> {
    let items: Vec<_> = (0..8_193u64).map(|key| (key, key * 2)).collect();
    let tree = PersistentTree::<RelationFixture>::from_sorted_items(&items)?;
    let update = tree.prepare_update(&[TreeChange {
        key: 4_096,
        after: Some(0xfeed_face),
    }])?;

    // The target has a single path-copied neighborhood.  Its amount of new
    // canonical structure depends on tree height, rather than row count.
    assert!(update.work().copied_nodes < 16);
    assert!(update.work().visited_nodes < 32);
    assert!(update.work().reused_nodes > 0);
    assert_eq!(update.commit().get(&4_096), Some(&0xfeed_face));
    Ok(())
}

#[test]
fn typed_id_ordering_does_not_require_marker_traits() {
    let tree = PersistentTree::<MarkerRelation>::empty();
    let first = tree.root_handle().id();
    let second = tree.root_handle().id();
    assert_eq!(first, second);
    assert_eq!(first.cmp(&second), Ordering::Equal);
    assert_eq!(first.to_bytes(), second.to_bytes());

    let root = canonical_empty::<MarkerRelation>().commitment();
    assert_eq!(root, root);
    assert_eq!(root.cmp(&root), Ordering::Equal);
}

#[test]
fn persistent_kernel_accepts_checked_interner_handles() -> Result<(), TreeError> {
    #[derive(Clone, Default)]
    struct CountingInterner(std::sync::Arc<AtomicUsize>);

    impl TreeInterner<RelationFixture> for CountingInterner {
        fn intern(&self, node: TreeNodeHandle<RelationFixture>) -> TreeNodeHandle<RelationFixture> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let summary = node.summary();
            assert_eq!(summary.commitment, node.canonical().commitment());
            node
        }
    }

    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let interner = CountingInterner(calls.clone());
    let tree = PersistentTree::from_sorted_items_with_interner(&[(1, 2), (2, 3)], interner)?;
    assert!(calls.load(std::sync::atomic::Ordering::Relaxed) > 0);
    let update = tree.prepare_update_with_interner(&[TreeChange {
        key: 2,
        after: Some(8),
    }])?;
    assert!(update.work().visited_nodes > 0);
    assert_eq!(update.commit().get(&2), Some(&8));
    Ok(())
}

#[test]
fn interner_update_work_scales_with_path_for_large_tree() -> Result<(), TreeError> {
    #[derive(Clone)]
    struct CountingInterner(std::sync::Arc<AtomicUsize>);

    impl TreeInterner<RelationFixture> for CountingInterner {
        fn intern(&self, node: TreeNodeHandle<RelationFixture>) -> TreeNodeHandle<RelationFixture> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            node
        }
    }

    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let interner = CountingInterner(calls.clone());
    let items: Vec<_> = (0..100_000u64).map(|key| (key, key * 2)).collect();
    let (tree, bulk_work) =
        PersistentTree::from_sorted_items_with_interner_with_work(&items, interner)?;
    assert_eq!(
        bulk_work.nodes,
        calls.load(std::sync::atomic::Ordering::Relaxed)
    );

    calls.store(0, std::sync::atomic::Ordering::Relaxed);
    let update = tree.prepare_update_with_interner(&[TreeChange {
        key: 50_000,
        after: Some(7),
    }])?;
    let update_calls = calls.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        update_calls <= 16,
        "rebuilt path was unexpectedly broad: {update_calls}"
    );
    assert!(update.work().visited_nodes <= 16);
    assert_eq!(update.work().nodes, update_calls);
    let updated = update.commit();
    assert_eq!(updated.get(&50_000), Some(&7));

    let mut range_work = TreeWork::default();
    let narrow = updated
        .range_with_work(49_995..=50_005, &mut range_work)
        .map(|(key, value)| (*key, *value))
        .collect::<Vec<_>>();
    assert_eq!(narrow.len(), 11);
    assert_eq!(narrow.first(), Some(&(49_995, 99_990)));
    assert_eq!(narrow.get(5), Some(&(50_000, 7)));
    assert_eq!(narrow.last(), Some(&(50_005, 100_010)));
    assert!(range_work.visited_nodes <= 16);
    assert_eq!(
        updated
            .range((
                std::ops::Bound::Excluded(49_999),
                std::ops::Bound::Excluded(50_002)
            ))
            .map(|(key, _)| *key)
            .collect::<Vec<_>>(),
        vec![50_000, 50_001]
    );
    assert_eq!(
        updated.range(..3).map(|(key, _)| *key).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        updated
            .range(99_998..)
            .map(|(key, _)| *key)
            .collect::<Vec<_>>(),
        vec![99_998, 99_999]
    );
    assert_eq!(
        updated
            .range((std::ops::Bound::Included(1), std::ops::Bound::Excluded(3)))
            .map(|(key, _)| *key)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(
        updated
            .range((std::ops::Bound::Excluded(1), std::ops::Bound::Included(3)))
            .map(|(key, _)| *key)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );

    calls.store(0, std::sync::atomic::Ordering::Relaxed);
    let no_op = updated.prepare_update_with_interner(&[])?;
    assert_eq!(no_op.work(), TreeWork::default());
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(
        updated.root().as_bytes().as_ptr(),
        no_op.tree().root().as_bytes().as_ptr()
    );
    Ok(())
}

#[test]
fn node_handles_have_stable_identity_and_weak_upgrade() -> Result<(), TreeError> {
    let tree = PersistentTree::<RelationFixture>::from_sorted_items(&[(1, 2), (2, 3)])?;
    let weak = {
        let handle = tree.root_handle();
        let weak = handle.downgrade();
        assert_eq!(weak.id(), handle.id());
        assert_eq!(weak.id().as_bytes(), tree.root().commitment().as_bytes());
        assert!(weak.upgrade().is_some());
        weak
    };
    assert!(weak.upgrade().is_some());
    drop(tree);
    assert!(weak.upgrade().is_none());
    Ok(())
}

#[test]
fn interner_cannot_substitute_a_different_checked_node() -> Result<(), TreeError> {
    #[derive(Clone)]
    struct WrongInterner(TreeNodeHandle<RelationFixture>);

    impl TreeInterner<RelationFixture> for WrongInterner {
        fn intern(
            &self,
            _node: TreeNodeHandle<RelationFixture>,
        ) -> TreeNodeHandle<RelationFixture> {
            self.0.clone()
        }
    }

    let wrong = PersistentTree::<RelationFixture>::from_sorted_items(&[(9, 9), (10, 10)])?;
    let result = PersistentTree::from_sorted_items_with_interner(
        &[(1, 2), (2, 3)],
        WrongInterner(wrong.root_handle()),
    );
    assert!(matches!(result, Err(TreeError::InvalidRoot)));
    Ok(())
}

#[test]
fn raw_scope_roots_cannot_self_mint_complete_coverage() -> Result<(), CoverageAdmissionError> {
    let scope = ScopeRoot::from_u64(7);
    let claim =
        AuthorityScopeClaim::from_object_version(ObjectVersion::<SchemaFixture>::from_value(&7));
    assert_eq!(
        admit_complete_scope(claim, admitted_observation(scope, [7; ID_BYTES])?),
        Err(CoverageAdmissionError::ScopeMismatch {
            declared: claim.scope_root(),
            observed: scope
        })
    );
    Ok(())
}

#[test]
fn admitted_observation_still_requires_exact_claimed_scope() -> Result<(), CoverageAdmissionError> {
    let claim =
        AuthorityScopeClaim::from_object_version(ObjectVersion::<SchemaFixture>::from_value(&7));
    let observed = ScopeRoot::from_u64(8);
    assert!(matches!(
        admit_complete_scope(claim, admitted_observation(observed, [7; ID_BYTES])?,),
        Err(CoverageAdmissionError::ScopeMismatch { .. })
    ));
    Ok(())
}

#[test]
fn complete_coverage_retains_admitted_producer_identity() -> Result<(), CoverageAdmissionError> {
    let claim =
        AuthorityScopeClaim::from_object_version(ObjectVersion::<SchemaFixture>::from_value(&7));
    let first = FixtureProducer {
        scope: claim.scope_root(),
        producer: [7; ID_BYTES],
    };
    let second = OtherFixtureProducer {
        scope: claim.scope_root(),
    };
    let first = admitted_observation(first.scope, first.producer)?;
    let second = admitted_observation(second.scope, [8; ID_BYTES])?;
    let first = admit_complete_scope(claim, first)?;
    let second = admit_complete_scope(claim, second)?;
    assert_ne!(first.producer_identity(), second.producer_identity());
    assert_eq!(first.scope_root(), second.scope_root());
    Ok(())
}

#[test]
fn producer_verifier_rejects_identity_context_and_evidence_drift() {
    struct StrictVerifier;

    impl ProducerObservationVerifier for StrictVerifier {
        type Error = &'static str;

        fn verify(&self, observation: &UntrustedProducerObservation) -> Result<(), Self::Error> {
            if observation.producer_identity() != [7; ID_BYTES]
                || observation.context() != [3; ID_BYTES]
                || observation.evidence() != [4, 5, 6]
            {
                return Err("producer session evidence drift");
            }
            Ok(())
        }
    }

    let scope = ScopeRoot::from_u64(7);
    let verifier = StrictVerifier;
    for (producer, context, evidence) in [
        ([8; ID_BYTES], [3; ID_BYTES], vec![4, 5, 6]),
        ([7; ID_BYTES], [2; ID_BYTES], vec![4, 5, 6]),
        ([7; ID_BYTES], [3; ID_BYTES], vec![9]),
    ] {
        assert!(matches!(
            admit_producer_observation(
                UntrustedProducerObservation::new(producer, scope, context, evidence),
                &verifier,
            ),
            Err(ProducerObservationAdmissionError::Rejected(
                "producer session evidence drift"
            ))
        ));
    }
}

#[test]
fn identity_classes_and_relation_domains_are_separated() {
    let key = ObjectKey::<SchemaFixture>::from_value(&7);
    let version = ObjectVersion::<SchemaFixture>::from_value(&7);
    assert_ne!(key.to_bytes(), version.to_bytes());

    let first = canonical_empty::<RelationFixture>().commitment();
    let second = canonical_empty::<OtherRelation>().commitment();
    assert_ne!(first.to_bytes(), second.to_bytes());
}

#[test]
fn malformed_ordering_is_rejected_at_each_boundary() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        cut_points(&[1u64, 1], DEFAULT_CUT_POLICY),
        Err(CutError::UnsortedOrDuplicate)
    );
    assert_eq!(
        canonical_leaf::<RelationFixture>(&[(2, 3), (1, 4)]),
        Err(NodeError::UnsortedOrDuplicate)
    );
    assert_eq!(
        prepare_delta(
            &state([(1, 2), (2, 3)])?,
            vec![
                MapChange {
                    key: 2,
                    before: Some(3),
                    after: Some(8),
                },
                MapChange {
                    key: 1,
                    before: Some(2),
                    after: Some(9),
                },
            ],
        ),
        Err(DeltaError::Unsorted)
    );
    Ok(())
}

#[test]
fn exact_transition_composition_and_inversion_hold() -> Result<(), Box<dyn std::error::Error>> {
    let base = state([(1, 2), (2, 3)])?;
    let first = prepare_delta(
        &base,
        vec![MapChange {
            key: 2,
            before: Some(3),
            after: Some(8),
        }],
    )?;
    let middle = apply_delta(&base, &first)?;
    let second = prepare_delta(
        &middle,
        vec![MapChange {
            key: 1,
            before: Some(2),
            after: None,
        }],
    )?;
    let target = apply_delta(&middle, &second)?;
    let composed = Delta::compose(&first, &second)?;
    assert_eq!(apply_delta(&base, &composed)?.root(), target.root());
    assert_eq!(
        apply_delta(&target, &composed.inverse())?.root(),
        base.root()
    );
    assert_eq!(composed.inverse().inverse().id(), composed.id());
    assert_eq!(
        Delta::compose(&second.inverse(), &first.inverse())?.id(),
        composed.inverse().id()
    );

    assert_eq!(Delta::compose(&first, &first), Err(DeltaError::NonAdjacent));
    Ok(())
}

#[test]
fn no_op_changes_canonicalize_to_empty_transition() -> Result<(), Box<dyn std::error::Error>> {
    let base = state([(1, 2)])?;
    let empty = prepare_delta(&base, Vec::new())?;
    let explicit = prepare_delta(
        &base,
        vec![MapChange {
            key: 1,
            before: Some(2),
            after: Some(2),
        }],
    )?;
    assert_eq!(explicit.id(), empty.id());
    assert_eq!(explicit.base(), explicit.target());
    assert_eq!(explicit.changes().count(), 0);
    Ok(())
}

#[test]
fn incomplete_coverage_cannot_prepare_or_apply_a_delta() -> Result<(), Box<dyn std::error::Error>> {
    let scope = ScopeRoot::from_u64(1);
    let partial = RelationState::<RelationFixture>::from_entries(
        [(1, 2)],
        CoverageWitness::Partial(UntrustedCoverageScope::from_scope_root(scope)),
    )?;
    assert_eq!(
        prepare_delta(
            &partial,
            vec![MapChange {
                key: 1,
                before: Some(2),
                after: Some(3),
            }]
        ),
        Err(DeltaError::IncompleteBase)
    );
    Ok(())
}

#[test]
fn closed_relation_scope_supports_delta_algebra_without_workspace_authority()
-> Result<(), Box<dyn std::error::Error>> {
    let scope = ClosedRelationScope::from_scope_root(ScopeRoot::from_u64(1));
    let coverage = CoverageWitness::closed_relation(scope);
    let base = RelationState::<RelationFixture>::from_entries([(1, 2)], coverage)?;
    let delta = prepare_delta(
        &base,
        vec![MapChange {
            key: 1,
            before: Some(2),
            after: Some(3),
        }],
    )?;
    let target = apply_delta(&base, &delta)?;
    assert_eq!(target.get(&1), Some(&3));
    assert_eq!(target.coverage(), coverage);
    let binding = RelationBinding::from_state(&base);
    assert!(!binding.is_complete());
    assert!(matches!(
        WorkspaceManifest::new_checked(
            1,
            vec![binding],
            vec![],
            ObjectClosure::from_version(ObjectVersion::<SchemaFixture>::from_value(&3)),
            coverage,
        ),
        Err(WorkspaceError::UnverifiedCoverage | WorkspaceError::IncompleteRelation)
    ));
    Ok(())
}

#[test]
fn unadmitted_complete_coverage_cannot_cross_delta_boundary()
-> Result<(), Box<dyn std::error::Error>> {
    let scope = ScopeRoot::from_u64(1);
    let untrusted_complete = CoverageWitness::from_untrusted_parts(Coverage::Complete, scope, None);
    let state = RelationState::<RelationFixture>::from_entries([(1, 2)], untrusted_complete)?;
    let change = MapChange {
        key: 1,
        before: Some(2),
        after: Some(3),
    };
    assert_eq!(
        prepare_delta(&state, vec![change.clone()]),
        Err(DeltaError::IncompleteBase)
    );
    let trusted = RelationState::<RelationFixture>::from_entries([(1, 2)], complete(1)?)?;
    let delta = prepare_delta(&trusted, vec![change])?;
    assert_eq!(apply_delta(&state, &delta), Err(DeltaError::IncompleteBase));
    Ok(())
}

#[test]
fn manifest_relation_set_schema_and_basis_drift_is_rejected()
-> Result<(), Box<dyn std::error::Error>> {
    let coverage = complete(1)?;
    let base_state = state([(1, 2)])?;
    let base = manifest(&base_state, coverage)?;
    let other_state = RelationState::<OtherRelation>::from_entries([(1, 2)], coverage)?;
    let extra = WorkspaceManifest::new_checked(
        1,
        vec![
            RelationBinding::from_state(&base_state),
            RelationBinding::from_state(&other_state),
        ],
        vec![],
        ObjectClosure::from_version(ObjectVersion::<SchemaFixture>::from_value(&3)),
        coverage,
    )?;
    assert_eq!(
        workspace_delta(&base, &extra, Vec::new()),
        Err(WorkspaceError::RelationSetMismatch)
    );

    let changed_relation_schema = WorkspaceManifest::new_checked(
        1,
        vec![RelationBinding::from_state(&other_state)],
        vec![],
        ObjectClosure::from_version(ObjectVersion::<SchemaFixture>::from_value(&3)),
        coverage,
    )?;
    assert_eq!(
        workspace_delta(&base, &changed_relation_schema, Vec::new()),
        Err(WorkspaceError::RelationSetMismatch)
    );

    assert_eq!(
        WorkspaceManifest::new_checked(
            2,
            vec![RelationBinding::from_state(&base_state)],
            vec![],
            ObjectClosure::from_version(ObjectVersion::<SchemaFixture>::from_value(&3)),
            coverage,
        ),
        Err(WorkspaceError::SchemaMismatch)
    );

    let basis_drift = WorkspaceManifest::new_checked(
        1,
        vec![RelationBinding::from_state(&base_state)],
        vec![BasisBinding::from_version(
            ObjectVersion::<SchemaFixture>::from_value(&8),
        )],
        ObjectClosure::from_version(ObjectVersion::<SchemaFixture>::from_value(&3)),
        coverage,
    )?;
    assert_eq!(
        workspace_delta(&base, &basis_drift, Vec::new()),
        Err(WorkspaceError::BasisMismatch)
    );
    Ok(())
}

#[test]
fn raw_workspace_descriptors_cannot_close_or_commit() -> Result<(), Box<dyn std::error::Error>> {
    let raw = WorkspaceManifest::new(
        1,
        vec![RelationBinding::new(
            SchemaIdentity::new(1, 7, 1),
            [1; ID_BYTES],
        )],
        Vec::new(),
        [3; ID_BYTES],
        CoverageWitness::Partial(partial_coverage(1)),
    )?;
    assert_eq!(raw.validate(), Err(WorkspaceError::UnverifiedManifest));
    assert_eq!(raw.authority_closure(), None);
    Ok(())
}

#[test]
fn workspace_delta_requires_exact_relation_transitions() -> Result<(), Box<dyn std::error::Error>> {
    let base_state = state([(1, 2)])?;
    let relation_delta = prepare_delta(
        &base_state,
        vec![MapChange {
            key: 1,
            before: Some(2),
            after: Some(3),
        }],
    )?;
    let target_state = apply_delta(&base_state, &relation_delta)?;
    let base = manifest(&base_state, complete(1)?)?;
    let target = manifest(&target_state, complete(1)?)?;
    assert_eq!(
        workspace_delta(&base, &target, Vec::new()),
        Err(WorkspaceError::MissingTransition)
    );
    let transition = RelationTransition::from_delta(&relation_delta);
    let checked = workspace_delta(&base, &target, vec![transition])?;
    assert_eq!(checked.base(), base.root());
    assert_eq!(checked.target(), target.root());

    let noop = prepare_delta(&base_state, Vec::new())?;
    let noop_transition = RelationTransition::from_delta(&noop);
    assert_eq!(
        workspace_delta(&base, &base, vec![noop_transition]),
        Err(WorkspaceError::UnexpectedTransition)
    );
    Ok(())
}

#[test]
fn history_and_state_identities_are_separate() -> Result<(), Box<dyn std::error::Error>> {
    let state = state([(1, 2)])?;
    let manifest = manifest(&state, complete(1)?)?;
    let authority = ObjectVersion::<SchemaFixture>::from_value(&3);
    let first = commit_checked(
        &manifest,
        Vec::new(),
        CommitProvenance::from_versions(
            authority,
            ObjectVersion::<SchemaFixture>::from_value(&1),
            b"first",
        ),
    )?;
    let second = commit_checked(
        &manifest,
        Vec::new(),
        CommitProvenance::from_versions(
            authority,
            ObjectVersion::<SchemaFixture>::from_value(&2),
            b"second",
        ),
    )?;
    assert_eq!(first.root(), second.root());
    assert_ne!(first.id(), second.id());
    assert!(first.parents().is_empty());
    Ok(())
}

#[test]
fn commit_binds_provenance_and_canonicalizes_parent_order() -> Result<(), Box<dyn std::error::Error>>
{
    let state = state([(1, 2)])?;
    let manifest = manifest(&state, complete(1)?)?;
    let authority = ObjectVersion::<SchemaFixture>::from_value(&3);
    let parent_one = commit_checked(
        &manifest,
        Vec::new(),
        CommitProvenance::from_versions(
            authority,
            ObjectVersion::<SchemaFixture>::from_value(&11),
            b"parent-one",
        ),
    )?;
    let parent_two = commit_checked(
        &manifest,
        Vec::new(),
        CommitProvenance::from_versions(
            authority,
            ObjectVersion::<SchemaFixture>::from_value(&12),
            b"parent-two",
        ),
    )?;
    let provenance = CommitProvenance::from_versions(
        authority,
        ObjectVersion::<SchemaFixture>::from_value(&13),
        b"merge",
    );
    let merged = commit_checked(
        &manifest,
        vec![parent_two.id(), parent_one.id()],
        provenance.clone(),
    )?;
    let mut expected = vec![parent_one.id(), parent_two.id()];
    expected.sort();
    assert_eq!(merged.parents(), expected.as_slice());
    assert_eq!(
        merged.id(),
        commit_checked(&manifest, expected, provenance)?.id()
    );
    assert_eq!(
        commit_checked(
            &manifest,
            vec![parent_one.id(), parent_one.id()],
            CommitProvenance::from_versions(
                authority,
                ObjectVersion::<SchemaFixture>::from_value(&14),
                b"duplicate",
            ),
        ),
        Err(CommitError::DuplicateParent)
    );
    assert_eq!(
        commit(manifest.root(), Vec::new(), b"legacy-unbound",),
        Err(CommitError::MissingProvenance)
    );
    Ok(())
}

#[test]
fn wire_context_and_length_mismatches_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = [7u8; ID_BYTES];
    let wire = WireId::<SchemaFixture>::decode(&bytes, IdContext::schema::<SchemaFixture>())?;
    assert_eq!(
        ObjectKey::<SchemaFixture>::admit(wire.into_untrusted()),
        Err(IdAdmissionError::ContextMismatch {
            expected: IdContext::object_key::<SchemaFixture>(),
            actual: IdContext::schema::<SchemaFixture>(),
        })
    );
    let matching =
        WireId::<SchemaFixture>::decode(&bytes, IdContext::object_key::<SchemaFixture>())?;
    assert_eq!(
        ObjectKey::<SchemaFixture>::admit(matching.into_untrusted()),
        Err(IdAdmissionError::UnverifiedDigest)
    );
    let expected = ObjectKey::<SchemaFixture>::from_value(&7);
    let claim = WireId::<SchemaFixture>::decode(
        expected.as_bytes(),
        IdContext::object_key::<SchemaFixture>(),
    )?;
    assert_eq!(
        ObjectKey::<SchemaFixture>::admit_value(claim.into_untrusted(), &7)?,
        expected
    );
    let forged =
        WireId::<SchemaFixture>::decode(&[9; ID_BYTES], IdContext::object_key::<SchemaFixture>())?;
    assert_eq!(
        ObjectKey::<SchemaFixture>::admit_value(forged.into_untrusted(), &7),
        Err(IdAdmissionError::DigestMismatch)
    );
    let node = canonical_empty::<RelationFixture>();
    let root_claim = WireId::<RelationFixture>::decode(
        node.commitment().as_bytes(),
        IdContext::relation::<RelationFixture>(),
    )?;
    assert_eq!(
        StateRoot::<RelationFixture>::admit_canonical_bytes(
            root_claim.into_untrusted(),
            node.as_bytes(),
        )?,
        node.commitment()
    );
    let base = state([(1, 2)])?;
    let delta = prepare_delta(
        &base,
        vec![MapChange {
            key: 1,
            before: Some(2),
            after: Some(3),
        }],
    )?;
    let cached_changes = delta.canonical_changes_bytes();
    assert_eq!(
        cached_changes.as_ptr(),
        delta.canonical_changes_bytes().as_ptr()
    );
    let transition = RelationTransition::from_delta(&delta);
    assert_eq!(
        cached_changes.as_ptr(),
        transition.canonical_changes_bytes().as_ptr()
    );
    let delta_claim = WireId::<RelationFixture>::decode(
        delta.id().as_bytes(),
        IdContext::delta::<RelationFixture>(),
    )?;
    assert_eq!(
        DeltaId::<RelationFixture>::admit_transition(
            delta_claim.into_untrusted(),
            delta.base(),
            delta.target(),
            &delta.canonical_changes(),
        )?,
        delta.id()
    );
    let wrong_version = IdContext::new(
        IdContext::schema::<SchemaFixture>().class(),
        IdContext::schema::<SchemaFixture>().domain(),
        IdContext::schema::<SchemaFixture>().ty(),
        99,
    );
    let claim = UntrustedId::<SchemaFixture>::from_wire(&bytes, wrong_version)?;
    assert!(matches!(
        ObjectVersion::<SchemaFixture>::admit(claim),
        Err(IdAdmissionError::ContextMismatch { .. })
    ));
    assert_eq!(
        WireId::<SchemaFixture>::decode(
            &bytes[..ID_BYTES - 1],
            IdContext::schema::<SchemaFixture>()
        ),
        Err(IdDecodeError::WrongLength)
    );
    Ok(())
}

#[test]
fn manifest_wire_roundtrip_requires_typed_closure_admission()
-> Result<(), Box<dyn std::error::Error>> {
    let relation_state = state([(1, 2)])?;
    let coverage = complete(1)?;
    let authority = ObjectVersion::<SchemaFixture>::from_value(&3);
    let original = manifest(&relation_state, coverage)?;
    let encoded = original.encode();
    let raw = WorkspaceManifest::decode_untrusted(&encoded)?;
    assert!(!raw.is_checked());
    assert_eq!(raw.authority_closure(), None);
    assert_eq!(raw.validate(), Err(WorkspaceError::UnverifiedManifest));

    let admitted = raw.admit_checked(
        vec![RelationBinding::from_state(&relation_state)],
        Vec::new(),
        ObjectClosure::from_version(authority),
        coverage,
    )?;
    assert!(admitted.is_checked());
    assert_eq!(admitted.root(), original.root());
    assert_eq!(admitted.encode(), encoded);
    let closure = admitted.closure_refs()?;
    assert_eq!(closure.len(), 2);
    assert_eq!(closure[0].kind(), ClosureKind::Relation);
    assert_eq!(closure[1].kind(), ClosureKind::Authority);

    let mut wrong_schema = encoded.clone();
    wrong_schema[8] = 2;
    assert_eq!(
        WorkspaceManifest::decode_untrusted(&wrong_schema),
        Err(WorkspaceDecodeError::UnsupportedVersion)
    );

    let mut old_wire_version = encoded.clone();
    old_wire_version[4] = 1;
    assert_eq!(
        WorkspaceManifest::decode_untrusted(&old_wire_version),
        Err(WorkspaceDecodeError::UnsupportedVersion)
    );

    let claim = AuthorityScopeClaim::from_object_version(authority);
    let different_producer = admit_producer_observation(
        UntrustedProducerObservation::new(
            [8; ID_BYTES],
            claim.scope_root(),
            [3; ID_BYTES],
            vec![4, 5, 6],
        ),
        &FixtureVerifier,
    )?;
    let different_coverage =
        CoverageWitness::Complete(admit_complete_scope(claim, different_producer)?);
    assert_eq!(
        WorkspaceManifest::decode_untrusted(&encoded)?.admit_checked(
            vec![RelationBinding::from_state(&relation_state)],
            Vec::new(),
            ObjectClosure::from_version(authority),
            different_coverage,
        ),
        Err(WorkspaceError::CoverageMismatch)
    );

    let same_producer_different_context = admit_producer_observation(
        UntrustedProducerObservation::new(
            [7; ID_BYTES],
            claim.scope_root(),
            [9; ID_BYTES],
            vec![4, 5, 6],
        ),
        &FixtureVerifier,
    )?;
    let context_coverage = CoverageWitness::Complete(admit_complete_scope(
        claim,
        same_producer_different_context,
    )?);
    assert_eq!(
        WorkspaceManifest::decode_untrusted(&encoded)?.admit_checked(
            vec![RelationBinding::from_state(&relation_state)],
            Vec::new(),
            ObjectClosure::from_version(authority),
            context_coverage,
        ),
        Err(WorkspaceError::CoverageMismatch)
    );

    let same_producer_different_evidence = admit_producer_observation(
        UntrustedProducerObservation::new(
            [7; ID_BYTES],
            claim.scope_root(),
            [3; ID_BYTES],
            vec![6, 5, 4],
        ),
        &FixtureVerifier,
    )?;
    let evidence_coverage = CoverageWitness::Complete(admit_complete_scope(
        claim,
        same_producer_different_evidence,
    )?);
    assert_eq!(
        WorkspaceManifest::decode_untrusted(&encoded)?.admit_checked(
            vec![RelationBinding::from_state(&relation_state)],
            Vec::new(),
            ObjectClosure::from_version(authority),
            evidence_coverage,
        ),
        Err(WorkspaceError::CoverageMismatch)
    );
    Ok(())
}

#[test]
fn persisted_root_binding_admits_manifest_without_relation_materialization()
-> Result<(), Box<dyn std::error::Error>> {
    let coverage = complete(1)?;
    let state = state([(1, 2)])?;
    let materialized = state.materialize();
    let claim = UntrustedId::<RelationFixture>::from_wire(
        state.root().as_bytes(),
        IdContext::relation::<RelationFixture>(),
    )?;
    let persisted = PersistedTreeRoot::admit(claim, materialized.canonical_bytes())?;
    let cloned = persisted.clone();
    assert_eq!(cloned.root(), persisted.root());
    let binding = RelationBinding::from_persisted_root(&persisted, coverage);
    assert!(binding.is_checked());
    assert!(binding.is_complete());

    let authority = ObjectClosure::from_version(ObjectVersion::<SchemaFixture>::from_value(&3));
    let original =
        WorkspaceManifest::new_checked(1, vec![binding], Vec::new(), authority, coverage)?;
    let raw = WorkspaceManifest::decode_untrusted(&original.encode())?;
    let admitted = raw.admit_checked(vec![binding], Vec::new(), authority, coverage)?;
    assert_eq!(admitted.root(), original.root());

    let mut forged = original.encode();
    let relation_root = state.root().to_bytes();
    let relation_root_at = forged
        .windows(ID_BYTES)
        .position(|window| window == relation_root)
        .ok_or(NodeError::MalformedEncoding)?;
    forged[relation_root_at] ^= 1;
    let raw = WorkspaceManifest::decode_untrusted(&forged)?;
    assert_eq!(
        raw.admit_checked(vec![binding], Vec::new(), authority, coverage),
        Err(WorkspaceError::UnverifiedRelation)
    );
    Ok(())
}

#[test]
fn transition_wire_roundtrip_requires_exact_typed_delta() -> Result<(), Box<dyn std::error::Error>>
{
    let base_state = state([(1, 2)])?;
    let delta = prepare_delta(
        &base_state,
        vec![MapChange {
            key: 1,
            before: Some(2),
            after: Some(3),
        }],
    )?;
    let transition = RelationTransition::from_delta(&delta);
    let changes = delta.changes().cloned().collect::<Vec<_>>();
    assert_eq!(
        canonical_delta_id(delta.base(), delta.target(), &changes),
        delta.id()
    );
    let raw = RelationTransition::decode_untrusted(&transition.encode())?;
    assert!(!raw.is_checked());
    assert_eq!(raw.admit_delta(&delta)?, transition);

    let mut forged = transition.encode();
    forged[41] ^= 1;
    let forged = RelationTransition::decode_untrusted(&forged)?;
    assert_eq!(
        forged.admit_delta(&delta),
        Err(WorkspaceError::TransitionMismatch)
    );

    let target_state = apply_delta(&base_state, &delta)?;
    let base = manifest(&base_state, complete(1)?)?;
    let target = manifest(&target_state, complete(1)?)?;
    let workspace = workspace_delta(&base, &target, vec![transition])?;
    let checked_workspace = workspace.checked();
    assert!(checked_workspace.binds_base(&base));
    assert!(checked_workspace.binds_target(&target));
    assert!(!checked_workspace.binds_target(&base));
    let raw_workspace = WorkspaceDelta::decode_untrusted(&workspace.encode())?;
    assert_eq!(
        raw_workspace.admit(&base, &target, vec![RelationTransition::from_delta(&delta)])?,
        workspace
    );
    Ok(())
}

#[test]
fn provenance_and_commit_wire_roundtrips_recompute_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let relation_state = state([(1, 2)])?;
    let coverage = complete(1)?;
    let manifest = manifest(&relation_state, coverage)?;
    let authority = ObjectVersion::<SchemaFixture>::from_value(&3);
    let transaction = ObjectVersion::<SchemaFixture>::from_value(&8);
    let provenance = CommitProvenance::from_versions(authority, transaction, b"wire");
    let raw_provenance = CommitProvenance::decode_untrusted(&provenance.encode())?;
    let admitted_provenance = raw_provenance.clone().admit(
        ObjectClosure::from_version(authority),
        ObjectClosure::from_version(transaction),
    )?;
    assert_eq!(admitted_provenance, provenance);
    assert_eq!(raw_provenance.encode(), provenance.encode());
    assert_eq!(provenance.closure_refs().len(), 2);

    let commit = commit_checked(&manifest, Vec::new(), provenance.clone())?;
    let raw_commit = Commit::decode_untrusted(&commit.encode())?;
    assert_eq!(raw_commit.encode(), commit.encode());
    assert_eq!(
        raw_commit.clone().admit(&manifest, provenance.clone())?,
        commit
    );
    let capability = raw_commit.admit_capability(&manifest, provenance)?;
    assert_eq!(capability.parent_count(), 0);
    assert!(capability.binds_target(&manifest));

    let mut forged = commit.encode();
    forged[5] ^= 1;
    let forged = Commit::decode_untrusted(&forged)?;
    assert_eq!(
        forged.admit(
            &manifest,
            CommitProvenance::from_versions(authority, transaction, b"wire",)
        ),
        Err(CommitError::IdMismatch)
    );
    Ok(())
}

#[test]
fn wire_rejects_version_trailing_parent_order_and_duplicate_errors()
-> Result<(), Box<dyn std::error::Error>> {
    let relation_state = state([(1, 2)])?;
    let manifest = manifest(&relation_state, complete(1)?)?;
    let authority = ObjectVersion::<SchemaFixture>::from_value(&3);
    let make_parent = |value| {
        commit_checked(
            &manifest,
            Vec::new(),
            CommitProvenance::from_versions(
                authority,
                ObjectVersion::<SchemaFixture>::from_value(&value),
                value.to_be_bytes(),
            ),
        )
    };
    let first = make_parent(10u64)?;
    let second = make_parent(11u64)?;
    let merge = commit_checked(
        &manifest,
        vec![first.id(), second.id()],
        CommitProvenance::from_versions(
            authority,
            ObjectVersion::<SchemaFixture>::from_value(&12),
            b"merge",
        ),
    )?;
    let encoded = merge.encode();
    let raw_merge = Commit::decode_untrusted(&encoded)?;
    let merge_provenance = CommitProvenance::from_versions(
        authority,
        ObjectVersion::<SchemaFixture>::from_value(&12),
        b"merge",
    );
    assert_eq!(
        raw_merge.clone().admit_with_parents(
            &manifest,
            vec![first.id(), second.id()],
            merge_provenance,
        )?,
        merge
    );
    assert_eq!(
        raw_merge.admit(
            &manifest,
            CommitProvenance::from_versions(
                authority,
                ObjectVersion::<SchemaFixture>::from_value(&12),
                b"merge",
            )
        ),
        Err(CommitError::UnverifiedParent)
    );
    assert!(matches!(
        Commit::decode_untrusted(&[encoded.as_slice(), &[0]].concat()),
        Err(WorkspaceDecodeError::TrailingBytes)
    ));
    let mut wrong_version = encoded.clone();
    wrong_version[4] = 99;
    assert_eq!(
        Commit::decode_untrusted(&wrong_version),
        Err(WorkspaceDecodeError::UnsupportedVersion)
    );

    let parent_start = 73;
    let mut unordered = encoded.clone();
    for index in 0..ID_BYTES {
        unordered.swap(parent_start + index, parent_start + ID_BYTES + index);
    }
    assert_eq!(
        Commit::decode_untrusted(&unordered),
        Err(WorkspaceDecodeError::UnorderedParents)
    );
    let mut duplicate = encoded;
    let first_parent = duplicate[parent_start..parent_start + ID_BYTES].to_vec();
    duplicate[parent_start + ID_BYTES..parent_start + ID_BYTES * 2].copy_from_slice(&first_parent);
    assert_eq!(
        Commit::decode_untrusted(&duplicate),
        Err(WorkspaceDecodeError::DuplicateParent)
    );
    Ok(())
}
