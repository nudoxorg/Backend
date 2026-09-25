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
        if range.contains("introduced:")
            || range.contains("fixed:")
            || range.contains("last_affected:")
        {
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
/// Parse one OSV JSON document into one [`AdvisorySource`] per affected
/// package.
///
/// A document with no `id` is rejected. An affected entry with no package name
/// is dropped. `withdrawn` sets `valid_to` and does not drop the advisory: a
/// withdrawn id still resolves. `stem_id` stays `None`; the feed name is kept
/// on [`AdvisorySource::affected_name`] for a later resolver.
pub fn parse_osv(body: &[u8], recorded_at: i64) -> Result<Vec<AdvisorySource>, String> {
    let value: serde_json::Value = serde_json::from_slice(body).map_err(|err| err.to_string())?;
    let id = value
        .get("id")
        .and_then(|id| id.as_str())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "osv document has no id".to_owned())?;
    let summary = value
        .get("summary")
        .and_then(|summary| summary.as_str())
        .map(str::to_owned);
    let url = value
        .get("references")
        .and_then(|references| references.as_array())
        .and_then(|references| {
            references
                .iter()
                .find_map(|reference| reference.get("url").and_then(|url| url.as_str()))
        })
        .map(str::to_owned);
    let severity = value
        .pointer("/database_specific/severity")
        .and_then(|severity| severity.as_str())
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("severity")
                .and_then(|severity| severity.as_array())
                .and_then(|severity| severity.first())
                .and_then(|entry| entry.get("score").and_then(|score| score.as_str()))
                .map(str::to_owned)
        });
    let valid_from = value
        .get("published")
        .and_then(|published| published.as_str())
        .and_then(rfc3339_millis)
        .unwrap_or(recorded_at);
    let valid_to = value
        .get("withdrawn")
        .and_then(|withdrawn| withdrawn.as_str())
        .and_then(rfc3339_millis);
    let affected = value
        .get("affected")
        .and_then(|affected| affected.as_array())
        .cloned()
        .unwrap_or_default();
    let mut sources = Vec::new();
    for entry in affected {
        let name = entry
            .pointer("/package/name")
            .and_then(|name| name.as_str())
            .filter(|name| !name.is_empty());
        let Some(name) = name else {
            continue;
        };
        let ecosystem = entry
            .pointer("/package/ecosystem")
            .and_then(|ecosystem| ecosystem.as_str())
            .map(str::to_owned);
        sources.push(AdvisorySource {
            upstream_id: id.to_owned(),
            stem_id: None,
            version_range: version_range(&entry),
            severity: severity.clone(),
            summary: summary.clone(),
            url: url.clone(),
            valid_from,
            valid_to,
            recorded_at,
            affected_name: Some(name.to_owned()),
            affected_ecosystem: ecosystem,
        });
    }
    if sources.is_empty() {
        sources.push(AdvisorySource {
            upstream_id: id.to_owned(),
            stem_id: None,
            version_range: None,
            severity,
            summary,
            url,
            valid_from,
            valid_to,
            recorded_at,
            affected_name: None,
            affected_ecosystem: None,
        });
    }
    Ok(sources)
}

fn version_range(entry: &serde_json::Value) -> Option<String> {
    if let Some(versions) = entry
        .get("versions")
        .and_then(|versions| versions.as_array())
    {
        let listed: Vec<&str> = versions
            .iter()
            .filter_map(|version| version.as_str())
            .collect();
        if !listed.is_empty() {
            return Some(listed.join(","));
        }
    }
    let events = entry
        .pointer("/ranges/0/events")
        .and_then(|events| events.as_array())?;
    let mut parts = Vec::new();
    for event in events {
        if let Some(introduced) = event.get("introduced").and_then(|value| value.as_str()) {
            parts.push(format!("introduced:{introduced}"));
        }
        if let Some(fixed) = event.get("fixed").and_then(|value| value.as_str()) {
            parts.push(format!("fixed:{fixed}"));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(","))
    }
}

/// `YYYY-MM-DDTHH:MM:SS` with a trailing `Z` or numeric offset, as unix millis.
/// A date that is not that shape is ignored.
fn rfc3339_millis(raw: &str) -> Option<i64> {
    let (date, time) = raw.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    let time = time.trim_end_matches('Z');
    let time = time.split(['+', '-']).next()?;
    let mut time_parts = time.split(':');
    let hour: u32 = time_parts.next()?.parse().ok()?;
    let minute: u32 = time_parts.next()?.parse().ok()?;
    let second: u32 = time_parts.next()?.split('.').next()?.parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let days = days_before_year(year) + days_before_month(year, month) + i64::from(day - 1);
    let secs = days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second);
    Some(secs.saturating_mul(1_000))
}

