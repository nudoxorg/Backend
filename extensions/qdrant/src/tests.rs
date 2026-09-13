//! Adversarial vector extension tests.

#![allow(
    clippy::expect_used,
    reason = "test fixtures use expect to make invariant failures local"
)]

use super::*;
use backend_version::{
    AuthorityScopeClaim, Coverage, CoverageWitness, ProducerObservationClaims,
    ProducerObservationVerifier, RelationState, ScopeRoot, UntrustedProducerObservation,
    WorkspaceManifest, WorkspaceRoot, admit_complete_scope, admit_producer_observation,
};
use std::sync::Arc;

struct FixtureCoverageVerifier {
    producer: [u8; 32],
    scope: ScopeRoot,
    context: [u8; 32],
    evidence: Vec<u8>,
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

pub(crate) fn binding(ids: &[(u64, Vec<u8>)]) -> (Binding, CoverageWitness) {
    let coverage = authorized_coverage(&[7; 32]);
    let state = RelationState::<CandidateRelation>::from_entries(ids.iter().cloned(), coverage)
        .expect("state");
    let recipe = Recipe::from_value(&[1; 32]);
    let authority = Authority::from_value(&[2; 32]);
    let read_manifest = ReadManifest::from_value(b"reads".as_slice());
    (
        Binding::new(workspace(), state.root(), recipe, authority, read_manifest),
        coverage,
    )
}

fn vector_overlay_fixture() -> (CandidateState, VectorIndex<MemorySource>, ModelVersion) {
    let coverage = authorized_coverage(&[7; 32]);
    let model = ModelVersion::from_value(&[8; 32]);
    let first = VectorPoint::new(CandidateId(1), vec![0.0, 0.0])
        .expect("point")
        .to_payload();
    let second = VectorPoint::new(CandidateId(2), vec![10.0, 10.0])
        .expect("point")
        .to_payload();
    let (binding, _) = binding(&[(1, first.clone()), (2, second.clone())]);
    let state = CandidateState::new(
        binding,
        coverage,
        vec![(CandidateId(1), first), (CandidateId(2), second)],
        Limits::default(),
    )
    .expect("state");
    let facts = VectorFacts::new(state.clone(), model, Metric::EuclideanSquared, 2).expect("facts");
    let base = AnnBase::from_facts(&facts, SearchQuality::Exact, Limits::default()).expect("base");
    let source = MemorySource::new(
        facts.binding(),
        facts.coverage(),
        vec![CandidateId(1), CandidateId(2)],
        Limits::default(),
    )
    .expect("source");
    let index = VectorIndex::new(base, facts, source).expect("index");
    (state, index, model)
}

fn restarted_vector_index(
    state: &CandidateState,
    model: ModelVersion,
) -> VectorIndex<MemorySource> {
    let facts = VectorFacts::new(
        CandidateState::new(
            state.binding(),
            state.coverage(),
            state
                .iter()
                .map(|(id, payload)| (id, payload.to_vec()))
                .collect(),
            Limits::default(),
        )
        .expect("restarted state"),
        model,
        Metric::EuclideanSquared,
        2,
    )
    .expect("restarted facts");
    let base = AnnBase::from_facts(&facts, SearchQuality::Exact, Limits::default())
        .expect("restarted base");
    let source = MemorySource::new(
        facts.binding(),
        facts.coverage(),
        facts.ids().collect(),
        Limits::default(),
    )
    .expect("restarted source");
    VectorIndex::new(base, facts, source).expect("restarted index")
}

fn embedding_recipe() -> EmbeddingRecipe {
    EmbeddingRecipe {
        model: ModelVersion::from_value(&[8; 32]),
        tokenizer: TokenizerVersion::from_value(&[9; 32]),
        dimensions: std::num::NonZeroU32::new(2).expect("non-zero dimensions"),
        metric: Metric::CosineDistance,
        pooling: EmbeddingPooling::Mean,
        normalization: EmbeddingNormalization::UnitL2,
        encoding: EmbeddingEncoding::Float32,
        query_treatment: TreatmentVersion::from_value(b"query: ".as_slice()),
        document_treatment: TreatmentVersion::from_value(b"passage: ".as_slice()),
    }
}

#[test]
fn embedding_recipe_separates_query_and_document_treatment() {
    let recipe = embedding_recipe();
    let document =
        DocumentVector::new(recipe, CandidateId(1), vec![0.6, 0.8]).expect("document vector");
    let query = QueryVector::new(recipe, vec![0.0, 1.0]).expect("query vector");
    assert_eq!(document.recipe().version(), query.recipe().version());
    assert_eq!(document.point().values(), &[0.6, 0.8]);
    assert_eq!(query.query().values(), &[0.0, 1.0]);

    let changed = EmbeddingRecipe {
        query_treatment: TreatmentVersion::from_value(b"search_query: ".as_slice()),
        ..recipe
    };
    assert_ne!(recipe.version(), changed.version());
    let mut legacy = Vec::new();
    legacy.extend_from_slice(b"backend.qdrant.embedding-recipe.v1\0");
    legacy.extend_from_slice(recipe.model.as_bytes());
    legacy.extend_from_slice(recipe.tokenizer.as_bytes());
    legacy.extend_from_slice(&recipe.dimensions.get().to_be_bytes());
    legacy.extend_from_slice(&[
        recipe.metric as u8,
        recipe.pooling as u8,
        recipe.normalization as u8,
        recipe.encoding as u8,
    ]);
    legacy.extend_from_slice(recipe.query_treatment.as_bytes());
    legacy.extend_from_slice(recipe.document_treatment.as_bytes());
    assert_ne!(recipe.version(), Recipe::from_value(&legacy));
    assert_eq!(
        QueryVector::new(recipe, vec![1.0, 1.0]),
        Err(Error::MalformedInput)
    );
}

#[test]
fn document_vectors_retain_shared_cached_coordinates() {
    let recipe = embedding_recipe();
    let coordinates: Arc<[f32]> = Arc::from([0.6, 0.8]);
    let pointer = coordinates.as_ptr();
    let document = DocumentVector::from_shared(recipe, CandidateId(1), coordinates)
        .expect("shared document vector");

    assert_eq!(document.point().values().as_ptr(), pointer);
}

#[derive(Clone)]
struct RefillingSource {
    coverage: CoverageWitness,
}

#[derive(Clone)]
struct BoundedRequestSource {
    coverage: CoverageWitness,
}

impl AnnSource for BoundedRequestSource {
    type Error = Error;

