//! crates.io catalog follower — the simplest correct V1 implementation.
//!
//! # Protocol
//!
//! crates.io does not have a V3 catalog; the closest equivalent is:
//!
//! 1. **New-crates feed** (`/api/v1/crates?sort=newest&per_page=100`) — polls
//!    for the most recently published crates/versions. Cursor = last-seen
//!    `updated_at` timestamp.
//!
//! 2. **Yanked crates check** — for packages we have held from previous polls,
//!    periodically re-check `/api/v1/crates/{name}/{version}` to see if
//!    `yanked == true`.
//!
//! # Simplifications (V1)
//!
//! This implementation intentionally stays simple and correct rather than
//! maximally efficient:
//!
//! - We poll one page of the newest crates per tick; the cursor advances only
//!   if all events on the page are strictly after the last-seen timestamp.
//! - Yanked checks are *not* implemented in V1 — crates.io does not notify on
//!   yank via the new-crates feed; a follow-up enhancement can add a periodic
//!   yank reconciliation sweep. When crates.io adds a Kafka/Delta feed this
//!   implementation should be replaced.
//! - The `Withdrawn` path is therefore limited to explicit `yanked: true` items
//!   that appear on the newest feed itself (this can happen when a crate is
//!   published and immediately yanked within the poll window).
//!
//! # Cursor encoding
//!
//! The cursor is a JSON string containing the RFC 3339 `updated_at` of the most
//! recently processed crate. The feed is polled newest-first; we collect all
//! items strictly newer than the cursor and return them, advancing the cursor
//! to the maximum seen timestamp.

use ecosystem::Language;
use serde::Deserialize;

use crate::upstream::{CatalogFollower, PollFuture, UpstreamClient, UpstreamError};
use super::catalog::{CatalogBatch, CatalogCursor, CatalogEvent};

/// crates.io new-crates endpoint.
const DEFAULT_API_BASE: &str = "https://crates.io/api/v1";
/// Crates to fetch per poll tick.
const PAGE_SIZE: u32 = 100;

// ─────────────────────────────────────────────────────────────────────────────
// Wire types
// ─────────────────────────────────────────────────────────────────────────────

/// `/api/v1/crates?sort=newest` response.
#[derive(Deserialize)]
struct NewestResponse {
	crates: Vec<CrateEntry>,
}

/// One entry in the newest-crates feed.
#[derive(Deserialize)]
struct CrateEntry {
	/// Crate name.
	name: String,
	/// Newest version string as returned by the feed.
	#[serde(rename = "newest_version")]
	version: String,
	/// RFC 3339 update timestamp — our cursor key.
	updated_at: String,
	/// `true` when the most recent version is yanked.
	#[serde(default)]
	yanked: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Follower
// ─────────────────────────────────────────────────────────────────────────────

/// crates.io new-crates feed follower (V1).
///
/// Polls `/api/v1/crates?sort=newest` once per tick and emits `Published` /
/// `Withdrawn` events for all entries strictly newer than the cursor. The
/// cursor advances to the maximum `updated_at` seen in the batch.
pub struct CratesCatalogFollower {
	api_base: String,
}

impl CratesCatalogFollower {
	/// Construct with a custom API base URL (for tests).
	pub fn new(api_base: impl Into<String>) -> Self {
		Self { api_base: api_base.into() }
	}

	/// Convenience constructor using the production crates.io endpoint.
	pub fn production() -> Self { Self::new(DEFAULT_API_BASE) }

	async fn poll_inner(
		&self,
		client: &UpstreamClient,
		cursor: &CatalogCursor,
	) -> Result<CatalogBatch, UpstreamError> {
		// ── Step 1: current cursor as comparable timestamp string ─────────────
		let after: &str = match &cursor.0 {
			serde_json::Value::String(ts) => ts.as_str(),
			_ => "",
		};

		// ── Step 2: fetch the newest-crates page ──────────────────────────────
		let url = format!(
			"{}/crates?sort=newest&per_page={}",
			self.api_base, PAGE_SIZE
		);
		let bytes = client.get(Language::Rust, &url).await?;
		let resp: NewestResponse = serde_json::from_slice(&bytes)
			.map_err(|e| UpstreamError::Parse(format!("crates.io newest: {e}")))?;

		// ── Step 3: collect events strictly newer than cursor ─────────────────
		let mut events = Vec::new();
		let mut max_ts = after.to_owned();

		for entry in &resp.crates {
			// Skip entries not newer than the cursor.
			if entry.updated_at.as_str() <= after {
				continue;
			}
			if entry.updated_at > max_ts {
				max_ts = entry.updated_at.clone();
			}
			let event = if entry.yanked {
				CatalogEvent::Withdrawn {
					name: entry.name.clone(),
					version: entry.version.clone(),
				}
			} else {
				CatalogEvent::Published {
					name: entry.name.clone(),
					version: entry.version.clone(),
				}
			};
			events.push(event);
		}

		// ── Step 4: determine exhaustion ──────────────────────────────────────
		// If every item in the feed was at or before the cursor (no new events),
		// we are caught up.
		let exhausted = events.is_empty();

		// If nothing changed, keep the old cursor; otherwise advance.
		let next = if max_ts.is_empty() || max_ts == after {
			cursor.clone()
		} else {
			CatalogCursor(serde_json::Value::String(max_ts))
		};

		Ok(CatalogBatch { events, next, exhausted })
	}
}

impl CatalogFollower for CratesCatalogFollower {
	fn language(&self) -> Language { Language::Rust }

