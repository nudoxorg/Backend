//! Advisory ingestion (REGISTRYLESS-PLAN §12): an OSV JSON document becomes one
//! [`AdvisorySource`] per affected package, then a
//! [`CatalogOp::UpsertAdvisory`]. [`parse_osv`] is the mapping. It does not
//! fetch the feed. [`resolve_osv`] fills `stem_id` when the OSV ecosystem token
//! maps onto a catalog language and the package name parses. Git-range ancestry
//! translation stays out of this module.
//!
//! The mapping is deliberately mechanical: an [`AdvisoryWire`] as OSV would
//! give it (id, affected slug, range, severity) becomes one `UpsertAdvisory`
//! op, and for each affected version the caller has already resolved, one
//! `SetListing { status: Advisory }` op (the bitemporal `listing_events` row
//! the Trustfall security policy plane reads — no new query surface, per §12).

use crate::{
    enums::ListingStatus,
    ids::{AdvisoryId, PackageId, PackageStemId},
    protocol::{AdvisoryWire, CatalogOp},
};
use heart::identity::{Id, namespace};

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
    /// Affected package name as the feed spelled it, before stem resolution.
    pub affected_name: Option<String>,
    /// Affected ecosystem token as the feed spelled it (`crates.io`, `npm`).
    pub affected_ecosystem: Option<String>,
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
            upstream_id: self.upstream_id.clone(),
        }
    }

    /// The [`CatalogOp::UpsertAdvisory`] op for this advisory.
    pub fn to_upsert_op(&self) -> CatalogOp {
        CatalogOp::UpsertAdvisory {
            advisory: self.to_wire(),
        }
    }

    /// Upsert plus one advisory listing per explicitly named version.
    ///
    /// A range written as `introduced` / `fixed` / `last_affected` events is
    /// not a version list, so it contributes the upsert only. A withdrawn
    /// advisory (`valid_to` set) and an unresolved stem do the same. Repeated
    /// version tokens collapse.
    pub fn catalog_ops(&self) -> Vec<CatalogOp> {
        let mut ops = vec![self.to_upsert_op()];
        ops.extend(self.listing_ops());
        ops
    }

    fn listing_ops(&self) -> Vec<CatalogOp> {
        if self.valid_to.is_some() {
            return Vec::new();
        }
        let Some(stem) = self.stem_id else {
            return Vec::new();
        };
        let Some(range) = self.version_range.as_deref() else {
            return Vec::new();
        };
        if range::is_event_range(range) {
            return Vec::new();
        }
        let mut seen = std::collections::HashSet::new();
        let mut ops = Vec::new();
        for version in range.split(',') {
            let version = version.trim();
            if version.is_empty() || !seen.insert(version.to_owned()) {
                continue;
            }
            let id = heart::identity::derive::package_id_from_parts([
                stem.to_blob().as_slice(),
                version.as_bytes(),
            ]);
            ops.push(advisory_listing_event(
                id,
                self.valid_from,
                &self.upstream_id,
            ));
        }
        ops
    }
}

/// Emit an `advisory` listing event for one affected version (REGISTRYLESS-PLAN
/// §12): a bitemporal `listing_events` row the Trustfall security policy plane
/// reads. The follower calls this once per version it has determined the
/// advisory's git range covers (`introduced ≤ rev < fixed`); that ancestry
/// translation is S-A proper and out of scope here.
pub fn advisory_listing_event(version: PackageId, valid_from: i64, upstream_id: &str) -> CatalogOp {
    CatalogOp::SetListing {
        version,
        status: ListingStatus::Advisory,
        valid_from,
        reason: Some(upstream_id.to_owned()),
    }
}

mod osv;
mod range;
mod rustsec;
mod time;
mod window;

pub use osv::{parse_osv, resolve_osv, resolve_stem};
pub use range::{range_listings_for, version_in_osv_range};
pub use rustsec::parse_rustsec;

#[cfg(test)]
mod tests;