    fn fetch(&self, request: &VectorSearchRequest) -> Result<AnnPage, Self::Error> {
        assert_eq!(request.limit, 4, "one result needs at most four candidates");
        Ok(AnnPage {
            schema: SchemaVersion::CURRENT,
            binding: request.binding,
            ids: vec![CandidateId(1)],
            next: None,
            coverage: self.coverage,
            quality: SearchQuality::Exact,
        })
    }
}

#[test]
fn vector_search_bounds_the_provider_page_to_the_refill_target() {
    let (state, memory_index, model) = vector_overlay_fixture();
    let facts = VectorFacts::new(state, model, Metric::EuclideanSquared, 2).expect("facts");
    let index = VectorIndex::new(
        memory_index.base().clone(),
        facts.clone(),
        BoundedRequestSource {
            coverage: facts.coverage(),
        },
    )
    .expect("index");
    let query = VectorQuery::new(model, Metric::EuclideanSquared, vec![0.0, 0.0]).expect("query");
    let result = index
        .search(&query, 1, Limits::default())
        .expect("bounded search");
    assert_eq!(result.candidates.len(), 1);
}

impl AnnSource for RefillingSource {
    type Error = Error;

    fn fetch(&self, request: &VectorSearchRequest) -> Result<AnnPage, Self::Error> {
        let offset = request.cursor.map_or(0, AnnCursor::offset);
        let (ids, next) = match offset {
            0 => (
                vec![CandidateId(98), CandidateId(99)],
                Some(AnnCursor::new(request.binding, 2)),
            ),
            2 => (vec![CandidateId(1), CandidateId(2)], None),
            _ => return Err(Error::InvalidCursor),
        };
        Ok(AnnPage {
            schema: SchemaVersion::CURRENT,
            binding: request.binding,
            ids,
            next,
            coverage: self.coverage,
            quality: SearchQuality::Exact,
        })
    }
}

#[test]
fn vector_search_refills_after_a_page_of_stale_provider_ids() {
    let (state, memory_index, model) = vector_overlay_fixture();
    let facts = VectorFacts::new(state, model, Metric::EuclideanSquared, 2).expect("facts");
    let base = memory_index.base().clone();
    let index = VectorIndex::new(
        base,
        facts.clone(),
        RefillingSource {
            coverage: facts.coverage(),
        },
    )
    .expect("index");
    let query = VectorQuery::new(model, Metric::EuclideanSquared, vec![0.0, 0.0]).expect("query");
    let result = index
        .search(
            &query,
            2,
            Limits {
                max_page: 2,
                ..Limits::default()
            },
        )
        .expect("refilled search");
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>(),
        vec![CandidateId(1), CandidateId(2)]
    );
}

