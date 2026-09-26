//! Pure native-registry URL and metadata adapters.
//!
//! These adapters contain no cursor, retry, storage, or publication state.
//! They normalize seven source grammars into one release descriptor consumed
//! by the shared acquisition owner.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use std::collections::BTreeMap;
use std::io::{Cursor, Read, Seek};
use zip::ZipArchive;

use super::identity::coordinate_from_registry_parts;
use super::transport::ArchiveIntegrity;
use super::{
    AcquisitionError, FeedPage, FeedRequest, PackageCoordinate, PackageName, PackageVersion,
    ProvenanceDigest, RegistryEcosystem, RegistryEndpoint, ReleaseFacts, RemotePackage,
    TransportFailure,
};
use std::sync::Arc;

/// Hard ceiling for a directly supplied native metadata body. HTTP callers
/// normally apply the tighter [`AcquisitionLimits`] feed cap first; retaining
/// this guard at the pure adapter boundary prevents an embedded caller from
/// handing `serde_json` an unbounded allocation.
pub(crate) const MAX_NATIVE_METADATA_BYTES: usize = 64 * 1024 * 1024;
/// Maximum number of release rows admitted from one package document.
pub(crate) const MAX_NATIVE_RELEASES: usize = 1_000_000;

/// Registry checksum algorithm declared by native metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChecksumAlgorithm {
    /// SHA-1 digest. Maven Central publishes this legacy digest for artifacts.
    Sha1,
    /// SHA-256 raw archive digest.
    Sha256,
    /// SHA-512 raw archive digest.
    Sha512,
    /// Go module zip hash (`h1:`), as defined by `golang.org/x/mod/dirhash`.
    GoModule,
}

/// Parsed native checksum claim with a closed algorithm.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryChecksum {
    algorithm: ChecksumAlgorithm,
    pub(crate) bytes: Box<[u8]>,
}

impl RegistryChecksum {
    pub(crate) fn sha1_hex(value: &str) -> Result<Self, TransportFailure> {
        Ok(Self {
            algorithm: ChecksumAlgorithm::Sha1,
            bytes: decode_hex(value, 20)?.into(),
        })
    }

    pub(crate) fn sha256_hex(value: &str) -> Result<Self, TransportFailure> {
        Ok(Self {
            algorithm: ChecksumAlgorithm::Sha256,
            bytes: decode_hex(value, 32)?.into(),
        })
    }

    pub(crate) fn sha512_base64(value: &str) -> Result<Self, TransportFailure> {
        let bytes = STANDARD
            .decode(value)
            .map_err(|_| TransportFailure::Protocol)?;
        if bytes.len() != 64 {
            return Err(TransportFailure::Protocol);
        }
        Ok(Self {
            algorithm: ChecksumAlgorithm::Sha512,
            bytes: bytes.into_boxed_slice(),
        })
    }

    pub(crate) fn sha1_base64(value: &str) -> Result<Self, TransportFailure> {
        let bytes = STANDARD
            .decode(value)
            .map_err(|_| TransportFailure::Protocol)?;
        if bytes.len() != 20 {
            return Err(TransportFailure::Protocol);
        }
        Ok(Self {
            algorithm: ChecksumAlgorithm::Sha1,
            bytes: bytes.into_boxed_slice(),
        })
    }

    pub(crate) fn sha256_base64(value: &str) -> Result<Self, TransportFailure> {
        let bytes = STANDARD
            .decode(value)
            .map_err(|_| TransportFailure::Protocol)?;
        if bytes.len() != 32 {
            return Err(TransportFailure::Protocol);
        }
        Ok(Self {
            algorithm: ChecksumAlgorithm::Sha256,
            bytes: bytes.into_boxed_slice(),
        })
    }

    pub(crate) fn sha512_hex(value: &str) -> Result<Self, TransportFailure> {
        Ok(Self {
            algorithm: ChecksumAlgorithm::Sha512,
            bytes: decode_hex(value, 64)?.into(),
        })
    }

    pub(crate) fn go_module_base64(value: &str) -> Result<Self, TransportFailure> {
        let bytes = STANDARD
            .decode(
                value
                    .strip_prefix("h1:")
                    .ok_or(TransportFailure::Protocol)?,
            )
            .map_err(|_| TransportFailure::Protocol)?;
        if bytes.len() != 32 {
            return Err(TransportFailure::Protocol);
        }
        Ok(Self {
            algorithm: ChecksumAlgorithm::GoModule,
            bytes: bytes.into_boxed_slice(),
        })
    }

    /// Verifies exact archive bytes against the registry-native content claim.
    #[must_use]
    pub fn verifies(&self, archive: &[u8]) -> bool {
        match self.algorithm {
            ChecksumAlgorithm::Sha1 => Sha1::digest(archive).as_slice() == self.bytes.as_ref(),
            ChecksumAlgorithm::Sha256 => Sha256::digest(archive).as_slice() == self.bytes.as_ref(),
            ChecksumAlgorithm::Sha512 => Sha512::digest(archive).as_slice() == self.bytes.as_ref(),
            ChecksumAlgorithm::GoModule => {
                go_module_zip_hash(archive).is_some_and(|digest| digest == self.bytes.as_ref())
            }
        }
    }

    pub(crate) fn verifies_reader<R: Read + Seek>(&self, reader: &mut R) -> bool {
        match self.algorithm {
            ChecksumAlgorithm::GoModule => go_module_zip_hash_reader(reader)
                .is_some_and(|digest| digest.as_ref() == self.bytes.as_ref()),
            _ => false,
        }
    }

    /// Declared checksum algorithm.
    #[must_use]
    pub const fn algorithm(&self) -> ChecksumAlgorithm {
        self.algorithm
    }

    /// Returns the authenticated digest bytes in their native algorithm width.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn cache_key(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.registry.archive-cache.v1\0");
        hasher.update(&[self.algorithm as u8]);
        hasher.update(self.bytes.as_ref());
        *hasher.finalize().as_bytes()
    }
}

