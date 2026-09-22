//! Behavioral laws for identities, roots, deltas, paging, and serialization.

use super::*;
use crate::{
    DependencyScope, GraphRelation, PackageDependencyRecord, ProductText, RegistryEcosystem, RowId,
    SemanticConfidence, SemanticGenerationId, SemanticLinkKind, ViewRoot, symbol_key,
    view_state_root,
};
use std::collections::BTreeSet;
use std::sync::Arc;

fn revision(seed: u8) -> RichGraphRevision {
    RichGraphRevision::new(
        [seed; 32],
        SemanticGenerationId::new([seed.saturating_add(1); 32]),
        [seed.saturating_add(2); 32],
    )
}

fn graph(revision: RichGraphRevision, extra: usize) -> RichGraphSnapshot {
    let center = GraphNodeId::for_symbol(symbol_key("pkg::centre"));
    let mut builder = RichGraphBuilder::new(revision, center);
    builder
        .add_node(
            RichGraphNode::new(
                center,
                "centre",
                Some("pkg::centre".to_owned()),
                Some("Function".to_owned()),
                GraphAvailability::Ready,
                None,
            )
            .expect("centre node"),
        )
        .expect("centre");
    for index in 0..extra {
        let symbol = symbol_key(&format!("pkg::target{index}"));
        let target = GraphNodeId::for_symbol(symbol);
        builder
            .add_node(
                RichGraphNode::new(
                    target,
                    format!("target{index}"),
                    None,
                    None,
                    GraphAvailability::Ready,
                    None,
                )
                .expect("target node"),
            )
            .expect("target");
        builder
            .add_edge(RichGraphEdge::new(
                center,
                target,
                GraphEdgeKind::Code {
                    relation: SemanticLinkKind::Calls,
                },
                GraphAvailability::Ready,
                GraphProvenance::semantic(
                    revision.semantic_generation,
                    revision.semantic_root,
                    [index as u8; 32],
                    SemanticConfidence::Compiler,
                ),
            ))
            .expect("edge");
    }
    builder.finish().expect("graph")
}

#[test]
fn ids_are_stable_and_relation_families_are_distinct() {
    let symbol = symbol_key("pkg::centre");
    assert_eq!(
        GraphNodeId::for_symbol(symbol),
        GraphNodeId::for_symbol(symbol)
    );
    let cargo_target =
        crate::PackageDependencyTarget::new(RegistryEcosystem::Cargo, "core", "*", None)
            .expect("cargo lineage");
    let npm_target = crate::PackageDependencyTarget::new(RegistryEcosystem::Npm, "core", "*", None)
        .expect("npm lineage");
    assert_ne!(
        GraphNodeId::for_unresolved_package(&cargo_target, GraphAuthority::RegistryMetadata),
        GraphNodeId::for_unresolved_package(&npm_target, GraphAuthority::RegistryMetadata)
    );
    assert_ne!(
        GraphNodeId::for_unresolved_package(&cargo_target, GraphAuthority::RegistryMetadata),
        GraphNodeId::for_unresolved_package(&cargo_target, GraphAuthority::LocalManifest)
    );
    let package = crate::PackageReference::parse("pkg:cargo/demo@1.0.0").expect("package");
    let local = crate::PackageReference::parse("demo").expect("local package");
    assert_ne!(
        GraphNodeId::for_package(&package),
        GraphNodeId::for_package(&local)
    );
    assert_ne!(
        GraphEdgeId::derive(
            GraphNodeId::from_bytes([1; 32]),
            GraphNodeId::from_bytes([2; 32]),
            GraphEdgeKind::Dependency {
                scope: DependencyScope::Runtime,
                optional: false,
            }
        ),
        GraphEdgeId::derive(
            GraphNodeId::from_bytes([1; 32]),
            GraphNodeId::from_bytes([2; 32]),
            GraphEdgeKind::Dependent {
                scope: DependencyScope::Runtime,
                optional: false,
            }
        )
    );
}

#[test]
fn graph_reconstruction_and_delta_are_inverse_laws() {
    let first = graph(revision(1), 3);
    let second = graph(revision(2), 4);
    let delta = first.diff(&second).expect("delta");
    assert_eq!(first.apply_delta(&delta).expect("apply"), second);
    assert_eq!(second.diff(&first).expect("reverse").target, first.revision);
    assert_eq!(first.apply_delta(&delta).expect("repeat source"), second);
}

#[test]
fn stale_roots_and_foreign_cursors_are_rejected() {
    let snapshot = graph(revision(4), 4);
    let center = snapshot.center;
    let request = RichGraphRequest::new(
        snapshot.revision,
        center,
        vec![GraphRelationFamily::Code],
        2,
    )
    .expect("request");
    let page = snapshot.page(&request).expect("first page");
    let GraphPageTerminal::More(cursor) = page.terminal else {
        panic!("expected a continuation");
    };
    let foreign = RichGraphRequest::new(revision(9), center, Vec::new(), 2)
        .expect("foreign")
        .with_cursor(cursor);
    assert_eq!(snapshot.page(&foreign), Err(RichGraphError::StaleRoot));
    let stale = RichGraphRequest::new(snapshot.revision, center, Vec::new(), 2)
        .expect("stale query")
        .with_cursor(RichGraphCursor {
            revision: snapshot.revision,
            recipe: [8; 32],
            ..cursor
        });
    assert_eq!(snapshot.page(&stale), Err(RichGraphError::CursorMismatch));
}

