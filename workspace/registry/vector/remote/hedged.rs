//! Hedged local + remote search (09-vector §20.5, R5/R6).
//!
//! [`hedged`] races a local search future against a remote search future:
//!
//! - `local` results are awaited unconditionally (they are the primary
//!   authoritative result set).
//! - `remote` results are awaited for up to `budget` (default: 250ms).
//! - If remote lands within the budget, the two result sets are merged via
//!   RRF fusion (R6: rank-based only; raw scores are not comparable across
//!   models/planes).
//! - If remote times out, the outcome is returned with `remote_omitted: true`
//!   — the flag is *never* silently dropped (I14: the caller can surface a
//!   "partial results" indicator to the user).
//! - Every hit is labeled by [`SourceTag`] so provenance survives the merge.

use std::future::Future;
use std::time::Duration;

use crate::vector::core::{
    FusedHit, RankedList, SearchHit, SourceTag,
    fusion::{RRF_K, rrf_fuse},
};

/// Default remote-side time budget: if the remote search does not complete
/// within this window, local-only results are returned with
/// `remote_omitted: true` (I14).
pub const DEFAULT_HEDGE_BUDGET: Duration = Duration::from_millis(250);

/// The outcome of a hedged search.
#[derive(Debug)]
pub struct HedgedOutcome {
    /// The local result set, returned first (always present).
    pub first: Vec<SearchHit>,

    /// The fused (local + remote) result set via RRF. `None` if the remote
    /// timed out.
    pub merged: Option<Vec<FusedHit>>,

    /// `true` iff the remote search did not complete within `budget`. When
    /// this is `true`, `merged` is `None` and only `first` is available.
    ///
    /// I14: this flag MUST NOT be silently dropped; callers must propagate it
    /// so users can see "partial results" if their UI has such an indicator.
    pub remote_omitted: bool,
}

/// Race `local` and `remote` futures, merging results by RRF when both land.
///
/// - `local` is awaited first (no timeout — local search must always succeed).
/// - `remote` is then raced against `budget`. On timeout, `remote_omitted`
///   is set to `true` and `merged` is `None`.
/// - When both land, hits are merged by [`rrf_fuse`] with [`RRF_K`] (R6).
///
/// Every hit in the fused list carries the [`SourceTag`] from its source,
/// and fused hits carry all sources they appeared in.
pub async fn hedged<LF, RF, LE, RE>(local: LF, remote: RF, budget: Duration) -> HedgedOutcome
where
    LF: Future<Output = Result<Vec<SearchHit>, LE>>,
    RF: Future<Output = Result<Vec<SearchHit>, RE>>,
    LE: std::fmt::Debug,
    RE: std::fmt::Debug,
{
    // Await local unconditionally — it is the primary source.
    let local_hits = match local.await {
        Ok(hits) => hits,
        Err(e) => {
            tracing::warn!(error = ?e, "local search failed in hedged combinator");
            Vec::new()
        }
    };

    // Race remote against the budget.
    let remote_result = tokio::time::timeout(budget, remote).await;

    match remote_result {
        Ok(Ok(remote_hits)) => {
            let merged = fuse(&local_hits, &remote_hits);
            HedgedOutcome {
                first: local_hits,
                merged: Some(merged),
                remote_omitted: false,
            }
        }
        Ok(Err(e)) => {
            tracing::warn!(error = ?e, "remote search returned an error in hedged combinator");
            HedgedOutcome {
                first: local_hits,
                merged: None,
                remote_omitted: true,
            }
        }
        Err(_timeout) => {
            tracing::debug!(
                budget_ms = budget.as_millis(),
                "remote search timed out; returning local-only results (I14)"
            );
            HedgedOutcome {
                first: local_hits,
                merged: None,
                remote_omitted: true,
            }
        }
    }
}

