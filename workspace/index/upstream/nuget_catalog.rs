//! NuGet V3 catalog follower — the **reference implementation** for
//! `CatalogFollower`.
//!
//! # NuGet V3 catalog protocol (summary)
//!
//! The NuGet V3 catalog is a set of append-only JSON pages that record every
//! package state change. The protocol is:
//!
//! 1. **Catalog index** (`/v3/catalog0/index.json`) — a list of catalog
//!    *pages*, each with a `commitTimeStamp` ISO 8601 string. Pages are ordered
//!    by commit time, and each page covers all changes up to its timestamp.
//!
//! 2. **Catalog page** (`/v3/catalog0/page{N}.json`) — a list of catalog *leaf*
//!    URLs plus their `commitTimeStamp`. Each leaf covers one package/version
//!    change.
//!
//! 3. **Catalog leaf** (individual URL) — one JSON document that describes the
//!    event. `@type` is the discriminant and is either a string or an array of
//!    strings. Page items use `nuget:PackageDetails` / `nuget:PackageDelete`
//!    with `nuget:id` / `nuget:version`. Leaf bodies use `PackageDetails` /
//!    `PackageDelete` (often beside `catalog:Permalink`) with `id` / `version`.
//!    Both shapes parse; unrecognized type tokens are ignored, and a leaf whose
//!    types are all unrecognized is skipped.
//!    - `PackageDetails` / `nuget:PackageDetails` with `listed != false` →
//!      Published (including a re-list).
//!    - `PackageDelete` / `nuget:PackageDelete` → Withdrawn (hard delete).
//!    - `PackageDetails` with `listed == false` → Withdrawn (soft unlist).
//!
//! # Cursor encoding
//!
//! The cursor is the last `commitTimeStamp` we fully processed, as an ISO 8601
//! string. We only process pages whose `commitTimeStamp > cursor`, so the
//! cursor advances monotonically.
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
    /// RFC 3339 commit timestamp for this page. We compare this
    /// lexicographically (ISO 8601 sorts correctly as a string for our
    /// purposes).
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
///
/// Accepts the page-item names (`nuget:id`, `nuget:version`, `@type` =
/// `nuget:PackageDetails` / `nuget:PackageDelete`) and the leaf-body names
/// (`id`, `version`, `@type` = `PackageDetails` / `PackageDelete`). `@type`
/// may be a string or an array of strings. The first recognized token wins;
/// unknown tokens such as `catalog:Permalink` are ignored.
#[derive(Deserialize)]
struct CatalogLeaf {
    /// Discriminant: page-item or leaf-body `@type`, as a string or an array.
    #[serde(rename = "@type", deserialize_with = "deserialize_leaf_type")]
    leaf_type: LeafType,
    /// The package id (name): `nuget:id` on page items, `id` on leaf bodies.
    #[serde(rename = "nuget:id", alias = "id")]
    package_id: Option<String>,
    /// The package version: `nuget:version` on page items, `version` on leaf
    /// bodies.
    #[serde(rename = "nuget:version", alias = "version")]
    package_version: Option<String>,
    /// `false` means the version is unlisted (soft Withdrawn).
    #[serde(default = "default_listed")]
    listed: bool,
    /// Framework groups. Absent on page items and on delete leaves.
    #[serde(default, rename = "dependencyGroups")]
    dependency_groups: Vec<DependencyGroup>,
    /// Base64 hash of the nupkg. Algorithm is [`Self::package_hash_algorithm`].
    #[serde(default, rename = "packageHash")]
    package_hash: Option<String>,
    /// Typically `SHA512`.
    #[serde(default, rename = "packageHashAlgorithm")]
    package_hash_algorithm: Option<String>,
}

#[derive(Deserialize, Default)]
struct DependencyGroup {
    #[serde(default)]
    dependencies: Vec<DependencyRef>,
}

#[derive(Deserialize)]
struct DependencyRef {
    #[serde(default)]
    id: Option<String>,
}