fn days_before_year(year: i64) -> i64 {
    let y = year - 1;
    y * 365 + y / 4 - y / 100 + y / 400
}

fn days_before_month(year: i64, month: u32) -> i64 {
    const CUMULATIVE: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let mut days = CUMULATIVE[(month - 1) as usize];
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    if leap && month > 2 {
        days += 1;
    }
    days
}

/// Map an OSV ecosystem token onto a catalog language.
///
/// The feed spells ecosystems as registry names (`crates.io`, `npm`, `PyPI`).
/// An unrecognized token leaves the advisory unresolved.
fn language_for_osv(token: &str) -> Option<crate::ecosystem::Language> {
    use crate::ecosystem::Language;
    match token {
        "crates.io" | "crates" => Some(Language::Rust),
        "npm" => Some(Language::Typescript),
        "Go" => Some(Language::Go),
        "PyPI" => Some(Language::Python),
        "Maven" => Some(Language::Java),
        "NuGet" => Some(Language::CSharp),
        other => crate::ecosystem::Language::from_token(other),
    }
}

/// Fill [`AdvisorySource::stem_id`] from the feed's ecosystem and package name.
///
/// The stem is the same `(language token, canonical name)` hash the catalog
/// uses for a package. A name that does not parse, or an ecosystem this index
/// does not speak, stays unresolved.
pub fn resolve_stem(source: &mut AdvisorySource) -> bool {
    use crate::ecosystem::LanguageExt;
    let (Some(name), Some(ecosystem)) = (
        source.affected_name.as_deref(),
        source.affected_ecosystem.as_deref(),
    ) else {
        return false;
    };
    let Some(language) = language_for_osv(ecosystem) else {
        return false;
    };
    let Some(parsed) = language.spec().parse_name(name) else {
        return false;
    };
    let id = heart::identity::derive::package_id_from_parts([
        language.as_token().as_bytes(),
        parsed.canonical().as_bytes(),
    ]);
    source.stem_id = Some(PackageStemId::from_uuid(*id.as_uuid()));
    true
}

/// Parse an OSV document and resolve every affected package that this index
/// can name.
pub fn resolve_osv(body: &[u8], recorded_at: i64) -> Result<Vec<AdvisorySource>, String> {
    let mut sources = parse_osv(body, recorded_at)?;
    for source in &mut sources {
        resolve_stem(source);
    }
    Ok(sources)
}

