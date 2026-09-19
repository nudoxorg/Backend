//! Structural tests for the bounded Merkle reconciler.

use super::*;
use backend_version::ObjectVersion;

use std::collections::BTreeMap;

#[derive(Default)]
struct Fixture {
    pages: BTreeMap<NodeDigest, MerklePage>,
    requests: Vec<MerklePageRequest>,
}

impl MerklePageSource for Fixture {
    fn page(&mut self, request: MerklePageRequest) -> Result<MerklePage, ReplicationError> {
        self.requests.push(request);
        let page = self
            .pages
            .get(&request.node)
            .cloned()
            .ok_or(ReplicationError::CorruptFrame)?;
        if page.cursor != request.cursor {
            return Err(ReplicationError::InvalidWire);
        }
        Ok(page)
    }
}

#[derive(Default)]
struct PagedFixture {
    pages: BTreeMap<(NodeDigest, u32), MerklePage>,
    requests: Vec<MerklePageRequest>,
}

impl MerklePageSource for PagedFixture {
    fn page(&mut self, request: MerklePageRequest) -> Result<MerklePage, ReplicationError> {
        self.requests.push(request);
        self.pages
            .get(&(request.node, request.cursor.offset))
            .cloned()
            .ok_or(ReplicationError::CorruptFrame)
    }
}

fn budget() -> ReconcileBudget {
    ReconcileBudget {
        max_pages: 8,
        max_pending: 16,
        max_deltas: 8,
        max_page_items: 4,
        max_key_bytes: 32,
    }
}

fn digest(value: u8) -> NodeDigest {
    NodeDigest([value; 32])
}

fn object(key: u8, version: u8) -> MerkleObject {
    MerkleObject {
        key: vec![key],
        key_id: [key; 32],
        version: [version; 32],
        len: 1,
    }
}

#[test]
fn root_is_bound_to_an_admitted_manifest_identity() {
    let manifest = ObjectVersion::<crate::ImmutableObjectSchema>::from_value(b"manifest");
    let root = MerkleRoot::from_admitted_manifest(1, manifest);
    assert_eq!(root.schema(), 1);
    assert_eq!(root.digest(), NodeDigest(manifest.to_bytes()));
}

#[test]
fn equal_roots_skip_all_pages() {
    let root = MerkleRoot::new(1, digest(1));
    let mut reconciler = MerkleReconciler::new(root, root, budget()).expect("cursor");
    let mut local = Fixture::default();
    let mut remote = Fixture::default();
    let page = reconciler
        .step(&mut local, &mut remote, budget())
        .expect("step");
    assert!(page.deltas.is_empty());
    assert!(page.next.is_none());
    assert!(local.requests.is_empty());
    assert!(remote.requests.is_empty());
}

#[test]
fn unequal_leaf_roots_emit_only_changed_rows() {
    let local_root = digest(1);
    let remote_root = digest(2);
    let local_binding = MerkleRoot::new(1, local_root);
    let remote_binding = MerkleRoot::new(1, remote_root);
    let mut local = Fixture::default();
    local.pages.insert(
        local_root,
        MerklePage {
            root: local_binding,
            node: local_root,
            level: 0,
            cursor: PageCursor::origin(),
            next: None,
            body: MerklePageBody::Leaf(vec![object(1, 1), object(2, 2)]),
        },
    );
    let mut remote = Fixture::default();
    remote.pages.insert(
        remote_root,
        MerklePage {
            root: remote_binding,
            node: remote_root,
            level: 0,
            cursor: PageCursor::origin(),
            next: None,
            body: MerklePageBody::Leaf(vec![object(1, 1), object(2, 9), object(3, 3)]),
        },
    );
    let mut reconciler =
        MerkleReconciler::new(local_binding, remote_binding, budget()).expect("cursor");
    let page = reconciler
        .step(&mut local, &mut remote, budget())
        .expect("step");
    assert_eq!(page.deltas.len(), 2);
    assert_eq!(page.deltas[0].key, vec![2]);
    assert_eq!(page.deltas[1].key, vec![3]);
    assert!(page.next.is_none());
    assert_eq!(local.requests.len(), 1);
    assert_eq!(remote.requests.len(), 1);
}