/// Kind of an artifact published alongside one native release.
///
/// Registry metadata often publishes several files for one version. The
/// acquisition owner selects one verified source artifact, while the adapter
/// retains the complete file set so a later policy or platform selection never
/// needs to re-fetch metadata.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum NativeArtifactKind {
    /// Cargo's `.crate` source archive.
    CargoCrate,
    /// npm's package tarball.
    NpmTarball,
    /// Python source distribution.
    PythonSdist,
    /// Python wheel distribution.
    PythonWheel,
    /// A detached Python distribution signature.
    PythonSignature,
    /// Maven bytecode JAR.
    MavenJar,
    /// Maven source JAR.
    MavenSources,
    /// Maven POM descriptor.
    MavenPom,
    /// Maven detached signature.
    MavenSignature,
    /// Maven sidecar checksum.
    MavenChecksum,
    /// NuGet package archive.
    NugetPackage,
    /// Go module source archive.
    GoSource,
    /// Conan source archive.
    ConanSource,
    /// Conan recipe export.
    ConanRecipe,
    /// A registry file that is retained but is not a source distribution.
    Other,
}

/// One complete, authenticated file claim from a native registry response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeArtifact {
    pub(crate) filename: Arc<str>,
    pub(crate) url: Arc<str>,
    pub(crate) checksum: RegistryChecksum,
    pub(crate) kind: NativeArtifactKind,
    pub(crate) requires_python: Option<Arc<str>>,
    pub(crate) size: Option<u64>,
    pub(crate) yanked: bool,
    pub(crate) yanked_reason: Option<Arc<str>>,
}

impl NativeArtifact {
    /// Registry filename, retained byte-for-byte after validation.
    #[must_use]
    pub fn filename(&self) -> &str {
        &self.filename
    }

    /// Resolved archive or artifact URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Authenticated file checksum.
    #[must_use]
    pub fn checksum(&self) -> &RegistryChecksum {
        &self.checksum
    }

    /// Native file classification.
    #[must_use]
    pub const fn kind(&self) -> NativeArtifactKind {
        self.kind
    }

    /// Python interpreter constraint, when supplied by the registry.
    #[must_use]
    pub fn requires_python(&self) -> Option<&str> {
        self.requires_python.as_deref()
    }

    /// Published byte length, when supplied by the registry.
    #[must_use]
    pub const fn size(&self) -> Option<u64> {
        self.size
    }

    /// Whether the registry withdrew this individual file.
    #[must_use]
    pub const fn yanked(&self) -> bool {
        self.yanked
    }

    /// Publisher-provided withdrawal reason, when one exists.
    #[must_use]
    pub fn yanked_reason(&self) -> Option<&str> {
        self.yanked_reason.as_deref()
    }
}

/// One Cargo feature declaration retained in canonical order.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NativeFeature {
    name: Arc<str>,
    members: Box<[Arc<str>]>,
}

impl NativeFeature {
    /// Feature name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Feature members in deterministic lexical order.
    #[must_use]
    pub fn members(&self) -> &[Arc<str>] {
        &self.members
    }
}

/// One npm dist-tag assignment retained in canonical lexical order.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NativeDistTag {
    name: Arc<str>,
    version: Arc<str>,
}

impl NativeDistTag {
    /// Dist-tag name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Version selected by this tag.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }
}

/// Builds the Go module zip hash used by the public checksum database.
///
/// Go does not publish a raw archive digest. `h1:` is the SHA-256 of sorted
/// `sha256(file)  filename\n` records, including the module-version directory
/// prefix present in proxy zips. The `zip` crate handles central directories,
/// data descriptors, ZIP64, CRC validation, and deflate decoding. We still
/// apply an explicit size/path budget here because registry archives are
/// untrusted input.
fn go_module_zip_hash(archive: &[u8]) -> Option<[u8; 32]> {
    go_module_zip_hash_reader(&mut Cursor::new(archive))
}

fn go_module_zip_hash_reader<R: Read + Seek>(reader: &mut R) -> Option<[u8; 32]> {
    const MAX_TOTAL_UNCOMPRESSED: u64 = 256 * 1024 * 1024;
    const MAX_FILE_UNCOMPRESSED: u64 = 64 * 1024 * 1024;

    let mut zip = ZipArchive::new(reader).ok()?;
    let mut files = BTreeMap::<String, [u8; 32]>::new();
    let mut total_uncompressed = 0_u64;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).ok()?;
        if entry.is_dir() || entry.enclosed_name().is_none() {
            return None;
        }
        let name = std::str::from_utf8(entry.name_raw()).ok()?.to_owned();
        if !is_safe_go_zip_name(&name) || files.contains_key(&name) {
            return None;
        }
        let size = entry.size();
        if size > MAX_FILE_UNCOMPRESSED {
            return None;
        }
        total_uncompressed = total_uncompressed.checked_add(size)?;
        if total_uncompressed > MAX_TOTAL_UNCOMPRESSED {
            return None;
        }
        if name.len() > 4096 {
            return None;
        }
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut read_total = 0_u64;
        loop {
            let read = entry.read(&mut buffer).ok()?;
            if read == 0 {
                break;
            }
            read_total = read_total.checked_add(u64::try_from(read).ok()?)?;
            if read_total > size {
                return None;
            }
            hasher.update(&buffer[..read]);
        }
        if read_total != size {
            return None;
        }
        // `ZipFile` validates CRC while reading. Keep the digest keyed by the
        // exact UTF-8 path emitted by the proxy, as required by dirhash.
        files.insert(name, hasher.finalize().into());
    }
    if files.is_empty() {
        return None;
    }
    let mut records = Vec::new();
    for (name, digest) in files {
        // Go's dirhash.Hash1 uses the `sha256sum` text format: two spaces
        // separate the digest from the path. The second space is part of the
        // signed summary and omitting it makes every proxy archive fail the
        // public h1 checksum even though the individual file digests match.
        let record_size = 66_usize.checked_add(name.len())?;
        let next = records.len().checked_add(record_size)?;
        if u64::try_from(next).ok()? > MAX_TOTAL_UNCOMPRESSED {
            return None;
        }
        records.extend(hex_bytes(&digest));
        records.extend_from_slice(b"  ");
        records.extend_from_slice(name.as_bytes());
        records.push(b'\n');
    }
    Some(*Sha256::digest(records).as_ref())
}

