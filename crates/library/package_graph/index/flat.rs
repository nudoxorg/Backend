//! Test-only flat ordinal package-graph index experiment.
//!
//! The production `PackageGraphIndex` remains the B-tree baseline. This
//! candidate is private to library tests and owns only checked-fact ordinals;
//! every lookup dereferences through the exact checked snapshot paired below.

use super::*;
use std::{cmp::Ordering, ops::Range};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceOrdinal(u32);

impl SourceOrdinal {
    fn from_index(index: usize) -> Result<Self, OrdinalWidthError> {
        u32::try_from(index)
            .map(Self)
            .map_err(|_| OrdinalWidthError::Source(index))
    }

    const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RowOrdinal(u16);

impl RowOrdinal {
    fn from_index(index: usize) -> Result<Self, OrdinalWidthError> {
        u16::try_from(index)
            .map(Self)
            .map_err(|_| OrdinalWidthError::Row(index))
    }

    const fn index(self) -> usize {
        self.0 as usize
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReversePosting {
    source: SourceOrdinal,
    row: RowOrdinal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrdinalWidthError {
    Source(usize),
    Row(usize),
    PostingCount,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FlatBuildError {
    Admission(PackageGraphAdmissionError),
    Ordinal(OrdinalWidthError),
}

impl From<PackageGraphAdmissionError> for FlatBuildError {
    fn from(error: PackageGraphAdmissionError) -> Self {
        Self::Admission(error)
    }
}

impl From<OrdinalWidthError> for FlatBuildError {
    fn from(error: OrdinalWidthError) -> Self {
        Self::Ordinal(error)
    }
}

#[derive(Clone, Debug, Default)]
struct FlatGraphIndex {
    reverse: Vec<ReversePosting>,
    first_incomplete_source: Option<SourceOrdinal>,
}

#[derive(Debug)]
pub(super) struct FlatCheckedGraph {
    checked: CheckedPackageGraphFacts,
    index: FlatGraphIndex,
}

impl FlatCheckedGraph {
    fn from_checked(checked: CheckedPackageGraphFacts) -> Result<Self, OrdinalWidthError> {
        let index = FlatGraphIndex::from_facts(checked.facts())?;
        Ok(Self { checked, index })
    }

    fn from_borrowed_facts<'a, I>(
        facts: I,
        limits: PackageGraphIndexLimits,
    ) -> Result<Self, FlatBuildError>
    where
        I: Iterator<Item = &'a PackageDependencySourceFacts> + Clone,
    {
        let checked = CheckedPackageGraphFacts::from_borrowed_facts(facts, limits)?;
        Self::from_checked(checked).map_err(FlatBuildError::Ordinal)
    }

    fn facts(&self) -> &[PackageDependencySourceFacts] {
        self.checked.facts()
    }

    fn witness(&self) -> [u8; 32] {
        self.checked.witness()
    }

    fn dependencies(&self, package: &PackageReference) -> PackageDependencyLookup<'_> {
        self.index.dependencies(self.facts(), package)
    }

    fn dependencies_for_source(
        &self,
        source: &PackageGraphSourceKey,
    ) -> Option<&DependencyFacts<Box<[PackageDependencyRecord]>>> {
        self.index.dependencies_for_source(self.facts(), source)
    }

    fn dependent_sources(&self, package: &PackageReference) -> DependentSources {
        self.index.dependent_sources(self.facts(), package)
    }

    fn reverse_coverage(&self) -> ReverseCoverage<'_> {
        self.index.reverse_coverage(self.facts())
    }

    fn dependent_coverage(&self, target: &crate::PackageCoordinate) -> ReverseCoverage<'_> {
        self.index.dependent_coverage(self.facts(), target)
    }

    fn dependent_edges_page(
        &self,
        package: &PackageReference,
        after: Option<[u8; 32]>,
        limit: u16,
    ) -> (Vec<&PackageDependencyRecord>, bool) {
        self.index
            .dependent_edges_page(self.facts(), package, after, limit)
    }

    fn posting_buffer_requested_bytes(&self) -> usize {
        self.index
            .reverse
            .capacity()
            .saturating_mul(std::mem::size_of::<ReversePosting>())
    }

    fn index_layout_bytes_including_vec_header(&self) -> usize {
        std::mem::size_of::<FlatGraphIndex>().saturating_add(self.posting_buffer_requested_bytes())
    }

    fn reverse_posting_count(&self) -> usize {
        self.index.reverse.len()
    }

    fn reverse_posting_capacity(&self) -> usize {
        self.index.reverse.capacity()
    }
}

impl FlatGraphIndex {
    fn from_facts(facts: &[PackageDependencySourceFacts]) -> Result<Self, OrdinalWidthError> {
        // Finish all ordinal and capacity arithmetic checks before reserving
        // the one posting vector. Checked facts already enforce the row ceiling
        // and source/row identity; these checks bound the compact representation.
        let posting_count = checked_posting_count(facts)?;
        let mut index = Self {
            reverse: Vec::with_capacity(posting_count),
            first_incomplete_source: None,
        };

        for (source_index, (source, state)) in facts.iter().enumerate() {
            let source_ordinal = SourceOrdinal::from_index(source_index)?;
            match state {
                DependencyFacts::Known(rows) => {
                    for (row_index, row) in rows.iter().enumerate() {
                        if !counts_for_reverse(row)
                            || row.source != source.coordinate
                            || row.source_authority != source.authority
                        {
                            continue;
                        }
                        index.reverse.push(ReversePosting {
                            source: source_ordinal,
                            row: RowOrdinal::from_index(row_index)?,
                        });
                    }
                }
                DependencyFacts::Unknown(_) | DependencyFacts::Unavailable(_) => {
                    if index.first_incomplete_source.is_none() {
                        index.first_incomplete_source = Some(source_ordinal);
                    }
                }
            }
        }

        index
            .reverse
            .sort_unstable_by(|left, right| compare_posting_order(facts, *left, *right));
        Ok(index)
    }

