//! Pure native-registry URL and metadata adapters.
//!
//! These adapters contain no cursor, retry, storage, or publication state.
//! They normalize seven source grammars into one release descriptor consumed
//! by the shared acquisition owner.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256, Sha512};

use super::identity::coordinate_from_registry_parts;
use super::transport::ArchiveIntegrity;
use super::{
    AcquisitionError, FeedPage, FeedRequest, PackageCoordinate, PackageName, PackageVersion,
    ProvenanceDigest, RegistryEcosystem, RegistryEndpoint, ReleaseFacts, RemotePackage,
    TransportFailure,
};
use std::sync::Arc;

/// Registry checksum algorithm declared by native metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChecksumAlgorithm {
    /// SHA-256 raw archive digest.
    Sha256,
    /// SHA-512 raw archive digest.
    Sha512,
}

/// Parsed native checksum claim with a closed algorithm.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryChecksum {
    algorithm: ChecksumAlgorithm,
    pub(crate) bytes: Box<[u8]>,
}

impl RegistryChecksum {
    fn sha256_hex(value: &str) -> Result<Self, TransportFailure> {
        Ok(Self {
            algorithm: ChecksumAlgorithm::Sha256,
            bytes: decode_hex(value, 32)?.into(),
        })
    }

    fn sha512_base64(value: &str) -> Result<Self, TransportFailure> {
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

    /// Verifies exact archive bytes against the registry-native content claim.
    #[must_use]
    pub fn verifies(&self, archive: &[u8]) -> bool {
        match self.algorithm {
            ChecksumAlgorithm::Sha256 => Sha256::digest(archive).as_slice() == self.bytes.as_ref(),
            ChecksumAlgorithm::Sha512 => Sha512::digest(archive).as_slice() == self.bytes.as_ref(),
        }
    }

    /// Declared checksum algorithm.
    #[must_use]
    pub const fn algorithm(&self) -> ChecksumAlgorithm {
        self.algorithm
    }
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
}

/// Stateless native registry adapter for one package feed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EcosystemAdapter {
    endpoint: RegistryEndpoint,
    package: PackageName,
    namespace: Option<PackageName>,
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
            RegistryEcosystem::Maven | RegistryEcosystem::Cpp => namespace.is_some(),
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
        })
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
                "{}/pypi/{}/json",
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
            RegistryEcosystem::Nuget => {
                format!(
                    "{}/{}/index.json",
                    self.endpoint.url(),
                    package.to_ascii_lowercase()
                )
            }
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
                let namespace = self.namespace_path('/');
                format!(
                    "{}/v2/conans/{package}/{namespace}/revisions",
                    self.endpoint.url()
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
        let mut releases = match self.endpoint.ecosystem() {
            RegistryEcosystem::Cargo => self.decode_cargo(bytes)?,
            RegistryEcosystem::Npm => self.decode_npm(bytes)?,
            RegistryEcosystem::Pypi => self.decode_python(bytes)?,
            RegistryEcosystem::Maven => self.decode_maven(bytes)?,
            RegistryEcosystem::Nuget => self.decode_nuget(bytes)?,
            RegistryEcosystem::Golang => self.decode_go(bytes)?,
            RegistryEcosystem::Cpp => self.decode_cpp(bytes)?,
        };
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
        let snapshot = blake3::hash(bytes);
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
                archive_url: Arc::from(release.archive_url.as_str()),
            })
            .collect();
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
            facts,
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
}

#[path = "ecosystem_decoders.rs"]
mod decoders;

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
    match name.len() {
        1 => format!("1/{name}"),
        2 => format!("2/{name}"),
        3 => format!("3/{}/{name}", &name[..1]),
        _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
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

    archive.query().is_none()
        && !archive.to_string().contains('#')
        && archive_authority.as_str().find('@').is_none()
        && ((archive_scheme == "https")
            || (archive_scheme == "http"
                && archive_authority
                    .host()
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())))
        && (same_authority || official_cargo_split || official_pypi_split)
}
