//! Full-corpus controls for the local exact path; no model inference evidence.

use crate::{
    AnnBase, Authority, Binding, CandidateChange, CandidateId, CandidateState, EmbeddingEncoding,
    EmbeddingNormalization, EmbeddingPooling, EmbeddingRecipe, Error, Frontier, Limits,
    MemorySource, Metric, ModelVersion, OverlayLimits, QueryVector, ReadManifest, Recipe,
    RefreshOutcome, SearchQuality, TokenizerVersion, TreatmentVersion, VectorFacts, VectorIndex,
    VectorPoint,
};
use std::num::NonZeroU32;

fn recipe(metric: Metric, dimensions: u32) -> EmbeddingRecipe {
    EmbeddingRecipe {
        model: ModelVersion::from_value(&[0xE1; 32]),
        tokenizer: TokenizerVersion::from_value(&[0xE2; 32]),
        dimensions: NonZeroU32::new(dimensions).expect("dimension"),
        metric,
        pooling: EmbeddingPooling::Mean,
        normalization: EmbeddingNormalization::None,
        encoding: EmbeddingEncoding::Float32,
        query_treatment: TreatmentVersion::from_value(b"query"),
        document_treatment: TreatmentVersion::from_value(b"document"),
    }
}

fn fixture(
    recipe: EmbeddingRecipe,
    points: &[(u64, Vec<f32>)],
) -> (CandidateState, VectorIndex<MemorySource>) {
    let entries = points
        .iter()
        .map(|(id, point)| {
            (
                *id,
                VectorPoint::new(CandidateId(*id), point.clone())
                    .expect("point")
                    .to_payload(),
            )
        })
        .collect::<Vec<_>>();
    let (binding, coverage) = crate::tests::binding(&entries);
    let binding = Binding {
        recipe: recipe.version(),
        ..binding
    };
    let state = CandidateState::new(
        binding,
        coverage,
        entries
            .into_iter()
            .map(|(id, payload)| (CandidateId(id), payload))
            .collect(),
        Limits::default(),
    )
    .expect("state");
    let facts = VectorFacts::from_recipe(state.clone(), recipe).expect("facts");
    let base = AnnBase::from_facts(&facts, SearchQuality::Exact, Limits::default()).expect("base");
    let source = MemorySource::new(binding, coverage, facts.ids().collect(), Limits::default())
        .expect("source");
    (state, VectorIndex::new(base, facts, source).expect("index"))
}