    fn dependencies<'a>(
        &self,
        facts: &'a [PackageDependencySourceFacts],
        package: &PackageReference,
    ) -> PackageDependencyLookup<'a> {
        // Match the typed coordinate identity used by the corrected map
        // baseline. A Local label and PURL may share display text, but they
        // are separate exact lookup ranges.
        let range = coordinate_range(facts, package);
        let Some(matches) = facts.get(range) else {
            return PackageDependencyLookup::Missing;
        };
        match matches {
            [] => PackageDependencyLookup::Missing,
            [(source, state)] => PackageDependencyLookup::Exact {
                source,
                facts: state,
            },
            many => PackageDependencyLookup::Ambiguous(
                many.iter()
                    .map(|(source, _)| source.clone())
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            ),
        }
    }

    fn dependencies_for_source<'a>(
        &self,
        facts: &'a [PackageDependencySourceFacts],
        source: &PackageGraphSourceKey,
    ) -> Option<&'a DependencyFacts<Box<[PackageDependencyRecord]>>> {
        // Source facts use the same exact typed-coordinate ordering as the
        // canonical checked snapshot, followed by authority identity.
        let start = facts.partition_point(|(candidate, _)| {
            compare_source_order(candidate, source) == Ordering::Less
        });
        let end = facts.partition_point(|(candidate, _)| {
            compare_source_order(candidate, source) != Ordering::Greater
        });
        facts
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .find(|(candidate, _)| *candidate == *source)
            .map(|(_, state)| state)
    }

    fn dependent_sources(
        &self,
        facts: &[PackageDependencySourceFacts],
        package: &PackageReference,
    ) -> DependentSources {
        let PackageReference::Purl(target) = package else {
            return DependentSources::NotPurl;
        };
        let Some(ecosystem) = target.package_type().registry() else {
            return DependentSources::Matched {
                sources: BTreeSet::new(),
                gap: self.reverse_coverage(facts).into_gap(),
            };
        };

        let qualified = target.qualifiers().is_some() || target.subpath().is_some();
        let gap = self.dependent_coverage(facts, target).into_gap();
        let ranges = self.matching_ranges(facts, ecosystem, target.lineage_name(), target.as_str());
        let unresolved = if qualified {
            empty_at(ranges.unresolved.start)
        } else {
            ranges.unresolved
        };
        let mut sources = BTreeSet::new();
        for posting in self
            .reverse
            .get(unresolved)
            .unwrap_or_default()
            .iter()
            .chain(self.reverse.get(ranges.resolved).unwrap_or_default())
        {
            if let Some((source, _)) = facts.get(posting.source.index()) {
                sources.insert(source.clone());
            }
        }
        DependentSources::Matched { sources, gap }
    }

    fn reverse_coverage<'a>(
        &self,
        facts: &'a [PackageDependencySourceFacts],
    ) -> ReverseCoverage<'a> {
        let Some(source_index) = self.first_incomplete_source else {
            return ReverseCoverage::Known;
        };
        match &facts
            .get(source_index.index())
            .expect("first incomplete ordinal belongs to its paired checked facts")
            .1
        {
            DependencyFacts::Unknown(reason) => ReverseCoverage::Unknown(reason),
            DependencyFacts::Unavailable(reason) => ReverseCoverage::Unavailable(reason),
            DependencyFacts::Known(_) => {
                unreachable!("first incomplete ordinal must retain its source state")
            }
        }
    }

    fn dependent_coverage<'a>(
        &self,
        facts: &'a [PackageDependencySourceFacts],
        target: &crate::PackageCoordinate,
    ) -> ReverseCoverage<'a> {
        let qualified = target.qualifiers().is_some() || target.subpath().is_some();
        let unresolved = qualified
            && target.package_type().registry().is_some_and(|ecosystem| {
                let ranges =
                    self.matching_ranges(facts, ecosystem, target.lineage_name(), target.as_str());
                !ranges.unresolved.is_empty()
            });
        if unresolved {
            ReverseCoverage::QualifiedUnresolved
        } else {
            self.reverse_coverage(facts)
        }
    }

    fn dependent_edges_page<'a>(
        &self,
        facts: &'a [PackageDependencySourceFacts],
        package: &PackageReference,
        after: Option<[u8; 32]>,
        limit: u16,
    ) -> (Vec<&'a PackageDependencyRecord>, bool) {
        let PackageReference::Purl(target) = package else {
            return (Vec::new(), false);
        };
        let Some(ecosystem) = target.package_type().registry() else {
            return (Vec::new(), false);
        };

        let ranges = self.matching_ranges(facts, ecosystem, target.lineage_name(), target.as_str());
        let qualified = target.qualifiers().is_some() || target.subpath().is_some();
        let unresolved_range = if qualified {
            empty_at(ranges.unresolved.start)
        } else {
            ranges.unresolved
        };
        let unresolved = self.reverse.get(unresolved_range).unwrap_or_default();
        let resolved = self.reverse.get(ranges.resolved).unwrap_or_default();
        let mut unresolved_index = after.map_or(0, |edge_id| {
            unresolved
                .partition_point(|posting| posting_row(facts, *posting).facts_version <= edge_id)
        });
        let mut resolved_index = after.map_or(0, |edge_id| {
            resolved
                .partition_point(|posting| posting_row(facts, *posting).facts_version <= edge_id)
        });
        let page_bound = usize::from(limit).saturating_add(1);
        let mut rows = Vec::with_capacity(page_bound);

        while rows.len() < page_bound {
            let next = match (
                unresolved.get(unresolved_index),
                resolved.get(resolved_index),
            ) {
                (None, None) => break,
                (Some(posting), None) => {
                    unresolved_index += 1;
                    *posting
                }
                (None, Some(posting)) => {
                    resolved_index += 1;
                    *posting
                }
                (Some(left), Some(right)) => {
                    if posting_row(facts, *left).facts_version
                        <= posting_row(facts, *right).facts_version
                    {
                        unresolved_index += 1;
                        *left
                    } else {
                        resolved_index += 1;
                        *right
                    }
                }
            };
            rows.push(posting_row(facts, next));
        }

        let more = rows.len() > usize::from(limit);
        rows.truncate(usize::from(limit));
        (rows, more)
    }

    fn matching_ranges(
        &self,
        facts: &[PackageDependencySourceFacts],
        ecosystem: RegistryEcosystem,
        name: &str,
        resolved: &str,
    ) -> MatchingRanges {
        let lower = self.reverse.partition_point(|posting| {
            compare_posting_name(facts, *posting, ecosystem, name) == Ordering::Less
        });
        let upper = self.reverse.partition_point(|posting| {
            compare_posting_name(facts, *posting, ecosystem, name) != Ordering::Greater
        });
        let group = self.reverse.get(lower..upper).unwrap_or_default();
        let unresolved_end =
            group.partition_point(|posting| posting_row(facts, *posting).target.resolved.is_none());
        let resolved_group = group.get(unresolved_end..).unwrap_or_default();
        let resolved_lower = resolved_group.partition_point(|posting| {
            compare_posting_resolution(facts, *posting, resolved) == Ordering::Less
        });
        let resolved_upper = resolved_group.partition_point(|posting| {
            compare_posting_resolution(facts, *posting, resolved) != Ordering::Greater
        });
        MatchingRanges {
            unresolved: lower..lower.saturating_add(unresolved_end),
            resolved: lower
                .saturating_add(unresolved_end)
                .saturating_add(resolved_lower)
                ..lower
                    .saturating_add(unresolved_end)
                    .saturating_add(resolved_upper),
        }
    }
}

#[derive(Clone, Debug)]
struct MatchingRanges {
    unresolved: Range<usize>,
    resolved: Range<usize>,
}

fn checked_posting_count(
    facts: &[PackageDependencySourceFacts],
) -> Result<usize, OrdinalWidthError> {
    let mut count = 0_usize;
    for (source_index, (_, state)) in facts.iter().enumerate() {
        SourceOrdinal::from_index(source_index)?;
        let DependencyFacts::Known(rows) = state else {
            continue;
        };
        if let Some(last_row) = rows.len().checked_sub(1) {
            RowOrdinal::from_index(last_row)?;
        }
        for _ in rows.iter().filter(|row| counts_for_reverse(row)) {
            count = count
                .checked_add(1)
                .ok_or(OrdinalWidthError::PostingCount)?;
        }
    }
    let posting_bytes = count
        .checked_mul(std::mem::size_of::<ReversePosting>())
        .ok_or(OrdinalWidthError::PostingCount)?;
    if posting_bytes > isize::MAX as usize {
        return Err(OrdinalWidthError::PostingCount);
    }
    Ok(count)
}

