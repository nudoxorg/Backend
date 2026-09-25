//! The `CatalogFollower` trait — the demand-driven abstraction over an upstream
//! registry's incremental event feed.
//!
//! Each follower represents one ecosystem's catalog: it knows how to talk to
//! its registry's native feed format (NuGet V3 catalog, crates.io new-crates
//! RSS, etc.) and produce a stream of `CatalogEvent`s that the driver
//! (`server/catalog_follower.rs::catalog_follower_worker`) converts into
//! idempotent package-registration calls.
//!
//! # Design constraints
//!
//! - **No new `async_trait` dep** — the driver holds `Box<dyn CatalogFollower>`
//!   objects, so the `poll` method must be object-safe. Rust 2024 edition does
//!   not yet make `async fn` in trait object-safe, so we use a hand-rolled
//!   `Pin<Box<dyn Future ...>>` return type throughout. No external crate is
//!   needed.
//! - **Opaque cursor** — `CatalogCursor` is a newtype over `serde_json::Value`
//!   so every follower can use its own native position type without a shared
//!   discriminant. The driver only serialises/deserialises it; it never
//!   inspects the contents.
//! - **Exhausted flag** — when the registry has no more pages after the current
//!   cursor, the follower sets `CatalogBatch::exhausted = true`. The driver
//!   sleeps `poll_interval` before calling again rather than tight-looping.

use std::{future::Future, pin::Pin};

use crate::ecosystem::Language;

use crate::upstream::UpstreamError;

/// An opaque cursor into a registry's incremental event feed.
///
/// Internally each follower stores whatever position type it needs (a NuGet
/// `commitTimeStamp` string, a crates.io Unix timestamp, ...) serialised to
/// JSON. The driver persists it verbatim as `catalog-cursor-{lang}.json` next
/// to the tantivy watermark files and hands it back on the next `poll` call.
/// A corrupt or missing cursor file restarts from zero (the follower receives
/// `CatalogCursor::zero()`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CatalogCursor(pub serde_json::Value);

impl CatalogCursor {
    /// The initial cursor — restart from the beginning of the feed.
    pub fn zero() -> Self {
        Self(serde_json::Value::Null)
    }

    /// Whether this is the zero (restart) cursor.
    pub fn is_zero(&self) -> bool {
        self.0.is_null()
    }
}

/// One registry event from a catalog page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogEvent {
    /// A new version was published.
    Published {
        /// Package name, exactly as the registry records it.
        name: String,
        /// Version string, exactly as the registry records it.
        version: String,
        /// Direct dependencies the feed already carried. Empty when the
        /// feed has no manifest body.
        dependencies: Vec<crate::record::DepEdge>,
        /// Registry checksum hex, when the feed or version document carried
        /// one.
        checksum: Option<String>,
    },
    /// A version was withdrawn (yanked/unlisted/deprecated).
    Withdrawn {
        /// Package name.
        name: String,
        /// Version string.
        version: String,
    },
}

impl CatalogEvent {
    /// The package name for this event.
    pub fn name(&self) -> &str {
        match self {
            CatalogEvent::Published { name, .. } | CatalogEvent::Withdrawn { name, .. } => name,
        }
    }

    /// The version string for this event.
    pub fn version(&self) -> &str {
        match self {
            CatalogEvent::Published { version, .. } | CatalogEvent::Withdrawn { version, .. } => {
                version
            }
        }
    }

    /// Direct dependencies carried by a publish event.
    pub fn dependencies(&self) -> &[crate::record::DepEdge] {
        match self {
            CatalogEvent::Published { dependencies, .. } => dependencies,
            CatalogEvent::Withdrawn { .. } => &[],
        }
    }

    /// Registry checksum hex, when this publish carried one.
    pub fn checksum(&self) -> Option<&str> {
        match self {
            CatalogEvent::Published { checksum, .. } => checksum.as_deref(),
            CatalogEvent::Withdrawn { .. } => None,
        }
    }
}

