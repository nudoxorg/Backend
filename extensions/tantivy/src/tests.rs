//! Adversarial lexical extension tests.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures use expect to make invariant failures local"
)]

use super::*;
use backend_semantic::{Entity, EntityId, Source, entity_key};
use backend_version::{
    AuthorityScopeClaim, Coverage, CoverageWitness, ProducerObservationClaims,
    ProducerObservationVerifier, RelationState, ScopeRoot, UntrustedProducerObservation,
    WorkspaceManifest, WorkspaceRoot, admit_complete_scope, admit_producer_observation,
};

struct FixtureCoverageVerifier {
    producer: [u8; 32],
    scope: ScopeRoot,
    context: [u8; 32],
    evidence: Vec<u8>,
}

fn document(ordinal: u64) -> EntityId {
    let source = Source::new(7, "tests/search.rs").expect("source identity");
    entity_key(
        &Entity::new(source, format!("declaration-{ordinal}"), None).expect("entity identity"),
    )
}

fn hit(ordinal: u64) -> RankedHit {
    RankedHit {
        document: document(ordinal),
        relevance: Relevance::exact(1),
    }
}

impl ProducerObservationVerifier for FixtureCoverageVerifier {
    type Error = ();

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        (observation.producer_identity() == self.producer
            && observation.scope_root() == self.scope
            && observation.context() == self.context
            && observation.evidence() == self.evidence.as_slice())
        .then(|| {
            ProducerObservationClaims::new(
                self.producer,
                self.scope,
                self.context,
                *blake3::hash(&self.evidence).as_bytes(),
            )
        })
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
        [(document(1), vec![("body".into(), "other".into())])],
        coverage,
    )
    .expect("wrong root")
    .root();
    let page = LexicalPage {
        schema: SchemaVersion::CURRENT,
        binding: wrong,
        query: query.version,
        hits: vec![hit(1)],
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
        hits: vec![hit(1)],
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
        hits: vec![hit(1), hit(2)],
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

fn binding(documents: &[(EntityId, Vec<(String, String)>)]) -> (Binding, CoverageWitness) {
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
    let (binding, coverage) = binding(&[(document(1), vec![("body".into(), "alpha".into())])]);
    let materialized = materialize(
        binding.root,
        binding.recipe,
        binding.authority,
        coverage,
        vec![DocumentChange::Add {
            id: document(1),
            fields: vec![("body".into(), "alpha".into())],
        }],
    )
    .expect("materialization");
    let stale = RelationState::<IndexRelation>::from_entries(
        [(document(1), vec![("body".into(), "beta".into())])],
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
fn compatibility_query_uses_the_same_exact_token_boundary() {
    let documents = vec![(document(1), vec![("name".into(), "map".into())])];
    let (binding, coverage) = binding(&documents);
    let materialized = materialize(
        binding.root,
        binding.recipe,
        binding.authority,
        coverage,
        vec![DocumentChange::Add {
            id: document(1),
            fields: vec![("name".into(), "map".into())],
        }],
    )
    .expect("materialization");
    assert!(
        query(&materialized, binding.root, &["ma"])
            .expect("exact query")
            .is_empty()
    );
    assert_eq!(
        query(&materialized, binding.root, &["MAP"]).expect("folded exact query"),
        vec![document(1)]
    );
}

#[test]
fn delete_and_readd_preserve_exact_delta_roots() {
    let (binding, coverage) = binding(&[(document(1), vec![("body".into(), "alpha".into())])]);
    let state = DocumentState::new(
        binding,
        coverage,
        vec![(document(1), vec![("body".into(), "alpha".into())])],
        Limits::default(),
    )
    .expect("state");
    let deleted = state
        .prepare_delta(vec![DocumentChange::Delete { id: document(1) }])
        .expect("delete");
    let empty = state.apply_delta(&deleted).expect("apply delete");
    let added = empty
        .prepare_delta(vec![DocumentChange::Add {
            id: document(1),
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
    let (binding, coverage) = binding(&[(document(1), vec![("body".into(), "alpha".into())])]);
    let state = DocumentState::new(
        binding,
        coverage,
        vec![(document(1), vec![("body".into(), "alpha".into())])],
        Limits::default(),
    )
    .expect("state");
    let mut delta = state
        .prepare_delta(vec![DocumentChange::Delete { id: document(1) }])
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
                id: document(1),
                fields: vec![("a".into(), "abc".into())],
            },
            DocumentChange::Add {
                id: document(2),
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
        vec![hit(3), hit(1), hit(2)],
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
    assert_eq!(first.hits, vec![hit(1), hit(2)]);
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
            vec![(document(1), vec![("body".into(), "x".into()); 2])],
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
        (document(1), vec![("body".into(), "alpha stable".into())]),
        (document(2), vec![("body".into(), "beta stable".into())]),
    ]);
    let state = DocumentState::new(
        binding,
        coverage,
        vec![
            (document(1), vec![("body".into(), "alpha stable".into())]),
            (document(2), vec![("body".into(), "beta stable".into())]),
        ],
        Limits::default(),
    )
    .expect("state");
    let view = LexicalView::from_state(&state).expect("base");
    let delta = state
        .prepare_delta(vec![DocumentChange::Add {
            id: document(1),
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
        Vec::<RankedHit>::new()
    );
    assert_eq!(
        advanced
            .search(&Query::new(vec!["gamma".into()], Limits::default()).expect("query"))
            .expect("search"),
        vec![hit(1)]
    );
    assert_eq!(
        advanced
            .search(&Query::new(vec!["beta".into()], Limits::default()).expect("query"))
            .expect("search"),
        vec![hit(2)]
    );
}

#[test]
fn lexical_overlay_delete_readd_matches_a_restarted_base() {
    let initial = vec![
        (document(1), vec![("body".into(), "alpha stable".into())]),
        (document(2), vec![("body".into(), "beta stable".into())]),
    ];
    let (binding, coverage) = binding(&initial);
    let state =
        DocumentState::new(binding, coverage, initial.clone(), Limits::default()).expect("state");
    let view = LexicalView::from_state(&state).expect("base");
    let replacement = state
        .prepare_delta(vec![DocumentChange::Add {
            id: document(1),
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
        Vec::<RankedHit>::new()
    );

    let deletion = replaced_state
        .prepare_delta(vec![DocumentChange::Delete { id: document(1) }])
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
        Vec::<RankedHit>::new()
    );

    let readd = deleted_state
        .prepare_delta(vec![DocumentChange::Add {
            id: document(1),
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
    let (binding, coverage) = binding(&[(document(1), vec![("body".into(), "alpha".into())])]);
    let state = DocumentState::new(
        binding,
        coverage,
        vec![(document(1), vec![("body".into(), "alpha".into())])],
        Limits::default(),
    )
    .expect("state");
    let view = LexicalView::from_state(&state).expect("base");
    let delta = state
        .prepare_delta(vec![DocumentChange::Add {
            id: document(1),
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

#[test]
fn prefix_rank_is_lossless_at_production_field_weight() {
    let documents = vec![
        (document(1), vec![("name".into(), "map_or_else".into())]),
        (document(2), vec![("name".into(), "map".into())]),
    ];
    let (index_binding, coverage) = binding(&documents);
    let state =
        DocumentState::new(index_binding, coverage, documents, Limits::default()).expect("state");
    let view = TantivySource::build(&state, Limits::default()).expect("view");
    let query = Query::prefix(vec!["ma".into()], Limits::default()).expect("prefix query");
    let hits = view.search(&query).expect("search");
    assert_eq!(
        hits.iter().map(|hit| hit.document).collect::<Vec<_>>(),
        [document(2), document(1)]
    );
    assert!(hits[0].relevance > hits[1].relevance);
}

#[test]
fn code_identifier_components_are_searchable_without_scanning() {
    let documents = vec![
        (
            document(1),
            vec![("name".into(), "std::collections::HashMap".into())],
        ),
        (
            document(2),
            vec![("name".into(), "parseHTTPResponse_snake_case".into())],
        ),
    ];
    let (binding, coverage) = binding(&documents);
    let state = DocumentState::new(binding, coverage, documents, Limits::default()).expect("state");
    let view = TantivySource::build(&state, Limits::default()).expect("view");
    for (term, expected) in [
        ("hash", document(1)),
        ("response", document(2)),
        ("case", document(2)),
    ] {
        let query = Query::prefix(vec![term.into()], Limits::default()).expect("query");
        let hits = view.search(&query).expect("search");
        assert_eq!(hits.first().map(|hit| hit.document), Some(expected));
    }
}

#[test]
fn field_selection_is_part_of_query_identity_and_execution() {
    let documents = vec![
        (document(1), vec![("name".into(), "alpha".into())]),
        (document(2), vec![("documentation".into(), "alpha".into())]),
    ];
    let (binding, coverage) = binding(&documents);
    let state = DocumentState::new(binding, coverage, documents, Limits::default()).expect("state");
    let view = LexicalView::from_state(&state).expect("view");
    let all = Query::new(vec!["alpha".into()], Limits::default()).expect("all fields");
    let names = Query::with_policy(
        vec!["alpha".into()],
        MatchMode::Exact,
        FieldSelection::Only("name".into()),
        Limits::default(),
    )
    .expect("name query");
    assert_ne!(all.version, names.version);
    assert_eq!(
        view.search(&names).expect("search"),
        vec![RankedHit {
            document: document(1),
            relevance: Relevance::exact(4),
        }]
    );
}

#[test]
fn case_policy_is_explicit_and_bound_into_query_identity() {
    let documents = vec![(document(1), vec![("name".into(), "Map".into())])];
    let (binding, coverage) = binding(&documents);
    let state = DocumentState::new(binding, coverage, documents, Limits::default()).expect("state");
    let view = LexicalView::from_state(&state).expect("view");
    let folded = Query::new(vec!["map".into()], Limits::default()).expect("folded");
    let sensitive = Query::with_options(
        vec!["map".into()],
        MatchMode::Exact,
        FieldSelection::All,
        CaseSensitivity::Sensitive,
        Limits::default(),
    )
    .expect("sensitive");
    assert_ne!(folded.version, sensitive.version);
    assert_eq!(view.search(&folded).expect("folded search").len(), 1);
    assert!(
        view.search(&sensitive)
            .expect("sensitive search")
            .is_empty()
    );
}

#[test]
fn ranked_cursor_pages_refill_without_per_page_truncation() {
    let documents = vec![
        (document(1), vec![("name".into(), "map_or_else".into())]),
        (document(2), vec![("name".into(), "map".into())]),
        (document(3), vec![("name".into(), "mapper".into())]),
    ];
    let (binding, coverage) = binding(&documents);
    let state = DocumentState::new(binding, coverage, documents, Limits::default()).expect("state");
    let view = LexicalView::from_state(&state).expect("view");
    let query = Query::prefix(vec!["ma".into()], Limits::default()).expect("query");
    let first = view
        .page(&query, None, 1, Limits::default())
        .expect("first");
    assert_eq!(first.hits[0].document, document(2));
    let second = view
        .page(&query, first.next, 1, Limits::default())
        .expect("second");
    assert_eq!(second.hits[0].document, document(3));
    let third = view
        .page(&query, second.next, 1, Limits::default())
        .expect("third");
    assert_eq!(third.hits[0].document, document(1));
    assert!(third.next.is_none());
}

#[test]
fn corpus_cardinality_is_independent_from_delta_batch_budget() {
    let documents = vec![
        (document(1), vec![("name".into(), "alpha".into())]),
        (document(2), vec![("name".into(), "beta".into())]),
    ];
    let (binding, coverage) = binding(&documents);
    let limits = Limits {
        max_delta_documents: 1,
        ..Limits::default()
    };
    let state = DocumentState::new(binding, coverage, documents, limits)
        .expect("corpus cardinality is not an operation budget");
    assert_eq!(state.iter().count(), 2);
    assert_eq!(
        state.prepare_delta(vec![
            DocumentChange::Delete { id: document(1) },
            DocumentChange::Delete { id: document(2) },
        ]),
        Err(Error::SizeLimit)
    );
}

#[test]
fn concrete_tantivy_matches_the_portable_oracle_across_query_policies() {
    let documents = vec![
        (
            document(1),
            vec![
                ("documentation".into(), "transform alpha".into()),
                ("name".into(), "Map map_or_else".into()),
            ],
        ),
        (
            document(2),
            vec![
                ("name".into(), "mapper".into()),
                ("signature".into(), "map alpha".into()),
            ],
        ),
        (
            document(3),
            vec![("documentation".into(), "map alpha".into())],
        ),
    ];
    let (binding, coverage) = binding(&documents);
    let state = DocumentState::new(binding, coverage, documents, Limits::default()).expect("state");
    let oracle = LexicalView::from_state(&state).expect("portable oracle");
    let tantivy = TantivySource::build(&state, Limits::default()).expect("Tantivy projection");
    let queries = [
        Query::new(vec!["map".into()], Limits::default()).expect("exact"),
        Query::prefix(vec!["ma".into()], Limits::default()).expect("prefix"),
        Query::with_policy(
            vec!["map".into()],
            MatchMode::Exact,
            FieldSelection::Only("name".into()),
            Limits::default(),
        )
        .expect("field query"),
        Query::with_options(
            vec!["Map".into()],
            MatchMode::Exact,
            FieldSelection::All,
            CaseSensitivity::Sensitive,
            Limits::default(),
        )
        .expect("case-sensitive query"),
        Query::new(vec!["map".into(), "alpha".into()], Limits::default()).expect("conjunction"),
    ];
    for query in queries {
        assert_eq!(
            tantivy.search(&query).expect("Tantivy search"),
            oracle.search(&query).expect("oracle search"),
            "query policies must have one canonical meaning: {query:?}"
        );
    }
}

#[test]
fn concrete_tantivy_pages_after_global_ranking_and_fences_the_binding() {
    let documents = vec![
        (document(1), vec![("name".into(), "map_or_else".into())]),
        (document(2), vec![("name".into(), "map".into())]),
        (document(3), vec![("name".into(), "mapper".into())]),
    ];
    let (binding, coverage) = binding(&documents);
    let limits = Limits {
        max_page: 1,
        ..Limits::default()
    };
    let authoritative_state =
        DocumentState::new(binding, coverage, documents, limits).expect("state");
    let adapter = Adapter::new(
        TantivySource::build(&authoritative_state, limits).expect("Tantivy projection"),
        limits,
    )
    .expect("adapter");
    let query = Query::prefix(vec!["ma".into()], limits).expect("query");
    let first = adapter
        .query(&QueryRequest {
            binding,
            query: query.clone(),
            cursor: None,
            limit: 1,
        })
        .expect("first page");
    let second = adapter
        .query(&QueryRequest {
            binding,
            query: query.clone(),
            cursor: first.next,
            limit: 1,
        })
        .expect("second page");
    let third = adapter
        .query(&QueryRequest {
            binding,
            query: query.clone(),
            cursor: second.next,
            limit: 1,
        })
        .expect("third page");
    assert_eq!(
        [
            first.hits[0].document,
            second.hits[0].document,
            third.hits[0].document,
        ],
        [document(2), document(3), document(1)]
    );

    let mut stale_binding = binding;
    stale_binding.recipe = Recipe::from_value(&[99; 32]);
    assert!(matches!(
        adapter.query(&QueryRequest {
            binding: stale_binding,
            query,
            cursor: None,
            limit: 1,
        }),
        Err(AdapterError::Provider(TantivySourceError::Contract(
            Error::StaleRoot
        )))
    ));
}

#[test]
fn concrete_tantivy_can_commit_its_projection_to_disk() {
    let documents = vec![
        (document(1), vec![("name".into(), "map".into())]),
        (
            document(2),
            vec![("name".into(), "std::collections::HashMap".into())],
        ),
    ];
    let (index_binding, coverage) = binding(&documents);
    let state =
        DocumentState::new(index_binding, coverage, documents, Limits::default()).expect("state");
    let directory = std::env::temp_dir().join(format!(
        "backend-tantivy-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir(&directory).expect("create test index directory");
    let source = TantivySource::build_in_dir(&state, Limits::default(), &directory)
        .expect("durable Tantivy projection");
    assert!(directory.join("meta.json").is_file());
    assert!(directory.join("backend-binding-v2").is_file());
    drop(source);
    let source = TantivySource::open_in_dir(&state, Limits::default(), &directory)
        .expect("reopen exact projection");
    let map_hits = source
        .search(&Query::new(vec!["map".into()], Limits::default()).expect("query"))
        .expect("search");
    assert_eq!(map_hits[0].document, document(1));
    assert_eq!(map_hits[0].relevance, Relevance::exact(4));
    assert_eq!(map_hits[1].document, document(2));
    assert_eq!(
        source
            .search(&Query::prefix(vec!["hash".into()], Limits::default()).expect("query"))
            .expect("component search")[0]
            .document,
        document(2)
    );
    drop(source);
    let other_documents = vec![(document(1), vec![("name".into(), "mapper".into())])];
    let (other_binding, other_coverage) = binding(&other_documents);
    let other_state = DocumentState::new(
        other_binding,
        other_coverage,
        other_documents,
        Limits::default(),
    )
    .expect("other state");
    assert!(matches!(
        TantivySource::open_in_dir(&other_state, Limits::default(), &directory),
        Err(TantivySourceError::Contract(Error::StaleRoot))
    ));
    std::fs::remove_dir_all(directory).expect("remove test index directory");
}

fn state_for(
    documents: Vec<(EntityId, Vec<(String, String)>)>,
    frontier: [u8; 32],
) -> DocumentState {
    let (binding, coverage) = binding(&documents);
    let binding = binding.with_frontier(Frontier::from_value(&frontier));
    DocumentState::new(binding, coverage, documents, Limits::default()).expect("document state")
}

fn term_hits(source: &TantivySource, term: &str) -> Vec<EntityId> {
    source
        .search(&Query::new(vec![term.to_owned()], Limits::default()).expect("query"))
        .expect("search")
        .into_iter()
        .map(|hit| hit.document)
        .collect()
}

#[test]
fn rebinding_a_view_keeps_every_posting_and_rejects_the_old_binding() {
    let documents = vec![
        (document(1), vec![("name".into(), "alpha".into())]),
        (document(2), vec![("name".into(), "beta".into())]),
    ];
    let current = state_for(documents.clone(), [1; 32]);
    let mut source = TantivySource::build(&current, Limits::default()).expect("projection");
    let before = source.indexed_postings();
    let next = state_for(documents, [2; 32]);
    let outcome = source
        .maintain(&next, OverlayLimits::default())
        .expect("rebind");
    assert_eq!(
        outcome,
        MaintainOutcome::Applied(ProjectionRevision {
            kind: ProjectionKind::Rebound,
            rewritten_documents: 0,
            retired_postings: 0,
            added_postings: 0,
        })
    );
    assert_eq!(source.indexed_postings(), before);
    assert_eq!(term_hits(&source, "alpha"), vec![document(1)]);
    assert_eq!(term_hits(&source, "beta"), vec![document(2)]);
    let stale = Query::new(vec!["alpha".into()], Limits::default()).expect("query");
    assert!(matches!(
        LexicalSource::fetch(
            &source,
            &QueryRequest {
                binding: current.binding(),
                query: stale.clone(),
                cursor: None,
                limit: 8,
            }
        ),
        Err(TantivySourceError::Contract(Error::StaleRoot))
    ));
    let page = LexicalSource::fetch(
        &source,
        &QueryRequest {
            binding: next.binding(),
            query: stale,
            cursor: None,
            limit: 8,
        },
    )
    .expect("new binding");
    assert_eq!(page.hits[0].document, document(1));
}

#[test]
fn one_document_revision_deletes_only_that_documents_postings() {
    let original = vec![
        (document(1), vec![("name".into(), "alpha".into())]),
        (document(2), vec![("name".into(), "beta".into())]),
        (document(3), vec![("name".into(), "gamma".into())]),
    ];
    let mut source =
        TantivySource::build(&state_for(original, [1; 32]), Limits::default()).expect("projection");
    let before = source.indexed_postings();
    let revised = vec![
        (document(1), vec![("name".into(), "alpha".into())]),
        (document(2), vec![("name".into(), "zephyr".into())]),
        (document(3), vec![("name".into(), "gamma".into())]),
    ];
    let outcome = source
        .maintain(&state_for(revised, [2; 32]), OverlayLimits::default())
        .expect("revise");
    let MaintainOutcome::Applied(revision) = outcome else {
        panic!("revision was refused");
    };
    assert_eq!(revision.kind, ProjectionKind::Revised);
    assert_eq!(revision.rewritten_documents, 1);
    assert_eq!(
        source.indexed_postings(),
        before - revision.retired_postings + revision.added_postings
    );
    assert_eq!(revision.retired_postings, revision.added_postings);
    assert!(revision.retired_postings > 0);
    assert_eq!(term_hits(&source, "alpha"), vec![document(1)]);
    assert_eq!(term_hits(&source, "gamma"), vec![document(3)]);
    assert_eq!(term_hits(&source, "zephyr"), vec![document(2)]);
    assert!(term_hits(&source, "beta").is_empty());
}

#[test]
fn deleting_and_prepending_documents_keeps_untouched_ordinals() {
    let original = vec![
        (document(1), vec![("name".into(), "alpha".into())]),
        (document(2), vec![("name".into(), "beta".into())]),
        (document(3), vec![("name".into(), "gamma".into())]),
    ];
    let mut source =
        TantivySource::build(&state_for(original, [1; 32]), Limits::default()).expect("projection");
    let without_middle = vec![
        (document(1), vec![("name".into(), "alpha".into())]),
        (document(3), vec![("name".into(), "gamma".into())]),
    ];
    source
        .maintain(
            &state_for(without_middle, [2; 32]),
            OverlayLimits::default(),
        )
        .expect("delete");
    assert!(term_hits(&source, "beta").is_empty());
    let with_predecessor = vec![
        (document(1), vec![("name".into(), "alpha".into())]),
        (document(3), vec![("name".into(), "gamma".into())]),
        (document(4), vec![("name".into(), "delta".into())]),
    ];
    source
        .maintain(
            &state_for(with_predecessor, [3; 32]),
            OverlayLimits::default(),
        )
        .expect("append");
    let edited = vec![
        (document(1), vec![("name".into(), "alpaca".into())]),
        (document(3), vec![("name".into(), "gamma".into())]),
        (document(4), vec![("name".into(), "delta".into())]),
    ];
    source
        .maintain(&state_for(edited, [4; 32]), OverlayLimits::default())
        .expect("edit survivor");
    assert!(term_hits(&source, "alpha").is_empty());
    assert!(term_hits(&source, "beta").is_empty());
    assert_eq!(term_hits(&source, "alpaca"), vec![document(1)]);
    assert_eq!(term_hits(&source, "gamma"), vec![document(3)]);
    assert_eq!(term_hits(&source, "delta"), vec![document(4)]);
}

#[test]
fn an_edit_past_the_budget_leaves_the_projection_unchanged() {
    let original = vec![
        (document(1), vec![("name".into(), "alpha".into())]),
        (document(2), vec![("name".into(), "beta".into())]),
    ];
    let current = state_for(original, [1; 32]);
    let mut source = TantivySource::build(&current, Limits::default()).expect("projection");
    let before = source.indexed_postings();
    let replaced = vec![
        (document(1), vec![("name".into(), "kappa".into())]),
        (document(2), vec![("name".into(), "lambda".into())]),
    ];
    let mut budget = OverlayLimits::default();
    budget.max_changed_documents = 1;
    let outcome = source
        .maintain(&state_for(replaced, [2; 32]), budget)
        .expect("budget");
    assert_eq!(outcome, MaintainOutcome::RebuildRequired);
    assert_eq!(source.indexed_postings(), before);
    assert_eq!(term_hits(&source, "alpha"), vec![document(1)]);
    assert!(term_hits(&source, "kappa").is_empty());
    let query = Query::new(vec!["alpha".into()], Limits::default()).expect("query");
    LexicalSource::fetch(
        &source,
        &QueryRequest {
            binding: current.binding(),
            query,
            cursor: None,
            limit: 8,
        },
    )
    .expect("old binding still serves");
}

#[test]
fn a_different_workspace_requires_a_rebuild() {
    let documents = vec![(document(1), vec![("name".into(), "alpha".into())])];
    let current = state_for(documents.clone(), [1; 32]);
    let mut source = TantivySource::build(&current, Limits::default()).expect("projection");
    let (mut binding, coverage) = binding(&documents);
    binding = binding.with_frontier(Frontier::from_value(&[2; 32]));
    binding.workspace = workspace_alt();
    let next = DocumentState::new(binding, coverage, documents, Limits::default())
        .expect("other workspace");
    assert_ne!(
        next.binding().workspace,
        current.binding().workspace,
        "alternate workspace collapsed onto the fixture workspace"
    );
    let outcome = source
        .maintain(&next, OverlayLimits::default())
        .expect("workspace fence");
    assert_eq!(outcome, MaintainOutcome::RebuildRequired);
    assert_eq!(term_hits(&source, "alpha"), vec![document(1)]);
}

fn workspace_alt() -> WorkspaceRoot {
    WorkspaceManifest::from_versions(
        1,
        Vec::new(),
        Vec::new(),
        Authority::from_value(&[8; 32]),
        authorized_coverage(&[5; 32]),
    )
    .expect("alternate workspace")
    .root()
}
