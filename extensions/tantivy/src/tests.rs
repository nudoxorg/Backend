//! Adversarial lexical extension tests.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures use expect to make invariant failures local"
)]

use super::*;
use crate::engine::test_support::{
    BINDING_FILE, DURABLE_ROOTS_DIRECTORY, INTEGRITY_FILE, MAX_PROJECTION_MANIFEST_BYTES,
    MAX_ORDINAL_MAP_BYTES, MAX_RETAINED_DURABLE_ROOTS,
    ORDINAL_MAP_FILE, ORDINAL_MAP_MAGIC, hex_fingerprint, projection_fingerprint, rank_cache_bytes,
    ordinal_map_capacity, write_projection_manifest,
};
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
        total: 1,
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
        total: 1,
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
        total: 2,
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
fn three_clause_ranking_uses_weakest_quality_sum_weight_and_multivalue_maxima() {
    // Put the exact-last-clause document at the larger identity so the old
    // clause-count comparison would incorrectly promote it. Both first rows
    // independently have a weakest ratio of 1/8, so stable identity orders
    // them. The third repeats `a` across indexed field values; that clause
    // contributes its best field once, while every query clause contributes
    // its own weight.
    let first_identity = document(1).min(document(2));
    let second_identity = document(1).max(document(2));
    let documents = vec![
        (
            first_identity,
            vec![("name".into(), "a bbbbbbbb cccccccc".into())],
        ),
        (
            second_identity,
            vec![("name".into(), "aaaaaaaa bbbbbbbb c".into())],
        ),
        (
            document(3),
            vec![
                ("name".into(), "a bbbbbbbb".into()),
                ("signature".into(), "a cccccccc".into()),
            ],
        ),
    ];
    let (binding, coverage) = binding(&documents);
    let state = DocumentState::new(binding, coverage, documents, Limits::default())
        .expect("three clause ranking state");
    let source = TantivySource::build(&state, Limits::default()).expect("projection");

    for terms in [
        vec!["a".into(), "b".into(), "c".into()],
        vec!["c".into(), "a".into(), "b".into()],
    ] {
        let query = Query::new(terms, Limits::default()).expect("three clause query");
        let hits = source.search(&query).expect("independent fixed-label query");
        assert_eq!(
            hits.iter().map(|hit| hit.document).collect::<Vec<_>>(),
            [first_identity, second_identity, document(3)]
        );
        assert_eq!(hits[0].relevance.rank_parts(), (1, 8, 12, 3));
        assert_eq!(hits[1].relevance.rank_parts(), (1, 8, 12, 3));
        assert_eq!(hits[2].relevance.rank_parts(), (1, 8, 11, 3));
        assert!(hits.iter().all(|hit| !hit.relevance.is_exact()));
    }
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
    assert_eq!(first.total, 3);
    assert_eq!(adapter.rank_evaluations(), 3);
}

#[test]
fn a_short_page_keeps_the_full_total_with_one_bounded_scan_per_page() {
    let documents = vec![
        (document(1), vec![("name".into(), "alpha".into())]),
        (document(2), vec![("name".into(), "alpine".into())]),
        (document(3), vec![("name".into(), "beta".into())]),
    ];
    let (binding, coverage) = binding(&documents);
    let limits = Limits {
        max_page: 1,
        ..Limits::default()
    };
    let state = DocumentState::new(binding, coverage, documents, limits).expect("state");
    let adapter = Adapter::new(
        TantivySource::build(&state, limits).expect("projection"),
        limits,
    )
    .expect("adapter");
    let query = Query::prefix(vec!["al".into()], limits).expect("query");
    let first = adapter
        .query(&QueryRequest {
            binding,
            query: query.clone(),
            cursor: None,
            limit: 1,
        })
        .expect("first");
    let second = adapter
        .query(&QueryRequest {
            binding,
            query,
            cursor: first.next,
            limit: 1,
        })
        .expect("second");
    assert_eq!(first.total, 2);
    assert_eq!(second.total, 2);
    assert!(second.next.is_none());
    assert_ne!(first.hits[0].document, second.hits[0].document);
    assert_eq!(adapter.rank_evaluations(), 2);
}

