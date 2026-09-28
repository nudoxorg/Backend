//! Tantivy projection for searchable fields on the selected registry catalog.
//!
//! The catalog remains authoritative. This index only stores ordinal keys and
//! source-backed text facets, so each result is hydrated from the current rows.

use super::super::discovery_search::{
    LineageKey, LineageSearchSource, SearchContinuation, SearchDocumentText, SearchPage,
    SearchStandingEvidence, SearchTextFacet, TextSearchIndex, lineage_sort_key,
};
use backend_engine::{DependencyFacts, RegistryPackageRecord};
use backend_library::{
    AdvisoryCategory, AdvisoryCoverage, AdvisoryStatus, PackageReference,
    RegistryNativeAvailability, RegistryNativeDetails, RegistryNativeMetadata,
    RegistryNativeObservation, RegistryReleaseMatchScope, RegistryReleaseStanding, SeverityLevel,
};
use std::collections::{BTreeMap, BTreeSet};

/// Search materialization for one immutable catalog revision.
pub(super) struct CatalogSearchIndex {
    inner: TextSearchIndex<usize>,
    lineage_inner: TextSearchIndex<LineageKey>,
    catalog_len: usize,
}

struct AcquiredLineageBuilder {
    coordinates: BTreeSet<String>,
    keywords: BTreeSet<String>,
    generic: BTreeSet<String>,
    advisories: BTreeSet<String>,
    native_metadata_known: bool,
    advisory_present: bool,
    advisory_coverage_complete: bool,
}

impl Default for AcquiredLineageBuilder {
    fn default() -> Self {
        Self {
            coordinates: BTreeSet::new(),
            keywords: BTreeSet::new(),
            generic: BTreeSet::new(),
            advisories: BTreeSet::new(),
            native_metadata_known: false,
            advisory_present: false,
            advisory_coverage_complete: true,
        }
    }
}