fn default_listed() -> bool {
    true
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LeafType {
    PackageDetails,
    PackageDelete,
    Unknown,
}

/// `@type` as it appears on the wire: one string, or an array of strings.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawLeafType {
    One(String),
    Many(Vec<String>),
}

fn deserialize_leaf_type<'de, D>(deserializer: D) -> Result<LeafType, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(leaf_type_from_raw(RawLeafType::deserialize(deserializer)?))
}

fn leaf_type_from_raw(raw: RawLeafType) -> LeafType {
    match raw {
        RawLeafType::One(token) => recognized_leaf_type(&token).unwrap_or(LeafType::Unknown),
        RawLeafType::Many(tokens) => tokens
            .iter()
            .find_map(|token| recognized_leaf_type(token))
            .unwrap_or(LeafType::Unknown),
    }
}

/// Page-item types are prefixed `nuget:`; leaf bodies use the short name.
fn recognized_leaf_type(token: &str) -> Option<LeafType> {
    match token {
        "PackageDetails" | "nuget:PackageDetails" => Some(LeafType::PackageDetails),
        "PackageDelete" | "nuget:PackageDelete" => Some(LeafType::PackageDelete),
        _ => None,
    }
}

/// Classify a parsed catalog leaf.
///
/// `PackageDelete` is a hard delete (always [`CatalogEvent::Withdrawn`]).
/// `PackageDetails` is [`CatalogEvent::Published`] unless `listed` is false
/// (soft unlist). Missing id/version or an unrecognized `@type` yields `None`;
/// the follower skips those leaves.
/// Dependency ids across every framework group.
///
/// NuGet ids are case-insensitive, so each id is lowercased before the sort.
/// An id that appears in two groups is kept once. A blank id is dropped.
fn dependency_names(groups: &[DependencyGroup]) -> Vec<String> {
    let mut names: Vec<String> = groups
        .iter()
        .flat_map(|group| group.dependencies.iter())
        .filter_map(|dependency| dependency.id.as_deref())
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(|id| id.to_ascii_lowercase())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// `SHA512` + base64 becomes an SRI token the artifact parser accepts.
/// A missing algorithm or a SHA-1 nupkg hash is not a content PID.
fn leaf_checksum(algorithm: Option<&str>, hash: Option<&str>) -> Option<String> {
    let token = format!(
        "{}-{}",
        algorithm?.trim().to_ascii_lowercase(),
        hash?.trim()
    );
    crate::pid::artifact_digest(&token).map(|_| token)
}

/// One catalog leaf body. `Ok(None)` is an unrecognized type.
pub fn parse_leaf(body: &[u8]) -> Result<Option<CatalogEvent>, crate::upstream::UpstreamError> {
    let leaf: CatalogLeaf = serde_json::from_slice(body)
        .map_err(|err| crate::upstream::UpstreamError::Parse(format!("nuget leaf: {err}")))?;
    Ok(event_from_leaf(leaf))
}

fn event_from_leaf(leaf: CatalogLeaf) -> Option<CatalogEvent> {
    let dependencies = dependency_names(&leaf.dependency_groups);
    let (Some(name), Some(version)) = (leaf.package_id, leaf.package_version) else {
        return None;
    };
    match leaf.leaf_type {
        LeafType::PackageDelete => Some(CatalogEvent::Withdrawn { name, version }),
        LeafType::PackageDetails if !leaf.listed => Some(CatalogEvent::Withdrawn { name, version }),
        LeafType::PackageDetails => Some(CatalogEvent::Published {
            name,
            version,
            dependencies,
            checksum: leaf_checksum(
                leaf.package_hash_algorithm.as_deref(),
                leaf.package_hash.as_deref(),
            ),
        }),
        LeafType::Unknown => None,
    }
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

            // Need both id and version to produce an event. Unknown `@type`
            // values are skipped. Classification itself lives in `event_from_leaf`.
            let missing_identity = leaf.package_id.is_none() || leaf.package_version.is_none();
            if let Some(event) = event_from_leaf(leaf) {
                events.push(event);
            } else if missing_identity {
                tracing::debug!(url = %leaf_entry.url, "NuGet catalog leaf missing id/version; skipping");
            } else {
                tracing::debug!(url = %leaf_entry.url, "unknown NuGet leaf @type; skipping");
            }
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
// Tests (offline — leaf JSON only, no HTTP)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_event(json: &str) -> Option<CatalogEvent> {
        parse_leaf(json.as_bytes()).expect("catalog leaf JSON")
    }

    fn assert_published(event: Option<CatalogEvent>, name: &str, version: &str) {
        match event {
            Some(CatalogEvent::Published {
                name: got_name,
                version: got_version,
                ..
            }) => {
                assert_eq!(got_name, name);
                assert_eq!(got_version, version);
            }
            other => panic!("expected Published {name} {version}, got {other:?}"),
        }
    }

    fn assert_withdrawn(event: Option<CatalogEvent>, name: &str, version: &str) {
        match event {
            Some(CatalogEvent::Withdrawn {
                name: got_name,
                version: got_version,
            }) => {
                assert_eq!(got_name, name);
                assert_eq!(got_version, version);
            }
            other => panic!("expected Withdrawn {name} {version}, got {other:?}"),
        }
    }

    #[test]
    fn leaf_body_package_details_publishes_and_package_delete_withdraws() {
        // Realistic NuGet leaf body: short names, `@type` as an array that also
        // carries the permalink type. `listed: true` is a publish.
        let published = parse_event(
            r#"{
                "@id": "https://api.nuget.org/v3/catalog0/data/2015.02.01.06.22.45/newtonsoft.json.13.0.3.json",
                "@type": ["PackageDetails", "catalog:Permalink"],
                "catalog:commitId": "1d6c0d3e-3f3f-4c3e-8f3e-1d6c0d3e3f3f",
                "catalog:commitTimeStamp": "2015-02-01T06:22:45.8488496Z",
                "id": "Newtonsoft.Json",
                "listed": true,
                "published": "2015-02-01T06:22:45.8488496Z",
                "version": "13.0.3"
            }"#,
        );
        assert_published(published, "Newtonsoft.Json", "13.0.3");

        // Hard delete leaf. No `listed` field; the type alone withdraws.
        let withdrawn = parse_event(
            r#"{
                "@id": "https://api.nuget.org/v3/catalog0/data/2015.02.01.06.22.45/newtonsoft.json.6.0.8.json",
                "@type": ["PackageDelete", "catalog:Permalink"],
                "catalog:commitId": "1d6c0d3e-3f3f-4c3e-8f3e-1d6c0d3e3f3f",
                "catalog:commitTimeStamp": "2015-02-01T06:30:11.1234567Z",
                "id": "Newtonsoft.Json",
                "originalId": "Newtonsoft.Json",
                "published": "2015-02-01T06:30:11.1234567Z",
                "version": "6.0.8"
            }"#,
        );
        assert_withdrawn(withdrawn, "Newtonsoft.Json", "6.0.8");

        // Page-item names and a string `@type` still classify.
        let page_item = parse_event(
            r#"{
                "@id": "https://api.nuget.org/v3/catalog0/data/2015.02.01.06.22.45/adam.jsgenerator.1.1.0.json",
                "@type": "nuget:PackageDetails",
                "commitTimeStamp": "2015-02-01T06:22:45.8488496Z",
                "nuget:id": "Adam.JSGenerator",
                "nuget:version": "1.1.0"
            }"#,
        );
        assert_published(page_item, "Adam.JSGenerator", "1.1.0");

        let with_deps = parse_event(
            r#"{
                "@type": ["PackageDetails", "catalog:Permalink"],
                "id": "Newtonsoft.Json",
                "version": "13.0.3",
                "dependencyGroups": [
                    {"targetFramework": ".NETStandard2.0", "dependencies": [
                        {"id": "Microsoft.CSharp", "range": "[4.3.0, )"},
                        {"id": "  ", "range": "1.0.0"}
                    ]},
                    {"dependencies": [
                        {"id": "microsoft.csharp", "range": "[4.3.0, )"},
                        {"id": "System.Runtime", "range": "[4.3.0, )"}
                    ]}
                ]
            }"#,
        );
        match with_deps {
            Some(CatalogEvent::Published { dependencies, .. }) => {
                assert_eq!(dependencies, vec![
                    "microsoft.csharp".to_owned(),
                    "system.runtime".to_owned()
                ]);
            }
            other => panic!("expected dependency names, got {other:?}"),
        }

        let page_delete = parse_event(
            r#"{
                "@type": "nuget:PackageDelete",
                "nuget:id": "Adam.JSGenerator",
                "nuget:version": "1.0.0"
            }"#,
        );
        assert_withdrawn(page_delete, "Adam.JSGenerator", "1.0.0");

        // Short names as a single string (not only as an array).
        let string_details = parse_event(
            r#"{
                "@type": "PackageDetails",
                "id": "Serilog",
                "version": "3.1.1",
                "listed": true
            }"#,
        );
        assert_published(string_details, "Serilog", "3.1.1");

        // Soft unlist on a leaf body.
        let unlisted = parse_event(
            r#"{
                "@type": ["PackageDetails", "catalog:Permalink"],
                "id": "Newtonsoft.Json",
                "version": "13.0.1",
                "listed": false
            }"#,
        );
        assert_withdrawn(unlisted, "Newtonsoft.Json", "13.0.1");

        // Unknown types still skip, including an array of only unknown tokens
        // and a permalink that precedes a recognized type (the recognized one wins).
        assert!(
            parse_event(
                r#"{
                    "@type": "catalog:Permalink",
                    "id": "Newtonsoft.Json",
                    "version": "13.0.3"
                }"#,
            )
            .is_none()
        );
        assert!(
            parse_event(
                r#"{
                    "@type": ["catalog:Permalink"],
                    "id": "Newtonsoft.Json",
                    "version": "13.0.3",
                    "listed": true
                }"#,
            )
            .is_none()
        );
        let permalink_first = parse_event(
            r#"{
                "@type": ["catalog:Permalink", "PackageDetails"],
                "id": "Newtonsoft.Json",
                "version": "13.0.3",
                "listed": true
            }"#,
        );
        assert_published(permalink_first, "Newtonsoft.Json", "13.0.3");

        let hashed = parse_event(
            r#"{
                "@type": "PackageDetails",
                "id": "Newtonsoft.Json",
                "version": "13.0.3",
                "packageHash": "gNTwQWwUA3adfelbX8plB64gYNCnXT1uKLszJM5i0fFEMxJrITma0J6ON/bDJui4te8/UIFNgWNg5T1Nk4JyQQ==",
                "packageHashAlgorithm": "SHA512"
            }"#,
        );
        match hashed {
            Some(CatalogEvent::Published {
                checksum: Some(token),
                ..
            }) => {
                assert!(token.starts_with("sha512-"));
                assert!(
                    crate::pid::artifact_digest(&token).is_some_and(|digest| matches!(
                        digest,
                        crate::pid::ContentDigest::Sha512(_)
                    ))
                );
            }
            other => panic!("expected a sha512 checksum, got {other:?}"),
        }
        let sha1 = parse_event(
            r#"{
                "@type": "PackageDetails",
                "id": "Newtonsoft.Json",
                "version": "6.0.1",
                "packageHash": "QsfKwaItloXI7UjFbiE7DiF26VI=",
                "packageHashAlgorithm": "SHA1"
            }"#,
        );
        assert!(matches!(
            sha1,
            Some(CatalogEvent::Published { checksum: None, .. })
        ));
    }
}
