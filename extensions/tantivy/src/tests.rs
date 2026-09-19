//! Adversarial lexical extension tests.

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

struct SubstitutingSource {
    page: LexicalPage,
}

impl LexicalSource for SubstitutingSource {
    type Error = ();

    fn fetch(&self, _: &QueryRequest) -> Result<LexicalPage, Self::Error> {
        Ok(self.page.clone())
    }
}

#[test]
fn provider_substitution_cannot_change_binding_or_coverage() {
    let (binding, coverage) = binding(&[]);
    let query = Query::new(vec!["alpha".into()], Limits::default()).expect("query");
    let mut wrong = binding;
    wrong.root = RelationState::<IndexRelation>::from_entries(
        [(1, vec![("body".into(), "other".into())])],
        coverage,
    )
    .expect("wrong root")
    .root();
    let page = LexicalPage {
        schema: SchemaVersion::CURRENT,
        binding: wrong,
        query: query.version,
        ids: vec![1],
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

    let incomplete = LexicalPage {
        schema: SchemaVersion::CURRENT,
        binding,
        query: request.query.version,
        ids: vec![1],
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
fn provider_page_cannot_exceed_requested_bound() {
    let (binding, coverage) = binding(&[]);
    let query = Query::new(vec!["alpha".into()], Limits::default()).expect("query");
    let page = LexicalPage {
        schema: SchemaVersion::CURRENT,
        binding,
        query: query.version,
        ids: vec![1, 2],
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
        Err(AdapterError::Extension(Error::SizeLimit))
    ));
}

fn binding(documents: &[(u64, Vec<(String, String)>)]) -> (Binding, CoverageWitness) {
    let coverage = authorized_coverage(&[7; 32]);
    let state = RelationState::<IndexRelation>::from_entries(documents.iter().cloned(), coverage)
        .expect("state");
    (
        Binding::new(
            workspace(),
            state.root(),
            Recipe::from_value(&[1; 32]),
            Authority::from_value(&[2; 32]),
            ReadManifest::from_value(b"reads".as_slice()),
        ),
        coverage,
    )
}

#[test]
fn stale_root_is_rejected() {
    let (binding, coverage) = binding(&[(1, vec![("body".into(), "alpha".into())])]);
    let materialized = materialize(
        binding.root,
        binding.recipe,
        binding.authority,
        coverage,
        vec![DocumentChange::Add {
            id: 1,
            fields: vec![("body".into(), "alpha".into())],
        }],
    )
    .expect("materialization");
    let stale = RelationState::<IndexRelation>::from_entries(
        [(1, vec![("body".into(), "beta".into())])],
        coverage,
    )
    .expect("stale state")
    .root();
    assert_eq!(
        query(&materialized, stale, &["alpha"]),
        Err(Error::StaleRoot)
    );
}

#[test]
fn delete_and_readd_preserve_exact_delta_roots() {
    let (binding, coverage) = binding(&[(1, vec![("body".into(), "alpha".into())])]);
    let state = DocumentState::new(
        binding,
        coverage,
        vec![(1, vec![("body".into(), "alpha".into())])],
        Limits::default(),
    )
    .expect("state");
    let deleted = state
        .prepare_delta(vec![DocumentChange::Delete { id: 1 }])
        .expect("delete");
    let empty = state.apply_delta(&deleted).expect("apply delete");
    let added = empty
        .prepare_delta(vec![DocumentChange::Add {
            id: 1,
            fields: vec![("body".into(), "beta".into())],
        }])
        .expect("readd");
    let restored = empty.apply_delta(&added).expect("apply readd");
    assert_eq!(restored.iter().count(), 1);
    assert_ne!(deleted.delta.id(), added.delta.id());
    assert_ne!(deleted.binding.root, added.binding.root);
}

#[test]
fn delta_frontier_is_bound_to_the_state() {
    let (binding, coverage) = binding(&[(1, vec![("body".into(), "alpha".into())])]);
    let state = DocumentState::new(
        binding,
        coverage,
        vec![(1, vec![("body".into(), "alpha".into())])],
        Limits::default(),
    )
    .expect("state");
    let mut delta = state
        .prepare_delta(vec![DocumentChange::Delete { id: 1 }])
        .expect("delta");
    delta.binding = delta.binding.with_frontier(Frontier::from_value(&[9; 32]));
    assert_eq!(state.apply_delta(&delta), Err(Error::StaleRoot));
}

#[test]
fn delta_field_bytes_are_bounded_across_the_whole_change() {
    let (binding, coverage) = binding(&[]);
    let limits = Limits {
        max_total_text_bytes: 6,
        ..Limits::default()
    };
    let state = DocumentState::new(binding, coverage, Vec::new(), limits).expect("empty state");
    assert_eq!(
        state.prepare_delta(vec![
            DocumentChange::Add {
                id: 1,
                fields: vec![("a".into(), "abc".into())],
            },
            DocumentChange::Add {
                id: 2,
                fields: vec![("b".into(), "abc".into())],
            },
        ]),
        Err(Error::SizeLimit)
    );
}

#[test]
fn cursor_is_bound_to_root_and_terms() {
    let (binding, coverage) = binding(&[]);
    let query = Query::new(vec!["alpha".into()], Limits::default()).expect("query");
    let source = MemorySource::new(
        binding,
        query.version,
        coverage,
        vec![3, 1, 2],
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
        .expect("first page");
    assert_eq!(first.ids, vec![1, 2]);
    let cursor = first.next.expect("next cursor");
    let other_query = Query::new(vec!["beta".into()], Limits::default()).expect("query");
    assert!(matches!(
        adapter.query(&QueryRequest {
            binding,
            query: other_query,
            cursor: Some(cursor),
            limit: 1,
        }),
        Err(AdapterError::Extension(Error::StaleCursor))
    ));
}

#[test]
fn malformed_and_oversized_inputs_are_rejected() {
    assert_eq!(
        Query::new(vec![String::new()], Limits::default()),
        Err(Error::MalformedInput)
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
        materialize_with_limits(
            binding.root,
            binding.recipe,
            binding.authority,
            incomplete_coverage(1, Coverage::Partial),
            Vec::new(),
            Limits::default(),
        ),
        Err(Error::IncompleteCoverage)
    );
    assert_eq!(
        DocumentState::new(
            binding,
            coverage,
            vec![(1, vec![("body".into(), "x".into()); 2])],
            Limits {
                max_fields_per_document: 1,
                ..Limits::default()
            },
        ),
        Err(Error::SizeLimit)
    );
}

#[test]
fn lexical_overlay_updates_only_changed_terms_and_keeps_base_shared() {
    let (binding, coverage) = binding(&[
        (1, vec![("body".into(), "alpha stable".into())]),
        (2, vec![("body".into(), "beta stable".into())]),
    ]);
    let state = DocumentState::new(
        binding,
        coverage,
        vec![
            (1, vec![("body".into(), "alpha stable".into())]),
            (2, vec![("body".into(), "beta stable".into())]),
        ],
        Limits::default(),
    )
    .expect("state");
    let view = LexicalView::from_state(&state).expect("base");
    let delta = state
        .prepare_delta(vec![DocumentChange::Add {
            id: 1,
            fields: vec![("body".into(), "gamma stable".into())],
        }])
        .expect("delta");
    let outcome = view
        .advance(&delta, OverlayLimits::default())
        .expect("advance");
    let advanced = match outcome {
        RefreshOutcome::Advanced(advanced) => advanced,
        other => {
            assert!(matches!(other, RefreshOutcome::Advanced(_)));
            return;
        }
    };
    assert!(std::ptr::eq(view.base(), advanced.base()));
    assert_eq!(
        advanced
            .search(&Query::new(vec!["alpha".into()], Limits::default()).expect("query"))
            .expect("search"),
        Vec::<u64>::new()
    );
    assert_eq!(
        advanced
            .search(&Query::new(vec!["gamma".into()], Limits::default()).expect("query"))
            .expect("search"),
        vec![1]
    );
    assert_eq!(
        advanced
            .search(&Query::new(vec!["beta".into()], Limits::default()).expect("query"))
            .expect("search"),
        vec![2]
    );
}

#[test]
fn lexical_overlay_delete_readd_matches_a_restarted_base() {
    let initial = vec![
        (1, vec![("body".into(), "alpha stable".into())]),
        (2, vec![("body".into(), "beta stable".into())]),
    ];
    let (binding, coverage) = binding(&initial);
    let state =
        DocumentState::new(binding, coverage, initial.clone(), Limits::default()).expect("state");
    let view = LexicalView::from_state(&state).expect("base");
    let replacement = state
        .prepare_delta(vec![DocumentChange::Add {
            id: 1,
            fields: vec![("body".into(), "gamma stable".into())],
        }])
        .expect("replacement delta");
    let replaced_state = state.apply_delta(&replacement).expect("replaced state");
    let outcome = view
        .advance(&replacement, OverlayLimits::default())
        .expect("advance replacement");
    assert!(matches!(&outcome, RefreshOutcome::Advanced(_)));
    let RefreshOutcome::Advanced(view) = outcome else {
        return;
    };
    assert_eq!(
        view.search(&Query::new(vec!["alpha".into()], Limits::default()).expect("query"))
            .expect("search"),
        Vec::<u64>::new()
    );

    let deletion = replaced_state
        .prepare_delta(vec![DocumentChange::Delete { id: 1 }])
        .expect("delete delta");
    let deleted_state = replaced_state
        .apply_delta(&deletion)
        .expect("deleted state");
    let outcome = view
        .advance(&deletion, OverlayLimits::default())
        .expect("advance deletion");
    assert!(matches!(&outcome, RefreshOutcome::Advanced(_)));
    let RefreshOutcome::Advanced(view) = outcome else {
        return;
    };
    assert_eq!(
        view.search(&Query::new(vec!["gamma".into()], Limits::default()).expect("query"))
            .expect("search"),
        Vec::<u64>::new()
    );

    let readd = deleted_state
        .prepare_delta(vec![DocumentChange::Add {
            id: 1,
            fields: vec![("body".into(), "delta stable".into())],
        }])
        .expect("readd delta");
    let restored_state = deleted_state.apply_delta(&readd).expect("restored state");
    let outcome = view
        .advance(&readd, OverlayLimits::default())
        .expect("advance readd");
    assert!(matches!(&outcome, RefreshOutcome::Advanced(_)));
    let RefreshOutcome::Advanced(view) = outcome else {
        return;
    };
    let query = Query::new(vec!["delta".into()], Limits::default()).expect("query");
    let incremental = view.search(&query).expect("incremental search");

    let restarted_state = DocumentState::new(
        restored_state.binding(),
        restored_state.coverage(),
        restored_state
            .iter()
            .map(|(id, fields)| (id, fields.to_vec()))
            .collect(),
        Limits::default(),
    )
    .expect("restarted state");
    let restarted = LexicalView::from_state(&restarted_state)
        .expect("restarted base")
        .search(&query)
        .expect("restarted search");
    assert_eq!(incremental, restarted);
    assert_eq!(view.binding(), restarted_state.binding());
}

#[test]
fn lexical_overlay_fails_closed_at_maintenance_budget() {
    let (binding, coverage) = binding(&[(1, vec![("body".into(), "alpha".into())])]);
    let state = DocumentState::new(
        binding,
        coverage,
        vec![(1, vec![("body".into(), "alpha".into())])],
        Limits::default(),
    )
    .expect("state");
    let view = LexicalView::from_state(&state).expect("base");
    let delta = state
        .prepare_delta(vec![DocumentChange::Add {
            id: 1,
            fields: vec![("body".into(), "beta".into())],
        }])
        .expect("delta");
    let plan = view
        .advance(
            &delta,
            OverlayLimits {
                max_changed_documents: 0,
                ..OverlayLimits::default()
            },
        )
        .expect_err("invalid budget");
    assert_eq!(plan, Error::InvalidLimits);

    let outcome = view
        .advance(
            &delta,
            OverlayLimits {
                max_changed_documents: 1,
                max_terms: 1,
                max_bytes: 1,
                ..OverlayLimits::default()
            },
        )
        .expect("bounded decision");
    assert!(matches!(outcome, RefreshOutcome::RebuildRequired(_)));
}
