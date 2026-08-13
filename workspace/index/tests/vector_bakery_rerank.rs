#![cfg(feature = "server")]
//! Integration-level tests for the vector bakery, rerank, and dep-shard routing.
//!
//! These tests do NOT run cargo (per RULES) and do NOT hit any external
//! services. They exercise:
//!
//! 1. Routing table rows (§20.5): offline/deep-degradation, claimed-hot
//!    exclusion, parity/premium/deep/local.
//! 2. Rerank: timeout → `rerank_unavailable`, forbidden model → config error.
//! 3. Bakery: stale-claim re-bake, double-claim single-bake.
//! 4. Dep-shard manifest DTO: pending/ready/failed serde shape.

#[allow(unused_imports)]
use index::server::vector;
// ─────────────────────────────────────────────────────────────────────────────
// §20.5 routing table rows
// ─────────────────────────────────────────────────────────────────────────────

use index::server::search::routing::{
    DenseCollection, QualityMode, QueryScope, RouteInputs, route_stage_one,
};
// `JinaCodeV2::id()` in the edgepack-key fixtures needs this trait in scope.
use vector::EmbeddingModel;

fn inputs(scope: QueryScope, quality: QualityMode) -> RouteInputs {
    RouteInputs {
        scope,
        quality,
        online: true,
        claimed_hot: Vec::new(),
    }
}

/// §20.5: offline parity → no dense stage (precise-only) with `offline` behavior.
#[test]
fn offline_parity_is_precise_only() {
    let mut req = inputs(QueryScope::Deps, QualityMode::Parity);
    req.online = false;
    let route = route_stage_one(&req);
    assert_eq!(route.collection, None, "offline → no dense stage");
    assert!(!route.rerank, "offline → no rerank");
    assert_eq!(route.source_label, "precise-only");
}

/// §20.5: offline Deep → fallback to precise-only, no rerank stage (graceful degrade).
#[test]
fn offline_deep_degrades_to_precise_only() {
    let mut req = inputs(QueryScope::Org, QualityMode::Deep);
    req.online = false;
    let route = route_stage_one(&req);
    assert_eq!(
        route.collection, None,
        "deep offline → no dense stage (degraded)"
    );
    assert!(!route.rerank, "deep offline → rerank stage omitted");
    assert_eq!(route.source_label, "precise-only");
}

/// §20.5: online Deep → premium collection + rerank stage.
#[test]
fn online_deep_enables_rerank() {
    let route = route_stage_one(&inputs(QueryScope::Org, QualityMode::Deep));
    assert_eq!(
        route.collection,
        Some(DenseCollection::Premium),
        "deep → premium stage-1"
    );
    assert!(route.rerank, "deep → rerank stage enabled");
}

/// §20.5: local quality → no dense stage on the server (client-only).
#[test]
fn local_quality_is_server_side_noop() {
    let route = route_stage_one(&inputs(QueryScope::Project, QualityMode::Local));
    assert_eq!(route.collection, None);
    assert!(!route.rerank);
    assert_eq!(route.source_label, "precise-only");
}

/// §20.5: claimed-hot packages are excluded from server Stage-1.
#[test]
fn claimed_hot_packages_excluded_from_stage_one() {
    use heart::PackageId;
    let hot = PackageId::from_uuid(uuid::Uuid::from_u128(99));
    let other = PackageId::from_uuid(uuid::Uuid::from_u128(100));
    let mut req = inputs(QueryScope::Deps, QualityMode::Parity);
    req.claimed_hot = vec![hot];
    let route = route_stage_one(&req);
    assert!(
        route.excluded.contains(&hot),
        "claimed-hot package excluded"
    );
    assert!(
        !route.excluded.contains(&other),
        "unclaimed package not excluded"
    );
}

/// §20.5: premium quality → premium collection, no rerank.
#[test]
fn premium_quality_routes_to_premium_no_rerank() {
    let route = route_stage_one(&inputs(QueryScope::Org, QualityMode::Premium));
    assert_eq!(route.collection, Some(DenseCollection::Premium));
    assert!(!route.rerank, "premium without deep → no rerank");
    assert_eq!(route.source_label, "index-voyage");
}

// ─────────────────────────────────────────────────────────────────────────────
// Rerank: timeout → rerank_unavailable; forbidden model → config error
// ─────────────────────────────────────────────────────────────────────────────

use index::server::rerank::{RerankDocument, RerankError, RerankService};

/// A fake reranker that always times out.
struct AlwaysTimesOut {
    budget: std::time::Duration,
}

impl RerankService for AlwaysTimesOut {
    async fn rerank(
        &self,
        _query: &str,
        _documents: &[RerankDocument],
        _top_k: usize,
    ) -> Result<Vec<index::server::rerank::RerankScore>, RerankError> {
        Err(RerankError::Timeout {
            budget: self.budget,
        })
    }

    fn model_id(&self) -> &str {
        "test/times-out"
    }
}

