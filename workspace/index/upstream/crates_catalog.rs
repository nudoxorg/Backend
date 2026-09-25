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
//!    periodically re-check `/api/v1/crates/{name}/{version}` to see if `yanked
//!    == true`.
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

use super::{
    catalog::{CatalogBatch, CatalogCursor, CatalogEvent},
    path_segment,
};
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
            let (dependencies, checksum) = if entry.yanked {
                (Vec::new(), None)
            } else {
                (
                    self.dependency_names(client, &entry.name, &entry.version)
                        .await,
                    self.version_checksum(client, &entry.name, &entry.version)
                        .await,
                )
            };
            let event = if entry.yanked {
                CatalogEvent::Withdrawn {
                    name: entry.name.clone(),
                    version: entry.version.clone(),
                }
            } else {
                CatalogEvent::Published {
                    name: entry.name.clone(),
                    version: entry.version.clone(),
                    dependencies,
                    checksum,
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

impl CratesCatalogFollower {
    /// Normal dependencies of one published version.
    ///
    /// A failed fetch or a body that is not the dependencies document yields
    /// an empty list. The publish still proceeds. Dev and build dependencies
    /// are omitted, matching [`crate::ecosystem::rust::parse_cargo_toml`],
    /// which reads only `[dependencies]`.
    async fn dependency_names(
        &self,
        client: &UpstreamClient,
        name: &str,
        version: &str,
    ) -> Vec<String> {
        let url = format!(
            "{}/crates/{}/{}/dependencies",
            self.api_base,
            path_segment(name),
            path_segment(version)
        );
        match client.get(Language::Rust, &url).await {
            Ok(bytes) => normal_dependency_names(&bytes),
            Err(_) => Vec::new(),
        }
    }

    async fn version_checksum(
        &self,
        client: &UpstreamClient,
        name: &str,
        version: &str,
    ) -> Option<String> {
        let url = format!(
            "{}/crates/{}/{}",
            self.api_base,
            path_segment(name),
            path_segment(version)
        );
        let bytes = client.get(Language::Rust, &url).await.ok()?;
        checksum_from_version_document(&bytes)
    }
}

/// `version.checksum` from a crates.io version document. A short or missing
/// field is `None`.
pub fn checksum_from_version_document(body: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct Body {
        version: Version,
    }
    #[derive(Deserialize)]
    struct Version {
        checksum: Option<String>,
    }
    let parsed = serde_json::from_slice::<Body>(body).ok()?;
    let checksum = parsed.version.checksum?;
    crate::pid::sha256(&checksum).map(|_| checksum)
}

/// Crate names from a dependencies document. `normal` or absent kind is kept.
pub fn normal_dependency_names(body: &[u8]) -> Vec<String> {
    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        dependencies: Vec<Dep>,
    }
    #[derive(Deserialize)]
    struct Dep {
        #[serde(default)]
        crate_id: String,
        #[serde(default)]
        kind: String,
    }
    let Ok(parsed) = serde_json::from_slice::<Body>(body) else {
        return Vec::new();
    };
    let mut names: Vec<String> = parsed
        .dependencies
        .into_iter()
        .filter(|dep| dep.kind.is_empty() || dep.kind == "normal")
        .map(|dep| dep.crate_id)
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect();
    names.sort();
    names.dedup();
    names
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests (offline)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{checksum_from_version_document, normal_dependency_names};

    #[test]
    fn a_version_document_yields_a_64_hex_checksum_only() {
        let full = "ab".repeat(32);
        let body = format!(r#"{{"version":{{"checksum":"{full}"}}}}"#);
        assert_eq!(checksum_from_version_document(body.as_bytes()), Some(full));
        let short = br#"{"version":{"checksum":"abcd"}}"#;
        assert_eq!(checksum_from_version_document(short), None);
        assert_eq!(checksum_from_version_document(br#"{"version":{}}"#), None);
    }

    #[test]
    fn normal_dependencies_are_kept_and_dev_dependencies_are_dropped() {
        let body = br#"{
            "dependencies": [
                {"crate_id": "serde", "req": "^1", "kind": "normal", "optional": false},
                {"crate_id": "serde", "req": "^1", "kind": "normal", "optional": true},
                {"crate_id": "tokio", "req": "1", "kind": "dev"},
                {"crate_id": "cc", "req": "1", "kind": "build"},
                {"crate_id": " libc ", "req": "0.2"},
                {"crate_id": " ", "kind": "normal"}
            ]
        }"#;
        assert_eq!(normal_dependency_names(body), vec![
            "libc".to_owned(),
            "serde".to_owned()
        ]);
        assert!(normal_dependency_names(b"not-json").is_empty());
        assert!(normal_dependency_names(b"{}").is_empty());
    }
}
