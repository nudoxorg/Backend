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

    pub(crate) fn cache_key(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.registry.archive-cache.v1\0");
        hasher.update(&[self.algorithm as u8]);
        hasher.update(self.bytes.as_ref());
        *hasher.finalize().as_bytes()
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
                advisory: None,
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
                advisory: None,
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
