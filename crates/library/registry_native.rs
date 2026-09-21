//! Bounded, versioned native registry metadata shared by every product surface.
//!
//! The engine owns acquisition and journal admission, while this value owns
//! the vocabulary exposed to local-service, JSON, MCP, CLI, and desktop. A
//! native feed may omit a fact; omission is represented by a typed observation
//! instead of an empty string or a fabricated value.

use crate::{
    DependencyAuthority, DependencyEvidence, DependencyFacts, DependencyScope,
    PackageDependencyRecord, PackageDependencyTarget, PackageReference, ProductAdmissionError,
    ProductText, RegistryEcosystem,
};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Current native metadata DTO schema.
pub const REGISTRY_NATIVE_METADATA_VERSION: u16 = 1;
/// Maximum number of rows retained in one native metadata collection.
pub const MAX_REGISTRY_NATIVE_ROWS: usize = 4_096;
/// Maximum text extent retained in one metadata field.
pub const MAX_REGISTRY_NATIVE_TEXT_BYTES: usize = 4_096;
/// Maximum serialized metadata extent admitted on a product surface.
pub const MAX_REGISTRY_NATIVE_METADATA_BYTES: usize = 2 * 1024 * 1024;

const REGISTRY_NATIVE_CODEC_MAGIC: &[u8; 4] = b"RNMD";
const REGISTRY_NATIVE_CODEC_VERSION: u8 = 1;

/// Errors returned while decoding the canonical native metadata wire value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistryNativeMetadataCodecError {
    /// The value ended before a complete field could be read.
    Truncated,
    /// A length or collection count exceeded its fixed bound.
    Bounds,
    /// A text field was not valid UTF-8 or did not pass product admission.
    Text,
    /// A closed enum contained an unknown discriminant.
    Tag,
    /// The decoded DTO failed its semantic admission checks.
    Admission,
}

impl fmt::Display for RegistryNativeMetadataCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Truncated => "truncated registry native metadata",
            Self::Bounds => "bounded registry native metadata field exceeded",
            Self::Text => "invalid registry native metadata text",
            Self::Tag => "unknown registry native metadata tag",
            Self::Admission => "registry native metadata failed admission",
        })
    }
}

impl std::error::Error for RegistryNativeMetadataCodecError {}

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
    CargoCrate,
    NpmTarball,
    PythonSdist,
    PythonWheel,
    PythonSignature,
    MavenJar,
    MavenSources,
    MavenPom,
    MavenSignature,
    MavenChecksum,
    NugetPackage,
    GoSource,
    ConanSource,
    ConanRecipe,
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
    Sha1,
    Sha256,
    Sha512,
    GoModule,
}

/// One bounded artifact claim retained from a native feed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryNativeArtifact {
    pub filename: String,
    pub url: String,
    pub checksum: RegistryNativeChecksum,
    pub kind: RegistryNativeArtifactKind,
    pub requires_python: Option<String>,
    pub size: Option<u64>,
    /// Yank state when the source grammar publishes one.
    pub yanked: Option<bool>,
    pub yanked_reason: Option<String>,
}

/// One Cargo feature and its canonical member list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryNativeFeature {
    pub name: String,
    pub members: Box<[String]>,
}

/// One npm dist-tag assignment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryNativeDistTag {
    pub name: String,
    pub version: String,
}

/// Cargo native details.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryCargoMetadata {
    pub artifacts: Box<[RegistryNativeArtifact]>,
    pub features: Box<[RegistryNativeFeature]>,
}

/// npm native details.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryNpmMetadata {
    pub artifacts: Box<[RegistryNativeArtifact]>,
    pub dist_tags: Box<[RegistryNativeDistTag]>,
    pub deprecation: Option<String>,
}

/// PyPI native details.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryPypiMetadata {
    pub artifacts: Box<[RegistryNativeArtifact]>,
    pub requires_python: Option<String>,
    pub standing_reason: Option<String>,
}

/// Maven sidecar checksum claim.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryMavenChecksum {
    pub url: String,
    pub checksum: RegistryNativeChecksum,
}

/// Maven signature, POM, or other bounded sidecar evidence claim.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryNativeEvidenceClaim {
    pub url: String,
    pub digest: [u8; 32],
    pub bytes: u64,
}

/// Maven native details.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryMavenMetadata {
    pub artifacts: Box<[RegistryNativeArtifact]>,
    pub checksum: RegistryNativeObservation<RegistryMavenChecksum>,
    pub signature: RegistryNativeObservation<RegistryNativeEvidenceClaim>,
    pub pom: RegistryNativeObservation<RegistryNativeEvidenceClaim>,
    pub dependencies: RegistryNativeObservation<DependencyFacts<Box<[PackageDependencyRecord]>>>,
}

/// One NuGet vulnerability observation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryNativeVulnerability {
    pub advisory_url: Option<String>,
    pub severity: u8,
}

/// NuGet native details.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryNugetMetadata {
    pub artifacts: Box<[RegistryNativeArtifact]>,
    pub vulnerabilities: RegistryNativeObservation<Box<[RegistryNativeVulnerability]>>,
    pub dependencies: RegistryNativeObservation<DependencyFacts<Box<[PackageDependencyRecord]>>>,
    pub deprecation: Option<String>,
}

/// One Go retract interval.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryGoRetract {
    pub lower: String,
    pub upper: String,
}

/// Go source documents retained as claims rather than fabricated text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryGoSourceFacts {
    pub module: String,
    pub version: String,
    pub info: RegistryNativeEvidenceClaim,
    pub module_file: RegistryNativeEvidenceClaim,
    pub checksum: RegistryNativeEvidenceClaim,
}

/// Go native details.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryGoMetadata {
    pub artifacts: Box<[RegistryNativeArtifact]>,
    pub retracts: Box<[RegistryGoRetract]>,
    pub source: RegistryNativeObservation<RegistryGoSourceFacts>,
}

/// Conan source availability at a selected recipe revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryConanSourceAvailability {
    Archive,
    RecipeOnly,
    Unavailable,
}

