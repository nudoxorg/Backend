//! Adversarial graph extension tests.

#![allow(
    clippy::expect_used,
    reason = "test fixtures use expect to make invariant failures local"
)]

use super::*;
use backend_version::{
    Coverage, CoverageWitness, RelationState, ScopeRoot, WorkspaceManifest, WorkspaceRoot,
};

fn workspace() -> WorkspaceRoot {
    WorkspaceManifest::new(
        1,
        Vec::new(),
        Vec::new(),
        [1; 32],
        complete_coverage(ScopeRoot::from_bytes([3; 32])).expect("complete scope"),
    )
    .expect("valid workspace")
    .root()
}

fn binding(rows: &[GraphRow]) -> (Binding, CoverageWitness) {
    let coverage = complete_coverage(ScopeRoot::from_bytes([7; 32])).expect("coverage");
    let entries = rows
        .iter()
        .map(|row| (row.key, row.values.clone()))
        .collect::<Vec<_>>();
    let state = RelationState::<SemanticRelation>::from_entries(entries, coverage).expect("state");
    let reads = vec![Read::new("semantic".into(), "name".into())];
    let query = Query::new(reads, Limits::default()).expect("query");
    (
        Binding::new(
            workspace(),
            state.root(),
            Recipe::from_value(&[1; 32]),
            Authority::from_value(&[2; 32]),
            query.read_manifest,
        ),
        coverage,
    )
}

#[test]
fn stale_root_is_rejected_before_graph_state_admission() {
    let (binding, coverage) = binding(&[GraphRow {
        key: 1,
        values: vec!["alpha".into()],
    }]);
    let input = QueryInput {
        root: binding.root,
        recipe: binding.recipe,
        authority: binding.authority,
        reads: vec![Read::new("semantic".into(), "name".into())],
        rows: vec![vec!["alpha".into()]],
    };
    let projection = execute(&input).expect("projection");
    let stale =
        RelationState::<SemanticRelation>::from_entries([(1, vec!["beta".into()])], coverage)
            .expect("stale state")
            .root();
    assert_eq!(projection.root, binding.root);
    assert_ne!(projection.root, stale);
    let mut stale_binding = binding;
    stale_binding.root = stale;
    assert_eq!(
        GraphState::new(
            stale_binding,
            coverage,
            vec![GraphRow {
                key: 1,
                values: vec!["alpha".into()],
            }],
            Limits::default(),
        ),
        Err(Error::StaleRoot)
    );
}

#[test]
fn delete_and_readd_are_exact_transitions() {
    let initial = [GraphRow {
        key: 1,
        values: vec!["alpha".into()],
    }];
    let (binding, coverage) = binding(&initial);
    let state =
        GraphState::new(binding, coverage, initial.to_vec(), Limits::default()).expect("state");
    let deletion = state
        .prepare_delta(vec![GraphChange::Delete { key: 1 }])
        .expect("delete");
    let empty = state.apply_delta(&deletion).expect("apply delete");
    let addition = empty
        .prepare_delta(vec![GraphChange::Upsert {
            key: 1,
            values: vec!["beta".into()],
        }])
        .expect("readd");
    let restored = empty.apply_delta(&addition).expect("apply readd");
    assert_eq!(restored.iter().count(), 1);
    assert_ne!(deletion.delta.id(), addition.delta.id());
}

#[test]
fn delta_frontier_is_bound_to_the_state() {
    let initial = [GraphRow {
        key: 1,
        values: vec!["alpha".into()],
    }];
    let (binding, coverage) = binding(&initial);
    let state =
        GraphState::new(binding, coverage, initial.to_vec(), Limits::default()).expect("state");
    let mut delta = state
        .prepare_delta(vec![GraphChange::Delete { key: 1 }])
        .expect("delta");
    delta.binding = delta.binding.with_frontier(Frontier::from_value(&[9; 32]));
    assert_eq!(state.apply_delta(&delta), Err(Error::StaleRoot));
}

#[test]
fn delta_bytes_are_bounded_across_the_whole_change() {
    let (binding, coverage) = binding(&[]);
    let limits = Limits {
        max_total_bytes: 5,
        ..Limits::default()
    };
    let state = GraphState::new(binding, coverage, Vec::new(), limits).expect("empty state");
    assert_eq!(
        state.prepare_delta(vec![
            GraphChange::Upsert {
                key: 1,
                values: vec!["abc".into()],
            },
            GraphChange::Upsert {
                key: 2,
                values: vec!["abc".into()],
            },
        ]),
        Err(Error::SizeLimit)
    );
}

