//! NuGet V3 catalog follower — the **reference implementation** for
//! `CatalogFollower`.
//!
//! # NuGet V3 catalog protocol (summary)
//!
//! The NuGet V3 catalog is a set of append-only JSON pages that record every
//! package state change. The protocol is:
//!
//! 1. **Catalog index** (`/v3/catalog0/index.json`) — a list of catalog *pages*,
//!    each with a `commitTimeStamp` ISO 8601 string. Pages are ordered by commit
//!    time, and each page covers all changes up to its timestamp.
//!
//! 2. **Catalog page** (`/v3/catalog0/page{N}.json`) — a list of catalog *leaf*
//!    URLs plus their `commitTimeStamp`. Each leaf covers one package/version
//!    change.
//!
//! 3. **Catalog leaf** (individual URL) — one JSON document that describes the
//!    event: the `@type` field is the discriminant.
//!    - `nuget:PackageDetails` → Published (or a re-list if `listed == true`).
//!    - `nuget:PackageDelete` → Withdrawn (hard delete).
//!    - `PackageDetails` with `listed == false` → Withdrawn (soft unlist).
//!
//! # Cursor encoding
//!
//! The cursor is the last `commitTimeStamp` we fully processed, as an ISO 8601
//! string. We only process pages whose `commitTimeStamp > cursor`, so the cursor
//! advances monotonically.
//!
//! A null cursor (`CatalogCursor::zero()`) starts from the very first page.
//!
//! # Idempotency
//!
//! The caller (driver) only commits the cursor *after* all events in the batch
//! have been registered, so a crash mid-batch re-delivers the whole page. NuGet
//! leaves may also duplicate across pages in some edge cases; the registration
//! entry point is idempotent so this is always safe.
//!
//! # Rate limiting / HTTP
//!
//! All fetches go through `UpstreamClient`, which applies the per-language
//! token-bucket rate limiter and retry-with-backoff from the ecosystem policy.

use crate::ecosystem::Language;
use serde::Deserialize;

use super::catalog::{CatalogBatch, CatalogCursor, CatalogEvent};
use crate::upstream::{CatalogFollower, PollFuture, UpstreamClient, UpstreamError};

// ─────────────────────────────────────────────────────────────────────────────
// Wire types — minimal, serde-only; no heavy registry types needed here.
// ─────────────────────────────────────────────────────────────────────────────

/// Top-level `catalog0/index.json` shape.
#[derive(Deserialize)]
struct CatalogIndex {
    items: Vec<PageEntry>,
}

/// One entry in the catalog index.
#[derive(Deserialize)]
struct PageEntry {
    /// The catalog page URL.
    #[serde(rename = "@id")]
    url: String,
    /// RFC 3339 commit timestamp for this page. We compare this lexicographically
    /// (ISO 8601 sorts correctly as a string for our purposes).
    #[serde(rename = "commitTimeStamp")]
    commit_time_stamp: String,
}

/// One entry in a catalog page.
#[derive(Deserialize)]
struct LeafEntry {
    /// The catalog leaf URL.
    #[serde(rename = "@id")]
    url: String,
    /// RFC 3339 commit timestamp of this leaf.
    #[serde(rename = "commitTimeStamp")]
    commit_time_stamp: String,
}

/// Catalog page JSON (abbreviated — we only need `items`; the page-level
/// timestamp is read from the index entry, not the page body).
#[derive(Deserialize)]
struct CatalogPage {
    items: Vec<LeafEntry>,
}

/// Catalog leaf JSON.
#[derive(Deserialize)]
struct CatalogLeaf {
    /// Discriminant: `"nuget:PackageDetails"` or `"nuget:PackageDelete"`.
    #[serde(rename = "@type")]
    leaf_type: LeafType,
    /// The package id (name).
    #[serde(rename = "nuget:id")]
    package_id: Option<String>,
    /// The package version.
    #[serde(rename = "nuget:version")]
    package_version: Option<String>,
    /// `false` means the version is unlisted (soft Withdrawn).
    #[serde(default = "default_listed")]
    listed: bool,
}

fn default_listed() -> bool {
    true
}

#[derive(Deserialize, PartialEq, Eq)]
enum LeafType {
    #[serde(rename = "nuget:PackageDetails")]
    PackageDetails,
    #[serde(rename = "nuget:PackageDelete")]
    PackageDelete,
    #[serde(other)]
    Unknown,
}

// ─────────────────────────────────────────────────────────────────────────────
// Follower implementation
// ─────────────────────────────────────────────────────────────────────────────

/// The NuGet V3 catalog-based follower.
///
/// One batch = one catalog page. For the first call after a cursor we find all
/// pages strictly after the cursor timestamp, take the first unprocessed page,
/// fetch every leaf, and return the events together with the page's
/// `commitTimeStamp` as the next cursor.
pub struct NuGetCatalogFollower {
    /// Base URL of the NuGet V3 service index, e.g. `https://api.nuget.org/v3`.
    catalog_index_url: String,
}