impl CatalogSearchIndex {
    /// Builds the replaceable projection from the authoritative rows.
    pub(super) fn build(catalog: &[RegistryPackageRecord]) -> Result<Self, String> {
        let mut inner = TextSearchIndex::new()?;
        let mut lineage_inner = TextSearchIndex::new()?;
        let mut lineages = BTreeMap::<LineageKey, AcquiredLineageBuilder>::new();
        let mut lineage_names = BTreeMap::<LineageKey, String>::new();
        for (ordinal, record) in catalog.iter().enumerate() {
            let (keywords, generic) = native_search_terms(&record.native_metadata);
            let advisory_terms = advisory_search_terms(&record.advisory);
            let advisories: Vec<&str> = advisory_terms.iter().map(String::as_str).collect();
            let native_known = matches!(
                &record.native_metadata.availability,
                RegistryNativeAvailability::Recorded
            );
            let mut lineage_group = None;
            if let PackageReference::Purl(coordinate) = &record.coordinate {
                let key = acquired_lineage_key(record, coordinate)?;
                lineage_group = Some(lineage_sort_key(&key));
                let builder = lineages.entry(key.clone()).or_default();
                builder
                    .coordinates
                    .insert(record.coordinate.as_str().to_owned());
                builder
                    .keywords
                    .extend(keywords.iter().map(|value| (*value).to_owned()));
                builder
                    .generic
                    .extend(generic.iter().map(|value| (*value).to_owned()));
                builder.advisories.extend(advisory_terms.iter().cloned());
                builder.native_metadata_known |= native_known;
                builder.advisory_present |= !advisory_terms.is_empty();
                builder.advisory_coverage_complete &=
                    record.advisory.coverage == AdvisoryCoverage::Complete;
                lineage_names
                    .entry(key.clone())
                    .or_insert_with(|| record.name.as_str().to_owned());
            }
            let mut search_text = SearchDocumentText {
                generic,
                aliases: SearchTextFacet::Unknown,
                keywords: if native_known {
                    SearchTextFacet::Known(keywords)
                } else {
                    SearchTextFacet::Absent
                },
                descriptions: SearchTextFacet::Unknown,
                advisories: if advisories.is_empty() {
                    match record.advisory.coverage {
                        AdvisoryCoverage::Complete => SearchTextFacet::Absent,
                        AdvisoryCoverage::Partial
                        | AdvisoryCoverage::Unknown
                        | AdvisoryCoverage::Unavailable => SearchTextFacet::Unknown,
                    }
                } else {
                    SearchTextFacet::Known(advisories)
                },
                // Standing and freshness are cheap mutable overlays. They are
                // attached from the supplied row when a page is read below.
                standing: SearchStandingEvidence::Unknown,
                ..SearchDocumentText::default()
            };
            search_text.lineage_group = lineage_group.as_deref();
            let sort_key = catalog_sort_key(ordinal, record);
            inner.add_document_with_search_text(
                ordinal,
                record.coordinate.as_str(),
                record.name.as_str(),
                search_text,
                record.ecosystem.as_str(),
                sort_key,
            )?;
        }
        inner.commit()?;
        for (key, builder) in lineages {
            let coordinate_variants = builder.coordinates.iter().map(String::as_str).collect();
            let keywords = if builder.native_metadata_known {
                SearchTextFacet::Known(builder.keywords.iter().map(String::as_str).collect())
            } else {
                SearchTextFacet::Absent
            };
            let advisories = if builder.advisory_present {
                SearchTextFacet::Known(builder.advisories.iter().map(String::as_str).collect())
            } else if builder.advisory_coverage_complete {
                SearchTextFacet::Absent
            } else {
                SearchTextFacet::Unknown
            };
            let search_text = SearchDocumentText {
                coordinate_variants,
                generic: builder.generic.iter().map(String::as_str).collect(),
                aliases: SearchTextFacet::Unknown,
                keywords,
                descriptions: SearchTextFacet::Unknown,
                advisories,
                standing: SearchStandingEvidence::Unknown,
                ..SearchDocumentText::default()
            };
            let lineage = key.normalized_lineage.as_str();
            let name = lineage_names.get(&key).map_or(lineage, String::as_str);
            let sort_key = lineage_sort_key(&key);
            lineage_inner.add_document_with_search_text(
                key.clone(),
                lineage,
                name,
                search_text,
                key.ecosystem.as_str(),
                sort_key,
            )?;
        }
        lineage_inner.commit()?;
        Ok(Self {
            inner,
            lineage_inner,
            catalog_len: catalog.len(),
        })
    }

    /// Returns one ranked posting per acquired package lineage. Every
    /// coordinate/version is indexed on that document, while the continuation
    /// advances by a stable source-scoped lineage key.
    pub(super) fn lineage_page_after(
        &self,
        catalog: &[RegistryPackageRecord],
        query: &str,
        limit: usize,
        continuation: Option<&SearchContinuation>,
    ) -> Result<SearchPage<LineageKey>, String> {
        if catalog.len() != self.catalog_len {
            return Err("catalog search projection does not match the catalog".to_owned());
        }
        self.lineage_inner
            .page_with_ecosystem(query, limit, None, continuation)
    }

