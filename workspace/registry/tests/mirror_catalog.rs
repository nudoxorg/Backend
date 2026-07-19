//! Phase 7 (mirror: catalog followers + delete/tombstones) — offline tests.
//!
//! These tests are **offline**: they use wiremock-style local axum servers on
//! `127.0.0.1:0` and a temp-dir tantivy index. No postgres, no qdrant, no
//! terminus. Tests that touch postgres require `REGISTRY_TEST_POSTGRES` (same
//! pattern as other registry tests).
//!
//! Coverage:
//! - `OutboxOp` round-trip through codec functions (unit).
//! - `PackageIndex::remove` — absorb 2, remove 1, query finds only the other.
//! - `PackageIndex::remove` watermark unaffected.
//! - `CatalogCursor` zero / corrupt / round-trip (unit).
//! - NuGet follower: Published event parse (via local HTTP server).
//! - NuGet follower: Withdrawn event from `nuget:PackageDelete`.
//! - crates.io follower: Published event + cursor advances to max timestamp.
//! - crates.io follower: exhausted when re-polled with advanced cursor.

mod common;

use heart::ResolutionState;
use registry::{
	GlobalPackage,
	search::tantivy::PackageIndex,
	upstream::catalog::{CatalogCursor, CatalogEvent, CatalogFollower as _},
};

// ─────────────────────────────────────────────────────────────────────────────
// OutboxOp round-trip
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn outbox_op_tokens_roundtrip() {
	use registry::coordination::OutboxOp;
	use registry::schema::codec::{outbox_op_from_token, outbox_op_token};

	assert_eq!(outbox_op_token(OutboxOp::Upsert), "upsert");
	assert_eq!(outbox_op_token(OutboxOp::Delete), "delete");
	assert_eq!(outbox_op_from_token("upsert").unwrap(), OutboxOp::Upsert);
	assert_eq!(outbox_op_from_token("delete").unwrap(), OutboxOp::Delete);
	assert!(outbox_op_from_token("bogus").is_err());
}

// ─────────────────────────────────────────────────────────────────────────────
// PackageIndex::remove
// ─────────────────────────────────────────────────────────────────────────────

fn unindexed(package: registry::Package) -> GlobalPackage {
	let id = package.id();
	GlobalPackage { id, package, state: ResolutionState::Unindexed { needed: false }, facets: None }
}

#[test]
fn package_index_remove_leaves_other_packages() {
	let dir = common::TempDir::new("remove-two");
	let mut index = PackageIndex::open(dir.path()).expect("open index");

	let pkg_a = unindexed(common::rust_package("my-crate-alpha", "1.0.0"));
	let pkg_b = unindexed(common::rust_package("my-crate-beta", "1.0.0"));

	// Absorb both packages at watermark 1000.
	index.absorb([&pkg_a, &pkg_b], 1000).expect("absorb");

	// Both found before remove.
	let results_before = index.query("my-crate", 10).expect("query before remove");
	assert_eq!(results_before.len(), 2, "both packages should be findable before remove");

	// Remove pkg_a.
	index.remove(pkg_a.id).expect("remove pkg_a");

	// After remove, only pkg_b should be found.
	let results_after = index.query("my-crate", 10).expect("query after remove");
	let found_ids: Vec<_> = results_after.iter().map(|(id, _)| *id).collect();
	assert!(
		found_ids.contains(&pkg_b.id),
		"pkg_b should still be findable after pkg_a removed"
	);
	assert!(
		!found_ids.contains(&pkg_a.id),
		"pkg_a should not be findable after removal"
	);
}

#[test]
fn package_index_remove_watermark_unaffected() {
	let dir = common::TempDir::new("remove-watermark");
	let mut index = PackageIndex::open(dir.path()).expect("open index");

	let pkg = unindexed(common::rust_package("watermark-test-crate", "1.0.0"));
	index.absorb([&pkg], 42_000).expect("absorb");

	let wm_before = index.watermark();
	index.remove(pkg.id).expect("remove");
	let wm_after = index.watermark();

	assert_eq!(wm_before, wm_after, "remove must not advance the sync watermark");
}