/// Conan native details.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryConanMetadata {
    pub artifacts: Box<[RegistryNativeArtifact]>,
    pub source: RegistryConanSourceAvailability,
    pub source_url: Option<String>,
}

impl RegistryNativeMetadata {
    /// Creates a typed unavailable value for a feed without native facts.
    #[must_use]
    pub fn unavailable(ecosystem: RegistryEcosystem, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            version: REGISTRY_NATIVE_METADATA_VERSION,
            availability: RegistryNativeAvailability::NotRecorded(reason.clone()),
            provenance: RegistryNativeProvenance::NotRecorded(reason.clone()),
            details: RegistryNativeDetails::Unavailable { ecosystem, reason },
        }
    }

    /// Validates bounds, canonical ordering, and closed checksum widths.
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        if self.version != REGISTRY_NATIVE_METADATA_VERSION {
            return Err(ProductAdmissionError::NativeMetadata);
        }
        validate_availability(&self.availability)?;
        validate_provenance(&self.provenance)?;
        match (&self.availability, &self.details) {
            (RegistryNativeAvailability::Recorded, RegistryNativeDetails::Unavailable { .. })
            | (
                RegistryNativeAvailability::NotRecorded(_),
                RegistryNativeDetails::Cargo(_)
                | RegistryNativeDetails::Npm(_)
                | RegistryNativeDetails::Pypi(_)
                | RegistryNativeDetails::Maven(_)
                | RegistryNativeDetails::Nuget(_)
                | RegistryNativeDetails::Golang(_)
                | RegistryNativeDetails::Cpp(_),
            ) => return Err(ProductAdmissionError::NativeMetadata),
            _ => {}
        }
        match (&self.availability, &self.provenance) {
            (RegistryNativeAvailability::Recorded, RegistryNativeProvenance::NotRecorded(_))
            | (
                RegistryNativeAvailability::NotRecorded(_),
                RegistryNativeProvenance::SourceDigest(_),
            ) => return Err(ProductAdmissionError::NativeMetadata),
            _ => {}
        }
        validate_details(&self.details)?;
        let encoded = self.encode_canonical();
        if encoded.len() > MAX_REGISTRY_NATIVE_METADATA_BYTES {
            return Err(ProductAdmissionError::NativeMetadata);
        }
        Ok(())
    }

    /// Encodes the DTO using the explicit, versioned canonical journal grammar.
    ///
    /// The value is normally admitted before it reaches durable storage. This
    /// encoder itself is deliberately infallible so a journal implementation
    /// cannot panic while constructing a record; the reader and publication
    /// admission reject malformed or over-bound values.
    #[must_use]
    pub fn encode_canonical(&self) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(REGISTRY_NATIVE_CODEC_MAGIC);
        output.push(REGISTRY_NATIVE_CODEC_VERSION);
        put_u16(&mut output, self.version);
        write_availability(&mut output, &self.availability);
        write_provenance(&mut output, &self.provenance);
        write_details(&mut output, &self.details);
        output
    }

    /// Decodes and admits the explicit canonical journal grammar.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, RegistryNativeMetadataCodecError> {
        if bytes.len() > MAX_REGISTRY_NATIVE_METADATA_BYTES {
            return Err(RegistryNativeMetadataCodecError::Bounds);
        }
        let mut reader = CanonicalReader::new(bytes);
        if reader.take_exact(4)? != REGISTRY_NATIVE_CODEC_MAGIC {
            return Err(RegistryNativeMetadataCodecError::Tag);
        }
        if reader.take_u8()? != REGISTRY_NATIVE_CODEC_VERSION {
            return Err(RegistryNativeMetadataCodecError::Tag);
        }
        let value = RegistryNativeMetadata {
            version: reader.take_u16()?,
            availability: read_availability(&mut reader)?,
            provenance: read_provenance(&mut reader)?,
            details: read_details(&mut reader)?,
        };
        if !reader.is_empty() {
            return Err(RegistryNativeMetadataCodecError::Bounds);
        }
        value
            .admit()
            .map_err(|_| RegistryNativeMetadataCodecError::Admission)?;
        Ok(value)
    }

    /// Computes the stable identity of this admitted metadata DTO.
    pub fn identity(&self) -> Result<[u8; 32], ProductAdmissionError> {
        self.admit()?;
        let encoded = self.encode_canonical();
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.registry.native-metadata.v1\0");
        hasher.update(&encoded);
        Ok(*hasher.finalize().as_bytes())
    }
}

fn put_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn put_u32(output: &mut Vec<u8>, value: usize) {
    output.extend_from_slice(&u32::try_from(value).unwrap_or(u32::MAX).to_be_bytes());
}

fn put_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn put_text(output: &mut Vec<u8>, value: &str) {
    put_u32(output, value.len());
    output.extend_from_slice(value.as_bytes());
}

fn put_optional_text(output: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            output.push(1);
            put_text(output, value);
        }
        None => output.push(0),
    }
}

fn put_optional_bool(output: &mut Vec<u8>, value: Option<bool>) {
    match value {
        None => output.push(0),
        Some(false) => output.push(1),
        Some(true) => output.push(2),
    }
}

fn write_availability(output: &mut Vec<u8>, value: &RegistryNativeAvailability) {
    match value {
        RegistryNativeAvailability::Recorded => output.push(0),
        RegistryNativeAvailability::NotRecorded(reason) => {
            output.push(1);
            put_text(output, reason);
        }
    }
}

fn write_provenance(output: &mut Vec<u8>, value: &RegistryNativeProvenance) {
    match value {
        RegistryNativeProvenance::SourceDigest(digest) => {
            output.push(0);
            output.extend_from_slice(digest);
        }
        RegistryNativeProvenance::NotRecorded(reason) => {
            output.push(1);
            put_text(output, reason);
        }
    }
}