    /// Finds matching release rows for one lineage. If the lineage matched
    /// only after unioning source metadata across versions, returns a bounded
    /// source-backed representative page and marks its scope explicitly.
    /// Tantivy probes one extra posting internally, so the public facet stays
    /// capped at sixteen while `more_releases` reports withheld rows.
    pub(super) fn matching_lineage_releases(
        &self,
        catalog: &[RegistryPackageRecord],
        key: &LineageKey,
        query: &str,
    ) -> Result<
        (
            Vec<RegistryPackageRecord>,
            bool,
            usize,
            RegistryReleaseMatchScope,
        ),
        String,
    > {
        if catalog.len() != self.catalog_len {
            return Err("catalog search projection does not match the catalog".to_owned());
        }
        let group_sort_key = lineage_sort_key(key);
        let mut page = self.inner.page_in_group(
            query,
            16,
            Some(key.ecosystem.as_str()),
            &group_sort_key,
            None,
        )?;
        let release_match_scope = if page.hits.is_empty() {
            // The lineage posting may match a union of keywords/advisories
            // contributed by different releases. Show navigable exact release
            // facts, but do not imply those versions individually matched.
            page = self.inner.page_in_group(
                "",
                16,
                Some(key.ecosystem.as_str()),
                &group_sort_key,
                None,
            )?;
            RegistryReleaseMatchScope::LineageMetadataOnly
        } else {
            RegistryReleaseMatchScope::ReleaseMatches
        };
        let mut records = Vec::with_capacity(page.hits.len().min(16));
        for hit in page.hits {
            let record = catalog
                .get(hit.key)
                .ok_or_else(|| "catalog search ordinal is outside its revision".to_owned())?;
            records.push(record.clone());
        }
        let more = page.next_cursor.is_some();
        Ok((records, more, page.posting_candidates, release_match_scope))
    }

    /// Returns indexed keys plus typed match evidence, hydrating current
    /// standing from the authoritative row without rebuilding the projection.
    pub(super) fn page(
        &self,
        catalog: &[RegistryPackageRecord],
        query: &str,
        limit: usize,
    ) -> Result<SearchPage<usize>, String> {
        if catalog.len() != self.catalog_len {
            return Err("catalog search projection does not match the catalog".to_owned());
        }
        let mut page = self.inner.page(query, limit)?;
        let now = super::current_epoch_millis();
        for hit in &mut page.hits {
            let record = catalog
                .get(hit.key)
                .ok_or_else(|| "catalog search ordinal is outside its revision".to_owned())?;
            hit.standing = standing_evidence(record, now);
        }
        Ok(page)
    }

    /// Continues one acquired-catalog posting stream from the exact last
    /// consumed key. The caller binds this continuation to the catalog
    /// snapshot carried by the public page token.
    pub(super) fn page_after(
        &self,
        catalog: &[RegistryPackageRecord],
        query: &str,
        limit: usize,
        continuation: Option<&SearchContinuation>,
    ) -> Result<SearchPage<usize>, String> {
        if catalog.len() != self.catalog_len {
            return Err("catalog search projection does not match the catalog".to_owned());
        }
        let mut page = self
            .inner
            .page_with_ecosystem(query, limit, None, continuation)?;
        let now = super::current_epoch_millis();
        for hit in &mut page.hits {
            let record = catalog
                .get(hit.key)
                .ok_or_else(|| "catalog search ordinal is outside its revision".to_owned())?;
            hit.standing = standing_evidence(record, now);
        }
        Ok(page)
    }
}

fn standing_evidence(record: &RegistryPackageRecord, now: u64) -> SearchStandingEvidence {
    match record.standing {
        RegistryReleaseStanding::Yanked => SearchStandingEvidence::Yanked,
        RegistryReleaseStanding::Removed => SearchStandingEvidence::Withdrawn,
        RegistryReleaseStanding::Available if super::has_current_standing_authority(record) => {
            let current = record.authority.is_some_and(|authority| {
                matches!(
                    authority.release_facts_freshness,
                    backend_engine::RegistryPackageFactFreshness::Current {
                        valid_until_millis,
                        ..
                    } if valid_until_millis > now
                )
            });
            if current {
                SearchStandingEvidence::Available
            } else {
                SearchStandingEvidence::Unknown
            }
        }
        RegistryReleaseStanding::Available
        | RegistryReleaseStanding::Deprecated
        | RegistryReleaseStanding::Unlisted
        | RegistryReleaseStanding::Retracted => SearchStandingEvidence::Unknown,
    }
}

