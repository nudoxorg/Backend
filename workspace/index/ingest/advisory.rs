//! Advisory ingestion **shape** (REGISTRYLESS-PLAN §12 Phase S-A prep): a typed
//! wire advisory → [`CatalogOp::UpsertAdvisory`] mapping plus a
//! `listing_events` emission helper. **No live OSV fetch this wave** — the OSV
//! GCS export follower and the git-range → affected-version translation are S-A
//! proper. This module is the vocabulary and the pure mapping those will use.
//!
//! The mapping is deliberately mechanical: an [`AdvisoryWire`] as OSV would give
//! it (id, affected slug, range, severity) becomes one `UpsertAdvisory` op, and
//! for each affected version the caller has already resolved, one
//! `SetListing { status: Advisory }` op (the bitemporal `listing_events` row the
//! Trustfall security policy plane reads — no new query surface, per §12).

use heart::identity::{Id, namespace};
use crate::enums::ListingStatus;
use crate::ids::{AdvisoryId, PackageId, PackageStemId};
use crate::protocol::{AdvisoryWire, CatalogOp};

/// A typed, transport-neutral advisory as an upstream feed (OSV) presents it,
/// before it is mapped onto the catalog. Deliberately a superset of the
/// catalog's `AdvisoryWire` so the OSV follower can populate it directly from a
/// GCS export entry, then this module derives the catalog op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisorySource {
    /// The upstream advisory identifier (e.g. an OSV id like `CVE-…` /
    /// `GHSA-…`). Used to derive a stable [`AdvisoryId`].
    pub upstream_id: String,
    /// The affected stem, once the follower has resolved the advisory's repo
    /// slug to a catalog stem (`None` until resolution).
    pub stem_id: Option<PackageStemId>,
    /// The affected version range expression, verbatim from the feed.
    pub version_range: Option<String>,
    /// The severity token (e.g. `"HIGH"`), verbatim.
    pub severity: Option<String>,
    /// A human summary.
    pub summary: Option<String>,
    /// The advisory's canonical URL.
    pub url: Option<String>,
    /// When this advisory became valid (unix milliseconds) — bitemporal window
    /// start.
    pub valid_from: i64,
    /// When it stopped being valid, if it was withdrawn (bitemporal end).
    pub valid_to: Option<i64>,
    /// When the follower recorded it.
    pub recorded_at: i64,
}

impl AdvisorySource {
    /// The deterministic [`AdvisoryId`] for this advisory — a v5 hash of its
    /// upstream id, so re-ingesting the same OSV entry upserts the same row.
    pub fn advisory_id(&self) -> AdvisoryId {
        let id: Id<heart::identity::Package> =
            Id::from_name(&namespace::PACKAGE, self.upstream_id.as_bytes());
        AdvisoryId::from_uuid(*id.as_uuid())
    }

    /// Map to the catalog's [`AdvisoryWire`].
    pub fn to_wire(&self) -> AdvisoryWire {
        AdvisoryWire {
            id: self.advisory_id(),
            stem_id: self.stem_id,
            version_range: self.version_range.clone(),
            severity: self.severity.clone(),
            summary: self.summary.clone(),
            url: self.url.clone(),
            valid_from: self.valid_from,
            valid_to: self.valid_to,
            recorded_at: self.recorded_at,
        }
    }

    /// The [`CatalogOp::UpsertAdvisory`] op for this advisory.
    pub fn to_upsert_op(&self) -> CatalogOp {
        CatalogOp::UpsertAdvisory { advisory: self.to_wire() }
    }
}

/// Emit an `advisory` listing event for one affected version (REGISTRYLESS-PLAN
/// §12): a bitemporal `listing_events` row the Trustfall security policy plane
/// reads. The follower calls this once per version it has determined the
/// advisory's git range covers (`introduced ≤ rev < fixed`); that ancestry
/// translation is S-A proper and out of scope here.
pub fn advisory_listing_event(
    version: PackageId,
    valid_from: i64,
    upstream_id: &str,
) -> CatalogOp {
    CatalogOp::SetListing {
        version,
        status: ListingStatus::Advisory,
        valid_from,
        reason: Some(upstream_id.to_owned()),
    }
}
