use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use serde::{Deserialize, Serialize};

/// Version of the durable advisory object schema.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[repr(u16)]
pub enum AdvisorySchema {
    /// First versioned object format.
    V1 = 1,
}

/// Authority which assigned the native advisory identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdvisorySource {
    /// Open Source Vulnerabilities database.
    Osv,
    /// `RustSec` advisory database.
    RustSec,
    /// GitHub Security Advisories.
    Ghsa,
}

/// A source-qualified identifier.  The source is never inferred from an alias.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct NativeAdvisoryId {
    /// Issuing authority.
    pub source: AdvisorySource,
    /// Native identifier assigned by that authority.
    pub id: String,
}

impl NativeAdvisoryId {
    pub(crate) fn new(source: AdvisorySource, id: impl Into<String>) -> Result<Self, &'static str> {
        let id = id.into();
        if id.is_empty() || id.len() > 256 || id.bytes().any(|b| b.is_ascii_control()) {
            return Err("invalid advisory identifier");
        }
        Ok(Self { source, id })
    }
}

/// Canonical identity after aliases have been admitted into the conflict graph.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct CanonicalAdvisoryId(pub String);

/// Stable advisory identity used by the journal.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct AdvisoryKey {
    /// Canonical coalesced identity.
    pub canonical: CanonicalAdvisoryId,
    /// Native identity which supplied the object.
    pub native: NativeAdvisoryId,
}

/// An alias edge claimed by one source.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Alias {
    /// Alias text, for example `CVE-2025-1234` or `GHSA-...`.
    pub value: String,
    /// Authority which published the edge.
    pub source: AdvisorySource,
}

/// A normalized package identity independent of a particular registry endpoint.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct PackageIdentity {
    /// Closed package ecosystem.
    pub ecosystem: String,
    /// Ecosystem-normalized package name.
    pub name: String,
    /// Canonical package URL when the source provided one.
    pub canonical_purl: Option<String>,
}

/// The range language used by an affected claim.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VersionSyntax {
    /// Cargo/npm/NuGet-compatible semantic version events.
    Semver,
    /// Python PEP 440.
    Pep440,
    /// Maven's tokenized version ordering.
    Maven,
    /// Go module semantic versions.
    Go,
    /// Conan version/range grammar.
    Conan,
    /// Source range cannot be proven by this build.
    Unsupported,
}

/// One boundary event in an OSV range.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct VersionEvent {
    /// Boundary operation.
    pub kind: VersionEventKind,
    /// Version text in the source grammar.
    pub version: String,
}

/// Meaning of a version boundary event.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionEventKind {
    /// Start of the affected interval.
    Introduced,
    /// End of the affected interval, exclusive.
    Fixed,
    /// End of the affected interval, inclusive.
    LastAffected,
    /// End of the affected interval, exclusive, from OSV's limit event.
    Limit,
}

/// A source-claimed affected range and exact-version set.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct AffectedRange {
    /// Package matched by this claim.
    pub package: PackageIdentity,
    /// Source-specific range semantics.
    pub matcher: VersionMatcher,
    /// Explicitly listed affected versions, retained even when a range exists.
    pub exact_versions: Box<[String]>,
}

/// Version matcher with explicit semantics per ecosystem.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum VersionMatcher {
    /// OSV event stream interpreted under one proven grammar.
    Events {
        /// Grammar of the event versions.
        syntax: VersionSyntax,
        /// Ordered events from the source.
        events: Box<[VersionEvent]>,
    },
    /// `RustSec`'s patched/unaffected sets.
    RustSec {
        /// Versions known to contain the fix.
        patched: Box<[String]>,
        /// Versions explicitly known to be unaffected.
        unaffected: Box<[String]>,
    },
    /// A range whose ecosystem semantics are deliberately unsupported.
    Unsupported {
        /// Original source syntax.
        raw: String,
        /// Stable reason for callers and telemetry.
        reason: String,
    },
}

/// Security category attached to an advisory claim.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdvisoryCategory {
    /// Generic vulnerability.
    Vulnerability,
    /// Malicious package or release.
    Malicious,
    /// Package is unmaintained.
    Unmaintained,
    /// Unsound API or implementation.
    Unsound,
}

/// Normalized severity level.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeverityLevel {
    /// No severity supplied.
    Unknown,
    /// CVSS low / source low.
    Low,
    /// CVSS medium / source moderate.
    Moderate,
    /// CVSS high.
    High,
    /// CVSS critical.
    Critical,
}

/// Severity plus source spelling and an optional normalized score in hundredths.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Severity {
    /// Normalized level used for policy ordering.
    pub level: SeverityLevel,
    /// Original vector or severity string.
    pub source: Option<String>,
    /// CVSS score multiplied by 100, when one was supplied.
    pub score_hundredths: Option<u16>,
}