#[derive(Clone)]
struct SubstitutingQuerySource {
    coverage: CoverageWitness,
}

impl AnnSource for SubstitutingQuerySource {
    type Error = Error;

    fn fetch(&self, request: &VectorSearchRequest) -> Result<AnnPage, Self::Error> {
        let other = VectorQuery::new(
            request.query.model(),
            request.query.metric(),
            vec![7.0; request.query.values().len()],
        )?;
        let binding =
            VectorQueryBinding::new(request.binding.base, &other, request.binding.requested)?;
        Ok(AnnPage {
            schema: SchemaVersion::CURRENT,
            binding,
            ids: vec![CandidateId(1)],
            next: None,
            coverage: self.coverage,
            quality: SearchQuality::Exact,
        })
    }
}

#[test]
fn provider_page_for_another_query_is_rejected_at_the_binding() {
    let (state, memory_index, model) = vector_overlay_fixture();
    let facts = VectorFacts::new(state, model, Metric::EuclideanSquared, 2).expect("facts");
    let index = VectorIndex::new(
        memory_index.base().clone(),
        facts.clone(),
        SubstitutingQuerySource {
            coverage: facts.coverage(),
        },
    )
    .expect("index");
    let query = VectorQuery::new(model, Metric::EuclideanSquared, vec![0.0, 0.0]).expect("query");
    assert!(matches!(
        index.search(&query, 1, Limits::default()),
        Err(AdapterError::Extension(Error::StaleRoot))
    ));
}

#[test]
fn stale_root_is_rejected_before_rerank() {
    let (binding, coverage) = binding(&[(1, vec![1])]);
    let accepted = accept_remote(
        binding.root,
        binding.recipe,
        binding.authority,
        coverage,
        Tombstones(Vec::new()),
        vec![CandidateId(1)],
    )
    .expect("accepted");
    let other = RelationState::<CandidateRelation>::from_entries([(1, vec![2])], coverage)
        .expect("other state")
        .root();
    assert_eq!(rerank(&accepted, other, |_| 1), Err(Error::StaleRoot));
}

