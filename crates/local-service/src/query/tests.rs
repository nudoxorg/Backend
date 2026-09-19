#![allow(clippy::expect_used, clippy::too_many_lines)]

use super::*;
use backend_engine::{Fragment, Row, RowId, ViewCoverage};
use backend_extension_qdrant as semantic;
use backend_version::RelationState;
use std::num::NonZeroU32;

struct OfflineSource;

impl semantic::AnnSource for OfflineSource {
    type Error = &'static str;

    fn fetch(&self, _: &semantic::VectorSearchRequest) -> Result<semantic::AnnPage, Self::Error> {
        Err("offline")
    }
}

pub(super) fn selected_view() -> (backend_engine::WorkspaceRoot, backend_engine::ViewRoot) {
    let head = crate::builtin::genesis().expect("genesis");
    let (base, _) = crate::builtin::initial_view().expect("initial view");
    let capability = crate::builtin::test_builtin_view_capability().expect("capability");
    let package = backend_engine::package_key("pkg");
    let project = Row::new(RowId::Package(package), base.basis(), "pkg");
    let first = Row::in_package(
        RowId::Symbol(backend_engine::symbol_key("alpha::exact")),
        base.basis(),
        package,
        "alpha exact",
    )
    .with_signature("fn alpha()")
    .with_document(vec![Fragment::Text("first candidate".to_owned())]);
    let second = Row::in_package(
        RowId::Symbol(backend_engine::symbol_key("alpha::prefix")),
        base.basis(),
        package,
        "alphabet prefix",
    )
    .with_signature("fn alphabet()")
    .with_document(vec![Fragment::Text("second candidate".to_owned())]);
    let semantic_only = Row::in_package(
        RowId::Symbol(backend_engine::symbol_key("meaning::related")),
        base.basis(),
        package,
        "meaning related",
    )
    .with_document(vec![Fragment::Text("semantic-only candidate".to_owned())]);
    let view = backend_engine::ViewRoot::new_checked(
        base.recipe(),
        base.basis(),
        base.frontier(),
        vec![project, first, second, semantic_only],
        vec![ViewCoverage::Complete],
        capability,
    )
    .expect("selected view");
    (head.root(), view)
}