#[test]
fn paging_and_cursor_binding_are_deterministic() {
    let rows = vec![
        GraphRow {
            key: 2,
            values: vec!["b".into()],
        },
        GraphRow {
            key: 1,
            values: vec!["a".into()],
        },
        GraphRow {
            key: 3,
            values: vec!["c".into()],
        },
    ];
    let (binding, coverage) = binding(&[]);
    let query = Query::new(
        vec![Read::new("semantic".into(), "name".into())],
        Limits::default(),
    )
    .expect("query");
    let source = MemorySource::new(
        binding,
        query.version,
        coverage,
        rows,
        Limits {
            max_page: 2,
            ..Limits::default()
        },
    )
    .expect("source");
    let adapter = Adapter::new(
        source,
        Limits {
            max_page: 2,
            ..Limits::default()
        },
    )
    .expect("adapter");
    let first = adapter
        .query(&QueryRequest {
            binding,
            query: query.clone(),
            cursor: None,
            limit: 2,
        })
        .expect("page");
    assert_eq!(
        first.rows.iter().map(|row| row.key).collect::<Vec<_>>(),
        vec![1, 2]
    );
    let cursor = first.next.expect("cursor");
    let other = Query::new(
        vec![Read::new("semantic".into(), "type".into())],
        Limits::default(),
    )
    .expect("other query");
    assert!(matches!(
        adapter.query(&QueryRequest {
            binding,
            query: other,
            cursor: Some(cursor),
            limit: 1,
        }),
        Err(AdapterError::Extension(Error::StaleCursor))
    ));
}

#[test]
fn malformed_reads_and_size_limits_are_rejected() {
    assert_eq!(
        Query::new(
            vec![Read::new(String::new(), "field".into())],
            Limits::default()
        ),
        Err(Error::UndeclaredRead)
    );
    assert_eq!(
        Limits {
            max_page: 0,
            ..Limits::default()
        }
        .validate(),
        Err(Error::InvalidLimits)
    );
    let (binding, coverage) = binding(&[]);
    assert_eq!(
        MemorySource::new(
            binding,
            QueryVersion::from_value(b"query".as_slice()),
            incomplete_coverage(1, Coverage::Partial),
            Vec::new(),
            Limits::default(),
        ),
        Err(Error::IncompleteCoverage)
    );
    assert_eq!(
        GraphState::new(
            binding,
            coverage,
            vec![GraphRow {
                key: 1,
                values: vec!["x".into(), "y".into()],
            }],
            Limits {
                max_fields_per_row: 1,
                ..Limits::default()
            },
        ),
        Err(Error::SizeLimit)
    );
}

struct SubstitutingSource {
    page: GraphPage,
}

impl GraphSource for SubstitutingSource {
    type Error = ();

    fn fetch(&self, _: &QueryRequest) -> Result<GraphPage, Self::Error> {
        Ok(self.page.clone())
    }
}

#[test]
fn provider_substitution_cannot_change_binding_or_coverage() {
    let (binding, coverage) = binding(&[]);
    let query = Query::new(
        vec![Read::new("semantic".into(), "name".into())],
        Limits::default(),
    )
    .expect("query");
    let mut wrong = binding;
    wrong.root =
        RelationState::<SemanticRelation>::from_entries([(1, vec!["other".into()])], coverage)
            .expect("wrong root")
            .root();
    let page = GraphPage {
        schema: SchemaVersion::CURRENT,
        binding: wrong,
        query: query.version,
        rows: vec![GraphRow {
            key: 1,
            values: vec!["alpha".into()],
        }],
        next: None,
        coverage,
    };
    let adapter = Adapter::new(SubstitutingSource { page }, Limits::default()).expect("adapter");
    let request = QueryRequest {
        binding,
        query,
        cursor: None,
        limit: 1,
    };
    assert!(matches!(
        adapter.query(&request),
        Err(AdapterError::Extension(Error::StaleRoot))
    ));

    let incomplete = GraphPage {
        schema: SchemaVersion::CURRENT,
        binding,
        query: request.query.version,
        rows: vec![GraphRow {
            key: 1,
            values: vec!["alpha".into()],
        }],
        next: None,
        coverage: incomplete_coverage(7, Coverage::Partial),
    };
    let adapter =
        Adapter::new(SubstitutingSource { page: incomplete }, Limits::default()).expect("adapter");
    assert!(matches!(
        adapter.query(&request),
        Err(AdapterError::Extension(Error::IncompleteCoverage))
    ));
}

#[test]
fn declared_read_budget_is_strictly_bounded() {
    let result = Query::new(
        vec![Read::new("contract".into(), "field".into())],
        Limits {
            max_total_bytes: 1,
            ..Limits::default()
        },
    );
    assert_eq!(result, Err(Error::SizeLimit));
}