#[test]
fn broad_keyset_pages_keep_only_the_current_page_and_row_scratch() {
    let documents = (1..=128)
        .map(|ordinal| {
            (
                document(ordinal),
                vec![("name".into(), "commonquery token".into())],
            )
        })
        .collect::<Vec<_>>();
    let (binding, coverage) = binding(&documents);
    let limits = Limits {
        max_page: 1,
        ..Limits::default()
    };
    let state = DocumentState::new(binding, coverage, documents, limits).expect("state");
    let adapter = Adapter::new(
        TantivySource::build(&state, limits).expect("projection"),
        limits,
    )
    .expect("adapter");
    let query = Query::new(vec!["commonquery".into()], limits).expect("query");
    let mut cursor = None;
    let mut observed = Vec::new();
    loop {
        let page = adapter
            .query(&QueryRequest {
                binding,
                query: query.clone(),
                cursor,
                limit: 1,
            })
            .expect("bounded keyset page");
        assert_eq!(page.total, 128);
        observed.extend(page.hits.iter().map(|hit| hit.document));
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(observed, (1..=128).map(document).collect::<Vec<_>>());
    assert_eq!(adapter.rank_evaluations(), 128);
    assert_eq!(adapter.source().rank_docs_visited(), 128 * 128);
    assert_eq!(rank_cache_bytes(&adapter).expect("retained rank bytes"), 0);
}

#[test]
fn exact_pages_stay_bounded_and_all_results_refuse_over_budget() {
    let documents = (1..=128)
        .map(|ordinal| {
            (
                document(ordinal),
                vec![("name".into(), "a bounded corpus row".into())],
            )
        })
        .collect::<Vec<_>>();
    let (binding, coverage) = binding(&documents);
    let limits = Limits {
        max_page: 1,
        ..Limits::default()
    };
    let state = DocumentState::new(binding, coverage, documents, limits).expect("state");
    let source = TantivySource::build(&state, limits)
        .expect("projection")
        .with_rank_snapshot_budget(
            RankSnapshotBudget::new(1024, 1024).expect("nonzero query budget"),
        );
    let adapter = Adapter::new(source, limits).expect("adapter");
    let query = Query::new(Vec::new(), limits).expect("all-documents query");
    let page = adapter
        .query(&QueryRequest {
            binding,
            query: query.clone(),
            cursor: None,
            limit: 1,
        })
        .expect("bounded top page does not retain every exact hit");
    assert_eq!(page.total, 128);
    assert_eq!(page.hits.len(), 1);
    assert_eq!(adapter.source().rank_docs_visited(), 128);
    let error = adapter.source().search(&query).expect_err(
        "the explicit all-results API must refuse output above its retained-byte budget",
    );
    let TantivySourceError::RankSnapshotBudgetExceeded {
        budget_bytes,
        required_bytes,
    } = error
    else {
        panic!("the all-results API should return its typed memory refusal");
    };
    assert_eq!(budget_bytes, 1024);
    assert!(required_bytes > budget_bytes);
    assert_eq!(rank_cache_bytes(&adapter).expect("retained rank bytes"), 0);
    assert!(adapter.source().rank_docs_visited() >= 128);
}

#[test]
fn oversized_query_scratch_is_refused_before_scorer_compilation() {
    let limits = Limits {
        max_terms: 64,
        ..Limits::default()
    };
    let (binding, coverage) = binding(&[]);
    let state = DocumentState::new(binding, coverage, Vec::new(), limits).expect("empty state");
    let source = TantivySource::build(&state, limits)
        .expect("empty projection")
        .with_rank_snapshot_budget(
            RankSnapshotBudget::new(2_048, 1_024).expect("nonzero query budget"),
        );
    let query = Query::new(
        (0..32)
            .map(|term| format!("absent-token-{term:02}"))
            .collect(),
        limits,
    )
    .expect("admitted no-hit query");

    let refused = |error: TantivySourceError| {
        assert!(matches!(
            error,
            TantivySourceError::RankSnapshotBudgetExceeded {
                budget_bytes: 2_048,
                required_bytes
            } if required_bytes > 2_048
        ));
    };
    refused(
        source
            .for_each_ranked_hit(&query, |_| {})
            .expect_err("query setup must fit the explicit scratch budget"),
    );
    refused(
        source
            .search(&query)
            .expect_err("all-results query setup must fit the scratch budget"),
    );
    refused(
        source
            .relevance_for_candidates(&query, &[document(1)])
            .expect_err("candidate scoring must fit the scratch budget"),
    );
    refused(
        source
            .fetch(&QueryRequest {
                binding,
                query,
                cursor: None,
                limit: 1,
            })
            .expect_err("page query setup must fit the scratch budget"),
    );
    assert_eq!(source.rank_evaluations(), 0);
    assert_eq!(source.rank_docs_visited(), 0);
}

#[test]
fn sparse_cold_reopen_residency_scales_with_live_rows_not_historical_slots() {
    static NEXT_ROOT: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT_ROOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "backend-tantivy-sparse-reopen-{}-{sequence}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).expect("create sparse projection directory");
    let documents = vec![(document(73), vec![("name".into(), "sparse canary".into())])];
    let state = state_for(documents, [0x7b; 32]);
    let slot_count = 4_000_000_u32;
    let ordinal = slot_count - 1;
    crate::engine::test_support::write_sparse_durable_fixture(
        &state,
        &directory,
        slot_count,
        ordinal,
    )
    .expect("write valid sparse durable projection");

    let source = TantivySource::open_in_dir(&state, Limits::default(), &directory)
        .expect("cold-open sparse ordinal root");
    let (resident_slots, resident_rows, resident_bytes) =
        crate::engine::test_support::ordinal_residency(&source);
    assert_eq!((resident_slots, resident_rows), (slot_count, 1));
    assert!(resident_bytes <= 128, "sparse residency is bounded by one live row");
    assert_eq!(
        source
            .search(&Query::new(vec!["sparse".into()], Limits::default()).expect("query"))
            .expect("search sparse reopened root")
            .iter()
            .map(|hit| hit.document)
            .collect::<Vec<_>>(),
        vec![document(73)]
    );
    drop(source);
    std::fs::remove_dir_all(directory).expect("remove sparse projection directory");
}

#[test]
fn a_rare_query_uses_sparse_ranks_instead_of_a_corpus_sized_vector() {
    let documents = (1..=128)
        .map(|ordinal| {
            let text = if ordinal == 73 {
                "needle only-here"
            } else {
                "ordinary corpus row"
            };
            (document(ordinal), vec![("name".into(), text.into())])
        })
        .collect::<Vec<_>>();
    let (binding, coverage) = binding(&documents);
    let limits = Limits::default();
    let state = DocumentState::new(binding, coverage, documents, limits).expect("state");
    let adapter = Adapter::new(
        TantivySource::build(&state, limits).expect("projection"),
        limits,
    )
    .expect("adapter");
    let query = Query::new(vec!["needle".into()], limits).expect("query");
    let page = adapter
        .query(&QueryRequest {
            binding,
            query,
            cursor: None,
            limit: 10,
        })
        .expect("rare query");
    assert_eq!(page.total, 1);
    assert_eq!(
        page.hits.iter().map(|hit| hit.document).collect::<Vec<_>>(),
        vec![document(73)]
    );
    assert_eq!(rank_cache_bytes(&adapter).expect("retained rank bytes"), 0);
}

#[test]
fn a_page_total_that_disagrees_with_the_cursor_is_rejected() {
    let (binding, coverage) = binding(&[]);
    let query = Query::new(vec!["alpha".into()], Limits::default()).expect("query");
    let page = LexicalPage {
        schema: SchemaVersion::CURRENT,
        binding,
        query: query.version,
        hits: vec![hit(1)],
        next: None,
        total: 2,
        coverage,
    };
    let adapter = Adapter::new(SubstitutingSource { page }, Limits::default()).expect("adapter");
    assert!(matches!(
        adapter.query(&QueryRequest {
            binding,
            query,
            cursor: None,
            limit: 1,
        }),
        Err(AdapterError::Extension(Error::InvalidCursor))
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
    assert!(directory.join("backend-binding-v3").is_file());
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
fn durable_selected_roots_reopen_update_and_roll_back_against_fixed_answers() {
    static NEXT_ROOT: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "backend-tantivy-selected-{}-{}",
        std::process::id(),
        NEXT_ROOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let initial = state_for(
        vec![
            (document(1), vec![("name".into(), "alpha".into())]),
            (document(2), vec![("name".into(), "beta".into())]),
        ],
        [41; 32],
    );
    let original = TantivySource::open_or_build_in_dir(
        &initial,
        Limits::default(),
        &root,
    )
    .expect("publish initial selected root");
    assert_eq!(term_hits(&original, "alpha"), vec![document(1)]);
    assert_eq!(term_hits(&original, "beta"), vec![document(2)]);

    let next_state = state_for(
        vec![
            (document(1), vec![("name".into(), "gamma".into())]),
            (document(3), vec![("name".into(), "delta".into())]),
        ],
        [42; 32],
    );
    let (next, revision) = TantivySource::open_or_advance_in_dir(
        &initial,
        &next_state,
        Limits::default(),
        OverlayLimits::default(),
        &root,
    )
    .expect("publish revised selected root");
    assert_eq!(
        revision.map(|revision| revision.kind),
        Some(ProjectionKind::Revised)
    );
    assert_eq!(term_hits(&next, "alpha"), Vec::<EntityId>::new());
    assert_eq!(term_hits(&next, "beta"), Vec::<EntityId>::new());
    assert_eq!(term_hits(&next, "gamma"), vec![document(1)]);
    assert_eq!(term_hits(&next, "delta"), vec![document(3)]);
    drop(next);

    let cold = TantivySource::open_or_build_in_dir(
        &next_state,
        Limits::default(),
        &root,
    )
    .expect("cold reopen exact selected root");
    assert_eq!(term_hits(&cold, "gamma"), vec![document(1)]);
    assert_eq!(term_hits(&cold, "delta"), vec![document(3)]);
    drop(cold);

    let rollback = TantivySource::open_or_build_in_dir(
        &initial,
        Limits::default(),
        &root,
    )
    .expect("reopen previous root for rollback");
    assert_eq!(term_hits(&rollback, "alpha"), vec![document(1)]);
    assert_eq!(term_hits(&rollback, "beta"), vec![document(2)]);
    assert_eq!(term_hits(&rollback, "gamma"), Vec::<EntityId>::new());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn public_mutation_cannot_rewrite_a_selected_durable_root() {
    let root = std::env::temp_dir().join(format!(
        "backend-tantivy-immutable-root-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let initial = state_for(
        vec![
            (document(1), vec![("name".into(), "old-alpha".into())]),
            (document(2), vec![("name".into(), "old-beta".into())]),
        ],
        [91; 32],
    );
    let next = state_for(
        vec![
            (document(1), vec![("name".into(), "new-gamma".into())]),
            (document(3), vec![("name".into(), "new-delta".into())]),
        ],
        [92; 32],
    );
    let mut selected_source =
        TantivySource::open_or_build_in_dir(&initial, Limits::default(), &root)
            .expect("publish initial selected root");
    let root_key = hex_fingerprint(projection_fingerprint(initial.binding()));
    let selected_root = root.join(DURABLE_ROOTS_DIRECTORY).join(root_key);
    let before_files = crate::engine::test_support::projection_files_for_test(&selected_root)
        .expect("hash exact selected files");
    let before_bytes = crate::engine::test_support::durable_root_bytes_for_test(&selected_root)
        .expect("measure selected root");

    assert!(matches!(
        selected_source.maintain(&next, OverlayLimits::default()),
        Err(TantivySourceError::DurableProjectionImmutable)
    ));
    assert_eq!(
        crate::engine::test_support::projection_files_for_test(&selected_root)
            .expect("selected files after rejected mutation"),
        before_files,
        "a rejected public mutation must leave every selected file byte-identical"
    );
    assert_eq!(
        crate::engine::test_support::durable_root_bytes_for_test(&selected_root)
            .expect("selected root size after rejected mutation"),
        before_bytes
    );
    drop(selected_source);

    let cold = TantivySource::open_or_build_in_dir(&initial, Limits::default(), &root)
        .expect("cold rollback to the original selected root");
    assert_eq!(term_hits(&cold, "old-alpha"), vec![document(1)]);
    assert_eq!(term_hits(&cold, "old-beta"), vec![document(2)]);
    assert!(term_hits(&cold, "new-gamma").is_empty());
    assert!(term_hits(&cold, "new-delta").is_empty());
    drop(cold);

    let raw_root = root.join("raw-durable-root");
    std::fs::create_dir_all(&raw_root).expect("create direct durable root");
    let mut raw_source = TantivySource::build_in_dir(&initial, Limits::default(), &raw_root)
        .expect("build direct durable root");
    assert!(matches!(
        raw_source.maintain(&next, OverlayLimits::default()),
        Err(TantivySourceError::DurableProjectionImmutable)
    ));
    drop(raw_source);
    let raw_cold = TantivySource::open_in_dir(&initial, Limits::default(), &raw_root)
        .expect("direct durable root remains reopenable");
    assert_eq!(term_hits(&raw_cold, "old-alpha"), vec![document(1)]);
    assert!(term_hits(&raw_cold, "new-gamma").is_empty());
    drop(raw_cold);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn durable_budget_refusal_keeps_a_valid_selected_root_for_later_reopen() {
    let root = std::env::temp_dir().join(format!(
        "backend-tantivy-budget-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let state = state_for(
        vec![(document(8), vec![("name".into(), "budget-canary".into())])],
        [81; 32],
    );
    let source = TantivySource::open_or_build_in_dir(&state, Limits::default(), &root)
        .expect("publish root under default cache budget");
    drop(source);
    let key = hex_fingerprint(projection_fingerprint(state.binding()));
    let selected = root.join(DURABLE_ROOTS_DIRECTORY).join(key);
    let original_manifest =
        std::fs::read(selected.join(INTEGRITY_FILE)).expect("integrity manifest");
    let full_root_bytes = crate::engine::test_support::durable_root_bytes_for_test(&selected)
        .expect("measure published root and marker bytes");
    let below_full_root = DurableCacheBudget::new(full_root_bytes - 1)
        .expect("nonzero budget below full selected root");
    assert!(matches!(
        TantivySource::open_in_dir_with_budget(
            &state,
            Limits::default(),
            &selected,
            below_full_root,
        ),
        Err(TantivySourceError::BudgetExceeded {
            budget_bytes,
            required_bytes,
        }) if budget_bytes == full_root_bytes - 1 && required_bytes == full_root_bytes
    ));
    assert!(selected.is_dir(), "root-byte refusal must preserve the selected root");
    let tiny_budget = DurableCacheBudget::new(1).expect("nonzero cache budget");

    assert!(matches!(
        TantivySource::open_or_build_in_dir_with_budget_and_action(
            &state,
            Limits::default(),
            &root,
            tiny_budget,
        ),
        Err(TantivySourceError::BudgetExceeded { budget_bytes: 1, .. })
    ));
    assert!(selected.is_dir(), "capacity refusal must retain the selected root");
    assert_eq!(
        std::fs::read(selected.join(INTEGRITY_FILE)).expect("retained manifest"),
        original_manifest
    );

    let reopened = TantivySource::open_or_build_in_dir(&state, Limits::default(), &root)
        .expect("later default-budget open should reuse the intact root");
    assert_eq!(term_hits(&reopened, "budget-canary"), vec![document(8)]);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn ordinal_map_capacity_is_typed_before_index_construction() {
    let header_bytes = ORDINAL_MAP_MAGIC.len() + 48;
    let maximum_records = usize::try_from(
        (MAX_ORDINAL_MAP_BYTES - u64::try_from(header_bytes).expect("header size")) / 108,
    )
    .expect("record count fits this platform");

    assert!(ordinal_map_capacity(maximum_records).is_ok());
    assert!(matches!(
        ordinal_map_capacity(maximum_records + 1),
        Err(TantivySourceError::OrdinalMapCapacityExceeded {
            maximum_bytes: MAX_ORDINAL_MAP_BYTES,
            ..
        })
    ));
}

#[test]
fn durable_delta_reopen_uses_the_persisted_sparse_ordinal_map() {
    static NEXT_ROOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "backend-tantivy-ordinal-map-{}-{}",
        std::process::id(),
        NEXT_ROOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let first = state_for(
        vec![
            (document(1), vec![("name".into(), "first-entry".into())]),
            (document(2), vec![("name".into(), "deleted-middle".into())]),
            (document(3), vec![("name".into(), "survivor-entry".into())]),
        ],
        [61; 32],
    );
    let source = TantivySource::open_or_build_in_dir(&first, Limits::default(), &root)
        .expect("build first durable root");
    assert_eq!(term_hits(&source, "first-entry"), vec![document(1)]);
    drop(source);

    let minimum_existing = [document(1), document(2), document(3)]
        .into_iter()
        .min()
        .expect("existing identity");
    let inserted = (4..100_000)
        .map(document)
        .find(|candidate| *candidate < minimum_existing)
        .expect("find an identity that sorts before the existing rows");
    let second = state_for(
        vec![
            (document(1), vec![("name".into(), "first-entry".into())]),
            (document(3), vec![("name".into(), "survivor-entry".into())]),
            (inserted, vec![("name".into(), "inserted-entry".into())]),
        ],
        [62; 32],
    );
    let (source, revision) = TantivySource::open_or_advance_in_dir(
        &first,
        &second,
        Limits::default(),
        OverlayLimits::default(),
        &root,
    )
    .expect("delete middle row and append a lower-sorting identity");
    assert_eq!(revision.map(|revision| revision.kind), Some(ProjectionKind::Revised));
    assert_eq!(term_hits(&source, "first-entry"), vec![document(1)]);
    assert_eq!(term_hits(&source, "deleted-middle"), Vec::<EntityId>::new());
    assert_eq!(term_hits(&source, "survivor-entry"), vec![document(3)]);
    assert_eq!(term_hits(&source, "inserted-entry"), vec![inserted]);
    drop(source);

    let cold_second = TantivySource::open_or_build_in_dir(&second, Limits::default(), &root)
        .expect("reopen sparse second generation");
    assert_eq!(term_hits(&cold_second, "first-entry"), vec![document(1)]);
    assert_eq!(term_hits(&cold_second, "deleted-middle"), Vec::<EntityId>::new());
    assert_eq!(term_hits(&cold_second, "survivor-entry"), vec![document(3)]);
    assert_eq!(term_hits(&cold_second, "inserted-entry"), vec![inserted]);
    drop(cold_second);

    let third = state_for(
        vec![
            (document(3), vec![("name".into(), "survivor-revised".into())]),
            (inserted, vec![("name".into(), "inserted-entry".into())]),
        ],
        [63; 32],
    );
    let (source, revision) = TantivySource::open_or_advance_in_dir(
        &second,
        &third,
        Limits::default(),
        OverlayLimits::default(),
        &root,
    )
    .expect("apply a second durable delta");
    assert_eq!(revision.map(|revision| revision.kind), Some(ProjectionKind::Revised));
    drop(source);

    let cold_third = TantivySource::open_or_build_in_dir(&third, Limits::default(), &root)
        .expect("reopen second sparse generation");
    assert_eq!(term_hits(&cold_third, "first-entry"), Vec::<EntityId>::new());
    assert_eq!(term_hits(&cold_third, "deleted-middle"), Vec::<EntityId>::new());
    assert_eq!(term_hits(&cold_third, "survivor-entry"), Vec::<EntityId>::new());
    assert_eq!(term_hits(&cold_third, "survivor-revised"), vec![document(3)]);
    assert_eq!(term_hits(&cold_third, "inserted-entry"), vec![inserted]);
    drop(cold_third);

    let fingerprint = projection_fingerprint(third.binding());
    let selected = root.join(DURABLE_ROOTS_DIRECTORY).join(hex_fingerprint(fingerprint));
    let ordinal_path = selected.join(ORDINAL_MAP_FILE);
    let mut ordinal_map = std::fs::read(&ordinal_path).expect("read ordinal map");
    let first_identity = ORDINAL_MAP_MAGIC.len() + 32 + 8 + 8 + 8;
    let original_ordinal_map = ordinal_map.clone();
    ordinal_map[first_identity] ^= 0x80;
    std::fs::write(&ordinal_path, ordinal_map).expect("damage ordinal identity");
    write_projection_manifest(&selected, fingerprint, DurableCacheBudget::default())
        .expect("refresh integrity manifest");
    assert!(matches!(
        TantivySource::open_in_dir(&third, Limits::default(), &selected),
        Err(TantivySourceError::Corrupt(_))
    ));
    ordinal_map = original_ordinal_map;
    let record_size = 8 + 32 + 32 + 4 + 32;
    let first_payload = ORDINAL_MAP_MAGIC.len() + 32 + 8 + 8 + 8;
    let second_payload = first_payload + record_size;
    let first_record_tail = ordinal_map[first_payload..first_payload + record_size - 8].to_vec();
    let second_record_tail = ordinal_map[second_payload..second_payload + record_size - 8].to_vec();
    ordinal_map[first_payload..first_payload + record_size - 8].copy_from_slice(&second_record_tail);
    ordinal_map[second_payload..second_payload + record_size - 8].copy_from_slice(&first_record_tail);
    std::fs::write(&ordinal_path, ordinal_map).expect("swap ordinal identities");
    write_projection_manifest(&selected, fingerprint, DurableCacheBudget::default())
        .expect("refresh integrity manifest");
    assert!(matches!(
        TantivySource::open_in_dir(&third, Limits::default(), &selected),
        Err(TantivySourceError::Corrupt(_))
    ));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn malformed_selected_root_and_interrupted_stage_rebuild_from_authoritative_state() {
    static NEXT_ROOT: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "backend-tantivy-recovery-{}-{}",
        std::process::id(),
        NEXT_ROOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let state = state_for(
        vec![(document(9), vec![("name".into(), "survivor".into())])],
        [51; 32],
    );
    let first = TantivySource::open_or_build_in_dir(&state, Limits::default(), &root)
        .expect("first durable projection");
    assert_eq!(term_hits(&first, "survivor"), vec![document(9)]);
    drop(first);

    let key = hex_fingerprint(projection_fingerprint(state.binding()));
    let version_root = root.join(DURABLE_ROOTS_DIRECTORY);
    let selected = version_root.join(key);
    let incomplete = version_root.join(format!(".{}.building-1-1", "0".repeat(64)));
    std::fs::create_dir(&incomplete).expect("incomplete staging directory");
    std::fs::write(selected.join(BINDING_FILE), b"truncated authority stamp")
        .expect("corrupt generation marker");

    let other = state_for(
        vec![(document(9), vec![("name".into(), "impostor".into())])],
        [52; 32],
    );
    assert!(matches!(
        TantivySource::open_in_dir(&other, Limits::default(), &selected),
        Err(TantivySourceError::Contract(Error::StaleRoot))
    ));

    let recovered = TantivySource::open_or_build_in_dir(&state, Limits::default(), &root)
        .expect("rebuild malformed selected root");
    assert_eq!(term_hits(&recovered, "survivor"), vec![document(9)]);
    assert_eq!(term_hits(&recovered, "impostor"), Vec::<EntityId>::new());
    assert!(!incomplete.exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn durable_root_pin_survives_cross_process_pruning_then_releases_for_eviction() {
    const CHILD_ROOT: &str = "BACKEND_TANTIVY_PIN_TEST_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let root = std::path::PathBuf::from(root);
        let initial = state_for(
            vec![(document(1), vec![("name".into(), "pinnedroot".into())])],
            [61; 32],
        );
        let source = TantivySource::open_or_build_in_dir(&initial, Limits::default(), &root)
            .expect("child opens and pins selected root");
        assert_eq!(term_hits(&source, "pinnedroot"), vec![document(1)]);
        println!("PINNED");
        std::io::Write::flush(&mut std::io::stdout()).expect("flush child readiness");
        let mut release = [0_u8; 1];
        std::io::Read::read_exact(&mut std::io::stdin(), &mut release)
            .expect("wait for parent release");
        drop(source);
        return;
    }

    let root = std::env::temp_dir().join(format!(
        "backend-tantivy-reader-pin-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let executable = std::env::current_exe().expect("test executable");
    let mut child = std::process::Command::new(executable)
        .arg("--exact")
        .arg("tests::durable_root_pin_survives_cross_process_pruning_then_releases_for_eviction")
        .arg("--nocapture")
        .env(CHILD_ROOT, &root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("spawn independent reader process");
    let mut child_output = std::io::BufReader::new(child.stdout.take().expect("child stdout"));
    let mut line = String::new();
    loop {
        line.clear();
        std::io::BufRead::read_line(&mut child_output, &mut line)
            .expect("read child readiness");
        if line.trim() == "PINNED" {
            break;
        }
        assert!(!line.is_empty(), "child exited before pinning its root");
    }

    let initial = state_for(
        vec![(document(1), vec![("name".into(), "pinnedroot".into())])],
        [61; 32],
    );
    let old_key = hex_fingerprint(projection_fingerprint(initial.binding()));
    let old_root = root.join(DURABLE_ROOTS_DIRECTORY).join(old_key);
    for ordinal in 2..=8 {
        let next = state_for(
            vec![(document(ordinal), vec![("name".into(), format!("revision{ordinal}"))])],
            [ordinal as u8; 32],
        );
        let source = TantivySource::open_or_build_in_dir(&next, Limits::default(), &root)
            .expect("publish newer immutable root");
        drop(source);
    }
    assert!(old_root.is_dir(), "active cross-process root must stay on disk");
    let version_root = root.join(DURABLE_ROOTS_DIRECTORY);
    for ordinal in 2..=4 {
        let revision = state_for(
            vec![(document(ordinal), vec![("name".into(), format!("revision{ordinal}"))])],
            [ordinal as u8; 32],
        );
        let path = version_root.join(hex_fingerprint(projection_fingerprint(revision.binding())));
        assert!(!path.exists(), "older unpinned revision {ordinal} should be pruned");
    }
    for ordinal in 5..=8 {
        let revision = state_for(
            vec![(document(ordinal), vec![("name".into(), format!("revision{ordinal}"))])],
            [ordinal as u8; 32],
        );
        let path = version_root.join(hex_fingerprint(projection_fingerprint(revision.binding())));
        assert!(path.is_dir(), "recent unpinned revision {ordinal} should be retained");
    }
    let retained_root_count = std::fs::read_dir(&version_root)
        .expect("list retained roots")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .count();
    assert_eq!(retained_root_count, MAX_RETAINED_DURABLE_ROOTS + 1);

    let mut child_stdin = child.stdin.take().expect("child stdin");
    std::io::Write::write_all(&mut child_stdin, b"x").expect("release child root lease");
    assert!(child.wait().expect("wait for child").success());

    let latest = state_for(
        vec![(document(8), vec![("name".into(), "revision8".into())])],
        [8; 32],
    );
    let reopened = TantivySource::open_or_build_in_dir(&latest, Limits::default(), &root)
        .expect("prune released old root");
    assert_eq!(term_hits(&reopened, "revision8"), vec![document(8)]);
    assert!(!old_root.exists(), "released old root should be evicted");
    drop(reopened);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn pinned_and_selected_root_bytes_remain_charged_after_evicting_unretained_roots() {
    let root = std::env::temp_dir().join(format!(
        "backend-tantivy-pin-quota-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("create cache root");
    let root_path = |digit: char| root.join(digit.to_string().repeat(64));
    let selected = root_path('0');
    let pinned = root_path('1');
    let evictable_a = root_path('2');
    let evictable_b = root_path('3');
    for path in [&selected, &pinned, &evictable_a, &evictable_b] {
        std::fs::create_dir(path).expect("create generation root");
        std::fs::write(path.join(BINDING_FILE), [0_u8; 32]).expect("binding stamp");
        std::fs::write(path.join(".last-used"), [0_u8; 16]).expect("use stamp");
    }
    let lease = crate::engine::test_support::pin_durable_root_for_test(&pinned)
        .expect("hold independent reader pin");
    let budget = DurableCacheBudget::new(95).expect("nonzero byte budget");
    assert!(matches!(
        crate::engine::test_support::prune_durable_roots_for_test(&root, &selected, budget),
        Err(TantivySourceError::BudgetExceeded {
            budget_bytes: 95,
            required_bytes: 96,
        })
    ));
    assert!(selected.is_dir());
    assert!(pinned.is_dir());
    assert!(!evictable_a.exists());
    assert!(!evictable_b.exists());
    drop(lease);
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn durable_manifest_reads_do_not_follow_symlinks_or_allocate_unbounded_bytes() {
    let root = std::env::temp_dir().join(format!(
        "backend-tantivy-manifest-bounds-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let state = state_for(
        vec![(document(7), vec![("name".into(), "recoverable".into())])],
        [71; 32],
    );
    let source = TantivySource::open_or_build_in_dir(&state, Limits::default(), &root)
        .expect("initial selected root");
    drop(source);
    let key = hex_fingerprint(projection_fingerprint(state.binding()));
    let selected = root.join(DURABLE_ROOTS_DIRECTORY).join(key);

    let external = root.join("external-manifest-target");
    std::fs::write(&external, b"outside bytes stay unchanged").expect("external sentinel");
    std::fs::remove_file(selected.join(INTEGRITY_FILE)).expect("remove manifest");
    std::os::unix::fs::symlink(&external, selected.join(INTEGRITY_FILE))
        .expect("insert final-component symlink");
    let recovered = TantivySource::open_or_build_in_dir(&state, Limits::default(), &root)
        .expect("rebuild root with symlink manifest");
    assert_eq!(term_hits(&recovered, "recoverable"), vec![document(7)]);
    drop(recovered);
    assert_eq!(std::fs::read(&external).expect("external target"), b"outside bytes stay unchanged");

    std::fs::write(
        selected.join(INTEGRITY_FILE),
        vec![0_u8; usize::try_from(MAX_PROJECTION_MANIFEST_BYTES).expect("bound") + 1],
    )
    .expect("write oversized manifest");
    let recovered_again = TantivySource::open_or_build_in_dir(&state, Limits::default(), &root)
        .expect("recover from oversized manifest");
    assert_eq!(term_hits(&recovered_again, "recoverable"), vec![document(7)]);
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn transient_selected_root_io_failure_preserves_the_durable_generation() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = std::env::temp_dir().join(format!(
        "backend-tantivy-transient-root-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let state = state_for(
        vec![(document(9), vec![("name".into(), "keepoldroot".into())])],
        [91; 32],
    );
    let source = TantivySource::open_or_build_in_dir(&state, Limits::default(), &root)
        .expect("publish selected root");
    drop(source);
    let key = hex_fingerprint(projection_fingerprint(state.binding()));
    let selected = root.join(DURABLE_ROOTS_DIRECTORY).join(key);
    let original_manifest = std::fs::read(selected.join(INTEGRITY_FILE)).expect("manifest bytes");
    std::fs::set_permissions(&selected, std::fs::Permissions::from_mode(0o000))
        .expect("make root unreadable where permissions are enforced");
    let reopened = TantivySource::open_or_build_in_dir(&state, Limits::default(), &root);
    std::fs::set_permissions(&selected, std::fs::Permissions::from_mode(0o700))
        .expect("restore root permissions");

    match reopened {
        Err(TantivySourceError::Io(error)) => {
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            assert!(selected.is_dir(), "transient read failure must retain the old root");
            assert_eq!(
                std::fs::read(selected.join(INTEGRITY_FILE)).expect("retained manifest"),
                original_manifest
            );
        }
        Ok(source) => drop(source), // Some test runners can bypass mode-bit permissions.
        Err(error) => panic!("unexpected selected-root recovery error: {error}"),
    }
    let _ = std::fs::remove_dir_all(root);
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
fn large_unchanged_corpus_rebind_keeps_exact_results_under_the_new_root() {
    const ROWS: u64 = 2_048;
    let documents = (1..=ROWS)
        .map(|ordinal| {
            (
                document(ordinal),
                vec![("name".into(), "stable shared-corpus-token".into())],
            )
        })
        .collect::<Vec<_>>();
    let expected = {
        let mut ids = documents
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    };
    let current = state_for(documents.clone(), [0x31; 32]);
    let mut source = TantivySource::build(&current, Limits::default()).expect("projection");
    let postings_before = source.indexed_postings();
    let next = state_for(documents, [0x32; 32]);

    let outcome = source
        .maintain(&next, OverlayLimits::default())
        .expect("rebind the unchanged corpus");
    assert_eq!(
        outcome,
        MaintainOutcome::Applied(ProjectionRevision {
            kind: ProjectionKind::Rebound,
            rewritten_documents: 0,
            retired_postings: 0,
            added_postings: 0,
        })
    );
    assert_eq!(source.indexed_postings(), postings_before);
    assert_eq!(
        source
            .search(&Query::new(vec!["stable".into()], Limits::default()).expect("query"))
            .expect("search rebound corpus")
            .into_iter()
            .map(|hit| hit.document)
            .collect::<Vec<_>>(),
        expected
    );

    let stale_query = Query::new(vec!["stable".into()], Limits::default()).expect("query");
    assert!(matches!(
        LexicalSource::fetch(
            &source,
            &QueryRequest {
                binding: current.binding(),
                query: stale_query.clone(),
                cursor: None,
                limit: 8,
            }
        ),
        Err(TantivySourceError::Contract(Error::StaleRoot))
    ));
    let first_page = LexicalSource::fetch(
        &source,
        &QueryRequest {
            binding: next.binding(),
            query: stale_query,
            cursor: None,
            limit: 8,
        },
    )
    .expect("new binding is admitted");
    assert_eq!(first_page.total, ROWS as usize);
    assert_eq!(
        first_page
            .hits
            .iter()
            .map(|hit| hit.document)
            .collect::<Vec<_>>(),
        expected[..8]
    );
}

#[test]
fn one_document_revision_deletes_only_that_documents_postings() {
    let original = vec![
        (document(1), vec![("name".into(), "alpha".into())]),
        (document(2), vec![("name".into(), "beta".into())]),
        (document(3), vec![("name".into(), "gamma".into())]),
    ];
    let current = state_for(original, [1; 32]);
    let mut source = TantivySource::build(&current, Limits::default()).expect("projection");
    let beta = Query::new(vec!["beta".into()], Limits::default()).expect("beta");
    let before_page = LexicalSource::fetch(
        &source,
        &QueryRequest {
            binding: current.binding(),
            query: beta.clone(),
            cursor: None,
            limit: 8,
        },
    )
    .expect("cached beta");
    assert_eq!(before_page.total, 1);
    let before = source.indexed_postings();
    let revised = vec![
        (document(1), vec![("name".into(), "alpha".into())]),
        (document(2), vec![("name".into(), "zephyr".into())]),
        (document(3), vec![("name".into(), "gamma".into())]),
    ];
    let revised_state = state_for(revised, [2; 32]);
    let outcome = source
        .maintain(&revised_state, OverlayLimits::default())
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
    let after_page = LexicalSource::fetch(
        &source,
        &QueryRequest {
            binding: revised_state.binding(),
            query: beta,
            cursor: None,
            limit: 8,
        },
    )
    .expect("beta after revision");
    assert_eq!(after_page.total, 0);
    assert!(after_page.hits.is_empty());
    assert_eq!(source.rank_evaluations(), 2);
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
