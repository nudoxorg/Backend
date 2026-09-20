//! Adversarial graph extension tests.

#![allow(
    clippy::expect_used,
    reason = "test fixtures use expect to make invariant failures local"
)]

use super::*;
use backend_version::{
    AuthorityScopeClaim, Coverage, CoverageWitness, ProducerObservationVerifier, RelationState,
    ScopeRoot, UntrustedProducerObservation, WorkspaceManifest, WorkspaceRoot,
    admit_complete_scope, admit_producer_observation,
};

struct FixtureCoverageVerifier {
    producer: [u8; 32],
    scope: ScopeRoot,
    context: [u8; 32],
    evidence: Vec<u8>,
}

impl ProducerObservationVerifier for FixtureCoverageVerifier {
    type Error = ();

    fn verify(&self, observation: &UntrustedProducerObservation) -> Result<(), Self::Error> {
        (observation.producer_identity() == self.producer
            && observation.scope_root() == self.scope
            && observation.context() == self.context
            && observation.evidence() == self.evidence.as_slice())
        .then_some(())
        .ok_or(())
    }
}

fn authorized_coverage(bytes: &[u8; 32]) -> CoverageWitness {
    let authority = Authority::from_value(bytes);
    let declaration = AuthorityScopeClaim::from_object_version(authority);
    let verifier = FixtureCoverageVerifier {
        producer: [9; 32],
        scope: declaration.scope_root(),
        context: [4; 32],
        evidence: vec![1, 2, 3],
    };
    let observation = UntrustedProducerObservation::new(
        verifier.producer,
        declaration.scope_root(),
        verifier.context,
        verifier.evidence.clone(),
    );
    let admitted = admit_producer_observation(observation, &verifier).expect("admitted producer");
    CoverageWitness::Complete(admit_complete_scope(declaration, admitted).expect("scope match"))
}

fn forged_complete_coverage() -> CoverageWitness {
    let manifest = WorkspaceManifest::from_versions(
        1,
        Vec::new(),
        Vec::new(),
        Authority::from_value(&[7; 32]),
        authorized_coverage(&[7; 32]),
    )
    .expect("complete manifest");
    WorkspaceManifest::decode_untrusted(&manifest.encode())
        .expect("decode manifest")
        .coverage()
}

fn workspace() -> WorkspaceRoot {
    WorkspaceManifest::from_versions(
        1,
        Vec::new(),
        Vec::new(),
        Authority::from_value(&[1; 32]),
        authorized_coverage(&[3; 32]),
    )
    .expect("valid workspace")
    .root()
}

