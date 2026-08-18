//! The ingestor → writer protocol (INDEX-PLAN §8.1) plus the two registryless
//! additions (REGISTRYLESS-PLAN §5).
//!
//! [`CatalogOp`] is the typed, serde-wire vocabulary the ingestor emits and the
//! single writer applies. A batch of ops is applied in **one** transaction
//! (INDEX-PLAN ID-3), including the outbox rows that fan out to projections.
//! Nothing here touches the engine; these are pure data.

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use heart::Language;

use crate::enums::{
    AliasConfidence, EdgeKind, EdgeSource, IrStatus, LineageEvidence, LineageRelation,
    ListingStatus, SourceKind,
};
use crate::ids::{ChannelTip, GenerationStamp, ObjectPackHash, PackageId, PackageStemId};

// ─────────────────────────────────────────────────────────────────────────────
// Wire sub-structures
// ─────────────────────────────────────────────────────────────────────────────

/// The published coordinates that name a package stem on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageStemWire {
    /// The stem identity.
    pub stem_id: PackageStemId,
    /// The ecosystem/registry world.
    pub ecosystem: Language,
    /// Structured-name canonical wire (purl-like).
    pub name_struct: String,
    /// Normalized name used for uniqueness + lookup.
    pub name_canonical: String,
    /// Name exactly as published.
    pub name_original: String,
}

/// The coordinates that name a specific published version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionCoordinates {
    /// The version instance id.
    pub version_id: PackageId,
    /// The owning stem.
    pub stem_id: PackageStemId,
    /// Canonical version string under the ecosystem grammar.
    pub version_canonical: String,
    /// Version string exactly as published.
    pub version_original: String,
}

/// One dependency edge as the extractor saw it (EDB; REGISTRYLESS RL-5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeWire {
    /// The literal requirement's ecosystem.
    pub dep_ecosystem: Language,
    /// The literal requirement token (`zlib`, `ZLIB`, `serde`, …).
    pub dep_name_canonical: String,
    /// The version requirement expression.
    pub requirement: String,
    /// The mechanism the edge came through.
    pub kind: EdgeKind,
    /// The provenance of this edge fact.
    pub source: EdgeSource,
    /// Resolved stem, when the extractor already knows it (usually filled later
    /// by the pure resolution pass, so normally `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_stem: Option<PackageStemId>,
}

/// Facet payload for a version (keywords, quality, extras JSON).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetWire {
    /// Space/comma-joined keyword string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keywords: Option<String>,
    /// Quality score in parts-per-million.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_ppm: Option<i64>,
    /// Free-form extras as a JSON string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extras: Option<String>,
}

/// Source acquisition provenance on the wire (INDEX-PLAN §6.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAcquisitionWire {
    /// How source bytes were obtained.
    pub source_kind: SourceKind,
    /// The sealed source ObjectPack, once acquired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_pack: Option<ObjectPackHash>,
    /// Git ref (tag/commit) used when `source_kind = git`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_rev: Option<String>,
    /// The published artifact checksum, always kept for honesty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_checksum: Option<String>,
    /// Ephemeral fetch locator for a reconstructed package (never opened live
    /// after the pack exists).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_package_uri: Option<String>,
}

/// The complete version payload carried by a reconciliation delta.
///
/// Keeping the payload separate from [`CatalogOp::UpsertVersion`] makes the
/// add/change/remove classification explicit on the wire while preserving the
/// existing producer-facing upsert operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionRecordWire {
    /// The version identity and published coordinates.
    pub coordinates: VersionCoordinates,
    /// Publish instant.
    pub published_at: Option<UnixMs>,
    /// Toolchain reference.
    pub toolchain: Option<ToolchainRef>,
    /// SPDX license expression.
    pub license: Option<String>,
    /// Dependency edges.
    pub edges: Vec<EdgeWire>,
    /// Search facets.
    pub facets: FacetWire,
    /// Source acquisition provenance.
    pub source: Option<SourceAcquisitionWire>,
}

/// One typed version reconciliation change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VersionDelta {
    /// A version absent from the durable catalog was observed upstream.
    Added { version: VersionRecordWire },
    /// An existing version's payload changed upstream.
    Changed { version: VersionRecordWire },
    /// A previously observed version disappeared upstream.
    Removed {
        /// The owning stem.
        stem_id: PackageStemId,
        /// The removed version identity.
        version_id: PackageId,
    },
}

/// Upstream repository facts (INDEX-PLAN §8 `repo_facts`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoFactsWire {
    /// Star count, when the host reports one.
    pub stars: Option<i64>,
    /// Last upstream activity instant.
    pub last_activity_at: Option<i64>,
    /// Whether the repository is archived.
    pub archived: bool,
    /// The default branch name.
    pub default_branch: Option<String>,
    /// When these facts were fetched.
    pub fetched_at: i64,
}

/// A security advisory on the wire (INDEX-PLAN §8 `advisories`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvisoryWire {
    /// Advisory identity.
    pub id: crate::ids::AdvisoryId,
    /// Affected stem, when known.
    pub stem_id: Option<PackageStemId>,
    /// Affected version range expression.
    pub version_range: Option<String>,
    /// Severity token.
    pub severity: Option<String>,
    /// Human summary.
    pub summary: Option<String>,
    /// Advisory URL.
    pub url: Option<String>,
    /// Bitemporal validity window start.
    pub valid_from: i64,
    /// Bitemporal validity window end.
    pub valid_to: Option<i64>,
    /// When the advisory was recorded.
    pub recorded_at: i64,
}