fn is_safe_go_zip_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && name
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn hex_bytes(bytes: &[u8; 32]) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = Vec::with_capacity(64);
    for byte in bytes {
        output.push(HEX[usize::from(byte >> 4)]);
        output.push(HEX[usize::from(byte & 0x0f)]);
    }
    output
}

/// One release normalized from native registry metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeRelease {
    /// Exact ecosystem/name/version coordinate.
    pub coordinate: PackageCoordinate,
    /// Absolute archive URL produced by the adapter's fixed grammar.
    pub archive_url: String,
    /// Registry-native archive checksum.
    pub checksum: RegistryChecksum,
    /// Digest binding the source metadata row used as provenance.
    pub provenance: ProvenanceDigest,
    /// Mutable registry policy and observations, versioned independently.
    pub facts: ReleaseFacts,
    /// Dependency facts captured from the native metadata row.
    pub dependency_facts:
        backend_library::DependencyFacts<Box<[backend_library::PackageDependencyRecord]>>,
    /// Every file advertised for this release, including non-source files.
    pub artifacts: Box<[NativeArtifact]>,
    /// Cargo feature declarations for this release.
    pub features: Box<[NativeFeature]>,
    /// npm dist-tags observed with the packument.
    pub dist_tags: Arc<[NativeDistTag]>,
    /// Publisher reason attached to a yanked or deprecated release.
    pub standing_reason: Option<Arc<str>>,
    /// Python `Requires-Python` of the selected artifact.
    pub requires_python: Option<Arc<str>>,
    /// Versioned native metadata carried through publication.
    pub native_metadata: backend_library::RegistryNativeMetadata,
}

impl NativeRelease {
    /// Returns all files advertised for this release.
    #[must_use]
    pub fn artifacts(&self) -> &[NativeArtifact] {
        &self.artifacts
    }

    /// Returns Cargo feature declarations.
    #[must_use]
    pub fn features(&self) -> &[NativeFeature] {
        &self.features
    }

    /// Returns the packument's npm dist-tags.
    #[must_use]
    pub fn dist_tags(&self) -> &[NativeDistTag] {
        &self.dist_tags
    }

    /// Returns a publisher's yanked/deprecation reason, if supplied.
    #[must_use]
    pub fn standing_reason(&self) -> Option<&str> {
        self.standing_reason.as_deref()
    }

    /// Returns the selected Python artifact's interpreter constraint.
    #[must_use]
    pub fn requires_python(&self) -> Option<&str> {
        self.requires_python.as_deref()
    }

    /// Returns the bounded native metadata DTO.
    #[must_use]
    pub fn metadata(&self) -> &backend_library::RegistryNativeMetadata {
        &self.native_metadata
    }

    pub(crate) fn set_artifacts(&mut self, artifacts: Vec<NativeArtifact>) {
        self.artifacts = artifacts.into_boxed_slice();
    }

    fn set_features(&mut self, features: Vec<NativeFeature>) {
        self.features = features.into_boxed_slice();
    }

    fn set_dist_tags(&mut self, dist_tags: Arc<[NativeDistTag]>) {
        self.dist_tags = dist_tags;
    }

    fn set_standing_reason(&mut self, reason: Option<Arc<str>>) {
        self.standing_reason = reason;
    }

    fn set_requires_python(&mut self, requires_python: Option<Arc<str>>) {
        self.requires_python = requires_python;
    }

    pub(crate) fn set_native_metadata(
        &mut self,
        metadata: backend_library::RegistryNativeMetadata,
    ) -> Result<(), TransportFailure> {
        let identity = metadata
            .identity()
            .map_err(|_| TransportFailure::Protocol)?;
        self.facts = self.facts.with_native_metadata(identity);
        self.native_metadata = metadata;
        Ok(())
    }

    /// Projects the common Cargo/npm/PyPI release fields into the shared DTO.
    pub(crate) fn record_common_native_metadata(
        &mut self,
        ecosystem: RegistryEcosystem,
        source: &[u8],
    ) -> Result<(), TransportFailure> {
        let artifacts = self
            .artifacts
            .iter()
            .map(native_artifact)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let details = match ecosystem {
            RegistryEcosystem::Cargo => backend_library::RegistryNativeDetails::Cargo(
                backend_library::RegistryCargoMetadata {
                    artifacts,
                    features: self
                        .features
                        .iter()
                        .map(|feature| backend_library::RegistryNativeFeature {
                            name: feature.name().to_owned(),
                            members: feature
                                .members()
                                .iter()
                                .map(|member| member.to_string())
                                .collect::<Vec<_>>()
                                .into_boxed_slice(),
                        })
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                },
            ),
            RegistryEcosystem::Npm => {
                backend_library::RegistryNativeDetails::Npm(backend_library::RegistryNpmMetadata {
                    artifacts,
                    dist_tags: self
                        .dist_tags
                        .iter()
                        .map(|tag| backend_library::RegistryNativeDistTag {
                            name: tag.name().to_owned(),
                            version: tag.version().to_owned(),
                        })
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                    deprecation: self.standing_reason().map(ToOwned::to_owned),
                })
            }
            RegistryEcosystem::Pypi => backend_library::RegistryNativeDetails::Pypi(
                backend_library::RegistryPypiMetadata {
                    artifacts,
                    requires_python: self.requires_python().map(ToOwned::to_owned),
                    standing_reason: self.standing_reason().map(ToOwned::to_owned),
                },
            ),
            ecosystem => backend_library::RegistryNativeDetails::Unavailable {
                ecosystem,
                reason: "native adapter did not record ecosystem details".to_owned(),
            },
        };
        self.set_native_metadata(backend_library::RegistryNativeMetadata {
            version: backend_library::REGISTRY_NATIVE_METADATA_VERSION,
            availability: backend_library::RegistryNativeAvailability::Recorded,
            provenance: backend_library::RegistryNativeProvenance::SourceDigest(
                *blake3::hash(source).as_bytes(),
            ),
            details,
        })
    }