fn counts_for_reverse(row: &PackageDependencyRecord) -> bool {
    matches!(
        row.scope,
        DependencyScope::Runtime | DependencyScope::Optional
    )
}

fn compare_source_order(left: &PackageGraphSourceKey, right: &PackageGraphSourceKey) -> Ordering {
    left.canonical_cmp(right)
}

fn coordinate_range(
    facts: &[PackageDependencySourceFacts],
    coordinate: &PackageReference,
) -> Range<usize> {
    let start = facts.partition_point(|(source, _)| {
        source.coordinate.canonical_cmp(coordinate) == Ordering::Less
    });
    let end = facts.partition_point(|(source, _)| {
        source.coordinate.canonical_cmp(coordinate) != Ordering::Greater
    });
    start..end
}

fn posting_row<'a>(
    facts: &'a [PackageDependencySourceFacts],
    posting: ReversePosting,
) -> &'a PackageDependencyRecord {
    match &facts
        .get(posting.source.index())
        .expect("reverse source ordinal belongs to paired checked facts")
        .1
    {
        DependencyFacts::Known(rows) => rows
            .get(posting.row.index())
            .expect("reverse row ordinal belongs to paired known facts"),
        DependencyFacts::Unknown(_) | DependencyFacts::Unavailable(_) => {
            unreachable!("reverse posting cannot reference incomplete facts")
        }
    }
}

fn compare_posting_name(
    facts: &[PackageDependencySourceFacts],
    posting: ReversePosting,
    ecosystem: RegistryEcosystem,
    name: &str,
) -> Ordering {
    let target = &posting_row(facts, posting).target;
    target
        .ecosystem
        .cmp(&ecosystem)
        .then_with(|| target.name.as_str().cmp(name))
}

fn compare_posting_resolution(
    facts: &[PackageDependencySourceFacts],
    posting: ReversePosting,
    resolved: &str,
) -> Ordering {
    posting_row(facts, posting)
        .target
        .resolved
        .as_ref()
        .map_or(Ordering::Less, |reference| reference.as_str().cmp(resolved))
}

fn compare_posting_order(
    facts: &[PackageDependencySourceFacts],
    left: ReversePosting,
    right: ReversePosting,
) -> Ordering {
    let left_row = posting_row(facts, left);
    let right_row = posting_row(facts, right);
    left_row
        .target
        .ecosystem
        .cmp(&right_row.target.ecosystem)
        .then_with(|| {
            left_row
                .target
                .name
                .as_str()
                .cmp(right_row.target.name.as_str())
        })
        .then_with(|| {
            left_row
                .target
                .resolved
                .as_ref()
                .map(PackageReference::as_str)
                .cmp(
                    &right_row
                        .target
                        .resolved
                        .as_ref()
                        .map(PackageReference::as_str),
                )
        })
        .then_with(|| left_row.facts_version.cmp(&right_row.facts_version))
}

