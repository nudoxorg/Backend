use super::local::{
    CoverageBasis, Freshness, Lane, LaneReport, LocalAnswer, QueryResult, RankedRow, SourceBasis,
};
use backend_extension_qdrant as semantic;
use std::collections::BTreeSet;
use std::fmt;

/// Closed policy for composing admitted semantic candidates with the
/// complete local lexical result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompositionPolicy {
    /// Semantic scores may only reorder lexical matches.
    RerankLexical,
    /// Semantic candidates may add a bounded number of canonical view rows.
    AugmentCanonical {
        /// Maximum non-lexical rows admitted into the displayed page.
        max_additions: usize,
    },
}

/// Optional semantic lane tied to a typed query embedding and vector index.
pub struct SemanticAcceleration<'a, S> {
    /// Exact vector index, including its complete current overlay.
    pub index: &'a semantic::VectorIndex<S>,
    /// Query-side embedding with the full model/tokenizer/treatment recipe.
    pub query: &'a semantic::QueryVector,
}

impl<'a, S> SemanticAcceleration<'a, S> {
    fn into_parts(self) -> (&'a semantic::VectorIndex<S>, &'a semantic::QueryVector) {
        (self.index, self.query)
    }
}

impl LocalAnswer {
    /// Attempts semantic ordering without surrendering the complete local
    /// answer. Provider failure, stale basis, and insufficient recall are
    /// represented in the semantic lane; admitted local rows stay usable.
    #[must_use]
    pub fn accelerate<S>(self, acceleration: SemanticAcceleration<'_, S>) -> QueryResult
    where
        S: semantic::AnnSource,
        S::Error: fmt::Display,
    {
        let max_additions = self.query.semantic_limit();
        self.accelerate_with(
            CompositionPolicy::AugmentCanonical { max_additions },
            acceleration,
        )
    }

    /// Applies an explicit bounded composition policy.
    #[must_use]
    pub fn accelerate_with<S>(
        self,
        policy: CompositionPolicy,
        acceleration: SemanticAcceleration<'_, S>,
    ) -> QueryResult
    where
        S: semantic::AnnSource,
        S::Error: fmt::Display,
    {
        let (index, query) = acceleration.into_parts();
        let Some((base, current)) = admitted_basis(&self, index) else {
            return self.unavailable_semantic(Freshness::Stale);
        };
        let refill = refill_limit(&self, policy);
        let Ok(result) = index.search_embedding(query, refill, semantic::Limits::default()) else {
            return self.unavailable_semantic(Freshness::Unknown);
        };
        self.compose_semantic_result(policy, query, result, base, current)
    }

    pub(super) fn accelerate_exact_memory(
        mut self,
        index: &semantic::VectorIndex<semantic::MemorySource>,
        query: &semantic::QueryVector,
        limits: semantic::Limits,
        mut checkpoint: impl FnMut() -> Result<(), super::LocalSemanticError>,
    ) -> Result<QueryResult, (Self, super::LocalSemanticError)> {
        let Some((base, current)) = admitted_basis(&self, index) else {
            return Err((
                self,
                super::LocalSemanticError::Vector(semantic::Error::StaleRoot),
            ));
        };
        let policy = CompositionPolicy::AugmentCanonical {
            max_additions: self.query.semantic_limit(),
        };
        let refill = refill_limit(&self, policy).min(limits.max_page);
        let result = match index.search_exact_embedding_controlled(
            current,
            query,
            refill,
            limits,
            &mut checkpoint,
        ) {
            Ok(result) => result,
            Err(error) => return Err((self, error)),
        };
        let mut entities = Vec::new();
        if let Err(error) = entities.try_reserve_exact(result.candidates.len()) {
            return Err((self, error.into()));
        }
        for candidate in &result.candidates {
            if let Err(error) = checkpoint() {
                return Err((self, error));
            }
            if let Some(entity) = self.candidate_entity(candidate.id) {
                entities.push(entity);
            }
        }
        let lexical =
            match self.lexical_relevance_for_candidates_controlled(&entities, &mut checkpoint) {
                Ok(lexical) => lexical,
                Err(error) => return Err((self, error)),
            };
        let composed = match compose_controlled(&self, policy, &result, &lexical, &mut checkpoint) {
            Ok(composed) => composed,
            Err(error) => return Err((self, error)),
        };
        if let Err(error) = checkpoint() {
            return Err((self, error));
        }
        self.lanes.push(LaneReport {
            lane: Lane::Semantic,
            coverage: CoverageBasis::CandidateSubset {
                admitted: composed.semantic_admitted,
                suppressed: composed.suppressed,
                augmented: composed.augmented,
                quality: result.quality,
            },
            freshness: Freshness::Current,
            source: Some(SourceBasis::Semantic {
                base: Box::new(base),
                current: Box::new(current),
                model: query.query().model(),
                quality: result.quality,
            }),
        });
        Ok(QueryResult {
            rows: composed.rows,
            total_matches: self.total_matches,
            lanes: self.lanes,
        })
    }

    fn compose_semantic_result(
        mut self,
        policy: CompositionPolicy,
        query: &semantic::QueryVector,
        result: semantic::VectorSearchResult,
        base: semantic::Binding,
        current: semantic::Binding,
    ) -> QueryResult {
        let candidate_entities = result
            .candidates
            .iter()
            .filter_map(|candidate| self.candidate_row(candidate.id).map(|(entity, _)| entity))
            .collect::<Vec<_>>();
        let Ok(lexical) = self.lexical_relevance_for_candidates(&candidate_entities) else {
            return self.unavailable_semantic(Freshness::Unknown);
        };
        let composed = compose(&self, policy, &result, &lexical);
        self.lanes.push(LaneReport {
            lane: Lane::Semantic,
            coverage: CoverageBasis::CandidateSubset {
                admitted: composed.semantic_admitted,
                suppressed: composed.suppressed,
                augmented: composed.augmented,
                quality: result.quality,
            },
            freshness: Freshness::Current,
            source: Some(SourceBasis::Semantic {
                base: Box::new(base),
                current: Box::new(current),
                model: query.query().model(),
                quality: result.quality,
            }),
        });
        QueryResult {
            rows: composed.rows,
            total_matches: self.total_matches,
            lanes: self.lanes,
        }
    }

    fn unavailable_semantic(mut self, freshness: Freshness) -> QueryResult {
        self.lanes.push(unavailable(freshness));
        QueryResult {
            rows: self.rows,
            total_matches: self.total_matches,
            lanes: self.lanes,
        }
    }
}

fn admitted_basis<S>(
    answer: &LocalAnswer,
    index: &semantic::VectorIndex<S>,
) -> Option<(semantic::Binding, semantic::Binding)> {
    let base = index.base().binding();
    let current = index.binding();
    let exact = [base, current]
        .iter()
        .all(|binding| answer.corpus.semantic_binding_matches(binding));
    exact.then_some((base, current))
}

fn refill_limit(answer: &LocalAnswer, policy: CompositionPolicy) -> usize {
    let additions = match policy {
        CompositionPolicy::RerankLexical => 0,
        CompositionPolicy::AugmentCanonical { max_additions } => max_additions,
    };
    let desired = answer.query.semantic_limit().saturating_add(additions);
    desired
        .saturating_mul(4)
        .max(desired)
        .min(semantic::Limits::default().max_page)
        .min(answer.selected_row_count().max(1))
}

struct Composition {
    rows: Vec<RankedRow>,
    semantic_admitted: usize,
    suppressed: usize,
    augmented: usize,
}

fn compose(
    answer: &LocalAnswer,
    policy: CompositionPolicy,
    result: &semantic::VectorSearchResult,
    lexical: &super::local::CandidateLexicalScores,
) -> Composition {
    let qualified = !answer.query.qualified_clauses().is_empty();
    let mut ranked = Vec::with_capacity(answer.query.limit());
    let mut admitted = BTreeSet::new();
    let mut semantic_admitted = 0usize;
    let mut suppressed = 0usize;
    let mut augmented = 0usize;
    for candidate in &result.candidates {
        let Some((entity, row)) = answer.candidate_row(candidate.id) else {
            suppressed = suppressed.saturating_add(1);
            continue;
        };
        let relevance = lexical.score(entity);
        if relevance.is_none() {
            if qualified {
                suppressed = suppressed.saturating_add(1);
                continue;
            }
            match policy {
                CompositionPolicy::RerankLexical => {
                    suppressed = suppressed.saturating_add(1);
                    continue;
                }
                CompositionPolicy::AugmentCanonical { max_additions }
                    if augmented >= max_additions =>
                {
                    suppressed = suppressed.saturating_add(1);
                    continue;
                }
                CompositionPolicy::AugmentCanonical { .. } => {
                    augmented = augmented.saturating_add(1);
                }
            }
        }
        if admitted.insert(entity) {
            semantic_admitted = semantic_admitted.saturating_add(1);
            ranked.push(RankedRow {
                row,
                lexical_relevance: relevance,
            });
        }
    }
    for local in &answer.rows {
        let Some(entity) = answer.entity_for_row(local.row.id) else {
            continue;
        };
        if admitted.insert(entity) {
            ranked.push(RankedRow {
                row: local.row.clone(),
                lexical_relevance: local.lexical_relevance,
            });
        }
    }
    // Provider scores order rows within each provenance tier. Include the
    // complete retained lexical prefix before this stable sort so an inferred
    // type or external endpoint cannot evict a sourced declaration first.
    // No row is removed from the corpus or from later lexical pages.
    ranked.sort_by_cached_key(|ranked| std::cmp::Reverse(answer.search_origin(ranked.row.id)));
    ranked.truncate(answer.query.limit());
    Composition {
        rows: ranked,
        semantic_admitted,
        suppressed,
        augmented,
    }
}

fn compose_controlled(
    answer: &LocalAnswer,
    policy: CompositionPolicy,
    result: &semantic::VectorSearchResult,
    lexical: &super::local::CandidateLexicalScores,
    checkpoint: &mut impl FnMut() -> Result<(), super::LocalSemanticError>,
) -> Result<Composition, super::LocalSemanticError> {
    checkpoint()?;
    let qualified = !answer.query.qualified_clauses().is_empty();
    let mut ranked = Vec::new();
    ranked.try_reserve_exact(answer.rows.len().saturating_add(result.candidates.len()))?;
    let mut admitted = BTreeSet::new();
    let mut semantic_admitted = 0usize;
    let mut suppressed = 0usize;
    let mut augmented = 0usize;
    for candidate in &result.candidates {
        checkpoint()?;
        let Some((entity, row)) = answer.candidate_row(candidate.id) else {
            suppressed = suppressed.saturating_add(1);
            continue;
        };
        let relevance = lexical.score(entity);
        if relevance.is_none() {
            if qualified {
                suppressed = suppressed.saturating_add(1);
                continue;
            }
            match policy {
                CompositionPolicy::RerankLexical => {
                    suppressed = suppressed.saturating_add(1);
                    continue;
                }
                CompositionPolicy::AugmentCanonical { max_additions }
                    if augmented >= max_additions =>
                {
                    suppressed = suppressed.saturating_add(1);
                    continue;
                }
                CompositionPolicy::AugmentCanonical { .. } => {
                    augmented = augmented.saturating_add(1);
                }
            }
        }
        if admitted.insert(entity) {
            semantic_admitted = semantic_admitted.saturating_add(1);
            ranked.push(RankedRow {
                row,
                lexical_relevance: relevance,
            });
        }
    }
    for local in &answer.rows {
        checkpoint()?;
        let Some(entity) = answer.entity_for_row(local.row.id) else {
            continue;
        };
        if admitted.insert(entity) {
            ranked.push(RankedRow {
                row: local.row.clone(),
                lexical_relevance: local.lexical_relevance,
            });
        }
    }
    // Provider scores order rows within each provenance tier. Include the
    // complete retained lexical prefix before this stable sort so an inferred
    // type or external endpoint cannot evict a sourced declaration first.
    // No row is removed from the corpus or from later lexical pages.
    let ranked = super::controlled_sort::sort_checked(
        ranked,
        |row| std::cmp::Reverse(answer.search_origin(row.row.id)),
        checkpoint,
    )?;
    let mut rows = Vec::new();
    rows.try_reserve_exact(answer.query.limit())?;
    for row in ranked {
        checkpoint()?;
        if rows.len() < answer.query.limit() {
            rows.push(row);
        }
    }
    checkpoint()?;
    Ok(Composition {
        rows,
        semantic_admitted,
        suppressed,
        augmented,
    })
}

const fn unavailable(freshness: Freshness) -> LaneReport {
    LaneReport {
        lane: Lane::Semantic,
        coverage: CoverageBasis::Unavailable,
        freshness,
        source: None,
    }
}

/// Semantic setup error reserved for callers that choose strict admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticError {
    /// Vector materialization names another selected workspace.
    StaleWorkspace,
}