// ─────────────────────────────────────────────────────────────────────────────
// CatalogCursor unit tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn catalog_cursor_zero_is_null() {
	let c = CatalogCursor::zero();
	assert!(c.is_zero());
	assert_eq!(c.0, serde_json::Value::Null);
}

#[test]
fn catalog_cursor_round_trips_through_json() {
	let ts = "2024-01-01T00:00:00Z";
	let c = CatalogCursor(serde_json::Value::String(ts.to_owned()));
	let bytes = serde_json::to_vec(&c).expect("serialize");
	let c2: CatalogCursor = serde_json::from_slice(&bytes).expect("deserialize");
	assert_eq!(c2.0.as_str().unwrap(), ts);
}

#[test]
fn catalog_cursor_corrupt_falls_back_to_zero() {
	// The driver's `load_cursor` falls back on corrupt JSON — simulate that.
	let bad_bytes = b"{{not valid json}}";
	let result: Result<CatalogCursor, _> = serde_json::from_slice(bad_bytes);
	assert!(result.is_err(), "corrupt JSON should fail to deserialize");
	// Simulate the driver's fallback:
	let loaded = result.unwrap_or_else(|_| CatalogCursor::zero());
	assert!(loaded.is_zero());
}

// ─────────────────────────────────────────────────────────────────────────────
// NuGet follower — integration-level (offline axum server)
// ─────────────────────────────────────────────────────────────────────────────

/// Build and launch a single-leaf NuGet catalog server on 127.0.0.1:0.
/// The listener is pre-bound before the router is constructed so the base URL
/// is known when building the absolute `@id` references in catalog documents.
async fn launch_nuget_server(
	leaf: serde_json::Value,
	ts: &'static str,
) -> (tokio::task::JoinHandle<()>, String) {
	use axum::{Router, routing::get};
	use tokio::net::TcpListener;

	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let base = format!("http://{}", listener.local_addr().unwrap());

	let index_doc = serde_json::json!({
		"items": [{ "@id": format!("{base}/page0.json"), "commitTimeStamp": ts }]
	});
	let page_doc = serde_json::json!({
		"commitTimeStamp": ts,
		"items": [{ "@id": format!("{base}/leaf0.json"), "commitTimeStamp": ts }]
	});

	let router = Router::new()
		.route("/index.json", get({
			let doc = index_doc.clone();
			move || { let d = doc.clone(); async move { axum::Json(d) } }
		}))
		.route("/page0.json", get({
			let doc = page_doc.clone();
			move || { let d = doc.clone(); async move { axum::Json(d) } }
		}))
		.route("/leaf0.json", get({
			let doc = leaf.clone();
			move || { let d = doc.clone(); async move { axum::Json(d) } }
		}));

	let handle = tokio::spawn(async move { axum::serve(listener, router).await.ok(); });
	(handle, base)
}

#[tokio::test]
async fn nuget_follower_published_event_parsed() {
	use registry::upstream::UpstreamClient;
	use registry::upstream::nuget_catalog::NuGetCatalogFollower;

	let leaf = serde_json::json!({
		"@type": "nuget:PackageDetails",
		"nuget:id": "Newtonsoft.Json",
		"nuget:version": "13.0.3",
		"listed": true
	});
	let (_handle, base) = launch_nuget_server(leaf, "2024-01-01T00:00:00Z").await;

	let follower = NuGetCatalogFollower::new(format!("{base}/index.json"));
	let client = UpstreamClient::new();
	let batch = follower.poll(&client, &CatalogCursor::zero()).await.unwrap();

	assert_eq!(batch.events.len(), 1, "expected exactly one event");
	match &batch.events[0] {
		CatalogEvent::Published { name, version } => {
			assert_eq!(name, "Newtonsoft.Json");
			assert_eq!(version, "13.0.3");
		}
		other => panic!("expected Published, got {other:?}"),
	}
	assert!(!batch.exhausted, "first batch must not be exhausted");
	// Cursor advances to the page timestamp.
	assert_eq!(batch.next.0.as_str().unwrap(), "2024-01-01T00:00:00Z");
}