pub fn advisory_listing_event(version: PackageId, valid_from: i64, upstream_id: &str) -> CatalogOp {
    CatalogOp::SetListing {
        version,
        status: ListingStatus::Advisory,
        valid_from,
        reason: Some(upstream_id.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osv_affected_packages_become_sources_and_withdrawn_keeps_the_id() {
        let body = br#"{
            "id": "RUSTSEC-2021-0001",
            "published": "2021-01-02T00:00:00Z",
            "withdrawn": "2021-02-03T00:00:00Z",
            "summary": "overflow",
            "references": [{"url": "https://rustsec.org/advisories/RUSTSEC-2021-0001"}],
            "database_specific": {"severity": "HIGH"},
            "affected": [
                {"package": {"ecosystem": "crates.io", "name": "smallvec"},
                 "ranges": [{"events": [{"introduced": "0"}, {"fixed": "1.6.1"}]}]},
                {"package": {"ecosystem": "crates.io", "name": ""}},
                {"package": {"name": "other"}}
            ]
        }"#;
        let sources = parse_osv(body, 1).expect("osv");
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].upstream_id, "RUSTSEC-2021-0001");
        assert_eq!(sources[0].affected_name.as_deref(), Some("smallvec"));
        assert_eq!(sources[0].affected_ecosystem.as_deref(), Some("crates.io"));
        assert_eq!(
            sources[0].version_range.as_deref(),
            Some("introduced:0,fixed:1.6.1")
        );
        assert_eq!(sources[0].severity.as_deref(), Some("HIGH"));
        assert!(sources[0].stem_id.is_none());
        assert!(sources[0].valid_to.is_some());
        assert_eq!(sources[1].affected_name.as_deref(), Some("other"));
        let op = sources[0].to_upsert_op();
        assert!(matches!(op, CatalogOp::UpsertAdvisory { .. }));
    }

    #[test]
    fn osv_without_id_is_rejected_and_versions_list_is_the_range() {
        assert!(parse_osv(br#"{"summary":"x"}"#, 0).is_err());
        let body = br#"{"id":"GHSA-1","affected":[{"package":{"ecosystem":"npm","name":"left-pad"},"versions":["1.1.2","1.0.0"]}]}"#;
        let sources = parse_osv(body, 9).expect("osv");
        assert_eq!(sources[0].version_range.as_deref(), Some("1.1.2,1.0.0"));
        assert_eq!(sources[0].valid_from, 9);
    }

    #[test]
    fn resolve_osv_assigns_one_stem_per_language_and_keeps_unknown_ecosystems_open() {
        let body = br#"{
            "id": "GHSA-9",
            "withdrawn": "2024-01-01T00:00:00Z",
            "affected": [
                {"package": {"ecosystem": "crates.io", "name": "Serde_JSON"}},
                {"package": {"ecosystem": "crates.io", "name": "serde-json"}},
                {"package": {"ecosystem": "npm", "name": "serde-json"}},
                {"package": {"ecosystem": "Packagist", "name": "serde-json"}},
                {"package": {"ecosystem": "crates.io", "name": "not a crate"}}
            ]
        }"#;
        let sources = resolve_osv(body, 1).expect("resolve");
        assert_eq!(sources.len(), 5);
        let rust_a = sources[0].stem_id.expect("Serde_JSON resolves");
        let rust_b = sources[1].stem_id.expect("serde-json resolves");
        assert_eq!(rust_a, rust_b, "crates.io folds case and underscores");
        let npm = sources[2].stem_id.expect("npm resolves");
        assert_ne!(
            npm, rust_a,
            "the same spelling in two ecosystems is two stems"
        );
        assert!(
            sources[3].stem_id.is_none(),
            "an unknown ecosystem stays unresolved"
        );
        assert!(
            sources[4].stem_id.is_none(),
            "a name that fails the grammar stays unresolved"
        );
        assert!(sources[0].valid_to.is_some(), "withdrawal still resolves");
        let direct = {
            use crate::ecosystem::{Language, LanguageExt};
            let parsed = Language::Rust
                .spec()
                .parse_name("serde-json")
                .expect("parse");
            let id = heart::identity::derive::package_id_from_parts([
                Language::Rust.as_token().as_bytes(),
                parsed.canonical().as_bytes(),
            ]);
            PackageStemId::from_uuid(*id.as_uuid())
        };
        assert_eq!(rust_a, direct);
    }

    #[test]
    fn explicit_versions_emit_one_listing_and_ranges_do_not() {
        let listed = br#"{"id":"GHSA-1","affected":[{"package":{"ecosystem":"npm","name":"left-pad"},"versions":["1.0.0"," 1.0.0 "," "]}]}"#;
        let sources = resolve_osv(listed, 9).expect("resolve");
        let ops = sources[0].catalog_ops();
        assert!(matches!(ops[0], CatalogOp::UpsertAdvisory { .. }));
        match &ops[1] {
            CatalogOp::SetListing {
                status: ListingStatus::Advisory,
                reason,
                version,
                ..
            } => {
                assert_eq!(reason.as_deref(), Some("GHSA-1"));
                let stem = sources[0].stem_id.expect("stem");
                let expected = heart::identity::derive::package_id_from_parts([
                    stem.to_blob().as_slice(),
                    b"1.0.0".as_slice(),
                ]);
                assert_eq!(*version, expected);
            }
            other => panic!("expected one advisory listing, got {other:?}"),
        }
        assert_eq!(ops.len(), 2);

        let ranged = br#"{"id":"RUSTSEC-1","affected":[{"package":{"ecosystem":"crates.io","name":"smallvec"},"ranges":[{"events":[{"introduced":"0"},{"fixed":"1.6.1"}]}]}]}"#;
        assert_eq!(
            resolve_osv(ranged, 1).expect("range")[0]
                .catalog_ops()
                .len(),
            1
        );
        let withdrawn = br#"{"id":"GHSA-2","withdrawn":"2024-01-01T00:00:00Z","affected":[{"package":{"ecosystem":"npm","name":"left-pad"},"versions":["1.0.0"]}]}"#;
        assert_eq!(
            resolve_osv(withdrawn, 1).expect("withdrawn")[0]
                .catalog_ops()
                .len(),
            1
        );
    }
}