/// A batch of events from one `poll` call.
#[derive(Debug)]
pub struct CatalogBatch {
    /// The events observed in this batch, in feed order.
    pub events: Vec<CatalogEvent>,
    /// The cursor to pass on the next `poll` call. The driver commits this
    /// cursor to disk **only after all events in the batch have been
    /// registered**, so a crash mid-batch re-delivers the whole batch on
    /// restart. The registration call is idempotent so re-delivery is safe.
    pub next: CatalogCursor,
    /// `true` when the feed is fully caught up and there is nothing more to
    /// read right now. The driver sleeps `poll_interval` before the next call.
    pub exhausted: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// The trait
// ─────────────────────────────────────────────────────────────────────────────

/// Names and an optional artifact checksum from one already-fetched document.
pub struct DocumentFacts {
    /// Direct dependencies.
    pub dependencies: Vec<crate::record::DepEdge>,
    /// Registry checksum spelling, when the document carried one.
    pub checksum: Option<String>,
}

impl DocumentFacts {
    /// A document that names dependencies and no checksum.
    pub fn names(dependencies: Vec<String>) -> Self {
        Self {
            dependencies: crate::record::runtime_edges_from_names(&dependencies),
            checksum: None,
        }
    }
}

/// Copy a document onto a publish. Dependencies replace the list. A checksum
/// lands only when the event has none and the spelling is an artifact digest.
pub fn merge_document_facts(event: &mut CatalogEvent, facts: DocumentFacts) {
    let CatalogEvent::Published {
        dependencies,
        checksum,
        ..
    } = event
    else {
        return;
    };
    *dependencies = facts.dependencies;
    if checksum.is_none() {
        *checksum = facts
            .checksum
            .filter(|text| crate::pid::artifact_digest(text).is_some());
    }
}

/// Fill a publish event from a second document.
///
/// `url_for` returns `None` when the coordinate cannot name a document; that
/// event keeps what it already has. A failed fetch does the same. A
/// successful body replaces the dependency list and, when the document names
/// a checksum, the event checksum. Withdrawn events are skipped.
pub async fn attach_document_dependencies(
    client: &crate::upstream::UpstreamClient,
    language: Language,
    events: &mut [CatalogEvent],
    url_for: impl Fn(&str, &str) -> Option<String>,
    facts_from: impl Fn(&[u8]) -> DocumentFacts,
) {
    for event in events.iter_mut() {
        if !matches!(event, CatalogEvent::Published { .. }) {
            continue;
        }
        let Some(url) = url_for(event.name(), event.version()) else {
            continue;
        };
        if let Ok(body) = client.get(language, &url).await {
            merge_document_facts(event, facts_from(&body));
        }
    }
}

/// A boxed, object-safe future that drives one `poll` call.
pub type PollFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CatalogBatch, UpstreamError>> + Send + 'a>>;

/// An incremental feed follower for one ecosystem's upstream registry.
///
/// The driver (`catalog_follower_worker`) holds a
/// `Vec<Box<dyn CatalogFollower + Send + Sync>>` and polls each one in its own
/// tokio task. Only one task per follower exists at a time; the advisory-lock
/// pattern that guards outbox consumers is *not* needed here because cursors
/// are local files, not shared postgres rows.
pub trait CatalogFollower: Send + Sync + 'static {
    /// The ecosystem this follower covers. Used to route events to the correct
    /// idempotent registration entry point.
    fn language(&self) -> Language;

    /// Fetch the next batch of events after `cursor` from the upstream
    /// registry.
    ///
    /// The returned [`CatalogBatch`] carries a `next` cursor the driver should
    /// pass on the subsequent call. The driver **does not commit the cursor
    /// until all events in the batch have been registered**, so a `poll`
    /// that returns `Ok` but is followed by a crash still re-delivers the
    /// batch.
    ///
    /// When the feed is fully caught up, return `CatalogBatch::exhausted =
    /// true` and the driver will sleep before calling again.
    fn poll<'a>(
        &'a self,
        client: &'a crate::upstream::UpstreamClient,
        cursor: &'a CatalogCursor,
    ) -> PollFuture<'a>;
}