/// A toolchain reference on the wire (opaque digest/ref string).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolchainRef(pub SmolStr);

/// A git revision (tag/commit) on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRev(pub SmolStr);

/// A generation stamp reference on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenStampWire {
    /// The generation stamp.
    pub gen_stamp: GenerationStamp,
    /// The IR channel tip, once sealed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_tip: Option<ChannelTip>,
}

/// Unix milliseconds on the wire.
pub type UnixMs = i64;

// ─────────────────────────────────────────────────────────────────────────────
// CatalogOp
// ─────────────────────────────────────────────────────────────────────────────

/// One typed catalog mutation (INDEX-PLAN §8.1 + REGISTRYLESS §5). Applied by
/// the single writer; a batch is one transaction (ID-3).
///
/// `Eq` is intentionally omitted: [`CatalogOp::UpsertLineage`] carries an
/// `Option<f32>` overlap ratio, and `f32` is only `PartialEq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CatalogOp {
    /// Insert or update a package stem.
    UpsertPackage {
        /// The stem coordinates.
        stem: PackageStemWire,
        /// Upstream repository URL.
        repo_url: Option<String>,
    },
    /// Insert or update a published version, with its edges, facets, source.
    UpsertVersion {
        /// The version coordinates.
        coordinates: VersionCoordinates,
        /// Publish instant.
        published_at: Option<UnixMs>,
        /// Toolchain reference.
        toolchain: Option<ToolchainRef>,
        /// SPDX license expression.
        license: Option<String>,
        /// Dependency edges (EDB).
        edges: Vec<EdgeWire>,
        /// Facet payload.
        facets: FacetWire,
        /// Source acquisition provenance.
        source: Option<SourceAcquisitionWire>,
    },
    /// Apply one add/change/remove version reconciliation delta.
    VersionDelta {
        /// The typed version change.
        delta: VersionDelta,
    },
    /// Replace repository facts for a stem.
    SetRepoFacts {
        /// The stem.
        stem: PackageStemId,
        /// The facts.
        facts: RepoFactsWire,
    },
    /// Append a listing lifecycle event (bitemporal).
    SetListing {
        /// The version.
        version: PackageId,
        /// The new listing status.
        status: ListingStatus,
        /// Validity window start.
        valid_from: UnixMs,
        /// Optional human reason.
        reason: Option<String>,
    },
    /// Insert or update an advisory.
    UpsertAdvisory {
        /// The advisory.
        advisory: AdvisoryWire,
    },
    /// Record that a stem's source moved to a new git rev (grit tick).
    SourceMoved {
        /// The stem.
        stem: PackageStemId,
        /// The new revision.
        rev: GitRev,
        /// When the move was observed.
        checked_at: UnixMs,
    },
    /// Set the IR pipeline status for a version (ID-15; always tracked).
    SetIrStatus {
        /// The version.
        version: PackageId,
        /// The IR status.
        status: IrStatus,
        /// The generation, when one exists.
        generation: Option<GenStampWire>,
    },
    /// Request a refresh of a stem (re-enumerate versions / re-fetch facts).
    Refresh {
        /// The stem.
        stem: PackageStemId,
    },

    // ── REGISTRYLESS-PLAN §5 additions ───────────────────────────────────────
    /// Insert or update a name alias → stem mapping (REGISTRYLESS RL-6).
    UpsertAlias {
        /// The alias's ecosystem.
        ecosystem: Language,
        /// The alias kind (`vcpkg_port`, `find_package`, …).
        kind: SmolStr,
        /// The normalized alias token.
        alias: SmolStr,
        /// The stem the alias resolves to.
        stem: PackageStemId,
        /// Confidence tier.
        confidence: AliasConfidence,
    },
    /// Insert or update a fork/mirror lineage edge (REGISTRYLESS RL-16).
    UpsertLineage {
        /// The fork/mirror stem.
        stem: PackageStemId,
        /// The relation to the target.
        relation: LineageRelation,
        /// The upstream/canonical stem.
        target: PackageStemId,
        /// Evidence class.
        evidence: LineageEvidence,
        /// Merge-base oid when evidence is git history.
        fork_point_rev: Option<GitRev>,
        /// Distinctive-file overlap when evidence is content overlap.
        overlap_ratio: Option<f32>,
        /// Confidence tier.
        confidence: AliasConfidence,
    },
}

impl CatalogOp {
    /// A short, stable tag for logging/metrics — never parsed.
    pub fn tag(&self) -> &'static str {
        match self {
            CatalogOp::UpsertPackage { .. } => "upsert_package",
            CatalogOp::UpsertVersion { .. } => "upsert_version",
            CatalogOp::VersionDelta { .. } => "version_delta",
            CatalogOp::SetRepoFacts { .. } => "set_repo_facts",
            CatalogOp::SetListing { .. } => "set_listing",
            CatalogOp::UpsertAdvisory { .. } => "upsert_advisory",
            CatalogOp::SourceMoved { .. } => "source_moved",
            CatalogOp::SetIrStatus { .. } => "set_ir_status",
            CatalogOp::Refresh { .. } => "refresh",
            CatalogOp::UpsertAlias { .. } => "upsert_alias",
            CatalogOp::UpsertLineage { .. } => "upsert_lineage",
        }
    }
}