    /// Records NuGet's typed vulnerability/dependency observations.
    pub(crate) fn record_nuget_native_metadata(
        &mut self,
        vulnerabilities: backend_library::RegistryNativeObservation<
            Box<[backend_library::RegistryNativeVulnerability]>,
        >,
        dependencies: backend_library::RegistryNativeObservation<
            backend_library::DependencyFacts<Box<[backend_library::PackageDependencyRecord]>>,
        >,
        deprecation: Option<String>,
    ) -> Result<(), TransportFailure> {
        self.set_native_metadata(backend_library::RegistryNativeMetadata {
            version: backend_library::REGISTRY_NATIVE_METADATA_VERSION,
            availability: backend_library::RegistryNativeAvailability::Recorded,
            provenance: backend_library::RegistryNativeProvenance::SourceDigest(
                self.provenance.as_bytes(),
            ),
            details: backend_library::RegistryNativeDetails::Nuget(
                backend_library::RegistryNugetMetadata {
                    artifacts: self
                        .artifacts
                        .iter()
                        .map(native_artifact_without_yanked)
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                    vulnerabilities,
                    dependencies,
                    deprecation,
                },
            ),
        })
    }

    /// Records Maven sidecar and POM observations alongside the selected archive.
    pub(crate) fn record_maven_native_metadata(
        &mut self,
        checksum_url: String,
        signature: backend_library::RegistryNativeObservation<
            backend_library::RegistryNativeEvidenceClaim,
        >,
        pom: backend_library::RegistryNativeObservation<
            backend_library::RegistryNativeEvidenceClaim,
        >,
        dependencies: backend_library::RegistryNativeObservation<
            backend_library::DependencyFacts<Box<[backend_library::PackageDependencyRecord]>>,
        >,
    ) -> Result<(), TransportFailure> {
        self.set_native_metadata(backend_library::RegistryNativeMetadata {
            version: backend_library::REGISTRY_NATIVE_METADATA_VERSION,
            availability: backend_library::RegistryNativeAvailability::Recorded,
            provenance: backend_library::RegistryNativeProvenance::SourceDigest(
                self.provenance.as_bytes(),
            ),
            details: backend_library::RegistryNativeDetails::Maven(
                backend_library::RegistryMavenMetadata {
                    artifacts: self
                        .artifacts
                        .iter()
                        .map(native_artifact_without_yanked)
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                    checksum: backend_library::RegistryNativeObservation::Recorded(
                        backend_library::RegistryMavenChecksum {
                            url: checksum_url,
                            checksum: native_checksum(&self.checksum),
                        },
                    ),
                    signature,
                    pom,
                    dependencies,
                },
            ),
        })
    }
}

pub(crate) fn native_checksum(
    checksum: &RegistryChecksum,
) -> backend_library::RegistryNativeChecksum {
    let algorithm = match checksum.algorithm() {
        ChecksumAlgorithm::Sha1 => backend_library::RegistryNativeChecksumAlgorithm::Sha1,
        ChecksumAlgorithm::Sha256 => backend_library::RegistryNativeChecksumAlgorithm::Sha256,
        ChecksumAlgorithm::Sha512 => backend_library::RegistryNativeChecksumAlgorithm::Sha512,
        ChecksumAlgorithm::GoModule => backend_library::RegistryNativeChecksumAlgorithm::GoModule,
    };
    backend_library::RegistryNativeChecksum {
        algorithm,
        digest: checksum.bytes().to_vec().into_boxed_slice(),
    }
}

fn native_artifact(artifact: &NativeArtifact) -> backend_library::RegistryNativeArtifact {
    // npm packuments do not publish a release/file yank bit. Preserve the
    // absence instead of projecting the adapter's internal standing default.
    let yanked = match artifact.kind() {
        NativeArtifactKind::NpmTarball => None,
        NativeArtifactKind::CargoCrate
        | NativeArtifactKind::PythonSdist
        | NativeArtifactKind::PythonWheel
        | NativeArtifactKind::PythonSignature
        | NativeArtifactKind::MavenJar
        | NativeArtifactKind::MavenSources
        | NativeArtifactKind::MavenPom
        | NativeArtifactKind::MavenSignature
        | NativeArtifactKind::MavenChecksum
        | NativeArtifactKind::NugetPackage
        | NativeArtifactKind::GoSource
        | NativeArtifactKind::ConanSource
        | NativeArtifactKind::ConanRecipe
        | NativeArtifactKind::Other => Some(artifact.yanked()),
    };
    native_artifact_with_yanked(artifact, yanked)
}

fn native_artifact_without_yanked(
    artifact: &NativeArtifact,
) -> backend_library::RegistryNativeArtifact {
    native_artifact_with_yanked(artifact, None)
}