#[test]
fn cancellation_is_bounded_and_preserves_exact_revision() {
    let snapshot = graph(revision(8), 40);
    let request = RichGraphRequest::new(
        snapshot.revision,
        snapshot.center,
        Vec::new(),
        MAX_RICH_GRAPH_PAGE_ROWS,
    )
    .expect("request")
    .cancelled();
    let page = snapshot.page(&request).expect("cancel");
    assert_eq!(page.terminal, GraphPageTerminal::Cancelled);
    assert!(page.nodes.is_empty());
    assert_eq!(page.revision, snapshot.revision);
}

#[test]
fn high_fanout_graphs_remain_page_bounded() {
    let snapshot = graph(revision(10), 1_400);
    let cloned = snapshot.clone();
    assert!(Arc::ptr_eq(&snapshot.nodes, &cloned.nodes));
    assert!(Arc::ptr_eq(&snapshot.edges, &cloned.edges));
    let same_fanout = graph(revision(11), 1_400);
    assert_eq!(
        snapshot
            .nodes
            .iter()
            .map(|node| node.id)
            .collect::<Vec<_>>(),
        same_fanout
            .nodes
            .iter()
            .map(|node| node.id)
            .collect::<Vec<_>>()
    );
    assert_eq!(snapshot.diff(&same_fanout), Err(RichGraphError::DeltaBound));
    let mut request = RichGraphRequest::new(
        snapshot.revision,
        snapshot.center,
        vec![GraphRelationFamily::Code],
        37,
    )
    .expect("request");
    let mut seen = BTreeSet::new();
    loop {
        let page = snapshot.page(&request).expect("bounded page");
        assert!(page.nodes.len() <= 37);
        seen.extend(page.nodes.iter().map(|node| node.id));
        match page.terminal {
            GraphPageTerminal::Complete => break,
            GraphPageTerminal::More(cursor) => {
                request = request.clone().with_cursor(cursor);
            }
            GraphPageTerminal::Cancelled => panic!("unexpected cancellation"),
        }
    }
    assert_eq!(seen.len(), snapshot.nodes.len());
}

#[test]
fn package_edges_keep_requirement_and_reverse_dependent_direction() {
    let source = crate::PackageReference::parse("pkg:cargo/demo@1.0.0").expect("source");
    let target = crate::PackageDependencyTarget::new(RegistryEcosystem::Cargo, "serde", "^1", None)
        .expect("target");
    let record = PackageDependencyRecord::new(
        source.clone(),
        target,
        DependencyScope::Runtime,
        false,
        crate::DependencyEvidence {
            authority: crate::DependencyAuthority::RegistryMetadata,
            frontier: [1; 32],
            provenance: [2; 32],
        },
    );
    let dependency = RichGraphEdge::from_dependency(&record, GraphRelationFamily::Dependency);
    let dependent = RichGraphEdge::from_dependency(&record, GraphRelationFamily::Dependent);
    assert_eq!(
        dependency
            .dependency_requirement
            .as_ref()
            .map(ProductText::as_str),
        Some("^1")
    );
    assert_eq!(dependency.from, dependent.to);
    assert_eq!(dependency.to, dependent.from);
    assert!(dependency.provenance.semantic_root.is_none());
}

#[test]
fn route_serialization_keeps_generation_root_and_layout_seed() {
    let snapshot = graph(revision(12), 2);
    let encoded = serde_json::to_vec(&snapshot).expect("encode");
    let decoded: RichGraphSnapshot = serde_json::from_slice(&encoded).expect("decode");
    assert_eq!(decoded, snapshot);
    decoded.admit().expect("admitted roundtrip");
    assert_eq!(decoded.revision.semantic_root, [14; 32]);
    assert_eq!(decoded.layout.seed, snapshot.layout.seed);
}

#[test]
fn from_view_rejects_relation_endpoint_outside_root() {
    let root = ViewRoot::new_incomplete(
        crate::view_key(b"graph"),
        crate::Basis::with_context(
            view_state_root(&[]),
            crate::object_version(b"source"),
            crate::branch_key("branch"),
            crate::log_key("log"),
            1,
        ),
        crate::Frontier::new(
            crate::branch_key("branch"),
            crate::log_key("log"),
            1,
            view_state_root(&[]),
            0,
        ),
        vec![],
        vec![],
    )
    .expect("root");
    let centre = symbol_key("pkg::centre");
    let target = symbol_key("pkg::target");
    let result = RichGraphSnapshot::from_view(
        &root,
        RowId::Symbol(centre),
        &[GraphRelation::new(
            RowId::Symbol(centre),
            RowId::Symbol(target),
            SemanticLinkKind::Calls,
        )],
        revision(1),
    );
    assert!(result.is_err());
}