fn empty_at(position: usize) -> Range<usize> {
    position..position
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(super) fn generous_limits() -> PackageGraphIndexLimits {
        PackageGraphIndexLimits {
            max_sources: 8_192,
            max_fact_bytes: 512 * 1024 * 1024,
            max_total_rows: 1_000_000,
            max_reverse_edges: 1_000_000,
            max_index_key_bytes: 512 * 1024 * 1024,
        }
    }

    fn registry_authority(seed: u8) -> PackageGraphSourceAuthority {
        PackageGraphSourceAuthority::Registry(RegistryAuthorityId::from_configured_source(
            [seed; 32],
        ))
    }

    fn source(coordinate: &str, authority: PackageGraphSourceAuthority) -> PackageGraphSourceKey {
        PackageGraphSourceKey::new(
            PackageReference::parse(coordinate).expect("source coordinate"),
            authority,
        )
    }

    fn row(
        source: &PackageGraphSourceKey,
        ecosystem: RegistryEcosystem,
        name: &str,
        resolved: Option<&str>,
        scope: DependencyScope,
        seed: u64,
    ) -> PackageDependencyRecord {
        let mut frontier = [0; 32];
        frontier[..8].copy_from_slice(&seed.to_be_bytes());
        let mut provenance = [0; 32];
        provenance[..8].copy_from_slice(&seed.wrapping_add(1).to_be_bytes());
        let evidence_authority = match source.authority {
            PackageGraphSourceAuthority::Registry(_)
            | PackageGraphSourceAuthority::Unattributed => DependencyAuthority::RegistryMetadata,
            PackageGraphSourceAuthority::Forge(_) => DependencyAuthority::ForgeManifest,
            PackageGraphSourceAuthority::Archive(_) => DependencyAuthority::ArchiveManifest,
            PackageGraphSourceAuthority::Local(_) => DependencyAuthority::LocalManifest,
        };
        PackageDependencyRecord::new_with_source_authority(
            source.coordinate.clone(),
            source.authority,
            PackageDependencyTarget::new(
                ecosystem,
                name,
                "^1",
                resolved.map(|value| PackageReference::parse(value).expect("resolved target")),
            )
            .expect("dependency target"),
            scope,
            false,
            DependencyEvidence {
                authority: evidence_authority,
                frontier,
                provenance,
            },
        )
    }

    fn known(
        source: PackageGraphSourceKey,
        rows: Vec<PackageDependencyRecord>,
    ) -> PackageDependencySourceFacts {
        (source, DependencyFacts::Known(rows.into_boxed_slice()))
    }

    fn adversarial_facts() -> Vec<PackageDependencySourceFacts> {
        let serde_v1 = "pkg:cargo/serde@1.0.0";
        let serde_v2 = "pkg:cargo/serde@2.0.0";
        let serde_qualified = "pkg:cargo/serde@1.0.0?repository_url=https%3A%2F%2Fmirror.example";
        let exact = source("pkg:cargo/shared@1.0.0", registry_authority(0x11));
        let exact_mirror = source("pkg:cargo/shared@1.0.0", registry_authority(0x22));
        let local_purl_spelling =
            PackageReference::parse("pkg:cargo/spelling-collision@1.0.0").expect("purl spelling");
        let local_same_spelling = PackageReference::Local(
            ProductText::new(local_purl_spelling.as_str()).expect("local spelling"),
        );
        let collision_authority = PackageGraphSourceAuthority::Unattributed;
        let collision_purl = PackageGraphSourceKey::new(local_purl_spelling, collision_authority);
        let collision_local = PackageGraphSourceKey::new(local_same_spelling, collision_authority);

        vec![
            (
                source("pkg:cargo/a-unknown@1.0.0", registry_authority(0x31)),
                DependencyFacts::Unknown(
                    ProductText::new("first unknown frontier").expect("unknown reason"),
                ),
            ),
            known(
                exact.clone(),
                vec![
                    row(
                        &exact,
                        RegistryEcosystem::Cargo,
                        "serde",
                        None,
                        DependencyScope::Runtime,
                        1,
                    ),
                    row(
                        &exact,
                        RegistryEcosystem::Cargo,
                        "serde",
                        Some(serde_v1),
                        DependencyScope::Runtime,
                        2,
                    ),
                    row(
                        &exact,
                        RegistryEcosystem::Cargo,
                        "serde",
                        Some(serde_v2),
                        DependencyScope::Optional,
                        3,
                    ),
                    row(
                        &exact,
                        RegistryEcosystem::Cargo,
                        "serde",
                        None,
                        DependencyScope::Development,
                        4,
                    ),
                    row(
                        &exact,
                        RegistryEcosystem::Cargo,
                        "serde",
                        None,
                        DependencyScope::Build,
                        5,
                    ),
                    row(
                        &exact,
                        RegistryEcosystem::Cargo,
                        "serde",
                        None,
                        DependencyScope::Peer,
                        6,
                    ),
                    row(
                        &exact,
                        RegistryEcosystem::Cargo,
                        "serde",
                        Some(serde_qualified),
                        DependencyScope::Runtime,
                        7,
                    ),
                    row(
                        &exact,
                        RegistryEcosystem::Cargo,
                        "serde",
                        Some("pkg:cargo/serde@1.0.0#src/lib.rs"),
                        DependencyScope::Optional,
                        70,
                    ),
                    row(
                        &exact,
                        RegistryEcosystem::Npm,
                        "serde",
                        Some("pkg:npm/serde@1.0.0"),
                        DependencyScope::Optional,
                        8,
                    ),
                    row(
                        &exact,
                        RegistryEcosystem::Pypi,
                        "serde",
                        None,
                        DependencyScope::Runtime,
                        9,
                    ),
                ],
            ),
            known(
                exact_mirror.clone(),
                vec![row(
                    &exact_mirror,
                    RegistryEcosystem::Cargo,
                    "serde",
                    None,
                    DependencyScope::Optional,
                    10,
                )],
            ),
            (
                source("pkg:cargo/z-unavailable@1.0.0", registry_authority(0x32)),
                DependencyFacts::Unavailable(
                    ProductText::new("later unavailable frontier").expect("unavailable reason"),
                ),
            ),
            known(
                collision_purl.clone(),
                vec![row(
                    &collision_purl,
                    RegistryEcosystem::Cargo,
                    "purl-spelling",
                    None,
                    DependencyScope::Runtime,
                    11,
                )],
            ),
            known(
                collision_local.clone(),
                vec![row(
                    &collision_local,
                    RegistryEcosystem::Cargo,
                    "local-spelling",
                    None,
                    DependencyScope::Runtime,
                    12,
                )],
            ),
        ]
    }

    pub(super) fn scan_edges<'a>(
        facts: &'a [PackageDependencySourceFacts],
        package: &PackageReference,
    ) -> Vec<&'a PackageDependencyRecord> {
        let PackageReference::Purl(target) = package else {
            return Vec::new();
        };
        let Some(ecosystem) = target.package_type().registry() else {
            return Vec::new();
        };
        let qualified = target.qualifiers().is_some() || target.subpath().is_some();
        let mut rows = Vec::new();
        for (_, state) in facts {
            let DependencyFacts::Known(source_rows) = state else {
                continue;
            };
            rows.extend(source_rows.iter().filter(|row| {
                if !counts_for_reverse(row)
                    || row.target.ecosystem != ecosystem
                    || row.target.name.as_str() != target.lineage_name()
                {
                    return false;
                }
                match &row.target.resolved {
                    Some(resolved) => resolved == package,
                    None => !qualified,
                }
            }));
        }
        rows.sort_unstable_by_key(|row| row.facts_version);
        rows
    }

    pub(super) fn scan_page<'a>(
        facts: &'a [PackageDependencySourceFacts],
        package: &PackageReference,
        after: Option<[u8; 32]>,
        limit: u16,
    ) -> (Vec<&'a PackageDependencyRecord>, bool) {
        let mut rows = scan_edges(facts, package)
            .into_iter()
            .filter(|row| after.is_none_or(|edge_id| row.facts_version > edge_id))
            .take(usize::from(limit).saturating_add(1))
            .collect::<Vec<_>>();
        let more = rows.len() > usize::from(limit);
        rows.truncate(usize::from(limit));
        (rows, more)
    }

    fn scan_dependencies_by_coordinate<'a>(
        facts: &'a [PackageDependencySourceFacts],
        package: &PackageReference,
    ) -> PackageDependencyLookup<'a> {
        let matches = facts
            .iter()
            .filter(|(source, _)| &source.coordinate == package)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [] => PackageDependencyLookup::Missing,
            [(source, state)] => PackageDependencyLookup::Exact {
                source,
                facts: state,
            },
            many => PackageDependencyLookup::Ambiguous(
                many.iter()
                    .map(|(source, _)| (*source).clone())
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            ),
        }
    }

    fn scan_dependencies_for_exact_source<'a>(
        facts: &'a [PackageDependencySourceFacts],
        source: &PackageGraphSourceKey,
    ) -> Option<&'a DependencyFacts<Box<[PackageDependencyRecord]>>> {
        facts
            .iter()
            .find(|(candidate, _)| candidate == source)
            .map(|(_, state)| state)
    }

    fn scan_reverse_coverage<'a>(facts: &'a [PackageDependencySourceFacts]) -> ReverseCoverage<'a> {
        for (_, state) in facts {
            match state {
                DependencyFacts::Known(_) => {}
                DependencyFacts::Unknown(reason) => return ReverseCoverage::Unknown(reason),
                DependencyFacts::Unavailable(reason) => {
                    return ReverseCoverage::Unavailable(reason);
                }
            }
        }
        ReverseCoverage::Known
    }

    fn scan_dependent_coverage<'a>(
        facts: &'a [PackageDependencySourceFacts],
        target: &crate::PackageCoordinate,
    ) -> ReverseCoverage<'a> {
        let qualified = target.qualifiers().is_some() || target.subpath().is_some();
        let unresolved = qualified
            && target.package_type().registry().is_some_and(|ecosystem| {
                facts.iter().any(|(_, state)| match state {
                    DependencyFacts::Known(rows) => rows.iter().any(|row| {
                        counts_for_reverse(row)
                            && row.target.ecosystem == ecosystem
                            && row.target.name.as_str() == target.lineage_name()
                            && row.target.resolved.is_none()
                    }),
                    DependencyFacts::Unknown(_) | DependencyFacts::Unavailable(_) => false,
                })
            });
        if unresolved {
            ReverseCoverage::QualifiedUnresolved
        } else {
            scan_reverse_coverage(facts)
        }
    }

    fn checked_pair(
        facts: Vec<PackageDependencySourceFacts>,
    ) -> (IndexedCheckedPackageGraph, FlatCheckedGraph) {
        let limits = generous_limits();
        let checked = CheckedPackageGraphFacts::new_with_limits(facts, limits)
            .expect("checked adversarial graph");
        let map = IndexedCheckedPackageGraph::from_checked_facts(checked.clone(), limits)
            .expect("map baseline");
        let flat = FlatCheckedGraph::from_checked(checked).expect("flat candidate");
        (map, flat)
    }

    fn next_random(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        *state
    }

    fn generated_coordinate(
        ecosystem: RegistryEcosystem,
        name: &str,
        version: u8,
        subpath: bool,
    ) -> PackageReference {
        let ecosystem = match ecosystem {
            RegistryEcosystem::Cargo => "cargo",
            RegistryEcosystem::Npm => "npm",
            RegistryEcosystem::Pypi => "pypi",
            other => panic!("generator ecosystem must be registry-backed: {other:?}"),
        };
        let subpath = if subpath { "#src/lib.rs" } else { "" };
        PackageReference::parse(&format!("pkg:{ecosystem}/{name}@1.0.{version}{subpath}"))
            .expect("generated package URL")
    }

    pub(super) fn generated_facts(
        seed: u64,
        source_count: usize,
    ) -> (Vec<PackageDependencySourceFacts>, Vec<PackageReference>) {
        let ecosystems = [
            RegistryEcosystem::Cargo,
            RegistryEcosystem::Npm,
            RegistryEcosystem::Pypi,
        ];
        let scopes = [
            DependencyScope::Runtime,
            DependencyScope::Optional,
            DependencyScope::Development,
            DependencyScope::Build,
            DependencyScope::Peer,
        ];
        let mut random = seed.max(1);
        let mut facts = Vec::with_capacity(source_count);
        let mut queries = BTreeSet::new();
        let mut evidence_seed = seed.wrapping_add(1);

        for source_index in 0..source_count {
            let authority = registry_authority(
                u8::try_from(next_random(&mut random) % 8).expect("small authority id"),
            );
            let source_key = source(
                &format!("pkg:cargo/generated-{source_index}@1.0.0"),
                authority,
            );
            match next_random(&mut random) % 11 {
                0 => facts.push((
                    source_key,
                    DependencyFacts::Unknown(
                        ProductText::new("generated unknown").expect("reason"),
                    ),
                )),
                1 => facts.push((
                    source_key,
                    DependencyFacts::Unavailable(
                        ProductText::new("generated unavailable").expect("reason"),
                    ),
                )),
                _ => {
                    let row_count = usize::try_from(next_random(&mut random) % 9)
                        .expect("bounded generated row count");
                    let mut rows = Vec::with_capacity(row_count);
                    for row_index in 0..row_count {
                        let ecosystem = ecosystems[usize::try_from(
                            next_random(&mut random) % ecosystems.len() as u64,
                        )
                        .expect("ecosystem index")];
                        let name = format!("generated-target-{}", next_random(&mut random) % 17);
                        let version = u8::try_from(next_random(&mut random) % 4)
                            .expect("small generated version");
                        let subpath = next_random(&mut random) % 13 == 0;
                        let resolved = (next_random(&mut random) % 3 != 0)
                            .then(|| generated_coordinate(ecosystem, &name, version, subpath));
                        let query = generated_coordinate(ecosystem, &name, 0, false);
                        queries.insert(query);
                        if let Some(resolved) = &resolved {
                            queries.insert(resolved.clone());
                        }
                        let scope =
                            scopes[usize::try_from(next_random(&mut random) % scopes.len() as u64)
                                .expect("scope index")];
                        evidence_seed = evidence_seed.wrapping_add(1);
                        let edge = row(
                            &source_key,
                            ecosystem,
                            &name,
                            resolved.as_ref().map(PackageReference::as_str),
                            scope,
                            evidence_seed,
                        );
                        rows.push(edge);
                    }
                    facts.push(known(source_key, rows));
                }
            }
        }
        (facts, queries.into_iter().collect())
    }

    pub(super) fn typed_identity_collision_facts() -> Vec<PackageDependencySourceFacts> {
        let purl = PackageReference::parse("pkg:cargo/benchmark-kind-collision@1.0.0")
            .expect("PURL collision coordinate");
        let local = PackageReference::from_kind(crate::PackageReferenceKind::Local, purl.as_str())
            .expect("same-spelling local collision label");
        let purl_source = PackageGraphSourceKey::unattributed(purl.clone());
        let local_source = PackageGraphSourceKey::unattributed(local);
        let dependent_source = source(
            "pkg:cargo/benchmark-kind-dependent@1.0.0",
            registry_authority(0x7a),
        );
        vec![
            known(purl_source, Vec::new()),
            known(local_source, Vec::new()),
            known(
                dependent_source.clone(),
                vec![row(
                    &dependent_source,
                    RegistryEcosystem::Cargo,
                    "benchmark-kind-collision",
                    Some(purl.as_str()),
                    DependencyScope::Runtime,
                    0x7b,
                )],
            ),
        ]
    }

    fn interleaved_unresolved_resolved_facts() -> CheckedPackageGraphFacts {
        let source = source(
            "pkg:cargo/interleaved-owner@1.0.0",
            registry_authority(0x44),
        );
        for attempt in 0..128_u64 {
            let rows = (0..12_u64)
                .map(|row_index| {
                    row(
                        &source,
                        RegistryEcosystem::Cargo,
                        "interleaved-target",
                        (row_index % 2 == 1).then_some("pkg:cargo/interleaved-target@1.0.0"),
                        DependencyScope::Runtime,
                        1 + attempt * 16 + row_index,
                    )
                })
                .collect::<Vec<_>>();
            let checked = CheckedPackageGraphFacts::new(vec![known(source.clone(), rows)])
                .expect("interleaved checked facts");
            let DependencyFacts::Known(ordered) = &checked.facts()[0].1 else {
                unreachable!("interleaved fixture remains known");
            };
            let resolved = ordered
                .iter()
                .map(|row| row.target.resolved.is_some())
                .collect::<Vec<_>>();
            let transitions = resolved
                .windows(2)
                .filter(|pair| pair[0] != pair[1])
                .count();
            if transitions >= 6 {
                return checked;
            }
        }
        panic!("bounded deterministic search failed to find interleaved edge IDs");
    }

    #[test]
    fn flat_postings_are_compact_and_ordinal_boundaries_fail_closed() {
        assert_eq!(std::mem::size_of::<ReversePosting>(), 8);
        assert_eq!(SourceOrdinal::from_index(0), Ok(SourceOrdinal(0)));
        assert_eq!(RowOrdinal::from_index(0), Ok(RowOrdinal(0)));

        if let Ok(max_source) = usize::try_from(u32::MAX) {
            assert_eq!(
                SourceOrdinal::from_index(max_source),
                Ok(SourceOrdinal(u32::MAX))
            );
            if let Some(too_large) = max_source.checked_add(1) {
                assert_eq!(
                    SourceOrdinal::from_index(too_large),
                    Err(OrdinalWidthError::Source(too_large))
                );
            }
        }
        let max_row = usize::from(u16::MAX);
        assert_eq!(RowOrdinal::from_index(max_row), Ok(RowOrdinal(u16::MAX)));
        assert_eq!(
            RowOrdinal::from_index(max_row + 1),
            Err(OrdinalWidthError::Row(max_row + 1))
        );
    }

    #[test]
    fn map_and_flat_match_independent_forward_reverse_and_page_oracles() {
        let facts = adversarial_facts();
        let (map, flat) = checked_pair(facts);
        assert_eq!(map.witness(), flat.witness());
        assert_eq!(map.facts(), flat.facts());
        let expected_coverage = scan_reverse_coverage(map.facts());
        assert_eq!(map.reverse_coverage(), expected_coverage);
        assert_eq!(flat.reverse_coverage(), expected_coverage);

        for (source, _) in map.facts() {
            assert_eq!(
                map.dependencies_for_source(source),
                flat.dependencies_for_source(source),
                "exact typed source lookup for {} {:?}",
                source.coordinate.as_str(),
                source.authority
            );
            let expected = scan_dependencies_by_coordinate(map.facts(), &source.coordinate);
            assert_eq!(map.dependencies(&source.coordinate), expected);
            assert_eq!(flat.dependencies(&source.coordinate), expected);
            let exact_expected = scan_dependencies_for_exact_source(map.facts(), source);
            assert_eq!(map.dependencies_for_source(source), exact_expected);
            assert_eq!(flat.dependencies_for_source(source), exact_expected);
        }
        let missing =
            PackageReference::parse("pkg:cargo/zz-not-recorded@9.0.0").expect("missing coordinate");
        assert_eq!(map.dependencies(&missing), flat.dependencies(&missing));

        let local_serde_v1 = PackageReference::from_kind(
            crate::PackageReferenceKind::Local,
            "pkg:cargo/serde@1.0.0",
        )
        .expect("same-spelling local reverse-query label");
        let targets = [
            PackageReference::parse("pkg:cargo/serde@1.0.0").expect("cargo v1"),
            local_serde_v1,
            PackageReference::parse("pkg:cargo/serde@2.0.0").expect("cargo v2"),
            PackageReference::parse("pkg:cargo/serde@9.0.0").expect("cargo miss"),
            PackageReference::parse(
                "pkg:cargo/serde@1.0.0?repository_url=https%3A%2F%2Fmirror.example",
            )
            .expect("qualified cargo"),
            PackageReference::parse("pkg:cargo/serde@1.0.0#src/lib.rs").expect("subpath cargo"),
            PackageReference::parse("pkg:npm/serde@1.0.0").expect("npm"),
            PackageReference::parse("pkg:pypi/serde@1.0.0").expect("pypi"),
            PackageReference::parse("pkg:github/example/repo@1.0.0").expect("non-registry purl"),
            PackageReference::parse("local-target").expect("local"),
        ];
        for target in &targets {
            let expected = linear_dependent_sources(map.facts(), target);
            assert_eq!(map.dependent_sources(target), expected);
            assert_eq!(flat.dependent_sources(target), expected);
            if let PackageReference::Purl(target_url) = target {
                let expected_coverage = scan_dependent_coverage(map.facts(), target_url);
                assert_eq!(map.dependent_coverage(target_url), expected_coverage);
                assert_eq!(flat.dependent_coverage(target_url), expected_coverage);
            }

            for limit in [1, 3, 128] {
                let (expected_rows, expected_more) = scan_page(map.facts(), target, None, limit);
                assert_eq!(
                    map.dependent_edges_page(target, None, limit),
                    (expected_rows.clone(), expected_more),
                    "map first page: {target:?}, limit={limit}"
                );
                assert_eq!(
                    flat.dependent_edges_page(target, None, limit),
                    (expected_rows, expected_more),
                    "flat first page: {target:?}, limit={limit}"
                );

                let mut after = None;
                let mut observed = Vec::new();
                loop {
                    let (expected_rows, expected_more) =
                        scan_page(map.facts(), target, after, limit);
                    let map_page = map.dependent_edges_page(target, after, limit);
                    let flat_page = flat.dependent_edges_page(target, after, limit);
                    assert_eq!(map_page, (expected_rows.clone(), expected_more));
                    assert_eq!(flat_page, (expected_rows.clone(), expected_more));
                    observed.extend(expected_rows.iter().map(|row| row.facts_version));
                    if !expected_more {
                        break;
                    }
                    after = expected_rows.last().map(|row| row.facts_version);
                    assert!(after.is_some(), "a continuation must advance");
                }
                assert_eq!(
                    observed,
                    scan_edges(map.facts(), target)
                        .iter()
                        .map(|row| row.facts_version)
                        .collect::<Vec<_>>()
                );
            }
        }
    }

    #[test]
    fn seeded_adversarial_corpora_match_full_scan_forward_and_paged_reverse_oracles() {
        for seed in [1_u64, 7, 29, 0x5eed, 0x9e37_79b9] {
            let (mut facts, mut targets) = generated_facts(seed, 40);
            facts.extend(typed_identity_collision_facts());
            let purl_collision =
                PackageReference::parse("pkg:cargo/benchmark-kind-collision@1.0.0")
                    .expect("PURL collision target");
            let local_collision = PackageReference::from_kind(
                crate::PackageReferenceKind::Local,
                purl_collision.as_str(),
            )
            .expect("same-spelling local collision target");
            targets.extend([purl_collision, local_collision]);
            let (map, flat) = checked_pair(facts);
            assert_eq!(map.witness(), flat.witness(), "seed={seed:#x}");
            assert_eq!(map.facts(), flat.facts(), "seed={seed:#x}");
            let expected_coverage = scan_reverse_coverage(map.facts());
            assert_eq!(map.reverse_coverage(), expected_coverage, "seed={seed:#x}");
            assert_eq!(flat.reverse_coverage(), expected_coverage, "seed={seed:#x}");
            for (source, _) in map.facts() {
                let exact_expected = scan_dependencies_for_exact_source(map.facts(), source);
                assert_eq!(
                    map.dependencies_for_source(source),
                    exact_expected,
                    "map exact source mismatch at seed={seed:#x}: {source:?}"
                );
                assert_eq!(
                    flat.dependencies_for_source(source),
                    exact_expected,
                    "exact source mismatch at seed={seed:#x}: {source:?}"
                );
                let coordinate_expected =
                    scan_dependencies_by_coordinate(map.facts(), &source.coordinate);
                assert_eq!(
                    map.dependencies(&source.coordinate),
                    coordinate_expected,
                    "map exact typed-coordinate mismatch at seed={seed:#x}: {source:?}"
                );
                assert_eq!(
                    flat.dependencies(&source.coordinate),
                    coordinate_expected,
                    "exact typed-coordinate mismatch at seed={seed:#x}: {source:?}"
                );
            }
            for target in &targets {
                let expected_sources = linear_dependent_sources(map.facts(), target);
                assert_eq!(map.dependent_sources(target), expected_sources);
                assert_eq!(flat.dependent_sources(target), expected_sources);
                if let PackageReference::Purl(target_coordinate) = target {
                    let expected_coverage = scan_dependent_coverage(map.facts(), target_coordinate);
                    assert_eq!(
                        map.dependent_coverage(target_coordinate),
                        expected_coverage,
                        "map coverage mismatch at seed={seed:#x}: {target:?}"
                    );
                    assert_eq!(
                        flat.dependent_coverage(target_coordinate),
                        expected_coverage,
                        "coverage mismatch at seed={seed:#x}: {target:?}"
                    );
                }
                for limit in [1, 7, 128] {
                    let mut after = None;
                    let mut observed = Vec::new();
                    loop {
                        let oracle = scan_page(map.facts(), target, after, limit);
                        let map_page = map.dependent_edges_page(target, after, limit);
                        let flat_page = flat.dependent_edges_page(target, after, limit);
                        assert_eq!(map_page, oracle, "map seed={seed:#x}, {target:?}");
                        assert_eq!(flat_page, oracle, "flat seed={seed:#x}, {target:?}");
                        observed.extend(oracle.0.iter().map(|row| row.facts_version));
                        if !oracle.1 {
                            break;
                        }
                        after = oracle.0.last().map(|row| row.facts_version);
                        assert!(after.is_some(), "continuation must advance");
                    }
                    assert_eq!(
                        observed,
                        scan_edges(map.facts(), target)
                            .iter()
                            .map(|row| row.facts_version)
                            .collect::<Vec<_>>(),
                        "exhaustion mismatch at seed={seed:#x}, {target:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn flat_and_map_separate_purl_and_local_forward_coordinates_with_equal_spelling() {
        let authority = PackageGraphSourceAuthority::Unattributed;
        let purl =
            PackageReference::parse("pkg:cargo/spelling-collision@1.0.0").expect("Purl source");
        let local = PackageReference::Local(
            ProductText::new(purl.as_str()).expect("same-spelling Local source"),
        );
        let purl_source = PackageGraphSourceKey::new(purl.clone(), authority);
        let local_source = PackageGraphSourceKey::new(local.clone(), authority);
        let (map, flat) = checked_pair(vec![
            known(purl_source.clone(), Vec::new()),
            known(local_source.clone(), Vec::new()),
        ]);

        assert_eq!(purl.as_str(), local.as_str());
        for (coordinate, expected_source) in [(&purl, &purl_source), (&local, &local_source)] {
            let expected = scan_dependencies_by_coordinate(map.facts(), coordinate);
            assert!(
                matches!(&expected, PackageDependencyLookup::Exact { source, .. } if *source == expected_source),
                "the independent typed oracle must select only {expected_source:?}"
            );
            assert_eq!(map.dependencies(coordinate), expected);
            assert_eq!(flat.dependencies(coordinate), expected);
        }
        assert_eq!(
            map.dependencies_for_source(&purl_source),
            flat.dependencies_for_source(&purl_source)
        );
        assert_eq!(
            map.dependencies_for_source(&local_source),
            flat.dependencies_for_source(&local_source)
        );
        assert!(map.dependencies_for_source(&purl_source).is_some());
        assert!(map.dependencies_for_source(&local_source).is_some());
    }

    #[test]
    fn flat_and_map_reverse_resolved_target_queries_do_not_alias_local_labels() {
        let facts = typed_identity_collision_facts();
        let (map, flat) = checked_pair(facts);
        let purl = PackageReference::parse("pkg:cargo/benchmark-kind-collision@1.0.0")
            .expect("PURL resolved target");
        let local = PackageReference::from_kind(crate::PackageReferenceKind::Local, purl.as_str())
            .expect("same-spelling local query");
        assert_eq!(purl.as_str(), local.as_str());

        for target in [&purl, &local] {
            let expected_sources = linear_dependent_sources(map.facts(), target);
            assert_eq!(map.dependent_sources(target), expected_sources);
            assert_eq!(flat.dependent_sources(target), expected_sources);
            let (expected_page, expected_more) = scan_page(map.facts(), target, None, 1);
            assert_eq!(
                map.dependent_edges_page(target, None, 1),
                (expected_page.clone(), expected_more)
            );
            assert_eq!(
                flat.dependent_edges_page(target, None, 1),
                (expected_page, expected_more)
            );
        }
        assert_eq!(scan_edges(map.facts(), &purl).len(), 1);
        assert!(scan_edges(map.facts(), &local).is_empty());
    }

    #[test]
    fn flat_and_map_keep_authority_fanout_untruncated_at_lookup_boundary() {
        let coordinate = "pkg:cargo/fanout@1.0.0";
        let mut facts = Vec::new();
        for authority_index in 0..65_u8 {
            let key = source(coordinate, registry_authority(authority_index));
            facts.push(known(key, Vec::new()));
        }
        let (map, flat) = checked_pair(facts);
        let package = PackageReference::parse(coordinate).expect("fanout package");
        let map_result = map.dependencies(&package);
        assert_eq!(map_result, flat.dependencies(&package));
        assert!(matches!(map_result, PackageDependencyLookup::Ambiguous(keys) if keys.len() == 65));
    }

    #[test]
    fn flat_and_map_merge_interleaved_unresolved_and_resolved_edge_id_streams() {
        let checked = interleaved_unresolved_resolved_facts();
        let (map, flat) = checked_pair(checked.facts().to_vec());
        let target = PackageReference::parse("pkg:cargo/interleaved-target@1.0.0").expect("target");
        let DependencyFacts::Known(rows) = &map.facts()[0].1 else {
            unreachable!("interleaved fixture remains known");
        };
        let kinds = rows
            .iter()
            .map(|row| row.target.resolved.is_some())
            .collect::<Vec<_>>();
        assert!(
            kinds.windows(2).filter(|pair| pair[0] != pair[1]).count() >= 6,
            "fixture must interleave both independently sorted posting streams"
        );
        for limit in [1, 2, 5, 128] {
            let mut after = None;
            let mut observed = Vec::new();
            loop {
                let expected = scan_page(map.facts(), &target, after, limit);
                let map_page = map.dependent_edges_page(&target, after, limit);
                let flat_page = flat.dependent_edges_page(&target, after, limit);
                assert_eq!(map_page, expected);
                assert_eq!(flat_page, expected);
                observed.extend(expected.0.iter().map(|row| row.facts_version));
                if !expected.1 {
                    break;
                }
                after = expected.0.last().map(|row| row.facts_version);
                assert!(after.is_some(), "continuation must advance");
            }
            assert_eq!(
                observed,
                scan_edges(map.facts(), &target)
                    .iter()
                    .map(|row| row.facts_version)
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn maximum_checked_row_count_uses_the_last_u16_ordinal_without_truncation() {
        let source = source("pkg:cargo/maximum-rows@1.0.0", registry_authority(0x61));
        let rows = (0..MAX_PACKAGE_GRAPH_ROWS)
            .map(|row_index| {
                row(
                    &source,
                    RegistryEcosystem::Cargo,
                    "crowded-target",
                    None,
                    DependencyScope::Runtime,
                    u64::try_from(row_index).expect("row ordinal"),
                )
            })
            .collect::<Vec<_>>();
        let checked =
            CheckedPackageGraphFacts::new_with_limits(vec![known(source, rows)], generous_limits())
                .expect("maximum permitted rows");
        let graph = FlatCheckedGraph::from_checked(checked).expect("u16 row postings");
        assert_eq!(graph.reverse_posting_count(), MAX_PACKAGE_GRAPH_ROWS);
        assert!(graph.reverse_posting_capacity() >= MAX_PACKAGE_GRAPH_ROWS);
        assert_eq!(
            graph.posting_buffer_requested_bytes(),
            graph
                .reverse_posting_capacity()
                .saturating_mul(std::mem::size_of::<ReversePosting>())
        );
        let target =
            PackageReference::parse("pkg:cargo/crowded-target@1.0.0").expect("crowded target");
        let (first, more) = graph.dependent_edges_page(&target, None, 128);
        assert_eq!(first.len(), 128);
        assert!(more);
        let all = scan_edges(graph.facts(), &target);
        assert_eq!(all.len(), MAX_PACKAGE_GRAPH_ROWS);
        let mut after = None;
        let mut observed = Vec::new();
        loop {
            let (page, more) = graph.dependent_edges_page(&target, after, 128);
            observed.extend(page.iter().map(|row| row.facts_version));
            if !more {
                break;
            }
            after = page.last().map(|row| row.facts_version);
            assert!(after.is_some(), "a continuation must advance");
        }
        assert_eq!(
            observed,
            all.iter().map(|row| row.facts_version).collect::<Vec<_>>()
        );
    }

    #[test]
    fn malformed_checked_facts_and_fact_byte_admission_fail_before_flat_indexing() {
        let key = source("pkg:cargo/corrupt@1.0.0", registry_authority(0x71));
        let mut malformed = row(
            &key,
            RegistryEcosystem::Cargo,
            "target",
            None,
            DependencyScope::Runtime,
            1,
        );
        malformed.facts_version[0] ^= 1;
        assert_eq!(
            CheckedPackageGraphFacts::new(vec![known(key.clone(), vec![malformed])]).unwrap_err(),
            ProductAdmissionError::DependencyShape
        );

        let valid = vec![known(
            key,
            vec![row(
                &source("pkg:cargo/corrupt@1.0.0", registry_authority(0x71)),
                RegistryEcosystem::Cargo,
                "target",
                None,
                DependencyScope::Runtime,
                1,
            )],
        )];
        let exact_fact_bytes = valid
            .iter()
            .map(|(source, state)| {
                let row_bytes = match state {
                    DependencyFacts::Known(rows) => rows
                        .iter()
                        .map(|row| {
                            std::mem::size_of::<PackageDependencyRecord>()
                                + row.source.as_str().len()
                                + row.target.name.as_str().len()
                                + row.target.requirement.as_str().len()
                                + row
                                    .target
                                    .resolved
                                    .as_ref()
                                    .map_or(0, |resolved| resolved.as_str().len())
                        })
                        .sum::<usize>(),
                    DependencyFacts::Unknown(reason) | DependencyFacts::Unavailable(reason) => {
                        reason.as_str().len()
                    }
                };
                std::mem::size_of::<PackageDependencySourceFacts>()
                    + source.coordinate.as_str().len()
                    + row_bytes
            })
            .sum::<usize>();
        let mut limits = generous_limits();
        limits.max_sources = 1;
        limits.max_total_rows = 1;
        limits.max_reverse_edges = 1;
        limits.max_fact_bytes = exact_fact_bytes.saturating_sub(1);
        assert_eq!(
            FlatCheckedGraph::from_borrowed_facts(valid.iter(), limits).unwrap_err(),
            FlatBuildError::Admission(PackageGraphAdmissionError::FactByteLimit {
                observed: exact_fact_bytes,
                maximum: exact_fact_bytes - 1,
            })
        );
    }

    #[test]
    fn typed_variant_order_is_canonical_and_witness_pairs_stay_aligned() {
        let purl = PackageReference::parse("pkg:cargo/tie@1.0.0").expect("purl");
        let local = PackageReference::Local(ProductText::new(purl.as_str()).expect("local"));
        let authority = PackageGraphSourceAuthority::Unattributed;
        let purl_key = PackageGraphSourceKey::new(purl.clone(), authority);
        let local_key = PackageGraphSourceKey::new(local.clone(), authority);
        assert_eq!(compare_source_order(&purl_key, &local_key), Ordering::Less);

        let left = CheckedPackageGraphFacts::new(vec![
            known(purl_key.clone(), Vec::new()),
            known(local_key.clone(), Vec::new()),
        ])
        .expect("Purl then Local");
        let right = CheckedPackageGraphFacts::new(vec![
            known(local_key, Vec::new()),
            known(purl_key, Vec::new()),
        ])
        .expect("Local then Purl");
        assert_eq!(left.witness(), right.witness());
        assert_eq!(left.facts(), right.facts());
        assert_eq!(left.source_witnesses(), right.source_witnesses());
        assert_eq!(
            left.facts()[0].0.coordinate.kind(),
            crate::PackageReferenceKind::Purl
        );
        assert_eq!(
            left.facts()[1].0.coordinate.kind(),
            crate::PackageReferenceKind::Local
        );
        for checked in [&left, &right] {
            assert_eq!(checked.source_witnesses().len(), checked.facts().len());
            for (index, (source, state)) in checked.facts().iter().enumerate() {
                assert_eq!(
                    checked.source_witnesses()[index],
                    package_dependency_source_facts_witness_canonical(source, state),
                    "source witness remains paired with the exact typed key"
                );
            }
        }
        assert!(
            left.facts()
                .iter()
                .any(|(source, _)| { matches!(source.coordinate, PackageReference::Local(_)) })
        );
        assert!(
            left.facts()
                .iter()
                .any(|(source, _)| { matches!(source.coordinate, PackageReference::Purl(_)) })
        );
    }

    #[test]
    fn empty_graph_and_unknown_only_graph_preserve_coverage_states() {
        let limits = generous_limits();
        let empty_checked = CheckedPackageGraphFacts::new_with_limits(Vec::new(), limits)
            .expect("empty checked graph");
        let empty_map =
            IndexedCheckedPackageGraph::from_checked_facts(empty_checked.clone(), limits)
                .expect("empty map graph");
        let empty_flat = FlatCheckedGraph::from_checked(empty_checked).expect("empty flat graph");
        let target = PackageReference::parse("pkg:cargo/nothing@1.0.0").expect("target");
        assert_eq!(
            empty_map.dependent_sources(&target),
            empty_flat.dependent_sources(&target)
        );
        assert_eq!(
            empty_map.dependent_edges_page(&target, None, 1),
            empty_flat.dependent_edges_page(&target, None, 1)
        );
        assert_eq!(empty_map.reverse_coverage(), empty_flat.reverse_coverage());

        let unknown = source("pkg:cargo/only-unknown@1.0.0", registry_authority(0x72));
        let checked = CheckedPackageGraphFacts::new(vec![(
            unknown,
            DependencyFacts::Unknown(ProductText::new("metadata absent").expect("reason")),
        )])
        .expect("unknown graph");
        let map = IndexedCheckedPackageGraph::from_checked_facts(checked.clone(), limits)
            .expect("map unknown graph");
        let flat = FlatCheckedGraph::from_checked(checked).expect("flat unknown graph");
        assert_eq!(map.reverse_coverage(), flat.reverse_coverage());
        assert_eq!(
            map.dependent_sources(&target),
            flat.dependent_sources(&target)
        );
    }
}

#[cfg(test)]
#[path = "benchmark.rs"]
mod benchmark;