fn write_details(output: &mut Vec<u8>, value: &RegistryNativeDetails) {
    match value {
        RegistryNativeDetails::Cargo(value) => {
            output.push(0);
            write_artifacts(output, &value.artifacts);
            put_u32(output, value.features.len());
            for feature in &value.features {
                put_text(output, &feature.name);
                put_u32(output, feature.members.len());
                for member in &feature.members {
                    put_text(output, member);
                }
            }
        }
        RegistryNativeDetails::Npm(value) => {
            output.push(1);
            write_artifacts(output, &value.artifacts);
            put_u32(output, value.dist_tags.len());
            for tag in &value.dist_tags {
                put_text(output, &tag.name);
                put_text(output, &tag.version);
            }
            put_optional_text(output, value.deprecation.as_deref());
        }
        RegistryNativeDetails::Pypi(value) => {
            output.push(2);
            write_artifacts(output, &value.artifacts);
            put_optional_text(output, value.requires_python.as_deref());
            put_optional_text(output, value.standing_reason.as_deref());
        }
        RegistryNativeDetails::Maven(value) => {
            output.push(3);
            write_artifacts(output, &value.artifacts);
            write_observation(output, &value.checksum, write_maven_checksum);
            write_observation(output, &value.signature, write_evidence_claim);
            write_observation(output, &value.pom, write_evidence_claim);
            write_dependency_facts_observation(output, &value.dependencies);
        }
        RegistryNativeDetails::Nuget(value) => {
            output.push(4);
            write_artifacts(output, &value.artifacts);
            write_observation(output, &value.vulnerabilities, |output, rows| {
                put_u32(output, rows.len());
                for row in rows {
                    put_optional_text(output, row.advisory_url.as_deref());
                    output.push(row.severity);
                }
            });
            write_dependency_facts_observation(output, &value.dependencies);
            put_optional_text(output, value.deprecation.as_deref());
        }
        RegistryNativeDetails::Golang(value) => {
            output.push(5);
            write_artifacts(output, &value.artifacts);
            put_u32(output, value.retracts.len());
            for retract in &value.retracts {
                put_text(output, &retract.lower);
                put_text(output, &retract.upper);
            }
            write_observation(output, &value.source, write_go_source);
        }
        RegistryNativeDetails::Cpp(value) => {
            output.push(6);
            write_artifacts(output, &value.artifacts);
            output.push(match value.source {
                RegistryConanSourceAvailability::Archive => 0,
                RegistryConanSourceAvailability::RecipeOnly => 1,
                RegistryConanSourceAvailability::Unavailable => 2,
            });
            put_optional_text(output, value.source_url.as_deref());
        }
        RegistryNativeDetails::Unavailable { ecosystem, reason } => {
            output.push(7);
            output.push(*ecosystem as u8);
            put_text(output, reason);
        }
    }
}

fn write_artifacts(output: &mut Vec<u8>, values: &[RegistryNativeArtifact]) {
    put_u32(output, values.len());
    for value in values {
        put_text(output, &value.filename);
        put_text(output, &value.url);
        write_checksum(output, &value.checksum);
        output.push(value.kind as u8);
        put_optional_text(output, value.requires_python.as_deref());
        match value.size {
            Some(size) => {
                output.push(1);
                put_u64(output, size);
            }
            None => output.push(0),
        }
        put_optional_bool(output, value.yanked);
        put_optional_text(output, value.yanked_reason.as_deref());
    }
}

fn write_checksum(output: &mut Vec<u8>, value: &RegistryNativeChecksum) {
    output.push(match value.algorithm {
        RegistryNativeChecksumAlgorithm::Sha1 => 0,
        RegistryNativeChecksumAlgorithm::Sha256 => 1,
        RegistryNativeChecksumAlgorithm::Sha512 => 2,
        RegistryNativeChecksumAlgorithm::GoModule => 3,
    });
    put_u32(output, value.digest.len());
    output.extend_from_slice(&value.digest);
}

fn write_maven_checksum(output: &mut Vec<u8>, value: &RegistryMavenChecksum) {
    put_text(output, &value.url);
    write_checksum(output, &value.checksum);
}

fn write_evidence_claim(output: &mut Vec<u8>, value: &RegistryNativeEvidenceClaim) {
    put_text(output, &value.url);
    output.extend_from_slice(&value.digest);
    put_u64(output, value.bytes);
}

fn write_go_source(output: &mut Vec<u8>, value: &RegistryGoSourceFacts) {
    put_text(output, &value.module);
    put_text(output, &value.version);
    write_evidence_claim(output, &value.info);
    write_evidence_claim(output, &value.module_file);
    write_evidence_claim(output, &value.checksum);
}

fn write_observation<T>(
    output: &mut Vec<u8>,
    value: &RegistryNativeObservation<T>,
    write_recorded: impl Fn(&mut Vec<u8>, &T),
) {
    match value {
        RegistryNativeObservation::Recorded(value) => {
            output.push(0);
            write_recorded(output, value);
        }
        RegistryNativeObservation::NotRecorded(reason) => {
            output.push(1);
            put_text(output, reason);
        }
        RegistryNativeObservation::Unavailable(reason) => {
            output.push(2);
            put_text(output, reason);
        }
    }
}

fn write_dependency_facts_observation(
    output: &mut Vec<u8>,
    value: &RegistryNativeObservation<DependencyFacts<Box<[PackageDependencyRecord]>>>,
) {
    write_observation(output, value, write_dependency_facts);
}

fn write_dependency_facts(
    output: &mut Vec<u8>,
    value: &DependencyFacts<Box<[PackageDependencyRecord]>>,
) {
    match value {
        DependencyFacts::Known(rows) => {
            output.push(0);
            put_u32(output, rows.len());
            for row in rows {
                write_package_reference(output, &row.source);
                output.push(row.target.ecosystem as u8);
                put_text(output, row.target.name.as_str());
                put_text(output, row.target.requirement.as_str());
                match &row.target.resolved {
                    Some(reference) => {
                        output.push(1);
                        write_package_reference(output, reference);
                    }
                    None => output.push(0),
                }
                output.push(match row.scope {
                    DependencyScope::Runtime => 0,
                    DependencyScope::Optional => 1,
                    DependencyScope::Development => 2,
                    DependencyScope::Build => 3,
                    DependencyScope::Peer => 4,
                });
                output.push(u8::from(row.optional));
                output.push(match row.evidence.authority {
                    DependencyAuthority::RegistryMetadata => 0,
                    DependencyAuthority::ArchiveManifest => 1,
                    DependencyAuthority::ForgeManifest => 2,
                    DependencyAuthority::LocalManifest => 3,
                });
                output.extend_from_slice(&row.evidence.frontier);
                output.extend_from_slice(&row.evidence.provenance);
                output.extend_from_slice(&row.facts_version);
            }
        }
        DependencyFacts::Unknown(reason) => {
            output.push(1);
            put_text(output, reason.as_str());
        }
        DependencyFacts::Unavailable(reason) => {
            output.push(2);
            put_text(output, reason.as_str());
        }
    }
}