// Independent scalar full-corpus oracle over the original fixture coordinates.
// It does not use provider pages, encoded facts, production scoring or top-k.
fn oracle(
    metric: Metric,
    query: &[f32],
    points: &[(u64, Vec<f32>)],
    limit: usize,
) -> Vec<(CandidateId, f32)> {
    let mut scored = points
        .iter()
        .map(|(id, point)| {
            let mut distance = 0.0_f32;
            let mut dot = 0.0_f32;
            let mut query_norm = 0.0_f32;
            let mut point_norm = 0.0_f32;
            for dimension in 0..query.len() {
                let difference = query[dimension] - point[dimension];
                distance += difference * difference;
                dot += query[dimension] * point[dimension];
                query_norm += query[dimension] * query[dimension];
                point_norm += point[dimension] * point[dimension];
            }
            let score = match metric {
                Metric::EuclideanSquared => distance,
                Metric::NegativeDot => -dot,
                Metric::CosineDistance if query_norm == 0.0 || point_norm == 0.0 => 1.0,
                Metric::CosineDistance => 1.0 - dot / (query_norm.sqrt() * point_norm.sqrt()),
            };
            (CandidateId(*id), score)
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| left.1.total_cmp(&right.1).then(left.0.cmp(&right.0)));
    scored.truncate(limit);
    scored
}

fn assert_exact(
    index: &VectorIndex<MemorySource>,
    recipe: EmbeddingRecipe,
    query: &[f32],
    points: &[(u64, Vec<f32>)],
    limit: usize,
    limits: Limits,
) {
    let query_vector = QueryVector::new(recipe, query.to_vec()).expect("query");
    let result = index
        .search_exact_embedding(index.binding(), &query_vector, limit, limits)
        .expect("complete exact search");
    let expected = oracle(recipe.metric, query, points, limit);
    assert_eq!(result.binding, index.binding());
    assert_eq!(result.coverage, index.facts().coverage());
    assert_eq!(result.quality, SearchQuality::Exact);
    assert_eq!(result.candidates.len(), expected.len());
    for (actual, (id, score)) in result.candidates.iter().zip(expected) {
        assert_eq!(actual.id, id);
        assert_eq!(actual.score.to_bits(), score.to_bits());
    }
}

#[test]
fn exact_local_search_finds_the_winner_beyond_the_ann_refill_prefix() {
    let recipe = recipe(Metric::EuclideanSquared, 2);
    let mut points = (1..=73_u64)
        .map(|id| (id, vec![10.0, 10.0]))
        .collect::<Vec<_>>();
    points[72].1 = vec![0.0, 0.0];
    let (_, index) = fixture(recipe, &points);
    let query = QueryVector::new(recipe, vec![0.0, 0.0]).expect("query");
    let limits = Limits {
        max_page: 4,
        ..Limits::default()
    };
    // Existing ANN behavior remains bounded and unchanged; its ID-prefix
    // provider cannot find the last point. The complete local path must.
    let sampled = index
        .search_embedding(&query, 1, limits)
        .expect("ANN control");
    assert_ne!(sampled.candidates[0].id, CandidateId(73));
    let exact = index
        .search_exact_embedding(index.binding(), &query, 1, limits)
        .expect("exact");
    assert_eq!(exact.candidates[0].id, CandidateId(73));
    assert_exact(&index, recipe, &[0.0, 0.0], &points, 3, limits);
}

#[test]
fn exact_local_search_matches_full_scalar_oracle_across_metrics_and_dimensions() {
    for metric in [
        Metric::CosineDistance,
        Metric::EuclideanSquared,
        Metric::NegativeDot,
    ] {
        for dimensions in [1, 3, 17, 384] {
            let recipe = recipe(metric, dimensions);
            let query = (0..dimensions)
                .map(|d| (f32::from(u16::try_from(d).expect("fixture dimension")) + 1.0) / 397.0)
                .collect::<Vec<_>>();
            let points = (1..=89_u64)
                .rev()
                .map(|id| {
                    (
                        id,
                        (0..dimensions)
                            .map(|d| {
                                f32::from(
                                    u16::try_from((id * 37 + u64::from(d) * 11) % 101)
                                        .expect("fixture coordinate"),
                                ) / 103.0
                                    - 0.25
                            })
                            .collect(),
                    )
                })
                .collect::<Vec<_>>();
            let (_, index) = fixture(recipe, &points);
            for limit in [1, 3, 7] {
                assert_exact(
                    &index,
                    recipe,
                    &query,
                    &points,
                    limit,
                    Limits {
                        max_page: 7,
                        ..Limits::default()
                    },
                );
            }
        }
    }
}

#[test]
fn exact_local_ties_use_the_full_u64_identity_and_empty_complete_facts_are_exact() {
    let recipe = recipe(Metric::EuclideanSquared, 2);
    let points = vec![
        (u64::MAX, vec![1.0, 1.0]),
        (u64::MAX - 1, vec![1.0, 1.0]),
        (1_u64 << 54, vec![1.0, 1.0]),
        (1, vec![1.0, 1.0]),
    ];
    let (_, index) = fixture(recipe, &points);
    assert_exact(&index, recipe, &[0.0, 0.0], &points, 3, Limits::default());
    let (_, empty) = fixture(recipe, &[]);
    assert_exact(&empty, recipe, &[0.0, 0.0], &[], 1, Limits::default());
}

#[test]
fn exact_local_overlay_replacement_deletion_addition_and_readd_match_current_facts() {
    let recipe = recipe(Metric::EuclideanSquared, 2);
    let mut points = vec![
        (1, vec![1.0, 1.0]),
        (2, vec![2.0, 2.0]),
        (3, vec![3.0, 3.0]),
    ];
    let (mut state, mut index) = fixture(recipe, &points);
    let old_binding = index.binding();
    let rounds = [
        vec![
            CandidateChange::Upsert {
                id: CandidateId(2),
                payload: VectorPoint::new(CandidateId(2), vec![0.1, 0.1])
                    .expect("replacement")
                    .to_payload(),
            },
            CandidateChange::Delete { id: CandidateId(1) },
            CandidateChange::Upsert {
                id: CandidateId(100),
                payload: VectorPoint::new(CandidateId(100), vec![0.0, 0.0])
                    .expect("addition")
                    .to_payload(),
            },
        ],
        vec![
            CandidateChange::Delete {
                id: CandidateId(100),
            },
            CandidateChange::Upsert {
                id: CandidateId(1),
                payload: VectorPoint::new(CandidateId(1), vec![0.05, 0.05])
                    .expect("readd")
                    .to_payload(),
            },
        ],
    ];
    for changes in rounds {
        for change in &changes {
            match change {
                CandidateChange::Delete { id } => points.retain(|(key, _)| *key != id.0),
                CandidateChange::Upsert { id, payload } => {
                    points.retain(|(key, _)| *key != id.0);
                    points.push((
                        id.0,
                        VectorPoint::from_payload(*id, payload)
                            .expect("point")
                            .values()
                            .to_vec(),
                    ));
                }
            }
        }
        let delta = state.prepare_delta(changes).expect("delta");
        state = state.apply_delta(&delta).expect("target");
        let target = VectorFacts::from_recipe(state.clone(), recipe).expect("target facts");
        index = match index
            .advance(&delta, target, OverlayLimits::default())
            .expect("advance")
        {
            RefreshOutcome::Advanced(index) => index,
            other => {
                assert!(matches!(other, RefreshOutcome::Advanced(_)));
                return;
            }
        };
        assert_exact(&index, recipe, &[0.0, 0.0], &points, 3, Limits::default());
        let query = QueryVector::new(recipe, vec![0.0, 0.0]).expect("query");
        assert_eq!(
            index.search_exact_embedding(old_binding, &query, 1, Limits::default()),
            Err(Error::StaleRoot)
        );
        assert_eq!(
            index.search_exact_embedding(
                index.binding(),
                &query,
                1,
                Limits {
                    max_tombstones: 1,
                    ..Limits::default()
                }
            ),
            Err(Error::SizeLimit)
        );
        let (_, restarted) = fixture(recipe, &points);
        let current = index
            .search_exact_embedding(index.binding(), &query, 3, Limits::default())
            .expect("current");
        let reopened = restarted
            .search_exact_embedding(restarted.binding(), &query, 3, Limits::default())
            .expect("reopened");
        assert_eq!(current.candidates, reopened.candidates);
    }
}

#[test]
fn exact_local_search_refuses_every_selected_binding_field_and_recipe_change() {
    let recipe = recipe(Metric::EuclideanSquared, 2);
    let (_, index) = fixture(recipe, &[(1, vec![1.0, 1.0])]);
    let query = QueryVector::new(recipe, vec![0.0, 0.0]).expect("query");
    let binding = index.binding();
    let other_workspace = backend_version::WorkspaceManifest::from_versions(
        1,
        Vec::new(),
        Vec::new(),
        Authority::from_value(&[0xE3; 32]),
        index.facts().coverage(),
    )
    .expect("other workspace")
    .root();
    let changed = [
        Binding {
            workspace: other_workspace,
            ..binding
        },
        Binding {
            root: crate::tests::binding(&[]).0.root,
            ..binding
        },
        Binding {
            recipe: Recipe::from_value(b"other"),
            ..binding
        },
        Binding {
            authority: Authority::from_value(&[0xE4; 32]),
            ..binding
        },
        Binding {
            read_manifest: ReadManifest::from_value(b"other"),
            ..binding
        },
        Binding {
            frontier: Frontier::from_value(&[0xE5; 32]),
            ..binding
        },
    ];
    for selected in changed {
        assert_ne!(selected, binding);
        assert_eq!(
            index.search_exact_embedding(selected, &query, 1, Limits::default()),
            Err(Error::StaleRoot)
        );
    }
    for changed in [
        EmbeddingRecipe {
            model: ModelVersion::from_value(&[0xE6; 32]),
            ..recipe
        },
        EmbeddingRecipe {
            tokenizer: TokenizerVersion::from_value(&[0xE7; 32]),
            ..recipe
        },
        EmbeddingRecipe {
            pooling: EmbeddingPooling::LastToken,
            ..recipe
        },
        EmbeddingRecipe {
            query_treatment: TreatmentVersion::from_value(b"other"),
            ..recipe
        },
        EmbeddingRecipe {
            document_treatment: TreatmentVersion::from_value(b"other"),
            ..recipe
        },
        EmbeddingRecipe {
            dimensions: NonZeroU32::new(3).expect("dimension"),
            ..recipe
        },
        EmbeddingRecipe {
            metric: Metric::NegativeDot,
            ..recipe
        },
    ] {
        let values =
            vec![0.0; usize::try_from(changed.dimensions.get()).expect("fixture dimension")];
        let changed = QueryVector::new(changed, values).expect("changed query");
        assert_eq!(
            index.search_exact_embedding(binding, &changed, 1, Limits::default()),
            Err(Error::StaleRoot)
        );
    }
}

#[test]
fn exact_local_search_refuses_resource_limits_without_returning_a_prefix() {
    let recipe = recipe(Metric::EuclideanSquared, 2);
    let (_, index) = fixture(recipe, &[(1, vec![1.0, 1.0]), (2, vec![2.0, 2.0])]);
    let query = QueryVector::new(recipe, vec![0.0, 0.0]).expect("query");
    for limits in [
        Limits {
            max_candidates: 1,
            max_page: 1,
            ..Limits::default()
        },
        Limits {
            max_payload_bytes: 11,
            ..Limits::default()
        },
        Limits {
            max_total_payload_bytes: 23,
            ..Limits::default()
        },
    ] {
        assert_eq!(
            index.search_exact_embedding(index.binding(), &query, 1, limits),
            Err(Error::SizeLimit)
        );
    }
    assert_eq!(
        index.search_exact_embedding(index.binding(), &query, 0, Limits::default()),
        Err(Error::SizeLimit)
    );
    assert_eq!(
        index.search_exact_embedding(index.binding(), &query, 257, Limits::default()),
        Err(Error::SizeLimit)
    );
    assert_eq!(
        index.search_exact_embedding(
            index.binding(),
            &query,
            1,
            Limits {
                max_tombstones: 0,
                ..Limits::default()
            }
        ),
        Err(Error::InvalidLimits)
    );
    let (_, overflow) = fixture(recipe, &[(1, vec![f32::MAX, 0.0])]);
    assert_eq!(
        overflow.search_exact_embedding(overflow.binding(), &query, 1, Limits::default()),
        Err(Error::MalformedInput)
    );
    let huge = Limits {
        max_candidates: usize::MAX,
        max_tombstones: usize::MAX,
        max_payload_bytes: usize::MAX,
        max_total_payload_bytes: usize::MAX,
        max_page: usize::MAX,
    };
    let bounded = index
        .search_exact_embedding(index.binding(), &query, usize::MAX, huge)
        .expect("scratch is capped by complete actual corpus, not requested limit");
    assert_eq!(bounded.candidates.len(), 2);
}

#[test]
fn exact_local_search_refuses_legacy_facts_and_checks_complete_coverage() {
    let recipe = recipe(Metric::EuclideanSquared, 2);
    let (state, _) = fixture(recipe, &[(1, vec![1.0, 1.0])]);
    let query = QueryVector::new(recipe, vec![0.0, 0.0]).expect("query");
    let source = MemorySource::new(
        state.binding(),
        state.coverage(),
        vec![CandidateId(1)],
        Limits::default(),
    )
    .expect("source");
    for (model, metric) in [
        (recipe.model, recipe.metric),
        (ModelVersion::from_value(&[0xE8; 32]), recipe.metric),
        (recipe.model, Metric::NegativeDot),
    ] {
        // Matching model/metric/dimension and even a claimed recipe binding
        // cannot substitute for full stored-recipe admission.
        let facts = VectorFacts::new(state.clone(), model, metric, 2).expect("legacy facts");
        let base =
            AnnBase::from_facts(&facts, SearchQuality::Exact, Limits::default()).expect("base");
        let inconsistent = VectorIndex::new(base, facts, source.clone()).expect("index");
        assert_eq!(
            inconsistent.search_exact_embedding(
                inconsistent.binding(),
                &query,
                1,
                Limits::default()
            ),
            Err(Error::StaleRoot)
        );
        if model == recipe.model && metric == recipe.metric {
            assert!(
                inconsistent
                    .search_embedding(&query, 1, Limits::default())
                    .is_ok(),
                "legacy ANN remains usable"
            );
        }
    }
    let coverage = backend_version::CoverageWitness::closed_relation(
        backend_version::ClosedRelationScope::from_scope_root(
            backend_version::ScopeRoot::from_u64(1),
        ),
    );
    let payloads = state
        .iter()
        .map(|(id, payload)| (id.0, payload.to_vec()))
        .collect::<Vec<_>>();
    let relation = backend_version::RelationState::<crate::CandidateRelation>::from_entries(
        payloads.clone(),
        coverage,
    )
    .expect("closed relation");
    let binding = Binding {
        root: relation.root(),
        ..state.binding()
    };
    let closed = CandidateState::new(
        binding,
        coverage,
        payloads
            .into_iter()
            .map(|(id, payload)| (CandidateId(id), payload))
            .collect(),
        Limits::default(),
    )
    .expect("closed state");
    let facts = VectorFacts::from_recipe(closed, recipe).expect("closed facts");
    let base =
        AnnBase::from_facts(&facts, SearchQuality::Exact, Limits::default()).expect("closed base");
    let closed = VectorIndex::new(base, facts, source).expect("closed index");
    assert_eq!(
        closed.search_exact_embedding(binding, &query, 1, Limits::default()),
        Err(Error::IncompleteCoverage)
    );
}

fn rebind_recipe(state: &CandidateState, recipe: EmbeddingRecipe) -> CandidateState {
    CandidateState::new(
        Binding {
            recipe: recipe.version(),
            ..state.binding()
        },
        state.coverage(),
        state
            .iter()
            .map(|(id, payload)| (id, payload.to_vec()))
            .collect(),
        Limits::default(),
    )
    .expect("rebound recipe")
}

#[test]
fn full_recipe_facts_admit_stored_normalization_and_encoding_once() {
    let plain = recipe(Metric::CosineDistance, 2);
    let unit = EmbeddingRecipe {
        normalization: EmbeddingNormalization::UnitL2,
        ..plain
    };
    for values in [vec![2.0, 0.0], vec![0.0, 0.0]] {
        let (state, _) = fixture(plain, &[(1, values)]);
        assert_eq!(
            VectorFacts::from_recipe(rebind_recipe(&state, unit), unit),
            Err(Error::MalformedInput)
        );
    }
    let (state, _) = fixture(plain, &[(1, vec![0.6, 0.8])]);
    let facts = VectorFacts::from_recipe(rebind_recipe(&state, unit), unit).expect("unit facts");
    let base = AnnBase::from_facts(&facts, SearchQuality::Exact, Limits::default()).expect("base");
    let source = MemorySource::new(
        facts.binding(),
        facts.coverage(),
        facts.ids().collect(),
        Limits::default(),
    )
    .expect("source");
    let index = VectorIndex::new(base, facts, source).expect("index");
    assert_exact(
        &index,
        unit,
        &[0.0, 1.0],
        &[(1, vec![0.6, 0.8])],
        1,
        Limits::default(),
    );
    let quantized = EmbeddingRecipe {
        encoding: EmbeddingEncoding::SignedInt8,
        ..plain
    };
    assert_eq!(
        VectorFacts::from_recipe(rebind_recipe(&state, quantized), quantized),
        Err(Error::MalformedInput),
        "canonical facts contain float32, not signed-int8"
    );
}

#[test]
fn full_recipe_proof_survives_overlay_and_rebuild_and_cannot_be_downgraded() {
    let recipe = recipe(Metric::EuclideanSquared, 2);
    let (state, index) = fixture(recipe, &[(1, vec![1.0, 1.0])]);
    let delta = state
        .prepare_delta(vec![CandidateChange::Upsert {
            id: CandidateId(1),
            payload: VectorPoint::new(CandidateId(1), vec![0.0, 0.0])
                .expect("replacement")
                .to_payload(),
        }])
        .expect("delta");
    let target = state.apply_delta(&delta).expect("target state");
    let legacy =
        VectorFacts::new(target.clone(), recipe.model, recipe.metric, 2).expect("legacy target");
    assert_eq!(
        index.advance(&delta, legacy.clone(), OverlayLimits::default()),
        Err(Error::StaleRoot)
    );
    assert_eq!(
        index.rebuild(legacy, OverlayLimits::default()),
        Err(Error::StaleRoot)
    );
    let admitted = VectorFacts::from_recipe(target.clone(), recipe).expect("full target recipe");
    let outcome = index
        .advance(&delta, admitted.clone(), OverlayLimits::default())
        .expect("advance");
    let RefreshOutcome::Advanced(advanced) = outcome else {
        assert!(matches!(outcome, RefreshOutcome::Advanced(_)));
        return;
    };
    assert_exact(
        &advanced,
        recipe,
        &[0.0, 0.0],
        &[(1, vec![0.0, 0.0])],
        1,
        Limits::default(),
    );
    let rebuilt = advanced
        .rebuild(admitted, OverlayLimits::default())
        .expect("rebuild");
    assert!(rebuilt.overlay().is_none());
    assert_exact(
        &rebuilt,
        recipe,
        &[0.0, 0.0],
        &[(1, vec![0.0, 0.0])],
        1,
        Limits::default(),
    );
    for changed in [
        EmbeddingRecipe {
            pooling: EmbeddingPooling::LastToken,
            ..recipe
        },
        EmbeddingRecipe {
            tokenizer: TokenizerVersion::from_value(&[0xE9; 32]),
            ..recipe
        },
        EmbeddingRecipe {
            document_treatment: TreatmentVersion::from_value(b"changed treatment"),
            ..recipe
        },
    ] {
        let changed = VectorFacts::from_recipe(rebind_recipe(&target, changed), changed)
            .expect("changed recipe facts");
        assert_eq!(
            index.advance(&delta, changed.clone(), OverlayLimits::default()),
            Err(Error::StaleRoot)
        );
        assert_eq!(
            index.rebuild(changed, OverlayLimits::default()),
            Err(Error::StaleRoot)
        );
    }
}

#[derive(Debug)]
enum CpuRefusal {
    Vector(Error),
    Interrupted(usize),
}
impl From<Error> for CpuRefusal {
    fn from(error: Error) -> Self {
        Self::Vector(error)
    }
}

#[test]
fn controlled_exact_search_preserves_full_scalar_bits_and_stops_coordinate_work() {
    // 769 dimensions cross three checkpoint chunks; all scores retain scalar order.
    for metric in [
        Metric::EuclideanSquared,
        Metric::CosineDistance,
        Metric::NegativeDot,
    ] {
        let recipe = recipe(metric, 769);
        let points = (1..=31)
            .map(|id| {
                (
                    id,
                    (0..769)
                        .map(|coordinate| ((coordinate * 17 + id as usize * 13) % 89) as f32 / 97.0)
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        let (_, index) = fixture(recipe, &points);
        let query_values = (0..769)
            .map(|coordinate| ((coordinate * 7) % 71) as f32 / 79.0)
            .collect::<Vec<_>>();
        let query = QueryVector::new(recipe, query_values.clone()).expect("query");
        let mut full_visits = 0usize;
        let complete = index
            .search_exact_embedding_controlled(
                index.binding(),
                &query,
                5,
                Limits::default(),
                &mut || {
                    full_visits += 1;
                    Ok::<(), Error>(())
                },
            )
            .expect("complete");
        let expected = oracle(metric, &query_values, &points, 5);
        assert_eq!(complete.candidates.len(), expected.len());
        for (actual, (id, score)) in complete.candidates.iter().zip(expected) {
            assert_eq!(actual.id, id);
            assert_eq!(actual.score.to_bits(), score.to_bits());
        }
        for stop_at in [1, 5, points.len() + 3, points.len() + 5, full_visits - 1] {
            let mut visits = 0;
            let stopped = index.search_exact_embedding_controlled(
                index.binding(),
                &query,
                5,
                Limits::default(),
                &mut || {
                    visits += 1;
                    if visits == stop_at {
                        Err(CpuRefusal::Interrupted(visits))
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(matches!(stopped, Err(CpuRefusal::Interrupted(at)) if at == stop_at));
            assert_eq!(visits, stop_at);
        }
    }
}

#[test]
fn controlled_fact_base_source_admission_refuses_inside_complete_runs() {
    let recipe = recipe(Metric::EuclideanSquared, 769);
    let points = (1..=9).map(|id| (id, vec![1.0; 769])).collect::<Vec<_>>();
    let (state, _) = fixture(recipe, &points);
    let mut visited = 0;
    let refused = VectorFacts::from_recipe_controlled(state.clone(), recipe, &mut || {
        visited += 1;
        if visited == 5 {
            Err(CpuRefusal::Interrupted(visited))
        } else {
            Ok(())
        }
    });
    assert!(matches!(refused, Err(CpuRefusal::Interrupted(5))));
    assert_eq!(
        visited, 5,
        "finite-coordinate admission stops inside the first payload"
    );
    let facts = VectorFacts::from_recipe(state.clone(), recipe).expect("facts");
    let mut visited = 0;
    let refused = AnnBase::from_facts_controlled(
        &facts,
        SearchQuality::Exact,
        Limits::default(),
        &mut || {
            visited += 1;
            if visited == 4 {
                Err(CpuRefusal::Interrupted(visited))
            } else {
                Ok(())
            }
        },
    );
    assert!(matches!(refused, Err(CpuRefusal::Interrupted(4))));
    assert_eq!(visited, 4);
    let mut visited = 0;
    let refused = MemorySource::from_sorted_ids_controlled(
        state.binding(),
        state.coverage(),
        facts.ids().collect(),
        Limits::default(),
        &mut || {
            visited += 1;
            if visited == 4 {
                Err(CpuRefusal::Interrupted(visited))
            } else {
                Ok(())
            }
        },
    );
    assert!(matches!(refused, Err(CpuRefusal::Interrupted(4))));
    assert_eq!(visited, 4);
    let malformed = MemorySource::from_sorted_ids_controlled(
        state.binding(),
        state.coverage(),
        vec![CandidateId(2), CandidateId(1)],
        Limits::default(),
        &mut || Ok::<(), CpuRefusal>(()),
    );
    assert!(matches!(
        malformed,
        Err(CpuRefusal::Vector(Error::MalformedInput))
    ));
}

#[test]
fn controlled_full_recipe_normalization_stops_inside_coordinates() {
    let mut unit_recipe = recipe(Metric::CosineDistance, 769);
    unit_recipe.normalization = EmbeddingNormalization::UnitL2;
    let value = 1.0 / 769.0_f32.sqrt();
    let points = (1..=9).map(|id| (id, vec![value; 769])).collect::<Vec<_>>();
    let (state, _) = fixture(unit_recipe, &points);
    let mut shape_recipe = unit_recipe;
    shape_recipe.normalization = EmbeddingNormalization::None;
    let shape_state = rebind_recipe(&state, shape_recipe);
    let mut shape_visits = 0;
    VectorFacts::from_recipe_controlled(shape_state, shape_recipe, &mut || {
        shape_visits += 1;
        Ok::<(), Error>(())
    })
    .expect("shape");
    let stop_at = shape_visits + 3;
    let mut visits = 0;
    let refused = VectorFacts::from_recipe_controlled(state, unit_recipe, &mut || {
        visits += 1;
        if visits == stop_at {
            Err(CpuRefusal::Interrupted(visits))
        } else {
            Ok(())
        }
    });
    assert!(matches!(refused, Err(CpuRefusal::Interrupted(at)) if at == stop_at));
    assert_eq!(visits, stop_at);
}

#[test]
fn checkpointed_negative_dot_preserves_sum_signed_zero() {
    let recipe = recipe(Metric::NegativeDot, 769);
    let points = vec![(1, vec![-0.0; 769])];
    let (_, index) = fixture(recipe, &points);
    let query = QueryVector::new(recipe, vec![1.0; 769]).expect("query");
    let result = index
        .search_exact_embedding_controlled(
            index.binding(),
            &query,
            1,
            Limits::default(),
            &mut || Ok::<(), Error>(()),
        )
        .expect("exact");
    // Original Iterator::sum starts at -0.0, then NegativeDot negates the sum.
    let expected = -points[0].1.iter().map(|point| 1.0_f32 * point).sum::<f32>();
    assert_eq!(result.candidates[0].score.to_bits(), expected.to_bits());
    assert_eq!(expected.to_bits(), 0.0_f32.to_bits());
}
