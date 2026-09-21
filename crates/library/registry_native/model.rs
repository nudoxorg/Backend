use crate::{DependencyFacts, PackageDependencyRecord, RegistryEcosystem};
use serde::{Deserialize, Serialize};

/// One versioned native metadata snapshot attached to an immutable release.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryNativeMetadata {
    /// DTO schema version.
    pub version: u16,
    /// Whether the selected feed recorded native details.
    pub availability: RegistryNativeAvailability,
    /// Authenticated source frontier for the details.
    pub provenance: RegistryNativeProvenance,
    /// Ecosystem-specific details behind one closed vocabulary.
    pub details: RegistryNativeDetails,
}

/// Typed availability for a native metadata observation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "reason", rename_all = "kebab-case")]
pub enum RegistryNativeAvailability {
    /// The source published and the adapter admitted the details.
    Recorded,
    /// The configured source does not publish this native fact.
    NotRecorded(String),
}

/// Typed provenance for one native metadata snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum RegistryNativeProvenance {
    /// Digest of the bounded source documents used to construct the details.
    SourceDigest([u8; 32]),
    /// The source did not publish a native metadata document.
    NotRecorded(String),
}

/// One optional observation whose absence remains explicit and bounded.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "kebab-case")]
pub enum RegistryNativeObservation<T> {
    /// The source published a value and it passed admission.
    Recorded(T),
    /// The source does not publish this value.
    NotRecorded(String),
    /// The source advertised the value but it could not be fetched.
    Unavailable(String),
}

/// Closed native metadata details for the seven supported ecosystems.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case")]
pub enum RegistryNativeDetails {
    /// Cargo sparse index features and crate artifact claims.
    Cargo(RegistryCargoMetadata),
    /// npm packument tags, deprecation, and tarball claim.
    Npm(RegistryNpmMetadata),
    /// PyPI's complete file set and interpreter/yank facts.
    Pypi(RegistryPypiMetadata),
    /// Maven sidecar evidence and POM observation.
    Maven(RegistryMavenMetadata),
    /// NuGet vulnerability and dependency observations.
    Nuget(RegistryNugetMetadata),
    /// Go module source and retract facts.
    Golang(RegistryGoMetadata),
    /// Conan source availability at the selected recipe revision.
    Cpp(RegistryConanMetadata),
    /// A canonical feed or other source with no native grammar.
    Unavailable {
        /// Ecosystem that was requested.
        ecosystem: RegistryEcosystem,
        /// Bounded reason supplied by the adapter.
        reason: String,
    },
}

/// Native file classification shared by all ecosystem file claims.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryNativeArtifactKind {
    /// Cargo package archive.
    CargoCrate,
    /// npm package tarball.
    NpmTarball,
    /// Python source distribution.
    PythonSdist,
    /// Python binary wheel.
    PythonWheel,
    /// Python artifact signature.
    PythonSignature,
    /// Maven binary JAR.
    MavenJar,
    /// Maven source archive.
    MavenSources,
    /// Maven POM document.
    MavenPom,
    /// Maven signature sidecar.
    MavenSignature,
    /// Maven checksum sidecar.
    MavenChecksum,
    /// NuGet package archive.
    NugetPackage,
    /// Go module source archive.
    GoSource,
    /// Conan source archive.
    ConanSource,
    /// Conan recipe contents.
    ConanRecipe,
    /// Artifact type outside the closed native vocabulary.
    Other,
}

/// A bounded checksum claim retained as typed bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryNativeChecksum {
    /// Closed checksum algorithm.
    pub algorithm: RegistryNativeChecksumAlgorithm,
    /// Raw digest bytes in the algorithm's native width.
    pub digest: Box<[u8]>,
}

/// Native checksum algorithms emitted by registry protocols.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryNativeChecksumAlgorithm {
    /// SHA-1 digest.
    Sha1,
    /// SHA-256 digest.
    Sha256,
    /// SHA-512 digest.
    Sha512,
    /// Go module checksum representation.
    GoModule,
}

/// One bounded artifact claim retained from a native feed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryNativeArtifact {
    /// Registry-provided artifact filename.
    ///
    /// Artifacts are admitted in lexical filename order.
    pub filename: String,
    /// Fetch URL for the artifact.
    pub url: String,
    /// Content digest and algorithm.
    pub checksum: RegistryNativeChecksum,
    /// Native artifact classification.
    pub kind: RegistryNativeArtifactKind,
    /// Python interpreter constraint, when published.
    pub requires_python: Option<String>,
    /// Registry-provided artifact size in bytes.
    pub size: Option<u64>,
    /// Yank state when the source grammar publishes one.
    pub yanked: Option<bool>,
    /// Registry-provided explanation for a yanked artifact.
    pub yanked_reason: Option<String>,
}