#[tokio::test]
async fn nuget_follower_withdrawn_from_package_delete() {
	use registry::upstream::UpstreamClient;
	use registry::upstream::nuget_catalog::NuGetCatalogFollower;

	let leaf = serde_json::json!({
		"@type": "nuget:PackageDelete",
		"nuget:id": "Bad.Crate",
		"nuget:version": "1.0.0"
	});
	let (_handle, base) = launch_nuget_server(leaf, "2024-02-01T00:00:00Z").await;

	let follower = NuGetCatalogFollower::new(format!("{base}/index.json"));
	let client = UpstreamClient::new();
	let batch = follower.poll(&client, &CatalogCursor::zero()).await.unwrap();

	assert_eq!(batch.events.len(), 1, "expected exactly one event");
	assert!(
		matches!(&batch.events[0], CatalogEvent::Withdrawn { name, version }
			if name == "Bad.Crate" && version == "1.0.0"),
		"expected Withdrawn for PackageDelete leaf, got {:?}",
		batch.events[0]
	);
}

// ─────────────────────────────────────────────────────────────────────────────
// crates.io follower — integration-level (offline axum server)
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn crates_follower_published_and_cursor_advance() {
	use axum::{Router, routing::get};
	use tokio::net::TcpListener;
	use registry::upstream::UpstreamClient;
	use registry::upstream::crates_catalog::CratesCatalogFollower;

	let body = serde_json::json!({
		"crates": [
			{ "name": "serde", "newest_version": "1.0.200", "updated_at": "2024-06-01T12:00:00Z", "yanked": false },
			{ "name": "tokio", "newest_version": "1.40.0", "updated_at": "2024-06-02T12:00:00Z", "yanked": false },
		]
	});
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let addr = listener.local_addr().unwrap();
	let router = Router::new()
		.route("/crates", get(move || { let b = body.clone(); async move { axum::Json(b) } }));
	tokio::spawn(async move { axum::serve(listener, router).await.ok(); });

	let base = format!("http://{addr}");
	let follower = CratesCatalogFollower::new(base.clone());
	let client = UpstreamClient::new();
	let batch = follower.poll(&client, &CatalogCursor::zero()).await.unwrap();

	assert_eq!(batch.events.len(), 2, "both crates should yield events");
	assert!(
		batch.events.iter().all(|e| matches!(e, CatalogEvent::Published { .. })),
		"all events should be Published"
	);
	// Cursor advances to the maximum updated_at in the feed.
	assert_eq!(
		batch.next.0.as_str().unwrap(),
		"2024-06-02T12:00:00Z",
		"cursor should advance to max updated_at"
	);
	assert!(!batch.exhausted);
}

#[tokio::test]
async fn crates_follower_exhausted_when_re_polled_with_advanced_cursor() {
	use axum::{Router, routing::get};
	use tokio::net::TcpListener;
	use registry::upstream::UpstreamClient;
	use registry::upstream::crates_catalog::CratesCatalogFollower;

	// Feed contains one entry at a fixed timestamp.
	let ts = "2024-06-03T10:00:00Z";
	let body = serde_json::json!({
		"crates": [
			{ "name": "serde", "newest_version": "1.0.200", "updated_at": ts, "yanked": false },
		]
	});
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let addr = listener.local_addr().unwrap();
	let router = Router::new()
		.route("/crates", get(move || { let b = body.clone(); async move { axum::Json(b) } }));
	tokio::spawn(async move { axum::serve(listener, router).await.ok(); });

	let base = format!("http://{addr}");
	let follower = CratesCatalogFollower::new(base);
	let client = UpstreamClient::new();

	// First poll — finds the entry.
	let batch1 = follower.poll(&client, &CatalogCursor::zero()).await.unwrap();
	assert_eq!(batch1.events.len(), 1);
	assert!(!batch1.exhausted);

	// Re-poll with advanced cursor — no new entries → exhausted.
	let batch2 = follower.poll(&client, &batch1.next).await.unwrap();
	assert!(batch2.exhausted, "re-poll with current cursor should be exhausted");
	assert!(batch2.events.is_empty(), "no new events expected");
}