fn native_artifact_with_yanked(
    artifact: &NativeArtifact,
    yanked: Option<bool>,
) -> backend_library::RegistryNativeArtifact {
    backend_library::RegistryNativeArtifact {
        filename: artifact.filename().to_owned(),
        url: artifact.url().to_owned(),
        checksum: native_checksum(artifact.checksum()),
        kind: match artifact.kind() {
            NativeArtifactKind::CargoCrate => {
                backend_library::RegistryNativeArtifactKind::CargoCrate
            }
            NativeArtifactKind::NpmTarball => {
                backend_library::RegistryNativeArtifactKind::NpmTarball
            }
            NativeArtifactKind::PythonSdist => {
                backend_library::RegistryNativeArtifactKind::PythonSdist
            }
            NativeArtifactKind::PythonWheel => {
                backend_library::RegistryNativeArtifactKind::PythonWheel
            }
            NativeArtifactKind::PythonSignature => {
                backend_library::RegistryNativeArtifactKind::PythonSignature
            }
            NativeArtifactKind::MavenJar => backend_library::RegistryNativeArtifactKind::MavenJar,
            NativeArtifactKind::MavenSources => {
                backend_library::RegistryNativeArtifactKind::MavenSources
            }
            NativeArtifactKind::MavenPom => backend_library::RegistryNativeArtifactKind::MavenPom,
            NativeArtifactKind::MavenSignature => {
                backend_library::RegistryNativeArtifactKind::MavenSignature
            }
            NativeArtifactKind::MavenChecksum => {
                backend_library::RegistryNativeArtifactKind::MavenChecksum
            }
            NativeArtifactKind::NugetPackage => {
                backend_library::RegistryNativeArtifactKind::NugetPackage
            }
            NativeArtifactKind::GoSource => backend_library::RegistryNativeArtifactKind::GoSource,
            NativeArtifactKind::ConanSource => {
                backend_library::RegistryNativeArtifactKind::ConanSource
            }
            NativeArtifactKind::ConanRecipe => {
                backend_library::RegistryNativeArtifactKind::ConanRecipe
            }
            NativeArtifactKind::Other => backend_library::RegistryNativeArtifactKind::Other,
        },
        requires_python: artifact.requires_python().map(ToOwned::to_owned),
        size: artifact.size(),
        yanked,
        yanked_reason: artifact.yanked_reason().map(ToOwned::to_owned),
    }
}

/// Stateless native registry adapter for one package feed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EcosystemAdapter {
    endpoint: RegistryEndpoint,
    package: PackageName,
    namespace: Option<PackageName>,
    target_version: Option<PackageVersion>,
}

impl EcosystemAdapter {
    pub(crate) const fn endpoint(&self) -> &RegistryEndpoint {
        &self.endpoint
    }
    /// Admits one package-scoped native feed.
    ///
    /// Maven and C/C++ coordinates require a namespace; other ecosystems
    /// reject one so the feed identity cannot silently change shape.
    ///
    /// # Errors
    /// Returns invalid configuration for a missing or unexpected namespace.
    pub fn new(
        endpoint: RegistryEndpoint,
        package: PackageName,
        namespace: Option<PackageName>,
    ) -> Result<Self, AcquisitionError> {
        let namespace_is_valid = match endpoint.ecosystem() {
            RegistryEcosystem::Cpp | RegistryEcosystem::Maven => namespace.is_some(),
            RegistryEcosystem::Npm | RegistryEcosystem::Golang => true,
            RegistryEcosystem::Cargo | RegistryEcosystem::Pypi | RegistryEcosystem::Nuget => {
                namespace.is_none()
            }
        };
        if !namespace_is_valid {
            return Err(AcquisitionError::InvalidConfiguration);
        }
        Ok(Self {
            endpoint,
            package,
            namespace,
            target_version: None,
        })
    }

    /// Admits one exact release from a native package feed.
    ///
    /// Native package documents commonly contain hundreds or thousands of
    /// historical versions. A version-pinned product request must retain the
    /// source document as its provenance while admitting only the requested
    /// release, otherwise one `add` turns into an accidental full-registry
    /// ingest. The target is part of the feed snapshot identity, so a cursor
    /// for one release can never suppress a later release request.
    pub fn new_with_version(
        endpoint: RegistryEndpoint,
        package: PackageName,
        namespace: Option<PackageName>,
        target_version: PackageVersion,
    ) -> Result<Self, AcquisitionError> {
        let mut adapter = Self::new(endpoint, package, namespace)?;
        adapter.target_version = Some(target_version);
        Ok(adapter)
    }

    /// Builds the fixed metadata URL for this ecosystem and package.
    #[must_use]
    pub fn metadata_url(&self) -> String {
        let package = component(self.package.as_str());
        match self.endpoint.ecosystem() {
            RegistryEcosystem::Cargo => format!(
                "{}/{}",
                self.endpoint.url(),
                cargo_sparse_path(self.package.as_str())
            ),
            RegistryEcosystem::Npm => self.namespace.as_ref().map_or_else(
                || format!("{}/{package}", self.endpoint.url()),
                |namespace| {
                    format!(
                        "{}/{}%2F{package}",
                        self.endpoint.url(),
                        path_components(namespace.as_str())
                    )
                },
            ),
            RegistryEcosystem::Pypi => format!(
                "{}/simple/{}/",
                self.endpoint.url(),
                normalized_pypi_name(self.package.as_str())
            ),
            RegistryEcosystem::Maven => {
                let namespace = self.namespace_path('/');
                format!(
                    "{}/{namespace}/{package}/maven-metadata.xml",
                    self.endpoint.url()
                )
            }
            RegistryEcosystem::Nuget => nuget_service_index_url(self.endpoint.url()),
            RegistryEcosystem::Golang => self.namespace.as_ref().map_or_else(
                || {
                    format!(
                        "{}/{}/@v/list",
                        self.endpoint.url(),
                        go_proxy_escape(self.package.as_str())
                    )
                },
                |_| {
                    format!(
                        "{}/{}/@v/list",
                        self.endpoint.url(),
                        self.go_proxy_package()
                    )
                },
            ),
            RegistryEcosystem::Cpp => {
                let version = self.conan_recipe_version();
                format!(
                    "{}/v2/conans/{}/{}/_/_/revisions",
                    self.endpoint.url(),
                    package,
                    component(version)
                )
            }
        }
    }