	fn poll<'a>(
		&'a self,
		client: &'a UpstreamClient,
		cursor: &'a CatalogCursor,
	) -> PollFuture<'a> {
		Box::pin(self.poll_inner(client, cursor))
	}
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests (offline)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;
	use axum::{Router, routing::get};
	use tokio::net::TcpListener;

	async fn spawn_mock(router: Router) -> (tokio::task::JoinHandle<()>, String) {
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		let handle = tokio::spawn(async move { axum::serve(listener, router).await.ok(); });
		(handle, format!("http://{addr}"))
	}

	fn crates_response(entries: Vec<serde_json::Value>) -> serde_json::Value {
		serde_json::json!({ "crates": entries })
	}

	fn crate_entry(name: &str, version: &str, updated_at: &str, yanked: bool) -> serde_json::Value {
		serde_json::json!({
			"name": name,
			"newest_version": version,
			"updated_at": updated_at,
			"yanked": yanked
		})
	}

	/// Published event for a new crate.
	#[tokio::test]
	async fn published_event_for_new_crate() {
		let body = crates_response(vec![
			crate_entry("serde", "1.0.200", "2024-01-02T00:00:00Z", false),
		]);
		let (_h, base) = spawn_mock(Router::new()
			.route("/crates", get(move || {
				let b = body.clone();
				async move { axum::Json(b) }
			}))
		).await;

		let follower = CratesCatalogFollower::new(base);
		let client = UpstreamClient::new();
		let batch = follower.poll(&client, &CatalogCursor::zero()).await.unwrap();
		assert_eq!(batch.events.len(), 1);
		assert!(matches!(&batch.events[0], CatalogEvent::Published { name, version }
			if name == "serde" && version == "1.0.200"));
		assert!(!batch.exhausted);
	}

	/// Withdrawn event for a yanked crate.
	#[tokio::test]
	async fn withdrawn_event_for_yanked_crate() {
		let body = crates_response(vec![
			crate_entry("bad-crate", "0.1.0", "2024-01-03T00:00:00Z", true),
		]);
		let (_h, base) = spawn_mock(Router::new()
			.route("/crates", get(move || {
				let b = body.clone();
				async move { axum::Json(b) }
			}))
		).await;

		let follower = CratesCatalogFollower::new(base);
		let client = UpstreamClient::new();
		let batch = follower.poll(&client, &CatalogCursor::zero()).await.unwrap();
		assert_eq!(batch.events.len(), 1);
		assert!(matches!(&batch.events[0], CatalogEvent::Withdrawn { .. }));
	}

	/// Exhausted when all entries are at or before the cursor.
	#[tokio::test]
	async fn exhausted_when_all_entries_seen() {
		let ts = "2024-01-04T00:00:00Z";
		let body = crates_response(vec![
			crate_entry("serde", "1.0.200", ts, false),
		]);
		let (_h, base) = spawn_mock(Router::new()
			.route("/crates", get(move || {
				let b = body.clone();
				async move { axum::Json(b) }
			}))
		).await;

		let cursor = CatalogCursor(serde_json::Value::String(ts.to_owned()));
		let follower = CratesCatalogFollower::new(base);
		let client = UpstreamClient::new();
		let batch = follower.poll(&client, &cursor).await.unwrap();
		assert!(batch.exhausted);
		assert!(batch.events.is_empty());
	}

	/// Cursor advances to the latest seen timestamp.
	#[tokio::test]
	async fn cursor_advances_to_max_timestamp() {
		let body = crates_response(vec![
			crate_entry("alpha", "1.0.0", "2024-02-01T00:00:00Z", false),
			crate_entry("beta", "2.0.0", "2024-02-02T00:00:00Z", false),
		]);
		let (_h, base) = spawn_mock(Router::new()
			.route("/crates", get(move || {
				let b = body.clone();
				async move { axum::Json(b) }
			}))
		).await;

		let follower = CratesCatalogFollower::new(base);
		let client = UpstreamClient::new();
		let batch = follower.poll(&client, &CatalogCursor::zero()).await.unwrap();
		let next_ts = batch.next.0.as_str().unwrap();
		assert_eq!(next_ts, "2024-02-02T00:00:00Z");
	}
}