fn acquired_lineage_key(
    record: &RegistryPackageRecord,
    coordinate: &backend_library::PackageCoordinate,
) -> Result<LineageKey, String> {
    let lineage = super::registry_search_lineage(coordinate, record.ecosystem);
    let key = LineageKey {
        source: LineageSearchSource::Acquired {
            ecosystem: record.ecosystem,
            source: record.authority.map(|authority| authority.source),
        },
        ecosystem: record.ecosystem,
        normalized_lineage: lineage.chars().flat_map(char::to_lowercase).collect(),
    };
    key.admit()?;
    Ok(key)
}

fn catalog_sort_key(ordinal: usize, record: &RegistryPackageRecord) -> String {
    let authority = record
        .authority
        .map_or([0; 32], |authority| authority.source);
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{ordinal:020}",
        normalize(record.coordinate.as_str()),
        hex(&authority),
        hex(&record.facts_version),
    )
}

fn native_search_terms<'a>(metadata: &'a RegistryNativeMetadata) -> (Vec<&'a str>, Vec<&'a str>) {
    let mut keywords = Vec::new();
    let mut generic = Vec::new();
    if !matches!(&metadata.availability, RegistryNativeAvailability::Recorded) {
        return (keywords, generic);
    }

    match &metadata.details {
        RegistryNativeDetails::Cargo(details) => {
            for feature in &details.features {
                push_nonempty(&mut keywords, &feature.name);
                keywords.extend(feature.members.iter().map(String::as_str));
            }
            append_artifacts(&mut generic, &details.artifacts);
        }
        RegistryNativeDetails::Npm(details) => {
            for tag in &details.dist_tags {
                push_nonempty(&mut keywords, &tag.name);
                push_nonempty(&mut keywords, &tag.version);
            }
            append_optional(&mut generic, details.deprecation.as_deref());
            append_artifacts(&mut generic, &details.artifacts);
        }
        RegistryNativeDetails::Pypi(details) => {
            append_optional(&mut keywords, details.requires_python.as_deref());
            append_optional(&mut generic, details.standing_reason.as_deref());
            append_artifacts(&mut generic, &details.artifacts);
        }
        RegistryNativeDetails::Maven(details) => {
            append_artifacts(&mut generic, &details.artifacts);
            if let RegistryNativeObservation::Recorded(DependencyFacts::Known(rows)) =
                &details.dependencies
            {
                append_dependencies(&mut keywords, &mut generic, rows);
            }
        }
        RegistryNativeDetails::Nuget(details) => {
            append_optional(&mut generic, details.deprecation.as_deref());
            append_artifacts(&mut generic, &details.artifacts);
            if let RegistryNativeObservation::Recorded(DependencyFacts::Known(rows)) =
                &details.dependencies
            {
                append_dependencies(&mut keywords, &mut generic, rows);
            }
            if let RegistryNativeObservation::Recorded(vulnerabilities) = &details.vulnerabilities {
                for vulnerability in vulnerabilities.iter() {
                    if vulnerability.severity > 0 {
                        generic.push(severity_label(vulnerability.severity));
                    }
                }
            }
        }
        RegistryNativeDetails::Golang(details) => {
            append_artifacts(&mut generic, &details.artifacts);
            for retract in &details.retracts {
                push_nonempty(&mut generic, &retract.lower);
                push_nonempty(&mut generic, &retract.upper);
            }
            if let RegistryNativeObservation::Recorded(source) = &details.source {
                push_nonempty(&mut keywords, &source.module);
                push_nonempty(&mut generic, &source.version);
            }
        }
        RegistryNativeDetails::Cpp(details) => {
            append_artifacts(&mut generic, &details.artifacts);
        }
        RegistryNativeDetails::Unavailable { .. } => {}
    }
    (keywords, generic)
}