impl NuGetCatalogFollower {
    /// Construct with the NuGet catalog index URL.
    /// Default: `https://api.nuget.org/v3/catalog0/index.json`.
    pub fn new(catalog_index_url: impl Into<String>) -> Self {
        Self {
            catalog_index_url: catalog_index_url.into(),
        }
    }

    /// Convenience constructor with the production NuGet catalog endpoint.
    pub fn production() -> Self {
        Self::new("https://api.nuget.org/v3/catalog0/index.json")
    }

    /// Inner async impl — drives one batch.
    async fn poll_inner(
        &self,
        client: &UpstreamClient,
        cursor: &CatalogCursor,
    ) -> Result<CatalogBatch, UpstreamError> {
        // ── Step 1: fetch the catalog index ───────────────────────────────────
        let index_bytes = client
            .get(Language::CSharp, &self.catalog_index_url)
            .await?;
        let index: CatalogIndex = serde_json::from_slice(&index_bytes)
            .map_err(|e| UpstreamError::Parse(format!("NuGet catalog index: {e}")))?;

        // ── Step 2: find pages after the cursor ───────────────────────────────
        // Pages are in ascending commitTimeStamp order; we want the first page
        // strictly after the cursor (so on the first call with zero cursor we take
        // the very first page, and on subsequent calls we continue from where we
        // left off).
        let after = match &cursor.0 {
            serde_json::Value::String(ts) => ts.as_str(),
            _ => "",
        };

        // Find the first page whose commitTimeStamp is strictly after `after`.
        // If after == "" (zero cursor) all pages qualify.
        let next_page = index
            .items
            .into_iter()
            .find(|p| p.commit_time_stamp.as_str() > after);

        let Some(page_entry) = next_page else {
            // Fully caught up — no pages after cursor.
            return Ok(CatalogBatch {
                events: vec![],
                next: cursor.clone(),
                exhausted: true,
            });
        };

        let page_ts = page_entry.commit_time_stamp.clone();

        // ── Step 3: fetch the page ────────────────────────────────────────────
        let page_bytes = client.get(Language::CSharp, &page_entry.url).await?;
        let page: CatalogPage = serde_json::from_slice(&page_bytes).map_err(|e| {
            UpstreamError::Parse(format!("NuGet catalog page {}: {e}", page_entry.url))
        })?;

        // ── Step 4: fetch each leaf and classify ──────────────────────────────
        // We only fetch leaves whose commitTimeStamp > cursor (the page may
        // partially overlap with previously-processed leaves if the page was
        // partially processed before a crash). This is the fine-grained idempotency
        // guard: we re-deliver the whole page but skip already-seen leaves.
        let mut events = Vec::new();
        for leaf_entry in &page.items {
            if leaf_entry.commit_time_stamp.as_str() <= after {
                // Already processed in a previous batch — skip (idempotency guard).
                continue;
            }
            let leaf_bytes = match client.get(Language::CSharp, &leaf_entry.url).await {
                Ok(b) => b,
                Err(UpstreamError::NotFound) => {
                    // Leaf deleted between page fetch and leaf fetch — ignore.
                    tracing::debug!(url = %leaf_entry.url, "NuGet catalog leaf 404; skipping");
                    continue;
                }
                Err(e) => return Err(e),
            };

            let leaf: CatalogLeaf = match serde_json::from_slice(&leaf_bytes) {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(url = %leaf_entry.url, error = %e, "NuGet catalog leaf parse error; skipping");
                    continue;
                }
            };

            // Need both id and version to produce an event.
            let (Some(name), Some(version)) = (leaf.package_id, leaf.package_version) else {
                tracing::debug!(url = %leaf_entry.url, "NuGet catalog leaf missing id/version; skipping");
                continue;
            };

            let event = match leaf.leaf_type {
                LeafType::PackageDelete => {
                    // Hard delete: always Withdrawn.
                    CatalogEvent::Withdrawn { name, version }
                }
                LeafType::PackageDetails if !leaf.listed => {
                    // Soft unlist: catalogEntry.listed == false → Withdrawn.
                    CatalogEvent::Withdrawn { name, version }
                }
                LeafType::PackageDetails => {
                    // Normal publish (listed == true).
                    CatalogEvent::Published { name, version }
                }
                LeafType::Unknown => {
                    tracing::debug!(url = %leaf_entry.url, "unknown NuGet leaf @type; skipping");
                    continue;
                }
            };
            events.push(event);
        }

        // ── Step 5: advance the cursor to this page's commitTimeStamp ─────────
        // The driver will persist this only after all events above have been
        // registered; a crash between registration and persist simply re-delivers
        // the same page (idempotent by registration's upsert semantics).
        Ok(CatalogBatch {
            events,
            next: CatalogCursor(serde_json::Value::String(page_ts)),
            exhausted: false,
        })
    }
}

impl CatalogFollower for NuGetCatalogFollower {
    fn language(&self) -> Language {
        Language::CSharp
    }

    fn poll<'a>(&'a self, client: &'a UpstreamClient, cursor: &'a CatalogCursor) -> PollFuture<'a> {
        Box::pin(self.poll_inner(client, cursor))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests (offline — wiremock-style via axum on 127.0.0.1:0)
// ─────────────────────────────────────────────────────────────────────────────