#[test]
fn branch_descent_skips_equal_children_and_bounds_frontier() {
    let local_root = digest(10);
    let remote_root = digest(11);
    let local_binding = MerkleRoot::new(1, local_root);
    let remote_binding = MerkleRoot::new(1, remote_root);
    let shared = digest(12);
    let local_leaf = digest(13);
    let remote_leaf = digest(14);
    let branch = |root, node, child| MerklePage {
        root: MerkleRoot::new(1, root),
        node,
        level: 1,
        cursor: PageCursor::origin(),
        next: None,
        body: MerklePageBody::Branch(vec![
            MerkleChild {
                first_key: vec![0],
                end_key: Some(vec![5]),
                digest: shared,
                level: 0,
                row_count: 1,
            },
            MerkleChild {
                first_key: vec![5],
                end_key: None,
                digest: child,
                level: 0,
                row_count: 1,
            },
        ]),
    };
    let mut local = Fixture::default();
    local
        .pages
        .insert(local_root, branch(local_root, local_root, local_leaf));
    local.pages.insert(
        local_leaf,
        MerklePage {
            root: local_binding,
            node: local_leaf,
            level: 0,
            cursor: PageCursor::origin(),
            next: None,
            body: MerklePageBody::Leaf(vec![object(5, 1)]),
        },
    );
    let mut remote = Fixture::default();
    remote
        .pages
        .insert(remote_root, branch(remote_root, remote_root, remote_leaf));
    remote.pages.insert(
        remote_leaf,
        MerklePage {
            root: remote_binding,
            node: remote_leaf,
            level: 0,
            cursor: PageCursor::origin(),
            next: None,
            body: MerklePageBody::Leaf(vec![object(5, 2)]),
        },
    );
    let mut reconciler =
        MerkleReconciler::new(local_binding, remote_binding, budget()).expect("cursor");
    let page = reconciler
        .step(&mut local, &mut remote, budget())
        .expect("step");
    assert_eq!(page.deltas.len(), 1);
    assert_eq!(page.deltas[0].key, vec![5]);
    assert!(page.next.is_none());
    assert_eq!(local.requests.len(), 2);
    assert_eq!(remote.requests.len(), 2);
}

#[test]
fn leaf_pages_resume_without_losing_cross_boundary_keys() {
    let local_node = digest(20);
    let remote_node = digest(21);
    let local_root = MerkleRoot::new(1, local_node);
    let remote_root = MerkleRoot::new(1, remote_node);
    let page = |root, node, cursor, next, entries| MerklePage {
        root,
        node,
        level: 0,
        cursor,
        next,
        body: MerklePageBody::Leaf(entries),
    };
    let mut local = PagedFixture::default();
    local.pages.insert(
        (local_node, 0),
        page(
            local_root,
            local_node,
            PageCursor::origin(),
            Some(PageCursor { offset: 2 }),
            vec![object(1, 1), object(2, 2)],
        ),
    );
    local.pages.insert(
        (local_node, 2),
        page(
            local_root,
            local_node,
            PageCursor { offset: 2 },
            None,
            vec![object(3, 3), object(4, 4)],
        ),
    );
    let mut remote = PagedFixture::default();
    remote.pages.insert(
        (remote_node, 0),
        page(
            remote_root,
            remote_node,
            PageCursor::origin(),
            Some(PageCursor { offset: 3 }),
            vec![object(1, 1), object(2, 2), object(3, 3)],
        ),
    );
    remote.pages.insert(
        (remote_node, 3),
        page(
            remote_root,
            remote_node,
            PageCursor { offset: 3 },
            None,
            vec![object(4, 9), object(5, 5)],
        ),
    );
    let page_budget = ReconcileBudget {
        max_pages: 2,
        ..budget()
    };
    let mut reconciler =
        MerkleReconciler::new(local_root, remote_root, page_budget).expect("cursor");
    let mut deltas = Vec::new();
    for _ in 0..8 {
        let result = reconciler
            .step(&mut local, &mut remote, page_budget)
            .expect("step");
        deltas.extend(result.deltas.into_iter().map(|delta| delta.key));
        if result.next.is_none() {
            break;
        }
    }
    assert_eq!(deltas, vec![vec![4], vec![5]]);
    assert!(reconciler.is_complete());
    // The remote page containing key 3 is necessarily refetched while
    // the local cursor advances.  The cursor offsets make that replay
    // harmless and avoid retaining either page body across the call.
    assert!(
        remote
            .requests
            .iter()
            .any(|request| request.cursor.offset == 0)
    );
    assert!(
        local
            .requests
            .iter()
            .any(|request| request.cursor.offset == 2)
    );
}