fn append_artifacts<'a>(
    target: &mut Vec<&'a str>,
    artifacts: &'a [backend_library::RegistryNativeArtifact],
) {
    for artifact in artifacts {
        push_nonempty(target, &artifact.filename);
        append_optional(target, artifact.requires_python.as_deref());
        append_optional(target, artifact.yanked_reason.as_deref());
    }
}

fn append_dependencies<'a>(
    keywords: &mut Vec<&'a str>,
    generic: &mut Vec<&'a str>,
    rows: &'a [backend_engine::PackageDependencyRecord],
) {
    for row in rows {
        push_nonempty(keywords, row.target.name.as_str());
        push_nonempty(generic, row.target.requirement.as_str());
        if let Some(resolved) = &row.target.resolved {
            push_nonempty(generic, resolved.as_str());
        }
    }
}

fn advisory_search_terms(advisory: &backend_library::AdvisoryPackageDto) -> Vec<String> {
    let mut values = Vec::new();
    for item in advisory.advisories.iter() {
        push_owned_nonempty(&mut values, &item.canonical_id);
        values.extend(item.source_ids.iter().map(|id| id.id.clone()));
        values.extend(item.aliases.iter().cloned());
        values.extend(item.fixed_ranges.iter().cloned());
        if let Some(summary) = item.summary.as_deref().filter(|value| !value.is_empty()) {
            values.push(summary.to_owned());
        }
        values.push(severity_label_for_level(item.severity).to_owned());
        values.extend(
            item.statuses
                .iter()
                .map(|status| status_label(status).to_owned()),
        );
        values.extend(
            item.categories
                .iter()
                .map(|category| category_label(category).to_owned()),
        );
    }
    values.retain(|value| !value.is_empty());
    values
}

fn push_owned_nonempty(target: &mut Vec<String>, value: &str) {
    if !value.is_empty() {
        target.push(value.to_owned());
    }
}

fn push_nonempty<'a>(target: &mut Vec<&'a str>, value: &'a str) {
    if !value.is_empty() {
        target.push(value);
    }
}

fn append_optional<'a>(target: &mut Vec<&'a str>, value: Option<&'a str>) {
    if let Some(value) = value {
        push_nonempty(target, value);
    }
}

fn severity_label(value: u8) -> &'static str {
    match value {
        1 => "low",
        2 => "moderate",
        3 => "high",
        _ => "critical",
    }
}

fn severity_label_for_level(value: SeverityLevel) -> &'static str {
    match value {
        SeverityLevel::Unknown => "unknown",
        SeverityLevel::Low => "low",
        SeverityLevel::Moderate => "moderate",
        SeverityLevel::High => "high",
        SeverityLevel::Critical => "critical",
    }
}

fn status_label(value: &AdvisoryStatus) -> &'static str {
    match value {
        AdvisoryStatus::Yanked => "yanked",
        AdvisoryStatus::Unlisted => "unlisted",
        AdvisoryStatus::Vulnerable => "vulnerable",
        AdvisoryStatus::Malicious => "malicious",
        AdvisoryStatus::Unmaintained => "unmaintained",
        AdvisoryStatus::Unsound => "unsound",
        AdvisoryStatus::Notice => "notice",
        AdvisoryStatus::Withdrawn => "withdrawn",
        AdvisoryStatus::UnknownCoverage => "unknown coverage",
        AdvisoryStatus::Stale => "stale",
        AdvisoryStatus::Unavailable => "unavailable",
    }
}

fn category_label(value: &AdvisoryCategory) -> &'static str {
    match value {
        AdvisoryCategory::Vulnerability => "vulnerability",
        AdvisoryCategory::Malicious => "malicious",
        AdvisoryCategory::Unmaintained => "unmaintained",
        AdvisoryCategory::Unsound => "unsound",
        AdvisoryCategory::Notice => "notice",
    }
}

fn normalize(value: &str) -> String {
    value.chars().flat_map(char::to_lowercase).collect()
}

fn hex(value: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}
