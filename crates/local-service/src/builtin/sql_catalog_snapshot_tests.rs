//! End-to-end checks of authenticated but semantically inconsistent SQL rows.
//! These fixtures intentionally bypass the SQL owner's cross-row admission,
//! while retaining the canonical relation, object and workspace checks.
use super::*;
use backend_engine::WorkspaceHead;
use backend_engine::builtin::{
    SqlAnalysisRelation, SqlAttemptOutcome, sql_analysis_root_object, sql_attempt_key,
    sql_fact_key, sql_host_fact_digest, sql_host_key, sql_inventory_digest, sql_selection_digest,
};
use backend_semantic::sql_analysis::{SqlCoverage, SqlDiagnostic, SqliteLineageCoverage};
use backend_store::{ClosureManifest, FileStore, TypedObject, WorkspaceClosure};
use backend_version::{CoverageWitness, RelationState, ScopeRoot, UntrustedCoverageScope};
use std::sync::Arc;

#[derive(Clone, Copy)]
enum Defect {
    None,
    InventoryDigest,
    HostDigest,
    MissingFact,
    AttemptBinding,
}

fn fixture(defect: Defect) -> (tempfile::TempDir, WorkspaceSnapshot, [u8; 32]) {
    let project = [1; 32];
    let host_key = sql_host_key(project, "a.sql").unwrap();
    let fact_key = sql_fact_key(
        host_key,
        SqlDialect::Sqlite,
        [2; 32],
        [3; 32],
        [4; 32],
        [5; 32],
        Some([6; 32]),
        0,
    );
    let fact = SqlAnalysisRecord::Fact {
        host: host_key,
        ordinal: 0,
        fact: SqlFact::Diagnostic {
            code: SqlDiagnostic::Other,
            extent: None,
        },
    };
    let mut host = SqlAnalysisRecord::Host {
        project,
        path: "a.sql".into(),
        dialect: SqlDialect::Sqlite,
        input: Some([2; 32]),
        source: Some([3; 32]),
        length: Some(8),
        producer: Some([4; 32]),
        settings: Some([5; 32]),
        context: Some([6; 32]),
        terminal: SqlHostTerminal::Parsed {
            source: SqlCoverage::Unavailable,
            contextual: SqlCoverage::Partial,
            sqlite_lineage: Some(SqliteLineageCoverage::NativeComplete),
        },
        fact_count: 1,
        fact_digest: sql_host_fact_digest(&[(fact_key, &fact)]).unwrap(),
    };
    if let SqlAnalysisRecord::Host { fact_digest, .. } = &mut host {
        if matches!(defect, Defect::HostDigest) {
            *fact_digest = [42; 32];
        }
    }
    let mut selection = SqlAnalysisRecord::Selection {
        project,
        source_root: "/fixture".into(),
        config_input: [7; 32],
        config_source: [8; 32],
        config_length: 32,
        discovery: None,
        policy_complete: false,
        implicit: false,
        gaps: Default::default(),
        inventory: SqlInventory::Incomplete,
        inventory_digest: sql_inventory_digest(&[("a.sql".into(), SqlDialect::Sqlite)]).unwrap(),
        host_count: 1,
    };
    if let SqlAnalysisRecord::Selection {
        inventory_digest, ..
    } = &mut selection
    {
        if matches!(defect, Defect::InventoryDigest) {
            *inventory_digest = [43; 32];
        }
    }
    let mut selected = sql_selection_digest(project, &selection).unwrap();
    if matches!(defect, Defect::AttemptBinding) {
        selected = [44; 32];
    }
    let attempt = SqlAnalysisRecord::Attempt {
        project,
        outcome: SqlAttemptOutcome::Observed,
        served_selection: Some(selected),
    };
    let mut rows = vec![
        (sql_selection_key(project), selection),
        (sql_attempt_key(project), attempt),
        (host_key, host),
    ];
    if !matches!(defect, Defect::MissingFact) {
        rows.push((fact_key, fact));
    }
    rows.sort_by_key(|row| row.0);
    let coverage = CoverageWitness::Partial(UntrustedCoverageScope::from_scope_root(
        ScopeRoot::from_u64(1),
    ));
    let state = RelationState::<SqlAnalysisRelation>::from_entries(rows, coverage).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let registry = super::super::product_relation_registry().unwrap();
    let store =
        FileStore::open_with_registry(directory.path(), 8 * 1024 * 1024, registry.clone()).unwrap();
    store.write_relation_state(&state).unwrap();
    let base = super::super::genesis().unwrap();
    let base_snapshot = base.snapshot();
    let manifest = base_snapshot.manifest().clone();
    let mut objects = base_snapshot
        .closure()
        .control_manifest()
        .objects()
        .to_vec();
    objects.push(TypedObject::from_relation_state(&state).unwrap());
    objects.push(sql_analysis_root_object(state.root()));
    objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
    let objects = ClosureManifest::new_root_only_with_registry(objects, &registry).unwrap();
    let closure = WorkspaceClosure::from_checked_manifest_root_only_with_registry(
        &manifest, objects, &registry,
    )
    .unwrap();
    let head = WorkspaceHead::genesis_with_registry(manifest, closure, &registry).unwrap();
    (directory, head.snapshot_in(Arc::new(store)), project)
}

#[test]
fn authenticated_sql_snapshot_rejects_cross_row_tampering() {
    for (defect, expected) in [
        (
            Defect::InventoryDigest,
            "SQL selected inventory digest mismatch",
        ),
        (Defect::HostDigest, "SQL host fact digest mismatch"),
        (Defect::MissingFact, "SQL digest row missing"),
        (
            Defect::AttemptBinding,
            "SQL attempt does not bind selected analysis",
        ),
    ] {
        let (_directory, snapshot, project) = fixture(defect);
        let result = load_projects_bounded(
            &snapshot,
            std::iter::once(&project),
            &|| false,
            64 * 1024 * 1024,
            64,
        );
        assert!(
            matches!(result, Err(SqlCatalogError::Reason(reason)) if reason == expected),
            "expected {expected}"
        );
    }
}

#[test]
fn authenticated_sql_snapshot_accepts_matching_cross_row_bindings() {
    let (_directory, snapshot, project) = fixture(Defect::None);
    let (catalog, _) = load_projects_bounded(
        &snapshot,
        std::iter::once(&project),
        &|| false,
        64 * 1024 * 1024,
        64,
    )
    .unwrap();
    let selected = catalog.get(&project).expect("selected project");
    assert_eq!(selected.hosts.iter().count(), 1);
    assert_eq!(selected.hosts[0].path, "a.sql");
}