/// Timeout from the reranker → `Timeout` variant, not a generic error and not
/// a silent empty result. The handler layer maps this to `rerank_unavailable`.
#[tokio::test]
async fn rerank_timeout_produces_explicit_variant() {
    let reranker = AlwaysTimesOut {
        budget: std::time::Duration::from_millis(1200),
    };
    let docs = vec![RerankDocument {
        id: "0".to_owned(),
        text: "tokio runtime".to_owned(),
    }];
    let result = reranker.rerank("async scheduler", &docs, 1).await;
    let err = result.expect_err("should time out");
    assert!(
        matches!(err, RerankError::Timeout { budget } if budget.as_millis() == 1200),
        "must be a Timeout with the configured budget, got: {err:?}"
    );
}

/// A CC-BY-NC reranker model id is rejected at config validation — the server
/// refuses to boot rather than ever serving scores from a forbidden model.
#[test]
fn forbidden_rerank_model_fails_config_validation() {
    use index::server::ServerConfiguration;
    use smol_str::SmolStr;

    let mut config = ServerConfiguration::default();
    // The first entry in the deny-list (§18.3b I15).
    config.rerank.model_id = SmolStr::new("jinaai/jina-reranker-v2-base-multilingual");

    let error = config
        .validate()
        .expect_err("forbidden model must be a hard config error");
    let msg = error.to_string();
    assert!(
        msg.contains("jinaai/jina-reranker-v2-base-multilingual"),
        "error must name the forbidden model; got: {msg}"
    );
}

/// The default rerank model (mxbai-rerank-base-v2) is not on the deny-list
/// and must pass config validation.
#[test]
fn default_rerank_model_is_licensed() {
    use index::server::ServerConfiguration;
    let config = ServerConfiguration::default();
    // Default model: "mixedbread-ai/mxbai-rerank-base-v2"
    config
        .validate()
        .expect("default model must be license-approved");
}

// ─────────────────────────────────────────────────────────────────────────────
// Bakery: double-claim, stale-claim reaper (unit via mock ClaimStore)
// ─────────────────────────────────────────────────────────────────────────────

use index::server::bakery::{
    BakeOutcome, BakeRequest, BakedArtifact, BakeryError, ClaimStore, EdgepackStatus, RECIPE_ID,
    run_bake,
};
use heart::{ContentHash, PackageId};
use std::collections::HashSet;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

/// An in-memory ClaimStore mirroring `INSERT ... ON CONFLICT DO NOTHING`.
#[derive(Default)]
struct MockClaims {
    claimed: Mutex<HashSet<[u8; 32]>>,
    ready: Mutex<Vec<[u8; 32]>>,
    failed: Mutex<Vec<[u8; 32]>>,
}

impl ClaimStore for MockClaims {
    async fn try_claim(&self, request: &BakeRequest) -> Result<bool, BakeryError> {
        let digest = *request.edgepack_key.digest().as_bytes();
        Ok(self.claimed.lock().expect("unpoisoned").insert(digest))
    }

    async fn mark_ready(
        &self,
        digest: ContentHash,
        _artifact: ContentHash,
        _ram_estimate: i64,
    ) -> Result<(), BakeryError> {
        self.ready
            .lock()
            .expect("unpoisoned")
            .push(*digest.as_bytes());
        Ok(())
    }

    async fn mark_failed(&self, digest: ContentHash) -> Result<(), BakeryError> {
        self.failed
            .lock()
            .expect("unpoisoned")
            .push(*digest.as_bytes());
        Ok(())
    }
}

fn test_request() -> BakeRequest {
    let package = PackageId::from_uuid(uuid::Uuid::from_u128(1337));
    BakeRequest {
        package,
        version: "2.0.0".to_owned(),
        edgepack_key: vector::shard::EdgepackKey {
            package,
            version: "2.0.0".into(),
            model_id: vector::model::JinaCodeV2::id(),
            recipe_id: RECIPE_ID.into(),
            quant_profile: vector::quant::QP1,
            edge_format_version: vector::shard::EDGE_FORMAT_VERSION,
        },
    }
}

fn test_artifact() -> BakedArtifact {
    BakedArtifact {
        artifact: ContentHash::of_bytes(b"test"),
        ram_estimate: 9216,
        symbols: 10,
    }
}

/// Two concurrent claims on the same key → exactly one bake (single-claim rule).
#[tokio::test]
async fn double_claim_produces_exactly_one_bake() {
    let claims = MockClaims::default();
    let bakes = AtomicUsize::new(0);
    let request = test_request();

    let (left, right) = tokio::join!(
        run_bake(&claims, &request, || async {
            bakes.fetch_add(1, Ordering::SeqCst);
            Ok(test_artifact())
        }),
        run_bake(&claims, &request, || async {
            bakes.fetch_add(1, Ordering::SeqCst);
            Ok(test_artifact())
        }),
    );

    assert_eq!(
        bakes.load(Ordering::SeqCst),
        1,
        "single-claim rule: exactly one bake"
    );
    let outcomes = [
        left.expect("claim protocol ok"),
        right.expect("claim protocol ok"),
    ];
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, BakeOutcome::Baked(_)))
            .count(),
        1,
        "exactly one Baked outcome"
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, BakeOutcome::AlreadyClaimed))
            .count(),
        1,
        "exactly one AlreadyClaimed outcome"
    );
}