fn write_package_reference(output: &mut Vec<u8>, value: &PackageReference) {
    match value {
        PackageReference::Purl(value) => {
            output.push(0);
            put_text(output, value.as_str());
        }
        PackageReference::Local(value) => {
            output.push(1);
            put_text(output, value.as_str());
        }
    }
}

fn read_availability(
    reader: &mut CanonicalReader<'_>,
) -> Result<RegistryNativeAvailability, RegistryNativeMetadataCodecError> {
    Ok(match reader.take_u8()? {
        0 => RegistryNativeAvailability::Recorded,
        1 => RegistryNativeAvailability::NotRecorded(reader.take_text()?),
        _ => return Err(RegistryNativeMetadataCodecError::Tag),
    })
}

fn read_provenance(
    reader: &mut CanonicalReader<'_>,
) -> Result<RegistryNativeProvenance, RegistryNativeMetadataCodecError> {
    Ok(match reader.take_u8()? {
        0 => RegistryNativeProvenance::SourceDigest(reader.take_array()?),
        1 => RegistryNativeProvenance::NotRecorded(reader.take_text()?),
        _ => return Err(RegistryNativeMetadataCodecError::Tag),
    })
}

fn read_details(
    reader: &mut CanonicalReader<'_>,
) -> Result<RegistryNativeDetails, RegistryNativeMetadataCodecError> {
    Ok(match reader.take_u8()? {
        0 => {
            let artifacts = read_artifacts(reader)?;
            let features = reader.take_count()?;
            let mut values = Vec::with_capacity(features);
            for _ in 0..features {
                let name = reader.take_text()?;
                let members = reader.take_count()?;
                let mut entries = Vec::with_capacity(members);
                for _ in 0..members {
                    entries.push(reader.take_text()?);
                }
                values.push(RegistryNativeFeature {
                    name,
                    members: entries.into_boxed_slice(),
                });
            }
            RegistryNativeDetails::Cargo(RegistryCargoMetadata {
                artifacts,
                features: values.into_boxed_slice(),
            })
        }
        1 => {
            let artifacts = read_artifacts(reader)?;
            let tags = reader.take_count()?;
            let mut values = Vec::with_capacity(tags);
            for _ in 0..tags {
                values.push(RegistryNativeDistTag {
                    name: reader.take_text()?,
                    version: reader.take_text()?,
                });
            }
            RegistryNativeDetails::Npm(RegistryNpmMetadata {
                artifacts,
                dist_tags: values.into_boxed_slice(),
                deprecation: reader.take_optional_text()?,
            })
        }
        2 => RegistryNativeDetails::Pypi(RegistryPypiMetadata {
            artifacts: read_artifacts(reader)?,
            requires_python: reader.take_optional_text()?,
            standing_reason: reader.take_optional_text()?,
        }),
        3 => RegistryNativeDetails::Maven(RegistryMavenMetadata {
            artifacts: read_artifacts(reader)?,
            checksum: read_observation(reader, read_maven_checksum)?,
            signature: read_observation(reader, read_evidence_claim)?,
            pom: read_observation(reader, read_evidence_claim)?,
            dependencies: read_dependency_facts_observation(reader)?,
        }),
        4 => {
            let artifacts = read_artifacts(reader)?;
            let vulnerabilities = read_observation(reader, |reader| {
                let count = reader.take_count()?;
                let mut values = Vec::with_capacity(count);
                for _ in 0..count {
                    values.push(RegistryNativeVulnerability {
                        advisory_url: reader.take_optional_text()?,
                        severity: reader.take_u8()?,
                    });
                }
                Ok(values.into_boxed_slice())
            })?;
            RegistryNativeDetails::Nuget(RegistryNugetMetadata {
                artifacts,
                vulnerabilities,
                dependencies: read_dependency_facts_observation(reader)?,
                deprecation: reader.take_optional_text()?,
            })
        }
        5 => {
            let artifacts = read_artifacts(reader)?;
            let count = reader.take_count()?;
            let mut retracts = Vec::with_capacity(count);
            for _ in 0..count {
                retracts.push(RegistryGoRetract {
                    lower: reader.take_text()?,
                    upper: reader.take_text()?,
                });
            }
            RegistryNativeDetails::Golang(RegistryGoMetadata {
                artifacts,
                retracts: retracts.into_boxed_slice(),
                source: read_observation(reader, read_go_source)?,
            })
        }
        6 => RegistryNativeDetails::Cpp(RegistryConanMetadata {
            artifacts: read_artifacts(reader)?,
            source: match reader.take_u8()? {
                0 => RegistryConanSourceAvailability::Archive,
                1 => RegistryConanSourceAvailability::RecipeOnly,
                2 => RegistryConanSourceAvailability::Unavailable,
                _ => return Err(RegistryNativeMetadataCodecError::Tag),
            },
            source_url: reader.take_optional_text()?,
        }),
        7 => RegistryNativeDetails::Unavailable {
            ecosystem: RegistryEcosystem::try_from(reader.take_u8()?)
                .map_err(|_| RegistryNativeMetadataCodecError::Tag)?,
            reason: reader.take_text()?,
        },
        _ => return Err(RegistryNativeMetadataCodecError::Tag),
    })
}

