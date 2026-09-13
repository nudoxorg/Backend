use super::local::{
    CoverageBasis, Freshness, Lane, LaneReport, LocalAnswer, QueryResult, RankedRow, SourceBasis,
};
use backend_extension_qdrant as semantic;
use std::collections::{BTreeMap, BTreeSet};
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
        let max_additions = self.query.limit();
        self.accelerate_with(
            CompositionPolicy::AugmentCanonical { max_additions },
            acceleration,
        )
    }

    /// Applies an explicit bounded composition policy.
    #[must_use]
    pub fn accelerate_with<S>(
        mut self,
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
        let composed = compose(&self, policy, &result);
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
    let desired = answer.query.limit().saturating_add(additions);
    desired
        .saturating_mul(4)
        .max(desired)
        .min(semantic::Limits::default().max_page)
        .min(answer.corpus.entities.len().max(1))
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
) -> Composition {
    let lexical = answer.matches.iter().copied().collect::<BTreeMap<_, _>>();
    let mut ranked = Vec::with_capacity(answer.query.limit());
    let mut admitted = BTreeSet::new();
    let mut semantic_admitted = 0usize;
    let mut suppressed = 0usize;
    let mut augmented = 0usize;
    for candidate in &result.candidates {
        let Some(entity) = answer.corpus.candidates.get(&candidate.id).copied() else {
            suppressed = suppressed.saturating_add(1);
            continue;
        };
        let Some(row) = answer
            .corpus
            .entities
            .get(&entity)
            .and_then(|id| answer.corpus.view.row(*id))
        else {
            suppressed = suppressed.saturating_add(1);
            continue;
        };
        let relevance = lexical.get(&entity).copied();
        if relevance.is_none() {
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
    for (entity, relevance) in &answer.matches {
        if ranked.len() == answer.query.limit() {
            break;
        }
        if admitted.insert(*entity)
            && let Some(row) = answer
                .corpus
                .entities
                .get(entity)
                .and_then(|id| answer.corpus.view.row(*id))
        {
            ranked.push(RankedRow {
                row,
                lexical_relevance: Some(*relevance),
            });
        }
    }
    ranked.truncate(answer.query.limit());
    Composition {
        rows: ranked,
        semantic_admitted,
        suppressed,
        augmented,
    }
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