/// URL reference carried by an advisory.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Reference {
    /// Reference URL.
    pub url: String,
    /// Optional source label.
    pub kind: Option<String>,
}

/// Explicit malware coverage statement.  Absence is a parse failure for GHSA global feeds.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum MalwareCoverage {
    /// The feed explicitly evaluates malware claims.
    Covered,
    /// The feed explicitly does not evaluate malware claims.
    NotCovered,
}

/// Provenance for one durable advisory object.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Evidence {
    /// Source feed snapshot identity, usually `ETag` or content digest.
    pub snapshot: Option<String>,
    /// Time at which the source object was observed locally.
    pub observed_at: u64,
    /// Source-native verification label.
    pub verification: EvidenceKind,
}

/// How a source object was verified before admission.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceKind {
    /// Authenticated HTTPS response with a verified content digest.
    AuthenticatedDigest,
    /// Local signed or pinned database object.
    SignedDatabase,
    /// Parsed but not cryptographically authenticated.
    ParsedOnly,
}

/// Freshness state associated with an advisory frontier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FreshnessState {
    /// Feed was fetched during this process run.
    Fresh,
    /// Feed was validated by a 304 response.
    NotModified,
    /// Feed is older than the policy allows.
    Stale,
    /// No source frontier has ever been observed.
    Unknown,
}

/// User-visible security classification.  Registry yanks are deliberately separate from
/// advisory vulnerability claims.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdvisoryStatus {
    /// Publisher removed the release from selection.
    Yanked,
    /// Publisher retained the release but hides it from listings.
    Unlisted,
    /// One or more vulnerability claims match.
    Vulnerable,
    /// A malware claim matches.
    Malicious,
    /// Source marks the package unmaintained.
    Unmaintained,
    /// Source marks the package unsound.
    Unsound,
    /// Advisory was withdrawn by its issuing authority.
    Withdrawn,
    /// No source fully covers this package or ecosystem.
    UnknownCoverage,
    /// Source data has exceeded freshness policy.
    Stale,
}

/// One fully admitted, versioned advisory object.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct Advisory {
    /// Durable object schema.
    pub schema: AdvisorySchema,
    /// Coalesced identity.
    pub key: AdvisoryKey,
    /// All source aliases retained as claims.
    pub aliases: Box<[Alias]>,
    /// Affected package/range claims.
    pub affected: Box<[AffectedRange]>,
    /// Severity claim.
    pub severity: Severity,
    /// Security categories.
    pub categories: Box<[AdvisoryCategory]>,
    /// Publication timestamp as source RFC3339 text.
    pub published: Option<String>,
    /// Last modification timestamp as source RFC3339 text.
    pub modified: Option<String>,
    /// Withdrawal timestamp, if any.
    pub withdrawn: Option<String>,
    /// Human/source references.
    pub references: Box<[Reference]>,
    /// Explicit malware coverage.
    pub malware: MalwareCoverage,
    /// Freshness and verification evidence.
    pub evidence: Evidence,
}

impl Advisory {
    /// Returns true if the object is withdrawn.
    #[must_use]
    pub const fn is_withdrawn(&self) -> bool {
        self.withdrawn.is_some()
    }

    /// Computes categories which apply to a matching release.
    #[must_use]
    pub fn statuses(&self) -> Box<[AdvisoryStatus]> {
        let mut out = BTreeSet::new();
        if self.is_withdrawn() {
            out.insert(AdvisoryStatus::Withdrawn);
        }
        for category in &self.categories {
            out.insert(match category {
                AdvisoryCategory::Vulnerability => AdvisoryStatus::Vulnerable,
                AdvisoryCategory::Malicious => AdvisoryStatus::Malicious,
                AdvisoryCategory::Unmaintained => AdvisoryStatus::Unmaintained,
                AdvisoryCategory::Unsound => AdvisoryStatus::Unsound,
            });
        }
        out.into_iter().collect()
    }
}

/// A source-independent alias graph.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AliasGraph {
    parent: BTreeMap<String, String>,
    package_claims: BTreeMap<String, BTreeMap<String, BTreeSet<PackageIdentity>>>,
    range_claims: BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
    withdrawal_claims: BTreeMap<String, BTreeMap<String, Option<String>>>,
}

/// Conflict preventing silent alias coalescing.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AliasGraphError {
    /// One alias is connected to two unrelated canonical roots.
    AliasConflict {
        /// Alias whose identity was claimed by both roots.
        alias: String,
        /// First canonical root.
        left: String,
        /// Second canonical root.
        right: String,
    },
    /// Sources disagree about the package identity.
    PackageConflict {
        /// Canonical advisory root carrying the incompatible claims.
        canonical: String,
    },
    /// Sources disagree about the affected range.
    RangeConflict {
        /// Canonical advisory root carrying the incompatible claims.
        canonical: String,
    },
    /// Sources disagree about withdrawal state.
    WithdrawalConflict {
        /// Canonical advisory root carrying the incompatible claims.
        canonical: String,
    },
}