fn read_artifacts(
    reader: &mut CanonicalReader<'_>,
) -> Result<Box<[RegistryNativeArtifact]>, RegistryNativeMetadataCodecError> {
    let count = reader.take_count()?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(RegistryNativeArtifact {
            filename: reader.take_text()?,
            url: reader.take_text()?,
            checksum: read_checksum(reader)?,
            kind: match reader.take_u8()? {
                0 => RegistryNativeArtifactKind::CargoCrate,
                1 => RegistryNativeArtifactKind::NpmTarball,
                2 => RegistryNativeArtifactKind::PythonSdist,
                3 => RegistryNativeArtifactKind::PythonWheel,
                4 => RegistryNativeArtifactKind::PythonSignature,
                5 => RegistryNativeArtifactKind::MavenJar,
                6 => RegistryNativeArtifactKind::MavenSources,
                7 => RegistryNativeArtifactKind::MavenPom,
                8 => RegistryNativeArtifactKind::MavenSignature,
                9 => RegistryNativeArtifactKind::MavenChecksum,
                10 => RegistryNativeArtifactKind::NugetPackage,
                11 => RegistryNativeArtifactKind::GoSource,
                12 => RegistryNativeArtifactKind::ConanSource,
                13 => RegistryNativeArtifactKind::ConanRecipe,
                14 => RegistryNativeArtifactKind::Other,
                _ => return Err(RegistryNativeMetadataCodecError::Tag),
            },
            requires_python: reader.take_optional_text()?,
            size: match reader.take_u8()? {
                0 => None,
                1 => Some(reader.take_u64()?),
                _ => return Err(RegistryNativeMetadataCodecError::Tag),
            },
            yanked: match reader.take_u8()? {
                0 => None,
                1 => Some(false),
                2 => Some(true),
                _ => return Err(RegistryNativeMetadataCodecError::Tag),
            },
            yanked_reason: reader.take_optional_text()?,
        });
    }
    Ok(values.into_boxed_slice())
}

fn read_checksum(
    reader: &mut CanonicalReader<'_>,
) -> Result<RegistryNativeChecksum, RegistryNativeMetadataCodecError> {
    let algorithm = match reader.take_u8()? {
        0 => RegistryNativeChecksumAlgorithm::Sha1,
        1 => RegistryNativeChecksumAlgorithm::Sha256,
        2 => RegistryNativeChecksumAlgorithm::Sha512,
        3 => RegistryNativeChecksumAlgorithm::GoModule,
        _ => return Err(RegistryNativeMetadataCodecError::Tag),
    };
    let digest = reader.take_bytes()?;
    Ok(RegistryNativeChecksum {
        algorithm,
        digest: digest.into(),
    })
}

fn read_maven_checksum(
    reader: &mut CanonicalReader<'_>,
) -> Result<RegistryMavenChecksum, RegistryNativeMetadataCodecError> {
    Ok(RegistryMavenChecksum {
        url: reader.take_text()?,
        checksum: read_checksum(reader)?,
    })
}

fn read_evidence_claim(
    reader: &mut CanonicalReader<'_>,
) -> Result<RegistryNativeEvidenceClaim, RegistryNativeMetadataCodecError> {
    Ok(RegistryNativeEvidenceClaim {
        url: reader.take_text()?,
        digest: reader.take_array()?,
        bytes: reader.take_u64()?,
    })
}

fn read_go_source(
    reader: &mut CanonicalReader<'_>,
) -> Result<RegistryGoSourceFacts, RegistryNativeMetadataCodecError> {
    Ok(RegistryGoSourceFacts {
        module: reader.take_text()?,
        version: reader.take_text()?,
        info: read_evidence_claim(reader)?,
        module_file: read_evidence_claim(reader)?,
        checksum: read_evidence_claim(reader)?,
    })
}

fn read_observation<T>(
    reader: &mut CanonicalReader<'_>,
    read_recorded: impl Fn(&mut CanonicalReader<'_>) -> Result<T, RegistryNativeMetadataCodecError>,
) -> Result<RegistryNativeObservation<T>, RegistryNativeMetadataCodecError> {
    Ok(match reader.take_u8()? {
        0 => RegistryNativeObservation::Recorded(read_recorded(reader)?),
        1 => RegistryNativeObservation::NotRecorded(reader.take_text()?),
        2 => RegistryNativeObservation::Unavailable(reader.take_text()?),
        _ => return Err(RegistryNativeMetadataCodecError::Tag),
    })
}

fn read_dependency_facts_observation(
    reader: &mut CanonicalReader<'_>,
) -> Result<
    RegistryNativeObservation<DependencyFacts<Box<[PackageDependencyRecord]>>>,
    RegistryNativeMetadataCodecError,
> {
    read_observation(reader, read_dependency_facts)
}

fn read_dependency_facts(
    reader: &mut CanonicalReader<'_>,
) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, RegistryNativeMetadataCodecError> {
    match reader.take_u8()? {
        0 => {
            let count = reader.take_count()?;
            let mut rows = Vec::with_capacity(count);
            for _ in 0..count {
                let source = read_package_reference(reader)?;
                let ecosystem = RegistryEcosystem::try_from(reader.take_u8()?)
                    .map_err(|_| RegistryNativeMetadataCodecError::Tag)?;
                let target = PackageDependencyTarget::new(
                    ecosystem,
                    reader.take_text()?,
                    reader.take_text()?,
                    match reader.take_u8()? {
                        0 => None,
                        1 => Some(read_package_reference(reader)?),
                        _ => return Err(RegistryNativeMetadataCodecError::Tag),
                    },
                )
                .map_err(|_| RegistryNativeMetadataCodecError::Admission)?;
                let scope = match reader.take_u8()? {
                    0 => DependencyScope::Runtime,
                    1 => DependencyScope::Optional,
                    2 => DependencyScope::Development,
                    3 => DependencyScope::Build,
                    4 => DependencyScope::Peer,
                    _ => return Err(RegistryNativeMetadataCodecError::Tag),
                };
                let optional = match reader.take_u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(RegistryNativeMetadataCodecError::Tag),
                };
                let authority = match reader.take_u8()? {
                    0 => DependencyAuthority::RegistryMetadata,
                    1 => DependencyAuthority::ArchiveManifest,
                    2 => DependencyAuthority::ForgeManifest,
                    3 => DependencyAuthority::LocalManifest,
                    _ => return Err(RegistryNativeMetadataCodecError::Tag),
                };
                rows.push(PackageDependencyRecord {
                    source,
                    target,
                    scope,
                    optional,
                    evidence: DependencyEvidence {
                        authority,
                        frontier: reader.take_array()?,
                        provenance: reader.take_array()?,
                    },
                    facts_version: reader.take_array()?,
                });
            }
            let canonical = crate::admit_dependency_rows(rows.clone())
                .map_err(|_| RegistryNativeMetadataCodecError::Admission)?;
            if canonical.as_ref() != rows.as_slice() {
                return Err(RegistryNativeMetadataCodecError::Admission);
            }
            Ok(DependencyFacts::Known(canonical))
        }
        1 => Ok(DependencyFacts::Unknown(
            ProductText::new(reader.take_text()?)
                .map_err(|_| RegistryNativeMetadataCodecError::Text)?,
        )),
        2 => Ok(DependencyFacts::Unavailable(
            ProductText::new(reader.take_text()?)
                .map_err(|_| RegistryNativeMetadataCodecError::Text)?,
        )),
        _ => Err(RegistryNativeMetadataCodecError::Tag),
    }
}