    /// Decodes one bounded native metadata document into sorted unique releases.
    ///
    /// # Errors
    /// Returns a protocol error for malformed coordinates, checksums, URLs,
    /// duplicate versions, or an unexpected source grammar.
    pub fn decode(&self, bytes: &[u8]) -> Result<Vec<NativeRelease>, TransportFailure> {
        if bytes.len() > MAX_NATIVE_METADATA_BYTES {
            return Err(TransportFailure::Overrun {
                measured: u64::try_from(bytes.len()).map_err(|_| TransportFailure::Bounds)?,
                limit: u64::try_from(MAX_NATIVE_METADATA_BYTES)
                    .map_err(|_| TransportFailure::Bounds)?,
            });
        }
        let mut releases = match self.endpoint.ecosystem() {
            RegistryEcosystem::Cargo => self.decode_cargo(bytes)?,
            RegistryEcosystem::Npm => self.decode_npm(bytes)?,
            RegistryEcosystem::Pypi => self.decode_python(bytes)?,
            RegistryEcosystem::Maven => return Err(TransportFailure::DownloadUnavailable),
            RegistryEcosystem::Nuget => self.decode_nuget(bytes)?,
            RegistryEcosystem::Golang => return Err(TransportFailure::DownloadUnavailable),
            RegistryEcosystem::Cpp => return Err(TransportFailure::Protocol),
        };
        if let Some(target) = &self.target_version {
            releases.retain(|release| release.coordinate.version() == target.as_str());
        }
        releases.sort_by(|left, right| left.coordinate.cmp(&right.coordinate));
        if releases
            .windows(2)
            .any(|pair| pair[0].coordinate == pair[1].coordinate)
        {
            return Err(TransportFailure::Protocol);
        }
        Ok(releases)
    }

    pub(crate) fn admit_page(
        &self,
        bytes: &[u8],
        request: FeedRequest,
    ) -> Result<FeedPage, TransportFailure> {
        let releases = self.decode(bytes)?;
        let snapshot = self.page_identity(bytes);
        let mut prefix = [0_u8; 24];
        prefix.copy_from_slice(&snapshot.as_bytes()[..24]);
        let current = request.cursor.token();
        let start = if current == [0; 32] || current[..24] != prefix {
            0
        } else {
            usize::try_from(u64::from_be_bytes(
                current[24..]
                    .try_into()
                    .map_err(|_| TransportFailure::Protocol)?,
            ))
            .map_err(|_| TransportFailure::Bounds)?
        };
        if start > releases.len() {
            return Err(TransportFailure::Protocol);
        }
        let end = start.saturating_add(request.max_items).min(releases.len());
        let packages = releases[start..end]
            .iter()
            .map(|release| RemotePackage {
                coordinate: release.coordinate.clone(),
                integrity: ArchiveIntegrity::Native(release.checksum.clone()),
                provenance: release.provenance,
                facts: release.facts,
                native_metadata: release.native_metadata.clone(),
                advisory: None,
                dependency_facts: release.dependency_facts.clone(),
                archive_url: Arc::from(release.archive_url.as_str()),
            })
            .collect();
        if releases.is_empty() && self.target_version.is_some() {
            return Ok(FeedPage {
                base: request.cursor,
                next_token: request.cursor.token(),
                packages,
            });
        }
        let mut next_token = [0_u8; 32];
        next_token[..24].copy_from_slice(&prefix);
        next_token[24..].copy_from_slice(
            &u64::try_from(end)
                .map_err(|_| TransportFailure::Bounds)?
                .to_be_bytes(),
        );
        Ok(FeedPage {
            base: request.cursor,
            next_token,
            packages,
        })
    }

    fn release(
        &self,
        version: &str,
        archive_url: String,
        checksum: RegistryChecksum,
        provenance: &[u8],
    ) -> Result<NativeRelease, TransportFailure> {
        self.release_with_facts(
            version,
            archive_url,
            checksum,
            provenance,
            ReleaseFacts::default(),
        )
    }

    fn release_with_facts(
        &self,
        version: &str,
        archive_url: String,
        checksum: RegistryChecksum,
        provenance: &[u8],
        facts: ReleaseFacts,
    ) -> Result<NativeRelease, TransportFailure> {
        let version = PackageVersion::new(version).map_err(|_| TransportFailure::Protocol)?;
        let archive_url = resolve_archive_url(&self.metadata_url(), &archive_url)?;
        if !allowed_archive_authority(self.endpoint.ecosystem(), self.endpoint.url(), &archive_url)
        {
            return Err(TransportFailure::Configuration);
        }
        let name = match self.namespace.as_ref() {
            None => self.package.clone(),
            Some(namespace) => {
                // Keep a namespaced native coordinate lossless when it enters
                // the shared owner. The adapter still uses the separate
                // namespace/package components for URL construction.
                let separator = match self.endpoint.ecosystem() {
                    RegistryEcosystem::Maven | RegistryEcosystem::Cpp => ':',
                    _ => '/',
                };
                PackageName::new(format!(
                    "{}{separator}{}",
                    namespace.as_str(),
                    self.package.as_str()
                ))
                .map_err(|_| TransportFailure::Protocol)?
            }
        };
        let native_metadata = backend_library::RegistryNativeMetadata::unavailable(
            self.endpoint.ecosystem(),
            "native adapter did not record ecosystem details",
        );
        let native_metadata_version = native_metadata
            .identity()
            .map_err(|_| TransportFailure::Protocol)?;
        Ok(NativeRelease {
            coordinate: coordinate_from_registry_parts(
                self.endpoint.ecosystem(),
                name.as_str(),
                version.as_str(),
            )
            .map_err(|_| TransportFailure::Protocol)?,
            archive_url,
            checksum,
            provenance: ProvenanceDigest::from_authenticated_feed(
                *blake3::hash(provenance).as_bytes(),
            ),
            facts: facts.with_native_metadata(native_metadata_version),
            dependency_facts: backend_library::DependencyFacts::Unavailable(
                backend_library::ProductText::new("native feed omits dependency metadata")
                    .map_err(|_| TransportFailure::Protocol)?,
            ),
            artifacts: Box::new([]),
            features: Box::new([]),
            dist_tags: Arc::from([]),
            standing_reason: None,
            requires_python: None,
            native_metadata,
        })
    }