pub(super) fn semantic_evidence(
    workspace: backend_engine::WorkspaceRoot,
    view: &backend_engine::ViewRoot,
) -> backend_extension_trustfall::SemanticQueryCorpus {
    let package = backend_engine::package_key("pkg");
    let project = RowId::Package(package).stable_key();
    let profile =
        backend_semantic::vocabulary::LanguageProfile::Rust(backend_semantic::vocabulary::RustEdition::Rust2021);
    let facts = view
        .rows()
        .iter()
        .map(|row| {
            let evidence = if row.id == RowId::Package(package) {
                backend_extension_trustfall::SemanticQueryEvidence::Package(
                    backend_extension_trustfall::PackageScopeEvidence::new(package),
                )
            } else {
                backend_extension_trustfall::SemanticQueryEvidence::StructuralFallback(
                    backend_extension_trustfall::StructuralFallbackEvidence::new(
                        package, profile, [7; 32], [8; 32],
                    ),
                )
            };
            backend_extension_trustfall::SemanticQueryFact::new(
                evidence,
                backend_extension_trustfall::SemanticQueryPresentation {
                    id: row.id.stable_key(),
                    kind: row
                        .kind
                        .map_or("project", backend_engine::DeclarationKind::name)
                        .to_owned(),
                    coordinate: row.label.clone(),
                    name: row.label.clone(),
                    signature: row.signature.clone(),
                    documentation: row
                        .document
                        .iter()
                        .filter_map(|fragment| match fragment {
                            Fragment::Text(value) | Fragment::Code(value) => Some(value.as_str()),
                            Fragment::Link { label, .. } => Some(label.as_str()),
                            Fragment::Break => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                    score: None,
                    project: (row.id != RowId::Package(package)).then(|| project.clone()),
                    parent: None,
                    related: Box::new([]),
                },
            )
        })
        .collect();
    backend_extension_trustfall::SemanticQueryCorpus::admit(workspace, facts)
        .expect("typed semantic evidence")
}

#[test]
fn local_answer_is_complete_and_ranked_before_optional_work() {
    let (workspace, view) = selected_view();
    let evidence = semantic_evidence(workspace, &view);
    let coordinator = QueryCoordinator::new(
        workspace,
        view,
        crate::builtin::admitted_coverage().expect("coverage"),
        evidence,
    )
    .expect("coordinator");
    let local = coordinator
        .search_local(LocalQuery::prefix("alpha", 2).expect("query"))
        .expect("local search");
    assert_eq!(local.total_matches, 2);
    assert_eq!(local.rows[0].row.label, "alpha exact");
    assert!(
        local.rows[0]
            .lexical_relevance
            .is_some_and(backend_extension_tantivy::Relevance::is_exact)
    );
    assert_eq!(
        local.lanes[1].coverage,
        CoverageBasis::CompleteView { selected_rows: 4 }
    );
    let finished = local.finish();
    assert_eq!(finished.lanes[2].coverage, CoverageBasis::Unavailable);
}

#[test]
fn query_tokenization_matches_document_unicode_whitespace() {
    let query = LocalQuery::prefix("alpha\u{00a0}exact", 2).expect("query");
    assert_eq!(query.lexical().terms, ["alpha", "exact"]);

    let (workspace, view) = selected_view();
    let evidence = semantic_evidence(workspace, &view);
    let coordinator = QueryCoordinator::new(
        workspace,
        view,
        crate::builtin::admitted_coverage().expect("coverage"),
        evidence,
    )
    .expect("coordinator");
    let answer = coordinator.search_local(query).expect("local search");
    assert_eq!(answer.total_matches, 1);
    assert_eq!(answer.rows[0].row.label, "alpha exact");
}

#[test]
fn search_snapshot_owner_reuses_the_exact_published_selection() {
    let (workspace, view) = selected_view();
    let coverage = crate::builtin::admitted_coverage().expect("coverage");
    let mut owner = SearchSnapshotOwner::default();
    let evidence = semantic_evidence(workspace, &view);
    let first = std::sync::Arc::as_ptr(
        &owner
            .select(workspace, view.clone(), coverage, evidence.clone())
            .expect("first snapshot")
            .corpus,
    );
    let second = std::sync::Arc::as_ptr(
        &owner
            .select(workspace, view, coverage, evidence)
            .expect("reused snapshot")
            .corpus,
    );
    assert_eq!(first, second);
}

#[test]
fn semantic_candidate_identity_is_scoped_to_the_immutable_view() {
    let (workspace, _) = selected_view();
    let row = RowId::Symbol(backend_engine::symbol_key("same-declaration"));
    let entity = local::entity_id(workspace, row).expect("entity");
    let first = local::candidate_id(entity, [1; 32]).expect("first candidate");
    let second = local::candidate_id(entity, [2; 32]).expect("second candidate");
    assert_ne!(first, second);
}

fn recipe() -> semantic::EmbeddingRecipe {
    semantic::EmbeddingRecipe {
        model: semantic::ModelVersion::from_value(&[8; 32]),
        tokenizer: semantic::TokenizerVersion::from_value(&[9; 32]),
        dimensions: NonZeroU32::new(2).expect("dimension"),
        metric: semantic::Metric::EuclideanSquared,
        pooling: semantic::EmbeddingPooling::Mean,
        normalization: semantic::EmbeddingNormalization::None,
        encoding: semantic::EmbeddingEncoding::Float32,
        query_treatment: semantic::TreatmentVersion::from_value(b"query".as_slice()),
        document_treatment: semantic::TreatmentVersion::from_value(b"document".as_slice()),
    }
}

#[test]
fn semantic_lane_only_reorders_local_matches_and_suppresses_unknown_ids() {
    let (workspace, view) = selected_view();
    let coverage = crate::builtin::admitted_coverage().expect("coverage");
    let evidence = semantic_evidence(workspace, &view);
    let coordinator =
        QueryCoordinator::new(workspace, view, coverage, evidence).expect("coordinator");
    let local = coordinator
        .search_local(LocalQuery::prefix("alpha", 2).expect("query"))
        .expect("local search");
    let lexical_ids = local
        .matches
        .iter()
        .map(|(entity, _)| {
            local
                .corpus
                .candidates
                .iter()
                .find_map(|(candidate, selected)| (selected == entity).then_some(*candidate))
                .expect("lexical candidate")
        })
        .collect::<Vec<_>>();
    let lexical_entities = local
        .matches
        .iter()
        .map(|(entity, _)| *entity)
        .collect::<std::collections::BTreeSet<_>>();
    let semantic_only_row = RowId::Symbol(backend_engine::symbol_key("meaning::related"));
    let semantic_only_entity = local::entity_id(workspace, semantic_only_row).expect("entity");
    assert!(!lexical_entities.contains(&semantic_only_entity));
    let semantic_only = local
        .corpus
        .candidates
        .iter()
        .find_map(|(candidate, entity)| (*entity == semantic_only_entity).then_some(*candidate))
        .expect("semantic-only candidate");
    let unknown = semantic::CandidateId::new(u64::MAX).expect("unknown id");
    assert!(!local.corpus.candidates.contains_key(&unknown));
    let embedding = recipe();
    let payloads = vec![
        (
            lexical_ids[0].0,
            semantic::VectorPoint::new(lexical_ids[0], vec![1.0, 1.0])
                .expect("point")
                .to_payload(),
        ),
        (
            lexical_ids[1].0,
            semantic::VectorPoint::new(lexical_ids[1], vec![2.0, 2.0])
                .expect("point")
                .to_payload(),
        ),
        (
            semantic_only.0,
            semantic::VectorPoint::new(semantic_only, vec![0.1, 0.1])
                .expect("point")
                .to_payload(),
        ),
        (
            unknown.0,
            semantic::VectorPoint::new(unknown, vec![0.0, 0.0])
                .expect("point")
                .to_payload(),
        ),
    ];
    let relation =
        RelationState::<semantic::CandidateRelation>::from_entries(payloads.clone(), coverage)
            .expect("candidate relation");
    let binding = coordinator.semantic_binding(relation.root(), embedding.version());
    let state = semantic::CandidateState::new(
        binding,
        coverage,
        payloads
            .into_iter()
            .map(|(id, payload)| (semantic::CandidateId(id), payload))
            .collect(),
        semantic::Limits::default(),
    )
    .expect("candidate state");
    let facts = semantic::VectorFacts::from_recipe(state, embedding).expect("facts");
    let base = semantic::AnnBase::from_facts(
        &facts,
        semantic::SearchQuality::Exact,
        semantic::Limits::default(),
    )
    .expect("base");
    let source = semantic::MemorySource::new(
        binding,
        coverage,
        vec![unknown, semantic_only, lexical_ids[0], lexical_ids[1]],
        semantic::Limits::default(),
    )
    .expect("source");
    let index = semantic::VectorIndex::new(base, facts, source).expect("index");
    let query = semantic::QueryVector::new(embedding, vec![0.0, 0.0]).expect("query vector");
    let expected_first = local
        .corpus
        .candidates
        .get(&lexical_ids[0])
        .and_then(|entity| local.corpus.entities.get(entity))
        .copied()
        .expect("expected first row");
    let result = local.accelerate_with(
        CompositionPolicy::RerankLexical,
        SemanticAcceleration {
            index: &index,
            query: &query,
        },
    );
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0].row.id, expected_first);
    assert!(matches!(
        result.lanes[2].coverage,
        CoverageBasis::CandidateSubset { suppressed: 2, .. }
    ));

    let augmented_local = coordinator
        .search_local(LocalQuery::prefix("alpha", 3).expect("query"))
        .expect("local search");
    let augmented = augmented_local.accelerate(SemanticAcceleration {
        index: &index,
        query: &query,
    });
    assert_eq!(augmented.rows.len(), 3);
    assert_eq!(augmented.rows[0].row.label, "meaning related");
    assert!(augmented.rows[0].lexical_relevance.is_none());
    assert_eq!(augmented.lanes[2].freshness, Freshness::Current);
    assert!(matches!(
        augmented.lanes[2].source.as_ref(),
        Some(SourceBasis::Semantic { .. })
    ));
    assert!(matches!(
        augmented.lanes[2].coverage,
        CoverageBasis::CandidateSubset {
            augmented: 1,
            suppressed: 1,
            ..
        }
    ));

    let offline =
        semantic::VectorIndex::new(index.base().clone(), index.facts().clone(), OfflineSource)
            .expect("offline index");
    let outage_local = coordinator
        .search_local(LocalQuery::prefix("alpha", 2).expect("query"))
        .expect("outage local");
    let expected = outage_local
        .rows
        .iter()
        .map(|row| row.row.id)
        .collect::<Vec<_>>();
    let outage = outage_local.accelerate(SemanticAcceleration {
        index: &offline,
        query: &query,
    });
    assert_eq!(
        outage.rows.iter().map(|row| row.row.id).collect::<Vec<_>>(),
        expected
    );
    assert_eq!(outage.lanes[2].coverage, CoverageBasis::Unavailable);
}