fn read_package_reference(
    reader: &mut CanonicalReader<'_>,
) -> Result<PackageReference, RegistryNativeMetadataCodecError> {
    let tag = reader.take_u8()?;
    let text = reader.take_text()?;
    match tag {
        0 => PackageReference::parse(text)
            .map_err(|_| RegistryNativeMetadataCodecError::Admission)
            .and_then(|reference| match reference {
                PackageReference::Purl(_) => Ok(reference),
                PackageReference::Local(_) => Err(RegistryNativeMetadataCodecError::Admission),
            }),
        1 => Ok(PackageReference::Local(
            ProductText::new(text).map_err(|_| RegistryNativeMetadataCodecError::Text)?,
        )),
        _ => Err(RegistryNativeMetadataCodecError::Tag),
    }
}

struct CanonicalReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> CanonicalReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn take_exact(&mut self, length: usize) -> Result<&'a [u8], RegistryNativeMetadataCodecError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(RegistryNativeMetadataCodecError::Bounds)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(RegistryNativeMetadataCodecError::Truncated)?;
        self.offset = end;
        Ok(value)
    }

    fn take_u8(&mut self) -> Result<u8, RegistryNativeMetadataCodecError> {
        Ok(*self
            .take_exact(1)?
            .first()
            .ok_or(RegistryNativeMetadataCodecError::Truncated)?)
    }

    fn take_u16(&mut self) -> Result<u16, RegistryNativeMetadataCodecError> {
        Ok(u16::from_be_bytes(self.take_exact(2)?.try_into().map_err(
            |_| RegistryNativeMetadataCodecError::Truncated,
        )?))
    }

    fn take_u32(&mut self) -> Result<u32, RegistryNativeMetadataCodecError> {
        Ok(u32::from_be_bytes(self.take_exact(4)?.try_into().map_err(
            |_| RegistryNativeMetadataCodecError::Truncated,
        )?))
    }

    fn take_u64(&mut self) -> Result<u64, RegistryNativeMetadataCodecError> {
        Ok(u64::from_be_bytes(self.take_exact(8)?.try_into().map_err(
            |_| RegistryNativeMetadataCodecError::Truncated,
        )?))
    }

    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], RegistryNativeMetadataCodecError> {
        self.take_exact(N)?
            .try_into()
            .map_err(|_| RegistryNativeMetadataCodecError::Truncated)
    }

    fn take_bytes(&mut self) -> Result<Box<[u8]>, RegistryNativeMetadataCodecError> {
        let length = usize::try_from(self.take_u32()?)
            .map_err(|_| RegistryNativeMetadataCodecError::Bounds)?;
        if length > MAX_REGISTRY_NATIVE_TEXT_BYTES {
            return Err(RegistryNativeMetadataCodecError::Bounds);
        }
        Ok(self.take_exact(length)?.into())
    }

    fn take_count(&mut self) -> Result<usize, RegistryNativeMetadataCodecError> {
        let count = usize::try_from(self.take_u32()?)
            .map_err(|_| RegistryNativeMetadataCodecError::Bounds)?;
        if count > MAX_REGISTRY_NATIVE_ROWS {
            return Err(RegistryNativeMetadataCodecError::Bounds);
        }
        Ok(count)
    }

    fn take_text(&mut self) -> Result<String, RegistryNativeMetadataCodecError> {
        let length = usize::try_from(self.take_u32()?)
            .map_err(|_| RegistryNativeMetadataCodecError::Bounds)?;
        if length > MAX_REGISTRY_NATIVE_TEXT_BYTES {
            return Err(RegistryNativeMetadataCodecError::Bounds);
        }
        String::from_utf8(self.take_exact(length)?.to_vec())
            .map_err(|_| RegistryNativeMetadataCodecError::Text)
    }

    fn take_optional_text(&mut self) -> Result<Option<String>, RegistryNativeMetadataCodecError> {
        match self.take_u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.take_text()?)),
            _ => Err(RegistryNativeMetadataCodecError::Tag),
        }
    }
}

fn validate_availability(value: &RegistryNativeAvailability) -> Result<(), ProductAdmissionError> {
    match value {
        RegistryNativeAvailability::Recorded => Ok(()),
        RegistryNativeAvailability::NotRecorded(reason) => validate_text(reason),
    }
}

fn validate_provenance(value: &RegistryNativeProvenance) -> Result<(), ProductAdmissionError> {
    match value {
        RegistryNativeProvenance::SourceDigest(_) => Ok(()),
        RegistryNativeProvenance::NotRecorded(reason) => validate_text(reason),
    }
}