#[test]
fn delete_and_readd_are_distinct_exact_transitions() {
    let (binding, coverage) = binding(&[(1, vec![1])]);
    let state = CandidateState::new(
        binding,
        coverage,
        vec![(CandidateId(1), vec![1])],
        Limits::default(),
    )
    .expect("state");
    let deletion = state
        .prepare_delta(vec![CandidateChange::Delete { id: CandidateId(1) }])
        .expect("delete");
    let deleted = state.apply_delta(&deletion).expect("apply delete");
    assert_eq!(deleted.iter().count(), 0);
    let readd = deleted
        .prepare_delta(vec![CandidateChange::Upsert {
            id: CandidateId(1),
            payload: vec![9],
        }])
        .expect("readd");
    let restored = deleted.apply_delta(&readd).expect("apply readd");
    assert_eq!(
        restored.iter().collect::<Vec<_>>(),
        vec![(CandidateId(1), &[9][..])]
    );
    assert_ne!(deletion.delta.id(), readd.delta.id());
}

#[test]
fn delta_frontier_is_bound_to_the_state() {
    let (binding, coverage) = binding(&[(1, vec![1])]);
    let state = CandidateState::new(
        binding,
        coverage,
        vec![(CandidateId(1), vec![1])],
        Limits::default(),
    )
    .expect("state");
    let mut delta = state
        .prepare_delta(vec![CandidateChange::Delete { id: CandidateId(1) }])
        .expect("delta");
    delta.binding = delta.binding.with_frontier(Frontier::from_value(&[9; 32]));
    assert_eq!(state.apply_delta(&delta), Err(Error::StaleRoot));
}

#[test]
fn delta_payload_bytes_are_bounded_across_the_whole_change() {
    let (binding, coverage) = binding(&[]);
    let limits = Limits {
        max_total_payload_bytes: 2,
        ..Limits::default()
    };
    let state = CandidateState::new(binding, coverage, Vec::new(), limits).expect("empty state");
    assert_eq!(
        state.prepare_delta(vec![
            CandidateChange::Upsert {
                id: CandidateId(1),
                payload: vec![1, 2],
            },
            CandidateChange::Upsert {
                id: CandidateId(2),
                payload: vec![3],
            },
        ]),
        Err(Error::SizeLimit)
    );
}

