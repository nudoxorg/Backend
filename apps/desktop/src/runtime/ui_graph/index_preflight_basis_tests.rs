//! Saved claim authority checks, independent of transport attachment admission.
#![allow(clippy::expect_used)]

use super::{IndexPreflightRefusal, index_preflight_basis};
use crate::core::{LocalProjectId, VersionedRoot};
use crate::model::{AppSnapshot, IndexOperationClaim, WorkspaceProject};
use backend_library::{
    CURSOR_SCHEMA, Cursor, Frontier, branch_key, log_key, view_key, view_state_root,
    view_version,
};
use std::sync::Arc;

fn saved_request(current: VersionedRoot) -> (AppSnapshot, LocalProjectId, IndexOperationClaim) {
    let project = LocalProjectId::new("/tmp/nudox-saved-preflight-project")
        .expect("native project identity");
    let allocated = crate::model::index_operation::tests::claim(&project, 1);
    let operation: IndexOperationClaim = serde_json::from_slice(
        &serde_json::to_vec(&allocated).expect("saved caller payload"),
    ).expect("restored caller payload");
    assert_eq!(operation, allocated);
    let mut row = WorkspaceProject::indexing_with_id(project.clone());
    row.operation = Some(operation.clone());
    let snapshot = AppSnapshot::empty(current);
    let mut workspace = snapshot.workspace().clone();
    workspace.projects = Arc::from([row]);
    (snapshot.with_workspace(workspace), project, operation)
}

// Content/stream comparison fixtures only; they do not certify owner authority.
// The startup acceptance test below uses the checked owner publication fixture.
fn stream_cursor(sequence: u64, rows: &[(&str, &str)]) -> Cursor {
    let entries: Vec<(String, String)> = rows.iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    let root = view_state_root(&entries);
    Cursor::for_view(
        view_key(b"owned-index-view"),
        view_version(root.as_bytes()),
        Frontier::new(
            branch_key("main"), log_key("owned-index-log"), CURSOR_SCHEMA,
            root, sequence,
        ),
    )
}

#[test]
fn saved_unserved_claim_rebases_to_first_checked_owner_publication() {
    let view = crate::runtime::owner::publication_tests::view();
    let current = VersionedRoot::from_revision(1, Cursor::for_view_root_at(&view, 0), 2);
    let (snapshot, project, operation) = saved_request(current);
    let basis = VersionedRoot::unserved();
    assert!(!current.same_authority(basis));
    assert_eq!(index_preflight_basis(&snapshot, &project, &operation, basis), Ok(current));
    // Observation turns are not a new authority or a different placeholder.
    let observed_placeholder = VersionedRoot::from_revision(1, Cursor::new(), 17);
    assert!(observed_placeholder.is_unserved());
    assert_eq!(index_preflight_basis(&snapshot, &project, &operation, observed_placeholder), Ok(current));
}

#[test]
fn saved_zero_sequence_claim_rejects_each_replaced_producer_stream() {
    let old = stream_cursor(0, &[("src/lib.rs", "pub fn before() {}")]);
    let basis = VersionedRoot::from_revision(3, old, 0);
    assert!(!basis.is_unserved());
    let next = stream_cursor(1, &[("src/lib.rs", "pub fn after() {}")]);
    assert_ne!(old.root(), next.root());
    let variants = [
        ("epoch", VersionedRoot::from_revision(4, next, 1)),
        ("recipe", VersionedRoot::from_revision(3, Cursor::for_view(
            view_key(b"replacement-view"), next.version(),
            Frontier::new(next.branch(), next.log(), next.schema(), next.root(), next.sequence()),
        ), 1)),
        ("branch", VersionedRoot::from_revision(3, Cursor::for_view(
            next.recipe(), next.version(),
            Frontier::new(branch_key("replacement-branch"), next.log(), next.schema(), next.root(), next.sequence()),
        ), 1)),
        ("log", VersionedRoot::from_revision(3, Cursor::for_view(
            next.recipe(), next.version(),
            Frontier::new(next.branch(), log_key("replacement-log"), next.schema(), next.root(), next.sequence()),
        ), 1)),
        ("schema", VersionedRoot::from_revision(3, Cursor::for_view(
            next.recipe(), next.version(),
            Frontier::new(next.branch(), next.log(), next.schema() + 1, next.root(), next.sequence()),
        ), 1)),
    ];
    for (field, current) in variants {
        let (snapshot, project, operation) = saved_request(current);
        assert_eq!(index_preflight_basis(&snapshot, &project, &operation, basis),
            Err(IndexPreflightRefusal::ProducerStreamChanged), "changed {field}");
    }
}