fn validate_details(value: &RegistryNativeDetails) -> Result<(), ProductAdmissionError> {
    match value {
        RegistryNativeDetails::Cargo(value) => {
            validate_artifacts(&value.artifacts)?;
            if value.features.len() > MAX_REGISTRY_NATIVE_ROWS {
                return Err(ProductAdmissionError::NativeMetadata);
            }
            let mut previous = None;
            for feature in &value.features {
                validate_text(&feature.name)?;
                if previous.is_some_and(|previous| previous >= feature.name.as_str()) {
                    return Err(ProductAdmissionError::NativeMetadata);
                }
                previous = Some(feature.name.as_str());
                if feature.members.len() > MAX_REGISTRY_NATIVE_ROWS {
                    return Err(ProductAdmissionError::NativeMetadata);
                }
                validate_sorted_texts(&feature.members)?;
            }
        }
        RegistryNativeDetails::Npm(value) => {
            validate_artifacts(&value.artifacts)?;
            if value.dist_tags.len() > MAX_REGISTRY_NATIVE_ROWS {
                return Err(ProductAdmissionError::NativeMetadata);
            }
            let mut previous = None;
            for tag in &value.dist_tags {
                validate_text(&tag.name)?;
                validate_text(&tag.version)?;
                if previous.is_some_and(|previous| previous >= tag.name.as_str()) {
                    return Err(ProductAdmissionError::NativeMetadata);
                }
                previous = Some(tag.name.as_str());
            }
            validate_optional_text(value.deprecation.as_ref())?;
        }
        RegistryNativeDetails::Pypi(value) => {
            validate_artifacts(&value.artifacts)?;
            validate_optional_text(value.requires_python.as_ref())?;
            validate_optional_text(value.standing_reason.as_ref())?;
        }
        RegistryNativeDetails::Maven(value) => {
            validate_artifacts(&value.artifacts)?;
            validate_observation(&value.checksum, validate_maven_checksum)?;
            validate_observation(&value.signature, validate_evidence_claim)?;
            validate_observation(&value.pom, validate_evidence_claim)?;
            validate_dependency_observation(&value.dependencies)?;
        }
        RegistryNativeDetails::Nuget(value) => {
            validate_artifacts(&value.artifacts)?;
            validate_observation(&value.vulnerabilities, |rows| {
                if rows.len() > MAX_REGISTRY_NATIVE_ROWS {
                    return Err(ProductAdmissionError::NativeMetadata);
                }
                let mut previous = None;
                for row in rows {
                    if row.severity > 4 {
                        return Err(ProductAdmissionError::NativeMetadata);
                    }
                    validate_optional_text(row.advisory_url.as_ref())?;
                    let key = (row.advisory_url.as_deref().unwrap_or(""), row.severity);
                    if previous.is_some_and(|previous| previous >= key) {
                        return Err(ProductAdmissionError::NativeMetadata);
                    }
                    previous = Some(key);
                }
                Ok(())
            })?;
            validate_dependency_observation(&value.dependencies)?;
            validate_optional_text(value.deprecation.as_ref())?;
        }
        RegistryNativeDetails::Golang(value) => {
            validate_artifacts(&value.artifacts)?;
            if value.retracts.len() > MAX_REGISTRY_NATIVE_ROWS {
                return Err(ProductAdmissionError::NativeMetadata);
            }
            let mut previous = None;
            for retract in &value.retracts {
                validate_text(&retract.lower)?;
                validate_text(&retract.upper)?;
                let key = (retract.lower.as_str(), retract.upper.as_str());
                if previous.is_some_and(|previous| previous >= key) {
                    return Err(ProductAdmissionError::NativeMetadata);
                }
                previous = Some(key);
            }
            validate_observation(&value.source, validate_go_source)?;
        }
        RegistryNativeDetails::Cpp(value) => {
            validate_artifacts(&value.artifacts)?;
            validate_optional_text(value.source_url.as_ref())?;
        }
        RegistryNativeDetails::Unavailable { reason, .. } => validate_text(reason)?,
    }
    Ok(())
}

fn validate_artifacts(value: &[RegistryNativeArtifact]) -> Result<(), ProductAdmissionError> {
    if value.len() > MAX_REGISTRY_NATIVE_ROWS {
        return Err(ProductAdmissionError::NativeMetadata);
    }
    let mut previous = None;
    for artifact in value {
        validate_text(&artifact.filename)?;
        validate_text(&artifact.url)?;
        validate_checksum(&artifact.checksum)?;
        validate_optional_text(artifact.requires_python.as_ref())?;
        validate_optional_text(artifact.yanked_reason.as_ref())?;
        if artifact.yanked != Some(true) && artifact.yanked_reason.is_some() {
            return Err(ProductAdmissionError::NativeMetadata);
        }
        if previous.is_some_and(|previous| previous >= artifact.filename.as_str()) {
            return Err(ProductAdmissionError::NativeMetadata);
        }
        previous = Some(artifact.filename.as_str());
    }
    Ok(())
}

fn validate_checksum(value: &RegistryNativeChecksum) -> Result<(), ProductAdmissionError> {
    let expected = match value.algorithm {
        RegistryNativeChecksumAlgorithm::Sha1 => 20,
        RegistryNativeChecksumAlgorithm::Sha256 | RegistryNativeChecksumAlgorithm::GoModule => 32,
        RegistryNativeChecksumAlgorithm::Sha512 => 64,
    };
    (value.digest.len() == expected)
        .then_some(())
        .ok_or(ProductAdmissionError::NativeMetadata)
}

fn validate_maven_checksum(value: &RegistryMavenChecksum) -> Result<(), ProductAdmissionError> {
    validate_text(&value.url)?;
    validate_checksum(&value.checksum)
}

fn validate_evidence_claim(
    value: &RegistryNativeEvidenceClaim,
) -> Result<(), ProductAdmissionError> {
    validate_text(&value.url)
}

fn validate_go_source(value: &RegistryGoSourceFacts) -> Result<(), ProductAdmissionError> {
    validate_text(&value.module)?;
    validate_text(&value.version)?;
    validate_evidence_claim(&value.info)?;
    validate_evidence_claim(&value.module_file)?;
    validate_evidence_claim(&value.checksum)
}