impl fmt::Display for AliasGraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AliasConflict { alias, .. } => write!(f, "conflicting alias claim: {alias}"),
            Self::PackageConflict { canonical } => {
                write!(f, "conflicting package claims for {canonical}")
            }
            Self::RangeConflict { canonical } => {
                write!(f, "conflicting range claims for {canonical}")
            }
            Self::WithdrawalConflict { canonical } => {
                write!(f, "conflicting withdrawal claims for {canonical}")
            }
        }
    }
}

impl std::error::Error for AliasGraphError {}

impl AliasGraph {
    /// Admits all aliases and semantic claims from one advisory.
    ///
    /// # Errors
    ///
    /// Returns a conflict when aliases point at unrelated roots or source claims disagree. The
    /// graph is unchanged on error.
    pub fn admit(&mut self, advisory: &Advisory) -> Result<CanonicalAdvisoryId, AliasGraphError> {
        // Admission is transactional even when the graph is used directly (the journal also
        // applies its own copy-on-write transaction).  This prevents a rejected package/range
        // claim from leaving aliases attached to a root that was never actually admitted.
        let mut candidate = self.clone();
        let canonical = candidate.admit_mut(advisory)?;
        *self = candidate;
        Ok(canonical)
    }

    fn admit_mut(&mut self, advisory: &Advisory) -> Result<CanonicalAdvisoryId, AliasGraphError> {
        // Native IDs already carry source namespaces in all supported feeds (OSV-, GHSA-,
        // RUSTSEC-, CVE-).  Retaining the source-native spelling makes canonical IDs stable and
        // useful to product URLs while the typed `NativeAdvisoryId` keeps the issuing authority
        // explicit for callers that need to disambiguate a non-standard feed.
        let own = advisory.key.native.id.clone();
        let aliases: Vec<String> = advisory
            .aliases
            .iter()
            .map(|a| a.value.clone())
            .chain(std::iter::once(advisory.key.native.id.clone()))
            .collect();
        let mut roots = BTreeSet::new();
        for alias in &aliases {
            if self.parent.contains_key(alias) {
                roots.insert(self.root(alias));
            }
        }
        if roots.len() > 1 {
            let mut roots = roots.into_iter();
            return Err(AliasGraphError::AliasConflict {
                alias: aliases[0].clone(),
                left: roots.next().unwrap_or_default(),
                right: roots.next().unwrap_or_default(),
            });
        }
        let canonical = roots.into_iter().next().unwrap_or_else(|| own.clone());
        for alias in &aliases {
            self.parent.insert(alias.clone(), canonical.clone());
        }
        self.parent.insert(own, canonical.clone());
        let source = format!(
            "{:?}:{}",
            advisory.key.native.source, advisory.key.native.id
        );
        let package_claims = self.package_claims.entry(canonical.clone()).or_default();
        let source_packages = package_claims.entry(source.clone()).or_default();
        source_packages.clear();
        for range in &advisory.affected {
            source_packages.insert(range.package.clone());
        }
        if package_claims
            .values()
            .flat_map(BTreeSet::iter)
            .collect::<BTreeSet<_>>()
            .len()
            > 1
        {
            return Err(AliasGraphError::PackageConflict { canonical });
        }
        let ranges = self.range_claims.entry(canonical.clone()).or_default();
        let source_ranges = ranges.entry(source.clone()).or_default();
        source_ranges.clear();
        for range in &advisory.affected {
            source_ranges.insert(format!("{:?}", range.matcher));
        }
        if ranges
            .values()
            .flat_map(BTreeSet::iter)
            .collect::<BTreeSet<_>>()
            .len()
            > 1
        {
            return Err(AliasGraphError::RangeConflict { canonical });
        }
        let withdrawals = self.withdrawal_claims.entry(canonical.clone()).or_default();
        withdrawals.insert(source, advisory.withdrawn.clone());
        if withdrawals.values().collect::<BTreeSet<_>>().len() > 1 {
            return Err(AliasGraphError::WithdrawalConflict { canonical });
        }
        Ok(CanonicalAdvisoryId(canonical))
    }

    /// Resolves an alias to its stable canonical root.
    #[must_use]
    pub fn resolve(&self, alias: &str) -> Option<CanonicalAdvisoryId> {
        self.parent
            .get(alias)
            .map(|value| CanonicalAdvisoryId(self.root(value)))
    }

    fn root(&self, value: &str) -> String {
        let mut current = value;
        let mut seen = BTreeSet::new();
        while let Some(next) = self.parent.get(current) {
            if !seen.insert(current) || next == current {
                break;
            }
            current = next;
        }
        current.to_owned()
    }
}