#[test]
fn paging_rejects_stale_cursor_and_orders_ids() {
    let (binding, coverage) = binding(&[]);
    let source = MemorySource::new(
        binding,
        coverage,
        vec![CandidateId(3), CandidateId(1), CandidateId(2)],
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
        .search(SearchRequest::first(binding, 2))
        .expect("first page");
    assert_eq!(first.ids, vec![CandidateId(1), CandidateId(2)]);
    let cursor = first.next.expect("next cursor");
    let mut stale = binding;
    stale.root = RelationState::<CandidateRelation>::from_entries([(1, vec![4])], coverage)
        .expect("stale state")
        .root();
    assert!(matches!(
        adapter.search(SearchRequest {
            binding: stale,
            cursor: Some(cursor),
            limit: 2,
        }),
        Err(AdapterError::Extension(Error::StaleCursor))
    ));
}

#[test]
fn malformed_and_oversized_inputs_are_rejected() {
    assert_eq!(CandidateId::new(0), Err(Error::MalformedInput));
    assert_eq!(
        Limits::default().validate().expect("valid limits").max_page,
        256
    );
    assert_eq!(
        Tombstones::new(
            vec![CandidateId(1), CandidateId(1)],
            Limits {
                max_tombstones: 0,
                ..Limits::default()
            },
        ),
        Err(Error::InvalidLimits)
    );
    let coverage = incomplete_coverage(1, Coverage::Partial);
    let (binding, _) = binding(&[]);
    assert_eq!(
        MemorySource::new(binding, coverage, Vec::new(), Limits::default()),
        Err(Error::IncompleteCoverage)
    );
}

struct SubstitutingSource {
    page: CandidatePage,
}

impl CandidateSource for SubstitutingSource {
    type Error = ();

    fn fetch(&self, _: &SearchRequest) -> Result<CandidatePage, Self::Error> {
        Ok(self.page.clone())
    }
}

#[test]
fn provider_substitution_cannot_change_binding_or_coverage() {
    let (binding, coverage) = binding(&[]);
    let mut wrong = binding;
    wrong.root = RelationState::<CandidateRelation>::from_entries([(1, vec![9])], coverage)
        .expect("wrong root")
        .root();
    let page = CandidatePage {
        schema: SchemaVersion::CURRENT,
        binding: wrong,
        ids: vec![CandidateId(1)],
        next: None,
        coverage,
        quality: SearchQuality::Exact,
    };
    let adapter = Adapter::new(SubstitutingSource { page }, Limits::default()).expect("adapter");
    let request = SearchRequest::first(binding, 1);
    assert!(matches!(
        adapter.search(request),
        Err(AdapterError::Extension(Error::StaleRoot))
    ));

    let incomplete = CandidatePage {
        schema: SchemaVersion::CURRENT,
        binding,
        ids: vec![CandidateId(1)],
        next: None,
        coverage: incomplete_coverage(7, Coverage::Partial),
        quality: SearchQuality::Exact,
    };
    let adapter =
        Adapter::new(SubstitutingSource { page: incomplete }, Limits::default()).expect("adapter");
    assert!(matches!(
        adapter.search(request),
        Err(AdapterError::Extension(Error::IncompleteCoverage))
    ));
}

#[test]
fn forged_complete_coverage_is_rejected_at_provider_boundary() {
    let (binding, _) = binding(&[]);
    let forged = forged_complete_coverage();
    assert_eq!(
        MemorySource::new(binding, forged, Vec::new(), Limits::default()),
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
fn exact_overlay_hides_stale_base_and_reranks_fresh_points() {
    let coverage = authorized_coverage(&[7; 32]);
    let model = ModelVersion::from_value(&[9; 32]);
    let first = VectorPoint::new(CandidateId(1), vec![10.0, 10.0])
        .expect("point")
        .to_payload();
    let second = VectorPoint::new(CandidateId(2), vec![8.0, 8.0])
        .expect("point")
        .to_payload();
    let state = CandidateState::new(
        binding(&[(1, first), (2, second)]).0,
        coverage,
        vec![
            (
                CandidateId(1),
                VectorPoint::new(CandidateId(1), vec![10.0, 10.0])
                    .expect("point")
                    .to_payload(),
            ),
            (
                CandidateId(2),
                VectorPoint::new(CandidateId(2), vec![8.0, 8.0])
                    .expect("point")
                    .to_payload(),
            ),
        ],
        Limits::default(),
    )
    .expect("state");
    let facts = VectorFacts::new(state.clone(), model, Metric::EuclideanSquared, 2).expect("facts");
    let base = AnnBase::from_facts(&facts, SearchQuality::Exact, Limits::default()).expect("base");
    let source = MemorySource::new(
        facts.binding(),
        facts.coverage(),
        vec![CandidateId(1), CandidateId(2)],
        Limits::default(),
    )
    .expect("source");
    let index = VectorIndex::new(base, facts, source).expect("index");
    let delta = state
        .prepare_delta(vec![CandidateChange::Upsert {
            id: CandidateId(2),
            payload: VectorPoint::new(CandidateId(2), vec![0.1, 0.1])
                .expect("point")
                .to_payload(),
        }])
        .expect("delta");
    let target_state = state.apply_delta(&delta).expect("target");
    let target_facts =
        VectorFacts::new(target_state, model, Metric::EuclideanSquared, 2).expect("target facts");
    let index = match index
        .advance(&delta, target_facts, OverlayLimits::default())
        .expect("advance")
    {
        RefreshOutcome::Advanced(index) => index,
        other => {
            assert!(matches!(other, RefreshOutcome::Advanced(_)));
            return;
        }
    };
    let query = VectorQuery::new(model, Metric::EuclideanSquared, vec![0.0, 0.0]).expect("query");
    let result = index.search(&query, 2, Limits::default()).expect("search");
    assert_eq!(result.candidates[0].id, CandidateId(2));
    assert_eq!(result.binding().root, delta.binding.root);
    assert!(index.overlay().is_some());
}

#[test]
fn vector_overlay_delete_readd_matches_a_restarted_base() {
    let (state, index, model) = vector_overlay_fixture();

    let replacement = VectorPoint::new(CandidateId(2), vec![1.0, 1.0])
        .expect("replacement")
        .to_payload();
    let changed = state
        .prepare_delta(vec![CandidateChange::Upsert {
            id: CandidateId(2),
            payload: replacement,
        }])
        .expect("replacement delta");
    let changed_state = state.apply_delta(&changed).expect("changed state");
    let changed_facts = VectorFacts::new(changed_state.clone(), model, Metric::EuclideanSquared, 2)
        .expect("changed facts");
    let outcome = index
        .advance(&changed, changed_facts, OverlayLimits::default())
        .expect("advance replacement");
    assert!(matches!(&outcome, RefreshOutcome::Advanced(_)));
    let RefreshOutcome::Advanced(index) = outcome else {
        return;
    };

    let deletion = changed_state
        .prepare_delta(vec![CandidateChange::Delete { id: CandidateId(2) }])
        .expect("delete delta");
    let deleted_state = changed_state.apply_delta(&deletion).expect("deleted state");
    let deleted_facts = VectorFacts::new(deleted_state.clone(), model, Metric::EuclideanSquared, 2)
        .expect("deleted facts");
    let outcome = index
        .advance(&deletion, deleted_facts, OverlayLimits::default())
        .expect("advance deletion");
    assert!(matches!(&outcome, RefreshOutcome::Advanced(_)));
    let RefreshOutcome::Advanced(index) = outcome else {
        return;
    };
    let query = VectorQuery::new(model, Metric::EuclideanSquared, vec![0.0, 0.0]).expect("query");
    let deleted_result = index.search(&query, 2, Limits::default()).expect("search");
    assert_eq!(
        deleted_result
            .candidates
            .iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>(),
        vec![CandidateId(1)]
    );

    let readd = deleted_state
        .prepare_delta(vec![CandidateChange::Upsert {
            id: CandidateId(2),
            payload: VectorPoint::new(CandidateId(2), vec![2.0, 2.0])
                .expect("readd point")
                .to_payload(),
        }])
        .expect("readd delta");
    let restored_state = deleted_state.apply_delta(&readd).expect("restored state");
    let restored_facts =
        VectorFacts::new(restored_state.clone(), model, Metric::EuclideanSquared, 2)
            .expect("restored facts");
    let outcome = index
        .advance(&readd, restored_facts, OverlayLimits::default())
        .expect("advance readd");
    assert!(matches!(&outcome, RefreshOutcome::Advanced(_)));
    let RefreshOutcome::Advanced(index) = outcome else {
        return;
    };
    let incremental = index.search(&query, 2, Limits::default()).expect("search");

    let restarted = restarted_vector_index(&restored_state, model)
        .search(&query, 2, Limits::default())
        .expect("restarted search");
    assert_eq!(incremental.candidates, restarted.candidates);
    assert_eq!(incremental.binding, restarted.binding);
}

#[test]
fn ann_overlay_budget_requires_rebuild_and_never_claims_exactness() {
    let coverage = authorized_coverage(&[7; 32]);
    let point = VectorPoint::new(CandidateId(1), vec![1.0, 0.0])
        .expect("point")
        .to_payload();
    let (mut binding, _) = binding(&[(1, point.clone())]);
    let state = CandidateState::new(
        binding,
        coverage,
        vec![(CandidateId(1), point)],
        Limits::default(),
    )
    .expect("state");
    binding = state.binding();
    let model = ModelVersion::from_value(&[6; 32]);
    let facts = VectorFacts::new(state.clone(), model, Metric::CosineDistance, 2).expect("facts");
    let metadata = ApproximationMetadata::new(
        binding.recipe,
        model,
        RecallMetadata::new(800_000, None).expect("recall"),
    );
    let base = AnnBase::from_facts(
        &facts,
        SearchQuality::Approximate(metadata),
        Limits::default(),
    )
    .expect("base");
    let source = MemorySource::new(binding, coverage, vec![CandidateId(1)], Limits::default())
        .expect("source");
    let index = VectorIndex::new(base, facts, source).expect("index");
    let delta = state
        .prepare_delta(vec![CandidateChange::Upsert {
            id: CandidateId(1),
            payload: VectorPoint::new(CandidateId(1), vec![2.0, 0.0])
                .expect("point")
                .to_payload(),
        }])
        .expect("delta");
    let target_state = state.apply_delta(&delta).expect("target");
    let target_facts =
        VectorFacts::new(target_state, model, Metric::CosineDistance, 2).expect("target facts");
    let outcome = index
        .advance(
            &delta,
            target_facts,
            OverlayLimits {
                max_changed_points: 0,
                ..OverlayLimits::default()
            },
        )
        .expect_err("zero overlay budget");
    assert_eq!(outcome, Error::InvalidLimits);

    let outcome = index
        .advance(
            &delta,
            VectorFacts::new(
                state.apply_delta(&delta).expect("target"),
                model,
                Metric::CosineDistance,
                2,
            )
            .expect("target facts"),
            OverlayLimits {
                max_changed_points: 1,
                max_bytes: 1,
                ..OverlayLimits::default()
            },
        )
        .expect("budget decision");
    assert!(matches!(outcome, RefreshOutcome::RebuildRequired(_)));

    let query = VectorQuery::new(model, Metric::CosineDistance, vec![1.0, 0.0]).expect("query");
    let result = index.search(&query, 1, Limits::default()).expect("search");
    assert!(matches!(result.quality, SearchQuality::Approximate(_)));
}

#[test]
fn approximate_results_keep_recipe_model_recall_and_never_become_exact() {
    let (binding, coverage) = binding(&[]);
    let recall = RecallMetadata::new(900_000, Some(875_000)).expect("recall");
    let metadata =
        ApproximationMetadata::new(binding.recipe, ModelVersion::from_value(&[4; 32]), recall);
    let candidates = accept_remote_approximate(
        binding.root,
        binding.recipe,
        binding.authority,
        coverage,
        Tombstones(Vec::new()),
        vec![CandidateId(1), CandidateId(2)],
        metadata,
    )
    .expect("approximate candidates");
    assert_eq!(candidates.quality, SearchQuality::Approximate(metadata));
    assert!(!candidates.quality.is_exact());
    let reranked = rerank(&candidates, binding.root, |id| id.0).expect("rerank");
    assert_eq!(reranked.coverage, coverage);
    assert_eq!(reranked.quality, candidates.quality);
    assert!(!reranked.quality.is_exact());

    let wrong = ApproximationMetadata::new(
        Recipe::from_value(&[9; 32]),
        ModelVersion::from_value(&[4; 32]),
        recall,
    );
    assert_eq!(
        accept_remote_approximate(
            binding.root,
            binding.recipe,
            binding.authority,
            coverage,
            Tombstones(Vec::new()),
            vec![CandidateId(1)],
            wrong,
        ),
        Err(Error::ApproximationMismatch)
    );
    let forged = Candidates {
        quality: SearchQuality::Approximate(wrong),
        ..candidates
    };
    assert_eq!(
        rerank(&forged, binding.root, |_| 1),
        Err(Error::ApproximationMismatch)
    );
}

#[test]
fn provider_page_cannot_exceed_requested_bound() {
    let (binding, coverage) = binding(&[]);
    let page = CandidatePage {
        schema: SchemaVersion::CURRENT,
        binding,
        ids: vec![CandidateId(1), CandidateId(2)],
        next: None,
        coverage,
        quality: SearchQuality::Exact,
    };
    let adapter = Adapter::new(SubstitutingSource { page }, Limits::default()).expect("adapter");
    assert!(matches!(
        adapter.search(SearchRequest::first(binding, 1)),
        Err(AdapterError::Extension(Error::SizeLimit))
    ));
}