    fn namespace_path(&self, separator: char) -> String {
        self.namespace.as_ref().map_or_else(String::new, |value| {
            component(value.as_str()).replace('.', &separator.to_string())
        })
    }

    fn slash_qualified_package(&self) -> String {
        self.namespace.as_ref().map_or_else(
            || component(self.package.as_str()),
            |namespace| {
                format!(
                    "{}/{}",
                    path_components(namespace.as_str()),
                    component(self.package.as_str())
                )
            },
        )
    }

    fn go_proxy_package(&self) -> String {
        go_proxy_escape(&self.slash_qualified_package())
    }

    pub(crate) fn ecosystem(&self) -> RegistryEcosystem {
        self.endpoint.ecosystem()
    }

    pub(crate) fn endpoint_url(&self) -> &str {
        self.endpoint.url()
    }

    pub(crate) fn package_name(&self) -> &str {
        self.package.as_str()
    }

    pub(crate) fn namespace_name(&self) -> Option<&str> {
        self.namespace.as_ref().map(PackageName::as_str)
    }

    pub(crate) fn conan_recipe_version(&self) -> &str {
        self.target_version()
            .or_else(|| self.namespace_name())
            .expect("validated Conan recipe version")
    }

    pub(crate) fn release_from_checksum(
        &self,
        version: &str,
        archive_url: String,
        checksum: RegistryChecksum,
        provenance: &[u8],
        facts: ReleaseFacts,
    ) -> Result<NativeRelease, TransportFailure> {
        self.release_with_facts(version, archive_url, checksum, provenance, facts)
    }

    pub(crate) fn admit_releases(
        &self,
        releases: Vec<NativeRelease>,
        source: &[u8],
        request: FeedRequest,
    ) -> Result<FeedPage, TransportFailure> {
        let snapshot = self.page_identity(source);
        let mut prefix = [0_u8; 24];
        prefix.copy_from_slice(&snapshot.as_bytes()[..24]);
        let current = request.cursor.token();
        let start = if current == [0; 32] || current[..24] != prefix {
            0
        } else {
            usize::try_from(u64::from_be_bytes(
                current[24..]
                    .try_into()
                    .map_err(|_| TransportFailure::Protocol)?,
            ))
            .map_err(|_| TransportFailure::Bounds)?
        };
        if start > releases.len() {
            return Err(TransportFailure::Protocol);
        }
        let end = start.saturating_add(request.max_items).min(releases.len());
        let packages = releases[start..end]
            .iter()
            .map(|release| RemotePackage {
                coordinate: release.coordinate.clone(),
                integrity: ArchiveIntegrity::Native(release.checksum.clone()),
                provenance: release.provenance,
                facts: release.facts,
                native_metadata: release.native_metadata.clone(),
                advisory: None,
                dependency_facts: release.dependency_facts.clone(),
                archive_url: Arc::from(release.archive_url.as_str()),
            })
            .collect();
        if releases.is_empty() && self.target_version.is_some() {
            return Ok(FeedPage {
                base: request.cursor,
                next_token: request.cursor.token(),
                packages,
            });
        }
        let mut next_token = [0_u8; 32];
        next_token[..24].copy_from_slice(&prefix);
        next_token[24..].copy_from_slice(
            &u64::try_from(end)
                .map_err(|_| TransportFailure::Bounds)?
                .to_be_bytes(),
        );
        Ok(FeedPage {
            base: request.cursor,
            next_token,
            packages,
        })
    }

    pub(crate) fn target_version(&self) -> Option<&str> {
        self.target_version.as_ref().map(PackageVersion::as_str)
    }

    pub(crate) fn page_identity(&self, source: &[u8]) -> blake3::Hash {
        let Some(target) = &self.target_version else {
            return blake3::hash(source);
        };
        let mut hasher = blake3::Hasher::new();
        hasher.update(source);
        hasher.update(b"\0nudox-target-release\0");
        hasher.update(target.as_str().as_bytes());
        hasher.finalize()
    }
}

#[path = "ecosystem_decoders.rs"]
mod decoders;
pub(crate) use decoders::{ConanFileEntry, ConanSourceAvailability, conan_dependency_facts};

fn component(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@') {
            output.push(char::from(byte));
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            output.push('%');
            output.push(char::from(HEX[usize::from(byte >> 4)]));
            output.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    output
}

fn path_components(value: &str) -> String {
    value
        .split('/')
        .map(component)
        .collect::<Vec<_>>()
        .join("/")
}