/// One Cargo feature and its canonical member list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryNativeFeature {
    /// Cargo feature name.
    pub name: String,
    /// Canonically ordered feature members.
    pub members: Box<[String]>,
}

/// One npm dist-tag assignment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryNativeDistTag {
    /// npm tag name.
    pub name: String,
    /// Version selected by the tag.
    pub version: String,
}

/// Cargo native details.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryCargoMetadata {
    /// Cargo crate archive claims.
    pub artifacts: Box<[RegistryNativeArtifact]>,
    /// Canonically ordered Cargo features.
    pub features: Box<[RegistryNativeFeature]>,
}

/// npm native details.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryNpmMetadata {
    /// npm tarball claims.
    pub artifacts: Box<[RegistryNativeArtifact]>,
    /// Canonically ordered dist-tags.
    pub dist_tags: Box<[RegistryNativeDistTag]>,
    /// npm deprecation notice, when published.
    pub deprecation: Option<String>,
}

/// PyPI native details.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryPypiMetadata {
    /// PyPI file claims.
    pub artifacts: Box<[RegistryNativeArtifact]>,
    /// Package-level Python interpreter constraint.
    pub requires_python: Option<String>,
    /// Reason the release remains standing, when supplied.
    pub standing_reason: Option<String>,
}

/// Maven sidecar checksum claim.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryMavenChecksum {
    /// Sidecar URL containing the checksum.
    pub url: String,
    /// Parsed checksum claim.
    pub checksum: RegistryNativeChecksum,
}

/// Maven signature, POM, or other bounded sidecar evidence claim.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryNativeEvidenceClaim {
    /// Source document URL.
    pub url: String,
    /// Digest of the bounded source bytes.
    pub digest: [u8; 32],
    /// Number of source bytes retained or verified.
    pub bytes: u64,
}

/// Maven native details.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryMavenMetadata {
    /// Maven artifact claims.
    pub artifacts: Box<[RegistryNativeArtifact]>,
    /// Checksum sidecar observation.
    pub checksum: RegistryNativeObservation<RegistryMavenChecksum>,
    /// Signature sidecar observation.
    pub signature: RegistryNativeObservation<RegistryNativeEvidenceClaim>,
    /// POM document observation.
    pub pom: RegistryNativeObservation<RegistryNativeEvidenceClaim>,
    /// Dependency graph observation from the POM.
    pub dependencies: RegistryNativeObservation<DependencyFacts<Box<[PackageDependencyRecord]>>>,
}

/// One NuGet vulnerability observation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryNativeVulnerability {
    /// Advisory URL, when the feed publishes one.
    pub advisory_url: Option<String>,
    /// Bounded severity level from the feed.
    pub severity: u8,
}

/// NuGet native details.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryNugetMetadata {
    /// NuGet package claims.
    pub artifacts: Box<[RegistryNativeArtifact]>,
    /// Vulnerability observations.
    pub vulnerabilities: RegistryNativeObservation<Box<[RegistryNativeVulnerability]>>,
    /// Dependency graph observation.
    pub dependencies: RegistryNativeObservation<DependencyFacts<Box<[PackageDependencyRecord]>>>,
    /// NuGet deprecation notice, when published.
    pub deprecation: Option<String>,
}

/// One Go retract interval.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryGoRetract {
    /// Lower bound of the retracted version interval.
    pub lower: String,
    /// Upper bound of the retracted version interval.
    pub upper: String,
}

/// Go source documents retained as claims rather than fabricated text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryGoSourceFacts {
    /// Go module path.
    pub module: String,
    /// Go module version.
    pub version: String,
    /// `go.mod` info document claim.
    pub info: RegistryNativeEvidenceClaim,
    /// Module file document claim.
    pub module_file: RegistryNativeEvidenceClaim,
    /// Checksum document claim.
    pub checksum: RegistryNativeEvidenceClaim,
}

/// Go native details.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryGoMetadata {
    /// Go source archive claims.
    pub artifacts: Box<[RegistryNativeArtifact]>,
    /// Canonically ordered retract intervals.
    pub retracts: Box<[RegistryGoRetract]>,
    /// Go source document observation.
    pub source: RegistryNativeObservation<RegistryGoSourceFacts>,
}

/// Conan source availability at a selected recipe revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryConanSourceAvailability {
    /// A source archive is available.
    Archive,
    /// Only the recipe is available.
    RecipeOnly,
    /// The source was unavailable.
    Unavailable,
}

/// Conan native details.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryConanMetadata {
    /// Conan artifact claims.
    pub artifacts: Box<[RegistryNativeArtifact]>,
    /// Source availability classification.
    pub source: RegistryConanSourceAvailability,
    /// Source URL, when published.
    pub source_url: Option<String>,
}