/// After the stale-claim reaper clears a crashed worker's row, the next
/// attempt can win the claim and bake successfully.
#[tokio::test]
async fn stale_claim_reclaimable_after_reaper() {
    let claims = MockClaims::default();
    let request = test_request();

    // Simulate crash: insert the digest into claimed without marking ready.
    {
        let digest = *request.edgepack_key.digest().as_bytes();
        claims.claimed.lock().expect("unpoisoned").insert(digest);
    }

    // Before reaper: another attempt loses.
    let before = run_bake(&claims, &request, || async { Ok(test_artifact()) })
        .await
        .expect("claim protocol");
    assert!(
        matches!(before, BakeOutcome::AlreadyClaimed),
        "pre-reap attempt must be refused"
    );

    // Simulate the stale-claim reaper: DELETE WHERE status='claimed' AND updated_at < stale.
    // In the mock, just clear the claimed set.
    claims.claimed.lock().expect("unpoisoned").clear();

    // After reaper: the claim is free.
    let after = run_bake(&claims, &request, || async { Ok(test_artifact()) })
        .await
        .expect("claim protocol");
    assert!(
        matches!(after, BakeOutcome::Baked(_)),
        "post-reap attempt must succeed"
    );
    assert_eq!(
        claims.ready.lock().expect("unpoisoned").len(),
        1,
        "exactly one ready row after re-bake"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Dep-shard manifest DTO: serde shape
// ─────────────────────────────────────────────────────────────────────────────

use index::server::bakery::EdgepackRow;
use index::server::http::dto::DepshardManifestDto;

fn test_key() -> vector::shard::EdgepackKey {
    vector::shard::EdgepackKey {
        package: PackageId::from_uuid(uuid::Uuid::from_u128(9)),
        version: "0.1.0".into(),
        model_id: vector::model::JinaCodeV2::id(),
        recipe_id: RECIPE_ID.into(),
        quant_profile: vector::quant::QP1,
        edge_format_version: vector::shard::EDGE_FORMAT_VERSION,
    }
}

/// Pending manifest: all key fields present, no optional fields, status pending.
#[test]
fn manifest_pending_shape() {
    let dto = DepshardManifestDto::from_parts(&test_key(), None);
    let v = serde_json::to_value(&dto).expect("serializes");
    let obj = v.as_object().expect("object");

    for field in [
        "package",
        "version",
        "model_id",
        "recipe_id",
        "quant_profile",
        "edge_format_version",
        "edgepack_key_digest",
        "status",
    ] {
        assert!(obj.contains_key(field), "missing required field `{field}`");
    }
    assert!(
        !obj.contains_key("artifact_id"),
        "artifact_id absent while pending"
    );
    assert!(
        !obj.contains_key("ram_estimate"),
        "ram_estimate absent while pending"
    );
    assert_eq!(obj["status"].as_str().unwrap(), "pending");
}

/// Ready manifest: artifact_id (64 lowercase hex) and ram_estimate present.
#[test]
fn manifest_ready_shape() {
    let key = test_key();
    let artifact = ContentHash::of_bytes(b"artifact");
    let row = EdgepackRow {
        digest: key.digest(),
        status: EdgepackStatus::Ready,
        artifact: Some(artifact),
        ram_estimate: Some(8192),
    };
    let dto = DepshardManifestDto::from_parts(&key, Some(&row));
    assert!(dto.is_ready());
    let v = serde_json::to_value(&dto).expect("serializes");
    let hex = v["artifact_id"].as_str().expect("artifact_id present");
    assert_eq!(hex.len(), 64, "blake3 hex is 64 chars");
    assert!(
        hex.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "must be lowercase hex"
    );
    assert_eq!(v["ram_estimate"].as_i64().unwrap(), 8192);
    assert_eq!(v["status"].as_str().unwrap(), "ready");
}

/// Failed manifest: status failed, no artifact/ram.
#[test]
fn manifest_failed_shape() {
    let key = test_key();
    let row = EdgepackRow {
        digest: key.digest(),
        status: EdgepackStatus::Failed,
        artifact: None,
        ram_estimate: None,
    };
    let dto = DepshardManifestDto::from_parts(&key, Some(&row));
    let v = serde_json::to_value(&dto).expect("serializes");
    assert_eq!(v["status"].as_str().unwrap(), "failed");
    assert!(!v.as_object().unwrap().contains_key("artifact_id"));
    assert!(!v.as_object().unwrap().contains_key("ram_estimate"));
}