impl fmt::Display for SemanticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("semantic materialization is stale for the selected workspace")
    }
}

impl std::error::Error for SemanticError {}

#[cfg(test)]
mod controlled_composition_tests {
    use super::*;
    use crate::builtin::query::{LocalQuery, QueryCoordinator};

    #[test]
    fn composition_preserves_stable_provenance_order_and_stops_at_each_row() {
        let (workspace, view) = super::super::tests::selected_view();
        let coverage = crate::builtin::admitted_coverage().expect("coverage");
        let evidence = super::super::tests::semantic_evidence(workspace, &view);
        let coordinator = QueryCoordinator::new(
            workspace,
            view.clone(),
            view.capability().expect("capability"),
            coverage,
            evidence,
        )
        .expect("coordinator");
        let local = coordinator
            .search_local(LocalQuery::prefix("alpha", 2).expect("query"))
            .expect("local");
        let entities = local
            .matches
            .iter()
            .rev()
            .map(|(entity, _)| *entity)
            .collect::<Vec<_>>();
        let lexical = local
            .lexical_relevance_for_candidates(&entities)
            .expect("scores");
        let root =
            backend_version::RelationState::<semantic::CandidateRelation>::empty(coverage).root();
        let binding = coordinator
            .semantic_binding(root, semantic::Recipe::from_value(b"composition-control"));
        let candidates = entities
            .iter()
            .filter_map(|entity| local.candidate_for_entity(*entity))
            .enumerate()
            .map(|(order, id)| semantic::ScoredCandidate {
                id,
                score: order as f32,
            })
            .collect::<Vec<_>>();
        let result = semantic::VectorSearchResult {
            binding,
            coverage,
            candidates,
            quality: semantic::SearchQuality::Exact,
        };
        let policy = CompositionPolicy::AugmentCanonical { max_additions: 2 };
        // Independent original stable-tier oracle over admitted row IDs, not bucket implementation.
        let mut seen = BTreeSet::new();
        let mut expected = Vec::new();
        for candidate in &result.candidates {
            let (entity, row) = local.candidate_row(candidate.id).expect("candidate");
            if seen.insert(entity) {
                expected.push(RankedRow {
                    row,
                    lexical_relevance: lexical.score(entity),
                });
            }
        }
        for row in &local.rows {
            let entity = local.entity_for_row(row.row.id).expect("entity");
            if seen.insert(entity) {
                expected.push(row.clone());
            }
        }
        expected.sort_by_cached_key(|row| std::cmp::Reverse(local.search_origin(row.row.id)));
        expected.truncate(local.query.limit());
        let mut total = 0;
        let complete = compose_controlled(&local, policy, &result, &lexical, &mut || {
            total += 1;
            Ok::<(), super::super::LocalSemanticError>(())
        })
        .expect("complete");
        assert_eq!(complete.rows, expected);
        assert_eq!(complete.augmented, 0);
        assert_eq!(complete.suppressed, 0);
        assert!(total > result.candidates.len() + local.rows.len());
        for stop_at in [2, result.candidates.len() + 2, total - 2, total] {
            let mut visits = 0;
            let stopped = compose_controlled(&local, policy, &result, &lexical, &mut || {
                visits += 1;
                if visits == stop_at {
                    Err(super::super::LocalSemanticError::Inference(
                        backend_compile::EmbeddingExecutableError::Process(
                            backend_compile::ProcessError::Cancelled,
                        ),
                    ))
                } else {
                    Ok(())
                }
            });
            assert!(matches!(
                stopped,
                Err(super::super::LocalSemanticError::Inference(
                    backend_compile::EmbeddingExecutableError::Process(
                        backend_compile::ProcessError::Cancelled
                    )
                ))
            ));
            assert_eq!(
                visits, stop_at,
                "no row clone/tier traversal after interruption"
            );
        }
    }
}