fn binding(rows: &[GraphRow]) -> (Binding, CoverageWitness) {
    let coverage = authorized_coverage(&[7; 32]);
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
fn forged_complete_coverage_is_rejected_at_provider_boundary() {
    let (binding, _) = binding(&[]);
    let forged = forged_complete_coverage();
    assert_eq!(
        MemorySource::new(
            binding,
            QueryVersion::from_value(b"query".as_slice()),
            forged,
            Vec::new(),
            Limits::default(),
        ),
        Err(Error::IncompleteCoverage)
    );
}

#[test]
fn wrong_producer_is_rejected_before_complete_admission() {
    let authority = Authority::from_value(&[7; 32]);
    let declaration = AuthorityScopeClaim::from_object_version(authority);
    let verifier = FixtureCoverageVerifier {
        producer: [9; 32],
        scope: declaration.scope_root(),
        context: [4; 32],
        evidence: vec![1, 2, 3],
    };
    let observation = UntrustedProducerObservation::new(
        [8; 32],
        declaration.scope_root(),
        verifier.context,
        verifier.evidence.clone(),
    );
    assert!(admit_producer_observation(observation, &verifier).is_err());
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

#[test]
fn graph_arrangement_reuses_base_and_keeps_query_manifest_bound() {
    let initial = vec![
        GraphRow {
            key: 1,
            values: vec!["alpha".into()],
        },
        GraphRow {
            key: 2,
            values: vec!["beta".into()],
        },
    ];
    let (binding, coverage) = binding(&initial);
    let state = GraphState::new(binding, coverage, initial, Limits::default()).expect("state");
    let arrangement = GraphArrangement::from_state(&state).expect("arrangement");
    let query = Query::new(
        vec![Read::new("semantic".into(), "name".into())],
        Limits::default(),
    )
    .expect("query");
    let plan = QueryPlan::new(binding, coverage, query.clone()).expect("plan");
    assert_eq!(
        arrangement
            .execute(&plan)
            .expect("execute")
            .iter()
            .map(|row| row.key)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let delta = state
        .prepare_delta(vec![GraphChange::Upsert {
            key: 1,
            values: vec!["gamma".into()],
        }])
        .expect("delta");
    let outcome = arrangement
        .advance(&delta, ArrangementLimits::default())
        .expect("advance");
    let advanced = match outcome {
        RefreshOutcome::Advanced(advanced) => advanced,
        other => {
            assert!(matches!(other, RefreshOutcome::Advanced(_)));
            return;
        }
    };
    assert!(std::ptr::eq(arrangement.base(), advanced.base()));
    let target_plan = QueryPlan::new(delta.binding, delta.coverage, query).expect("target plan");
    assert_eq!(
        advanced.execute(&target_plan).expect("target execute")[0].values,
        vec!["gamma"]
    );
}

#[test]
fn graph_arrangement_pages_merge_replacements_and_deletions_once() {
    let initial = vec![
        GraphRow {
            key: 1,
            values: vec!["alpha".into()],
        },
        GraphRow {
            key: 2,
            values: vec!["beta".into()],
        },
        GraphRow {
            key: 3,
            values: vec!["gamma".into()],
        },
    ];
    let (binding, coverage) = binding(&initial);
    let state = GraphState::new(binding, coverage, initial, Limits::default()).expect("state");
    let arrangement = GraphArrangement::from_state(&state).expect("arrangement");
    let delta = state
        .prepare_delta(vec![
            GraphChange::Upsert {
                key: 2,
                values: vec!["replaced".into()],
            },
            GraphChange::Delete { key: 3 },
            GraphChange::Upsert {
                key: 4,
                values: vec!["added".into()],
            },
        ])
        .expect("delta");
    let advanced = match arrangement
        .advance(&delta, ArrangementLimits::default())
        .expect("advance")
    {
        RefreshOutcome::Advanced(value) => value,
        other => {
            assert!(matches!(other, RefreshOutcome::Advanced(_)));
            return;
        }
    };
    let query = Query::new(
        vec![Read::new("semantic".into(), "name".into())],
        Limits::default(),
    )
    .expect("query");
    let plan = QueryPlan::new(delta.binding, delta.coverage, query).expect("plan");
    let mut cursor = None;
    let mut keys = Vec::new();
    loop {
        let page = advanced
            .page(&plan, cursor, 1, Limits::default())
            .expect("page");
        keys.extend(page.rows.iter().map(|row| row.key));
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(keys, vec![1, 2, 4]);
}

#[test]
fn graph_arrangement_delete_readd_matches_a_restarted_base() {
    let initial = vec![
        GraphRow {
            key: 1,
            values: vec!["alpha".into()],
        },
        GraphRow {
            key: 2,
            values: vec!["beta".into()],
        },
    ];
    let (binding, coverage) = binding(&initial);
    let state = GraphState::new(binding, coverage, initial, Limits::default()).expect("state");
    let arrangement = GraphArrangement::from_state(&state).expect("base");

    let replacement = state
        .prepare_delta(vec![GraphChange::Upsert {
            key: 1,
            values: vec!["gamma".into()],
        }])
        .expect("replacement delta");
    let replaced_state = state.apply_delta(&replacement).expect("replaced state");
    let outcome = arrangement
        .advance(&replacement, ArrangementLimits::default())
        .expect("advance replacement");
    assert!(matches!(&outcome, RefreshOutcome::Advanced(_)));
    let RefreshOutcome::Advanced(arrangement) = outcome else {
        return;
    };

    let deletion = replaced_state
        .prepare_delta(vec![GraphChange::Delete { key: 1 }])
        .expect("delete delta");
    let deleted_state = replaced_state
        .apply_delta(&deletion)
        .expect("deleted state");
    let outcome = arrangement
        .advance(&deletion, ArrangementLimits::default())
        .expect("advance deletion");
    assert!(matches!(&outcome, RefreshOutcome::Advanced(_)));
    let RefreshOutcome::Advanced(arrangement) = outcome else {
        return;
    };

    let readd = deleted_state
        .prepare_delta(vec![GraphChange::Upsert {
            key: 1,
            values: vec!["delta".into()],
        }])
        .expect("readd delta");
    let restored_state = deleted_state.apply_delta(&readd).expect("restored state");
    let outcome = arrangement
        .advance(&readd, ArrangementLimits::default())
        .expect("advance readd");
    assert!(matches!(&outcome, RefreshOutcome::Advanced(_)));
    let RefreshOutcome::Advanced(arrangement) = outcome else {
        return;
    };

    let query = Query::new(
        vec![Read::new("semantic".into(), "name".into())],
        Limits::default(),
    )
    .expect("query");
    let plan = QueryPlan::new(restored_state.binding(), coverage, query.clone()).expect("plan");
    let incremental = arrangement.execute(&plan).expect("incremental query");

    let restarted_state = GraphState::new(
        restored_state.binding(),
        restored_state.coverage(),
        restored_state.iter().collect(),
        Limits::default(),
    )
    .expect("restarted state");
    let restarted = GraphArrangement::from_state(&restarted_state)
        .expect("restarted base")
        .execute(
            &QueryPlan::new(restarted_state.binding(), restarted_state.coverage(), query)
                .expect("restarted plan"),
        )
        .expect("restarted query");
    assert_eq!(incremental, restarted);
    assert_eq!(arrangement.binding(), restarted_state.binding());
}

#[test]
fn graph_arrangement_rejects_stale_plan_and_bounds_rebuild() {
    let initial = vec![GraphRow {
        key: 1,
        values: vec!["alpha".into()],
    }];
    let (binding, coverage) = binding(&initial);
    let state = GraphState::new(binding, coverage, initial, Limits::default()).expect("state");
    let arrangement = GraphArrangement::from_state(&state).expect("arrangement");
    let query = Query::new(
        vec![Read::new("semantic".into(), "name".into())],
        Limits::default(),
    )
    .expect("query");
    let mut stale_binding = binding;
    stale_binding.root =
        RelationState::<SemanticRelation>::from_entries([(1, vec!["other".into()])], coverage)
            .expect("stale")
            .root();
    assert_eq!(
        arrangement.execute(&QueryPlan::new(stale_binding, coverage, query).expect("plan")),
        Err(Error::StaleRoot)
    );
    let delta = state
        .prepare_delta(vec![GraphChange::Upsert {
            key: 1,
            values: vec!["beta".into()],
        }])
        .expect("delta");
    let outcome = arrangement
        .advance(
            &delta,
            ArrangementLimits {
                max_changed_rows: 1,
                max_bytes: 1,
                ..ArrangementLimits::default()
            },
        )
        .expect("plan");
    assert!(matches!(outcome, RefreshOutcome::RebuildRequired(_)));
}