fn validate_dependency_observation(
    value: &RegistryNativeObservation<DependencyFacts<Box<[PackageDependencyRecord]>>>,
) -> Result<(), ProductAdmissionError> {
    validate_observation(value, |facts| match facts {
        DependencyFacts::Known(rows) => {
            let canonical = crate::admit_dependency_rows(rows.to_vec())
                .map_err(|_| ProductAdmissionError::DependencyShape)?;
            if canonical.as_ref() == rows.as_ref() {
                Ok(())
            } else {
                Err(ProductAdmissionError::NativeMetadata)
            }
        }
        DependencyFacts::Unknown(reason) | DependencyFacts::Unavailable(reason) => {
            validate_text(reason.as_str())
        }
    })
}

fn validate_observation<T>(
    value: &RegistryNativeObservation<T>,
    validate: impl Fn(&T) -> Result<(), ProductAdmissionError>,
) -> Result<(), ProductAdmissionError> {
    match value {
        RegistryNativeObservation::Recorded(value) => validate(value),
        RegistryNativeObservation::NotRecorded(reason)
        | RegistryNativeObservation::Unavailable(reason) => validate_text(reason),
    }
}

fn validate_sorted_texts(values: &[String]) -> Result<(), ProductAdmissionError> {
    let mut previous = None;
    for value in values {
        validate_text(value)?;
        if previous.is_some_and(|previous| previous >= value.as_str()) {
            return Err(ProductAdmissionError::NativeMetadata);
        }
        previous = Some(value.as_str());
    }
    Ok(())
}

fn validate_optional_text(value: Option<&String>) -> Result<(), ProductAdmissionError> {
    value.map_or(Ok(()), |value| validate_text(value))
}

fn validate_text(value: &str) -> Result<(), ProductAdmissionError> {
    crate::ProductText::new(value.to_owned()).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorded(details: RegistryNativeDetails) -> RegistryNativeMetadata {
        RegistryNativeMetadata {
            version: REGISTRY_NATIVE_METADATA_VERSION,
            availability: RegistryNativeAvailability::Recorded,
            provenance: RegistryNativeProvenance::SourceDigest([7; 32]),
            details,
        }
    }

    #[test]
    fn unavailable_metadata_round_trips_and_has_stable_identity() {
        let value = RegistryNativeMetadata::unavailable(
            RegistryEcosystem::Cargo,
            "canonical feed omits native details",
        );
        value.admit().expect("bounded unavailable metadata");
        let canonical = value.encode_canonical();
        assert_eq!(
            RegistryNativeMetadata::decode_canonical(&canonical),
            Ok(value.clone())
        );
        let encoded = serde_json::to_vec(&value).expect("encode metadata");
        let decoded: RegistryNativeMetadata =
            serde_json::from_slice(&encoded).expect("decode metadata");
        assert_eq!(decoded, value);
        assert_eq!(decoded.identity(), value.identity());
    }

    #[test]
    fn recorded_metadata_round_trips_the_canonical_binary_shape() {
        let value = recorded(RegistryNativeDetails::Cargo(RegistryCargoMetadata {
            artifacts: vec![RegistryNativeArtifact {
                filename: "demo-1.0.0.crate".to_owned(),
                url: "https://registry.example/demo-1.0.0.crate".to_owned(),
                checksum: RegistryNativeChecksum {
                    algorithm: RegistryNativeChecksumAlgorithm::Sha256,
                    digest: Box::new([3; 32]),
                },
                kind: RegistryNativeArtifactKind::CargoCrate,
                requires_python: None,
                size: Some(42),
                yanked: Some(false),
                yanked_reason: None,
            }]
            .into_boxed_slice(),
            features: vec![RegistryNativeFeature {
                name: "default".to_owned(),
                members: vec!["dep:serde".to_owned()].into_boxed_slice(),
            }]
            .into_boxed_slice(),
        }));
        let canonical = value.encode_canonical();
        assert_eq!(
            RegistryNativeMetadata::decode_canonical(&canonical),
            Ok(value.clone())
        );
        assert_eq!(
            value.identity(),
            RegistryNativeMetadata::decode_canonical(&canonical)
                .expect("recorded metadata decodes")
                .identity()
        );
    }

    #[test]
    fn cargo_features_must_be_lexically_sorted() {
        let value = recorded(RegistryNativeDetails::Cargo(RegistryCargoMetadata {
            artifacts: Box::new([]),
            features: vec![
                RegistryNativeFeature {
                    name: "z".to_owned(),
                    members: Box::new([]),
                },
                RegistryNativeFeature {
                    name: "a".to_owned(),
                    members: Box::new([]),
                },
            ]
            .into_boxed_slice(),
        }));
        assert_eq!(value.admit(), Err(ProductAdmissionError::NativeMetadata));
    }

    #[test]
    fn checksum_width_is_admitted_before_publication() {
        let value = recorded(RegistryNativeDetails::Npm(RegistryNpmMetadata {
            artifacts: vec![RegistryNativeArtifact {
                filename: "demo.tgz".to_owned(),
                url: "https://registry.example/demo.tgz".to_owned(),
                checksum: RegistryNativeChecksum {
                    algorithm: RegistryNativeChecksumAlgorithm::Sha256,
                    digest: Box::new([0; 31]),
                },
                kind: RegistryNativeArtifactKind::NpmTarball,
                requires_python: None,
                size: None,
                yanked: Some(false),
                yanked_reason: None,
            }]
            .into_boxed_slice(),
            dist_tags: Box::new([]),
            deprecation: None,
        }));
        assert_eq!(value.admit(), Err(ProductAdmissionError::NativeMetadata));
    }

    #[test]
    fn identity_changes_when_a_native_fact_changes() {
        let first = RegistryNativeMetadata::unavailable(RegistryEcosystem::Npm, "missing");
        let second = RegistryNativeMetadata::unavailable(RegistryEcosystem::Npm, "withheld");
        assert_ne!(first.identity(), second.identity());
    }
}