#[test]
fn uneven_branch_pages_compare_only_their_interval_intersection() {
    let local_root_node = digest(30);
    let remote_root_node = digest(31);
    let local_child_left = digest(32);
    let local_child_right = digest(33);
    let remote_child_left = digest(34);
    let remote_child_right = digest(35);
    let local_root = MerkleRoot::new(1, local_root_node);
    let remote_root = MerkleRoot::new(1, remote_root_node);
    let branch_page = |root, node, cursor, next, children| MerklePage {
        root,
        node,
        level: 1,
        cursor,
        next,
        body: MerklePageBody::Branch(children),
    };
    let leaf_page = |root, node, entries| MerklePage {
        root,
        node,
        level: 0,
        cursor: PageCursor::origin(),
        next: None,
        body: MerklePageBody::Leaf(entries),
    };
    let child = |first: u8, end: Option<u8>, node: NodeDigest| MerkleChild {
        first_key: vec![first],
        end_key: end.map(|end| vec![end]),
        digest: node,
        level: 0,
        row_count: 1,
    };
    let mut local = PagedFixture::default();
    local.pages.insert(
        (local_root_node, 0),
        branch_page(
            local_root,
            local_root_node,
            PageCursor::origin(),
            Some(PageCursor { offset: 1 }),
            vec![child(0, Some(5), local_child_left)],
        ),
    );
    local.pages.insert(
        (local_root_node, 1),
        branch_page(
            local_root,
            local_root_node,
            PageCursor { offset: 1 },
            None,
            vec![child(5, None, local_child_right)],
        ),
    );
    local.pages.insert(
        (local_child_left, 0),
        leaf_page(local_root, local_child_left, vec![object(1, 1)]),
    );
    local.pages.insert(
        (local_child_right, 0),
        leaf_page(local_root, local_child_right, vec![object(6, 3)]),
    );

    let mut remote = PagedFixture::default();
    remote.pages.insert(
        (remote_root_node, 0),
        branch_page(
            remote_root,
            remote_root_node,
            PageCursor::origin(),
            Some(PageCursor { offset: 1 }),
            vec![child(0, Some(7), remote_child_left)],
        ),
    );
    remote.pages.insert(
        (remote_root_node, 1),
        branch_page(
            remote_root,
            remote_root_node,
            PageCursor { offset: 1 },
            None,
            vec![child(7, None, remote_child_right)],
        ),
    );
    remote.pages.insert(
        (remote_child_left, 0),
        leaf_page(
            remote_root,
            remote_child_left,
            vec![object(1, 2), object(6, 3)],
        ),
    );
    remote.pages.insert(
        (remote_child_right, 0),
        leaf_page(remote_root, remote_child_right, vec![object(8, 4)]),
    );

    let mut reconciler = MerkleReconciler::new(local_root, remote_root, budget()).expect("cursor");
    let mut deltas = Vec::new();
    for _ in 0..8 {
        let result = reconciler
            .step(&mut local, &mut remote, budget())
            .expect("step");
        deltas.extend(result.deltas);
        if result.next.is_none() {
            break;
        }
    }
    assert_eq!(
        deltas
            .iter()
            .map(|delta| delta.key.clone())
            .collect::<Vec<_>>(),
        vec![vec![1], vec![8]]
    );
    assert!(deltas[0].local.is_some() && deltas[0].remote.is_some());
    assert!(deltas[1].local.is_none() && deltas[1].remote.is_some());
}

#[test]
fn continued_branch_page_requires_an_explicit_boundary() {
    let root = MerkleRoot::new(1, digest(40));
    let page = MerklePage {
        root,
        node: root.digest(),
        level: 1,
        cursor: PageCursor::origin(),
        next: Some(PageCursor { offset: 1 }),
        body: MerklePageBody::Branch(vec![MerkleChild {
            first_key: vec![1],
            end_key: None,
            digest: digest(41),
            level: 0,
            row_count: 1,
        }]),
    };
    assert_eq!(page.validate(4, 32), Err(ReplicationError::InvalidWire));
}