#[test]
fn saved_empty_owned_root_does_not_become_a_launch_placeholder() {
    let basis = VersionedRoot::from_revision(1, stream_cursor(0, &[]), 0);
    assert_eq!(basis.root(), VersionedRoot::unserved().root());
    assert!(!basis.is_unserved());
    let current = VersionedRoot::from_revision(2,
        stream_cursor(1, &[("src/lib.rs", "pub fn published() {}")]), 1);
    let (snapshot, project, operation) = saved_request(current);
    assert_eq!(index_preflight_basis(&snapshot, &project, &operation, basis),
        Err(IndexPreflightRefusal::ProducerStreamChanged));
}

#[test]
fn saved_synthetic_zero_sequence_root_cannot_cross_owner_epochs() {
    let entries = [("src/lib.rs".to_owned(), "pub fn fixture() {}".to_owned())];
    let basis = VersionedRoot::synthetic(view_state_root(&entries), 1);
    assert!(!basis.is_unserved());
    let current = VersionedRoot::from_revision(2, Cursor::at(basis.root(), 1), 1);
    let (snapshot, project, operation) = saved_request(current);
    assert_eq!(index_preflight_basis(&snapshot, &project, &operation, basis),
        Err(IndexPreflightRefusal::ProducerStreamChanged));
}

#[test]
fn saved_zero_sequence_claim_retains_same_stream_publication_and_identity() {
    let basis = VersionedRoot::from_revision(3,
        stream_cursor(0, &[("src/lib.rs", "pub fn before() {}")]), 0);
    let current = VersionedRoot::from_revision(3,
        stream_cursor(1, &[("src/lib.rs", "pub fn published() {}")]), 1);
    let (snapshot, project, operation) = saved_request(current);
    assert_eq!(index_preflight_basis(&snapshot, &project, &operation, basis), Ok(current));
    assert_eq!(snapshot.workspace().projects[0].operation.as_ref(), Some(&operation));
    assert_eq!(index_preflight_basis(&snapshot, &project, &operation, current), Ok(current));
}

#[test]
fn saved_zero_sequence_claim_rejects_same_sequence_conflicting_root() {
    let basis = VersionedRoot::from_revision(3,
        stream_cursor(0, &[("src/lib.rs", "pub fn before() {}")]), 0);
    let current = VersionedRoot::from_revision(3,
        stream_cursor(0, &[("src/lib.rs", "pub fn conflicting() {}")]), 1);
    assert_ne!(basis.root(), current.root());
    let (snapshot, project, operation) = saved_request(current);
    assert_eq!(index_preflight_basis(&snapshot, &project, &operation, basis),
        Err(IndexPreflightRefusal::AuthorityRegressed));
}

#[test]
fn unserved_bootstrap_does_not_bypass_saved_claim_identity() {
    let current = VersionedRoot::from_revision(1,
        stream_cursor(1, &[("src/lib.rs", "pub fn published() {}")]), 1);
    let (snapshot, project, operation) = saved_request(current);
    let replacement = crate::model::index_operation::tests::claim(&project, 2);
    let mut workspace = snapshot.workspace().clone();
    let mut row = workspace.projects[0].clone();
    row.operation = Some(replacement);
    workspace.projects = Arc::from([row]);
    assert_eq!(index_preflight_basis(&snapshot.with_workspace(workspace), &project,
        &operation, VersionedRoot::unserved()), Err(IndexPreflightRefusal::ClaimChanged));
}
