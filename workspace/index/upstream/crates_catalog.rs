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

use crate::ecosystem::Language;
use serde::Deserialize;

use super::catalog::{CatalogBatch, CatalogCursor, CatalogEvent};
use crate::upstream::{CatalogFollower, PollFuture, UpstreamClient, UpstreamError};

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
        Self {
            api_base: api_base.into(),
        }
    }

    /// Convenience constructor using the production crates.io endpoint.
    pub fn production() -> Self {
        Self::new(DEFAULT_API_BASE)
    }

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
                max_ts.clone_from(&entry.updated_at);
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

        Ok(CatalogBatch {
            events,
            next,
            exhausted,
        })
    }
}

impl CatalogFollower for CratesCatalogFollower {
    fn language(&self) -> Language {
        Language::Rust
    }

    fn poll<'a>(&'a self, client: &'a UpstreamClient, cursor: &'a CatalogCursor) -> PollFuture<'a> {
        Box::pin(self.poll_inner(client, cursor))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests (offline)
// ─────────────────────────────────────────────────────────────────────────────