fn go_proxy_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            'A'..='Z' => {
                escaped.push('!');
                escaped.extend(character.to_lowercase());
            }
            '!' => escaped.push_str("!!"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn normalized_pypi_name(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut separator = false;
    for character in value.chars() {
        if matches!(character, '-' | '_' | '.') {
            if !separator {
                normalized.push('-');
                separator = true;
            }
        } else {
            normalized.extend(character.to_lowercase());
            separator = false;
        }
    }
    normalized
}

fn cargo_sparse_path(name: &str) -> String {
    let name = name.to_ascii_lowercase();
    match name.len() {
        1 => format!("1/{name}"),
        2 => format!("2/{name}"),
        3 => format!("3/{}/{name}", &name[..1]),
        _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
    }
}

/// Resolves a native registry file URL against the metadata request URL.
///
/// PEP 691 explicitly permits relative file links and private mirrors often
/// use them for all three supported ecosystems. Resolution is performed here,
/// before the URL reaches the transport, so an adapter can validate the final
/// authority exactly once and the transport never has to guess at URL bases.
pub(crate) fn resolve_archive_url(base: &str, raw: &str) -> Result<String, TransportFailure> {
    if raw.is_empty()
        || raw
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        || raw.contains(['#', '?', '\\'])
    {
        return Err(TransportFailure::Protocol);
    }
    let base_uri = base
        .parse::<ureq::http::Uri>()
        .map_err(|_| TransportFailure::Protocol)?;
    let (scheme, authority) = (
        base_uri.scheme_str().ok_or(TransportFailure::Protocol)?,
        base_uri.authority().ok_or(TransportFailure::Protocol)?,
    );

    let absolute = raw.parse::<ureq::http::Uri>();
    let is_absolute = absolute
        .as_ref()
        .is_ok_and(|uri| uri.scheme_str().is_some() && uri.authority().is_some());
    let (resolved_scheme, resolved_authority, raw_path) = if is_absolute {
        let uri = absolute.map_err(|_| TransportFailure::Protocol)?;
        let scheme = uri.scheme_str().ok_or(TransportFailure::Protocol)?;
        let authority = uri.authority().ok_or(TransportFailure::Protocol)?;
        (
            scheme.to_owned(),
            authority.as_str().to_owned(),
            uri.path().to_owned(),
        )
    } else if let Some(raw) = raw.strip_prefix("//") {
        let uri = format!("{scheme}://{raw}")
            .parse::<ureq::http::Uri>()
            .map_err(|_| TransportFailure::Protocol)?;
        let authority = uri.authority().ok_or(TransportFailure::Protocol)?;
        (
            scheme.to_owned(),
            authority.as_str().to_owned(),
            uri.path().to_owned(),
        )
    } else {
        (
            scheme.to_owned(),
            authority.as_str().to_owned(),
            raw.to_owned(),
        )
    };

    let path = if is_absolute || raw.starts_with("//") {
        raw_path
    } else {
        let base_path = base_uri.path();
        let path = if raw.starts_with('/') {
            raw.to_owned()
        } else {
            let directory = base_path.rsplit_once('/').map_or("/", |(prefix, _)| {
                if prefix.is_empty() { "/" } else { prefix }
            });
            format!("{directory}/{raw}")
        };
        path
    };
    // Credentials in a registry-provided archive URL are never meaningful to
    // the native feed: accepting them would let metadata smuggle secrets into
    // the transport and makes authority checks ambiguous. Authentication is
    // supplied by the configured registry transport instead.
    if resolved_authority.contains('@') {
        return Err(TransportFailure::Protocol);
    }
    let normalized_path = normalize_url_path(&path)?;
    let resolved = format!("{resolved_scheme}://{resolved_authority}{normalized_path}");
    let uri = resolved
        .parse::<ureq::http::Uri>()
        .map_err(|_| TransportFailure::Protocol)?;
    if uri.scheme_str().is_none() || uri.authority().is_none() {
        return Err(TransportFailure::Protocol);
    }
    Ok(resolved)
}

fn normalize_url_path(path: &str) -> Result<String, TransportFailure> {
    if path.is_empty() || !path.starts_with('/') {
        return Err(TransportFailure::Protocol);
    }
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop().ok_or(TransportFailure::Configuration)?;
            }
            value if value.bytes().any(|byte| byte.is_ascii_control()) => {
                return Err(TransportFailure::Protocol);
            }
            value => components.push(value),
        }
    }
    let mut normalized = String::from('/');
    normalized.push_str(&components.join("/"));
    Ok(normalized)
}

fn nuget_service_index_url(endpoint: &str) -> String {
    if endpoint.ends_with("/index.json") {
        endpoint.to_owned()
    } else if endpoint.ends_with("/v3") {
        format!("{endpoint}/index.json")
    } else {
        format!("{endpoint}/v3/index.json")
    }
}

fn decode_hex(value: &str, bytes: usize) -> Result<Vec<u8>, TransportFailure> {
    if value.len() != bytes * 2 {
        return Err(TransportFailure::Protocol);
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| Ok((hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?))
        .collect()
}

fn hex_nibble(value: u8) -> Result<u8, TransportFailure> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(TransportFailure::Protocol),
    }
}

fn allowed_archive_authority(ecosystem: RegistryEcosystem, base: &str, archive: &str) -> bool {
    let (Ok(base), Ok(archive)) = (
        base.parse::<ureq::http::Uri>(),
        archive.parse::<ureq::http::Uri>(),
    ) else {
        return false;
    };
    let Some(archive_scheme) = archive.scheme_str() else {
        return false;
    };
    let Some(archive_authority) = archive.authority() else {
        return false;
    };
    let same_authority = base.authority() == Some(archive_authority);
    let official_cargo_split = ecosystem == RegistryEcosystem::Cargo
        && base
            .authority()
            .is_some_and(|authority| authority.host().eq_ignore_ascii_case("index.crates.io"))
        && archive_authority
            .host()
            .eq_ignore_ascii_case("static.crates.io");
    let official_pypi_split = ecosystem == RegistryEcosystem::Pypi
        && base
            .authority()
            .is_some_and(|authority| authority.host().eq_ignore_ascii_case("pypi.org"))
        && archive_authority
            .host()
            .eq_ignore_ascii_case("files.pythonhosted.org");
    let official_conan_split = ecosystem == RegistryEcosystem::Cpp
        && base
            .authority()
            .is_some_and(|authority| authority.host().eq_ignore_ascii_case("center2.conan.io"))
        && (archive_authority.host().eq_ignore_ascii_case("zlib.net")
            || archive_authority.host().eq_ignore_ascii_case("github.com"));
    // NuGet advertises its registration and flat-container authorities in the
    // service index. The transport admits those authorities from that signed
    // configuration before downloading the archive; the adapter only checks
    // that the URL is an HTTPS resource without embedded credentials.
    let nuget_resource = ecosystem == RegistryEcosystem::Nuget;

    archive.query().is_none()
        && !archive.to_string().contains('#')
        && archive_authority.as_str().find('@').is_none()
        && ((archive_scheme == "https")
            || (archive_scheme == "http"
                && archive_authority
                    .host()
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())))
        && (same_authority
            || official_cargo_split
            || official_pypi_split
            || official_conan_split
            || nuget_resource)
}