/// Build fused hits from two ranked lists via RRF (R6).
fn fuse(local: &[SearchHit], remote: &[SearchHit]) -> Vec<FusedHit> {
    let local_list = RankedList {
        source: SourceTag::Local,
        ids: local.iter().map(|h| h.id).collect(),
    };
    let remote_list = RankedList {
        source: remote.first().map_or(SourceTag::IndexJina, |h| h.source),
        ids: remote.iter().map(|h| h.id).collect(),
    };
    rrf_fuse(&[local_list, remote_list], RRF_K)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::core::{Payload, PointId, SearchHit, SourceTag};
    use uuid::Uuid;

    fn hit(n: u128, source: SourceTag) -> SearchHit {
        SearchHit {
            id: PointId::from_uuid(Uuid::from_u128(n)),
            score: (n as f32).mul_add(-0.01, 0.9),
            payload: Payload::default(),
            source,
        }
    }

    /// When the remote completes quickly, results are fused and remote_omitted
    /// is false.
    #[tokio::test]
    async fn remote_lands_before_budget_fuses_results() {
        let local_hits = vec![hit(1, SourceTag::Local), hit(2, SourceTag::Local)];
        let remote_hits = vec![
            hit(1, SourceTag::IndexVoyage),
            hit(3, SourceTag::IndexVoyage),
        ];

        let local_fut = async { Ok::<_, String>(local_hits.clone()) };
        let remote_fut = async { Ok::<_, String>(remote_hits.clone()) };

        let outcome = hedged(local_fut, remote_fut, DEFAULT_HEDGE_BUDGET).await;

        assert!(
            !outcome.remote_omitted,
            "remote landed; remote_omitted must be false"
        );
        assert!(
            outcome.merged.is_some(),
            "merged must be Some when remote lands"
        );
        let merged = outcome.merged.unwrap();
        // point 1 appears in both lists → highest RRF score.
        assert_eq!(merged[0].id, hit(1, SourceTag::Local).id);
        assert!(
            merged[0].sources.len() == 2,
            "hit 1 must be from both sources"
        );
    }

    /// When the remote times out, remote_omitted is true and merged is None.
    #[tokio::test]
    async fn remote_timeout_sets_remote_omitted() {
        let local_hits = vec![hit(1, SourceTag::Local)];

        let local_fut = async { Ok::<_, String>(local_hits.clone()) };
        // Remote sleeps longer than the budget.
        let remote_fut = async {
            tokio::time::sleep(Duration::from_mins(1)).await;
            Ok::<_, String>(Vec::new())
        };

        let outcome = hedged(local_fut, remote_fut, Duration::from_millis(1)).await;

        assert!(
            outcome.remote_omitted,
            "timeout must set remote_omitted = true (I14)"
        );
        assert!(outcome.merged.is_none(), "merged must be None on timeout");
        assert_eq!(
            outcome.first.len(),
            1,
            "local results must still be present"
        );
    }

    /// When the remote returns an error, remote_omitted is true.
    #[tokio::test]
    async fn remote_error_sets_remote_omitted() {
        let local_hits = vec![hit(1, SourceTag::Local)];

        let local_fut = async { Ok::<_, String>(local_hits) };
        let remote_fut = async { Err::<Vec<SearchHit>, _>("remote failure".to_owned()) };

        let outcome = hedged(local_fut, remote_fut, DEFAULT_HEDGE_BUDGET).await;

        assert!(
            outcome.remote_omitted,
            "remote error must set remote_omitted = true"
        );
        assert!(outcome.merged.is_none());
    }

    /// When local fails, first is empty but we still proceed.
    #[tokio::test]
    async fn local_failure_produces_empty_first() {
        let local_fut = async { Err::<Vec<SearchHit>, _>("local failure".to_owned()) };
        let remote_fut = async { Ok::<_, String>(vec![hit(1, SourceTag::IndexJina)]) };

        let outcome = hedged(local_fut, remote_fut, DEFAULT_HEDGE_BUDGET).await;

        assert!(outcome.first.is_empty());
        // Remote landed and local was empty, so merged exists.
        assert!(outcome.merged.is_some());
        assert!(!outcome.remote_omitted);
    }

    // ── Adversarial: late remote not merged; 1ms-early resolve merged ─────────

    /// Remote resolves AFTER the budget → `remote_omitted = true`, `merged = None`.
    /// The late result is NOT merged — never use-after-deadline.
    #[tokio::test]
    async fn remote_resolves_after_budget_value_not_merged() {
        let budget = Duration::from_millis(10);
        let local_hits = vec![hit(1, SourceTag::Local), hit(2, SourceTag::Local)];

        let local_fut = async { Ok::<_, String>(local_hits.clone()) };
        // Remote takes 500ms — well after the 10ms budget.
        let remote_fut = async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            // This value must NEVER appear in merged.
            Ok::<_, String>(vec![hit(99, SourceTag::IndexVoyage)])
        };

        let outcome = hedged(local_fut, remote_fut, budget).await;

        assert!(
            outcome.remote_omitted,
            "late remote must set remote_omitted=true (I14)"
        );
        assert!(
            outcome.merged.is_none(),
            "merged must be None; late result not merged"
        );
        // first must carry the local results intact.
        assert_eq!(outcome.first.len(), 2, "local results must be present");
        let first_ids: Vec<_> = outcome.first.iter().map(|h| h.id).collect();
        assert!(first_ids.contains(&hit(1, SourceTag::Local).id));
        assert!(first_ids.contains(&hit(2, SourceTag::Local).id));
        // Point 99 (the late remote result) must not appear anywhere.
        assert!(!first_ids.contains(&hit(99, SourceTag::IndexVoyage).id));
    }

    /// Remote resolves 1ms before the budget (200ms budget, 1ms sleep) →
    /// `remote_omitted = false`, `merged = Some(...)`.
    #[tokio::test]
    async fn remote_resolves_one_ms_before_budget_is_merged() {
        let budget = Duration::from_millis(200);
        let local_hits = vec![hit(1, SourceTag::Local)];
        let remote_hits = vec![hit(2, SourceTag::IndexVoyage)];

        let local_fut = async { Ok::<_, String>(local_hits) };
        let remote_fut = async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            Ok::<_, String>(remote_hits)
        };

        let outcome = hedged(local_fut, remote_fut, budget).await;

        assert!(
            !outcome.remote_omitted,
            "remote within budget; remote_omitted must be false"
        );
        assert!(
            outcome.merged.is_some(),
            "merged must be Some when remote lands in time"
        );
        let merged = outcome.merged.unwrap();
        let ids: Vec<_> = merged.iter().map(|h| h.id).collect();
        assert!(
            ids.contains(&hit(1, SourceTag::Local).id),
            "local hit must be in merged"
        );
        assert!(
            ids.contains(&hit(2, SourceTag::IndexVoyage).id),
            "remote hit must be in merged"
        );
    }

    /// Local failure + remote success: exact contract.
    ///   - `first` is empty (local failed).
    ///   - `remote_omitted` is false (remote succeeded).
    ///   - `merged` is Some (remote hits were fused with the empty local).
    #[tokio::test]
    async fn local_failure_remote_success_exact_contract() {
        let remote_hits = vec![hit(7, SourceTag::IndexVoyage)];
        let local_fut = async { Err::<Vec<SearchHit>, _>("disk error".to_owned()) };
        let remote_fut = async { Ok::<_, String>(remote_hits.clone()) };

        let outcome = hedged(local_fut, remote_fut, DEFAULT_HEDGE_BUDGET).await;

        assert!(
            outcome.first.is_empty(),
            "first must be empty when local fails"
        );
        assert!(
            !outcome.remote_omitted,
            "remote succeeded; remote_omitted must be false"
        );
        assert!(
            outcome.merged.is_some(),
            "merged must be Some even when local was empty"
        );
        let merged = outcome.merged.unwrap();
        assert_eq!(merged.len(), 1, "one remote hit in merged output");
        assert_eq!(
            merged[0].id,
            hit(7, SourceTag::IndexVoyage).id,
            "remote hit present in merged"
        );
    }

    // ── Adversarial: R6 — score magnitudes must NOT leak into fusion ──────────

    /// R6 (09-vector §20.5): fusion is rank-based only. Raw scores from different
    /// planes are not comparable and must never affect the fused order.
    ///
    /// Scenario:
    ///   local:  [A (rank 1), B (rank 2), C (rank 3)]  scores: 0.1, 0.09, 0.08
    ///   remote: [C (rank 1), D (rank 2), A (rank 3)]  scores: 999.0, 998.0, 997.0
    ///
    /// RRF scores (k=60):
    ///   A: 1/61 + 1/63 ≈ 0.03226
    ///   C: 1/63 + 1/61 ≈ 0.03226  (tie with A; tiebreak: PointId ascending)
    ///   B: 1/62          ≈ 0.01613
    ///   D: 1/62          ≈ 0.01613  (tie with B; tiebreak: PointId ascending)
    ///
    /// PointIds: A=1, B=2, C=3, D=4 → order: [A, C, B, D].
    ///
    /// The remote scores of 997–999 must have zero effect on this order.
    #[tokio::test]
    async fn fusion_rank_only_high_remote_scores_do_not_promote() {
        let a = PointId::from_uuid(uuid::Uuid::from_u128(1));
        let b = PointId::from_uuid(uuid::Uuid::from_u128(2));
        let c = PointId::from_uuid(uuid::Uuid::from_u128(3));
        let d = PointId::from_uuid(uuid::Uuid::from_u128(4));

        fn sh(id: PointId, score: f32, source: SourceTag) -> SearchHit {
            SearchHit {
                id,
                score,
                payload: Payload::default(),
                source,
            }
        }

        // Local: A (rank 1, low score), B (rank 2, low score), C (rank 3, low score)
        // Remote: C (rank 1, huge score), D (rank 2, huge score), A (rank 3, huge score)
        let local_hits = vec![
            sh(a, 0.10, SourceTag::Local),
            sh(b, 0.09, SourceTag::Local),
            sh(c, 0.08, SourceTag::Local),
        ];
        let remote_hits = vec![
            sh(c, 999.0, SourceTag::IndexVoyage),
            sh(d, 998.0, SourceTag::IndexVoyage),
            sh(a, 997.0, SourceTag::IndexVoyage),
        ];

        let local_fut = async { Ok::<_, String>(local_hits) };
        let remote_fut = async { Ok::<_, String>(remote_hits) };

        let outcome = hedged(local_fut, remote_fut, DEFAULT_HEDGE_BUDGET).await;

        assert!(!outcome.remote_omitted, "both planes present");
        let merged = outcome.merged.expect("merged must be Some");

        assert_eq!(
            merged.len(),
            4,
            "all 4 unique points in fused output: {merged:#?}"
        );

        // A and C tied; A (uuid 1) < C (uuid 3) → A first, C second.
        assert_eq!(
            merged[0].id, a,
            "A must lead (RRF tie → smallest PointId first)"
        );
        assert_eq!(
            merged[1].id, c,
            "C must be second (same RRF score as A, larger id)"
        );

        // B and D tied; B (uuid 2) < D (uuid 4) → B third, D fourth.
        assert_eq!(merged[2].id, b, "B must be third");
        assert_eq!(merged[3].id, d, "D must be last");

        // Every rrf_score must be << 1.0 — not the raw scores of 997–999.
        for fh in &merged {
            assert!(
                fh.rrf_score < 1.0,
                "rrf_score {:.6} looks like raw score leaked; id={:?}",
                fh.rrf_score,
                fh.id
            );
        }
    }

    /// A hit that appears in both lists carries both SourceTags in `merged`.
    /// This is the cross-plane provenance invariant.
    #[tokio::test]
    async fn cross_plane_hit_carries_both_sources() {
        let shared = PointId::from_uuid(uuid::Uuid::from_u128(42));

        let local_hits = vec![SearchHit {
            id: shared,
            score: 0.5,
            payload: Payload::default(),
            source: SourceTag::Local,
        }];
        let remote_hits = vec![SearchHit {
            id: shared,
            score: 0.99,
            payload: Payload::default(),
            source: SourceTag::IndexVoyage,
        }];

        let local_fut = async { Ok::<_, String>(local_hits) };
        let remote_fut = async { Ok::<_, String>(remote_hits) };

        let outcome = hedged(local_fut, remote_fut, DEFAULT_HEDGE_BUDGET).await;
        let merged = outcome.merged.expect("merged must be Some");

        assert_eq!(merged.len(), 1, "one unique point");
        let fh = &merged[0];
        assert_eq!(fh.id, shared);
        assert_eq!(
            fh.sources.len(),
            2,
            "cross-plane hit must carry both sources; got {:?}",
            fh.sources
        );
        assert!(fh.sources.contains(&SourceTag::Local));
        assert!(fh.sources.contains(&SourceTag::IndexVoyage));
    }
}
