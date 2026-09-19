//! Pure npm package authority: PURLs, registry metadata, tarballs, and roots.
#![allow(
    missing_docs,
    reason = "The public package vocabulary is documented at its capability level; field names are the wire facts."
)]

use std::{collections::BTreeMap, fmt, io::Read, str::FromStr};

use flate2::read::GzDecoder;
use serde::Deserialize;
use sha2::{Digest, Sha512};
use thiserror::Error;

/// Maximum number of members accepted in one package archive.
pub const MAX_TARBALL_MEMBERS: usize = 4_096;
/// Maximum decompressed archive size. This bounds both memory and parsing work.
pub const MAX_TARBALL_UNCOMPRESSED_BYTES: usize = 512 * 1024 * 1024;

/// A syntactically valid npm package URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackagePurl {
    name: String,
    version: String,
    subpath: Option<String>,
}

impl PackagePurl {
    /// Parses an npm PURL.
    pub fn parse(input: &str) -> Result<Self, PackagePurlError> {
        input.parse()
    }
    /// Package name, including a scope when present.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Requested package version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }
    /// Optional package-relative subpath.
    #[must_use]
    pub fn subpath(&self) -> Option<&str> {
        self.subpath.as_deref()
    }
}

impl FromStr for PackagePurl {
    type Err = PackagePurlError;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if !input.starts_with("npm:") {
            return Err(PackagePurlError::Scheme {
                input: input.to_owned(),
            });
        }
        let body = &input[4..];
        if body.is_empty() {
            return Err(PackagePurlError::EmptyName {
                input: input.to_owned(),
            });
        }
        if body.chars().any(char::is_whitespace) {
            return Err(PackagePurlError::Whitespace {
                input: input.to_owned(),
            });
        }
        let at = if body.starts_with('@') {
            body[1..].find('@').map(|offset| offset + 1)
        } else {
            body.find('@')
        };
        let Some(at) = at else {
            return Err(PackagePurlError::MissingVersion {
                input: input.to_owned(),
            });
        };
        let (name, remainder) = body.split_at(at);
        let remainder = &remainder[1..];
        let (version, subpath) = remainder
            .split_once('/')
            .map_or((remainder, None), |(a, b)| (a, Some(b)));
        if subpath.is_some_and(str::is_empty) {
            return Err(PackagePurlError::EmptySubpath {
                input: input.to_owned(),
            });
        }
        let scoped_parts = name
            .strip_prefix('@')
            .and_then(|value| value.split_once('/'));
        if name.is_empty()
            || (name.starts_with('@')
                && scoped_parts
                    .is_none_or(|(scope, package)| scope.is_empty() || package.is_empty()))
            || name.ends_with('/')
        {
            return Err(PackagePurlError::EmptyName {
                input: input.to_owned(),
            });
        }
        if version.is_empty() {
            return Err(PackagePurlError::MissingVersion {
                input: input.to_owned(),
            });
        }
        if !name.starts_with('@') && name.contains('/')
            || name.starts_with('@') && name.matches('/').count() != 1
        {
            return Err(PackagePurlError::MalformedName {
                input: input.to_owned(),
            });
        }
        Ok(Self {
            name: name.to_owned(),
            version: version.to_owned(),
            subpath: subpath.map(str::to_owned),
        })
    }
}

impl fmt::Display for PackagePurl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "npm:{}@{}", self.name, self.version)?;
        if let Some(subpath) = &self.subpath {
            write!(formatter, "/{subpath}")?;
        }
        Ok(())
    }
}

/// PURL syntax rejection.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PackagePurlError {
    #[error("not an npm PURL: {input}")]
    Scheme { input: String },
    #[error("PURL has an empty package name: {input}")]
    EmptyName { input: String },
    #[error("PURL has no version: {input}")]
    MissingVersion { input: String },
    #[error("PURL has a malformed package name: {input}")]
    MalformedName { input: String },
    #[error("PURL contains whitespace: {input}")]
    Whitespace { input: String },
    #[error("PURL has an empty subpath: {input}")]
    EmptySubpath { input: String },
}

#[derive(Debug, Deserialize, Default)]
struct RawPackument {
    #[serde(default)]
    versions: BTreeMap<String, RawVersion>,
}
#[derive(Debug, Deserialize, Default)]
struct RawVersion {
    #[serde(default)]
    dist: RawDist,
}
#[derive(Debug, Deserialize, Default)]
struct RawDist {
    version: Option<String>,
    tarball: Option<String>,
    integrity: Option<String>,
}

/// The registry facts needed to fetch and authenticate a package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryMetadata {
    pub version: String,
    pub tarball: String,
    pub integrity: String,
}

/// Decodes a registry packument and selects exactly one version entry.
pub fn decode_packument(bytes: &[u8], requested: &str) -> Result<RegistryMetadata, PackageError> {
    let packument: RawPackument =
        serde_json::from_slice(bytes).map_err(|source| PackageError::PackumentJson { source })?;
    let entry = packument
        .versions
        .get(requested)
        .ok_or_else(|| PackageError::MissingVersion {
            version: requested.to_owned(),
        })?;
    let version = entry
        .dist
        .version
        .clone()
        .ok_or_else(|| PackageError::MissingMetadata {
            field: "dist.version",
            version: requested.to_owned(),
        })?;
    let tarball = entry
        .dist
        .tarball
        .clone()
        .ok_or_else(|| PackageError::MissingMetadata {
            field: "dist.tarball",
            version: requested.to_owned(),
        })?;
    let integrity = entry
        .dist
        .integrity
        .clone()
        .ok_or_else(|| PackageError::MissingMetadata {
            field: "dist.integrity",
            version: requested.to_owned(),
        })?;
    if version != requested {
        return Err(PackageError::VersionMismatch {
            requested: requested.to_owned(),
            actual: version,
        });
    }
    Ok(RegistryMetadata {
        version,
        tarball,
        integrity,
    })
}

/// One archive member. Directory bytes are always empty.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TarballMember {
    pub name: String,
    pub kind: TarballMemberKind,
    pub bytes: Vec<u8>,
}
/// The two supported ustar member kinds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TarballMemberKind {
    Regular,
    Directory,
}

/// PURL, integrity, decompression, and ustar parsing rejection.
#[derive(Debug, Error)]
pub enum PackageError {
    #[error(transparent)]
    Purl(#[from] PackagePurlError),
    #[error("packument JSON is invalid")]
    PackumentJson {
        #[source]
        source: serde_json::Error,
    },
    #[error("packument has no requested version {version}")]
    MissingVersion { version: String },
    #[error("packument version {version} is missing {field}")]
    MissingMetadata {
        field: &'static str,
        version: String,
    },
    #[error("packument key requested {requested} but dist.version is {actual}")]
    VersionMismatch { requested: String, actual: String },
    #[error("gzip/tar read failed")]
    ArchiveIo {
        #[source]
        source: std::io::Error,
    },
    #[error("archive exceeds the {limit}-byte uncompressed limit")]
    ArchiveTooLarge { limit: usize },
    #[error("archive has more than {limit} members")]
    TooManyMembers { limit: usize },
    #[error("ustar archive is truncated at byte {offset}")]
    Truncated { offset: usize },
    #[error("ustar member {name} has unsupported typeflag {typeflag}")]
    UnsupportedMemberType { name: String, typeflag: u8 },
    #[error("ustar member {name} has invalid metadata")]
    InvalidMember { name: String },
    #[error("SRI integrity is invalid: {value}")]
    InvalidIntegrity { value: String },
    #[error("tarball integrity mismatch for {value}")]
    IntegrityMismatch { value: String },
    #[error("package.json at {path} is invalid")]
    ManifestJson {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("no package root matched {name}@{version}")]
    NoPackageRoot { name: String, version: String },
    #[error("multiple package roots matched {name}@{version}: {roots:?}")]
    MultiplePackageRoots {
        name: String,
        version: String,
        roots: Vec<String>,
    },
    #[error("PURL subpath was not found: {path}")]
    MissingSubpath { path: String },
}

/// Decompresses, authenticates, and reads a bounded gzip ustar archive.
pub fn read_tarball(bytes: &[u8], integrity: &str) -> Result<Vec<TarballMember>, PackageError> {
    verify_integrity(bytes, integrity)?;
    let mut decoder = GzDecoder::new(bytes);
    let mut archive = Vec::new();
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let count = decoder
            .read(&mut chunk)
            .map_err(|source| PackageError::ArchiveIo { source })?;
        if count == 0 {
            break;
        }
        if archive
            .len()
            .checked_add(count)
            .is_none_or(|size| size > MAX_TARBALL_UNCOMPRESSED_BYTES)
        {
            return Err(PackageError::ArchiveTooLarge {
                limit: MAX_TARBALL_UNCOMPRESSED_BYTES,
            });
        }
        archive.extend_from_slice(&chunk[..count]);
    }
    parse_ustar(&archive)
}

fn verify_integrity(bytes: &[u8], integrity: &str) -> Result<(), PackageError> {
    let Some(encoded) = integrity.strip_prefix("sha512-") else {
        return Err(PackageError::InvalidIntegrity {
            value: integrity.to_owned(),
        });
    };
    let expected = decode_base64(encoded).ok_or_else(|| PackageError::InvalidIntegrity {
        value: integrity.to_owned(),
    })?;
    let actual = Sha512::digest(bytes);
    if expected.as_slice() != actual.as_slice() {
        return Err(PackageError::IntegrityMismatch {
            value: integrity.to_owned(),
        });
    }
    Ok(())
}

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(input.len() * 3 / 4);
    let mut value = 0_u32;
    let mut bits = 0_u8;
    for byte in input.bytes() {
        let digit = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        value = (value << 6) | u32::from(digit);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((value >> bits) as u8);
        }
    }
    (bits == 0 || input.ends_with('=')).then_some(output)
}

fn parse_ustar(archive: &[u8]) -> Result<Vec<TarballMember>, PackageError> {
    let mut members = Vec::new();
    let mut offset = 0;
    let mut ended = false;
    while offset < archive.len() {
        let end = offset
            .checked_add(512)
            .ok_or(PackageError::Truncated { offset })?;
        if end > archive.len() {
            return Err(PackageError::Truncated { offset });
        }
        let header = &archive[offset..end];
        if header.iter().all(|byte| *byte == 0) {
            ended = true;
            break;
        }
        let name = member_name(header)?;
        let typeflag = header[156];
        if typeflag != b'0' && typeflag != 0 && typeflag != b'5' {
            return Err(PackageError::UnsupportedMemberType { name, typeflag });
        }
        let size = parse_octal(&header[124..136])
            .ok_or_else(|| PackageError::InvalidMember { name: name.clone() })?;
        let data_end = end
            .checked_add(size)
            .ok_or(PackageError::Truncated { offset })?;
        if data_end > archive.len() {
            return Err(PackageError::Truncated { offset: data_end });
        }
        if members.len() == MAX_TARBALL_MEMBERS {
            return Err(PackageError::TooManyMembers {
                limit: MAX_TARBALL_MEMBERS,
            });
        }
        let kind = if typeflag == b'5' {
            TarballMemberKind::Directory
        } else {
            TarballMemberKind::Regular
        };
        let bytes = if kind == TarballMemberKind::Regular {
            archive[end..data_end].to_vec()
        } else {
            Vec::new()
        };
        members.push(TarballMember { name, kind, bytes });
        offset = data_end
            .checked_add((512 - (size % 512)) % 512)
            .ok_or(PackageError::Truncated { offset: data_end })?;
    }
    if !ended {
        return Err(PackageError::Truncated { offset });
    }
    Ok(members)
}

fn member_name(header: &[u8]) -> Result<String, PackageError> {
    if &header[257..263] != b"ustar\0" || &header[263..265] != b"00" {
        return Err(PackageError::InvalidMember {
            name: String::from_utf8_lossy(&header[..100])
                .trim_end_matches('\0')
                .to_owned(),
        });
    }
    let name_bytes = header[..100].split(|byte| *byte == 0).next().unwrap_or(&[]);
    let name = String::from_utf8(name_bytes.to_vec()).map_err(|_| PackageError::InvalidMember {
        name: "<non-utf8>".to_owned(),
    })?;
    let prefix_bytes = header[345..500]
        .split(|byte| *byte == 0)
        .next()
        .unwrap_or(&[]);
    let prefix = String::from_utf8(prefix_bytes.to_vec())
        .map_err(|_| PackageError::InvalidMember { name: name.clone() })?;
    if prefix.is_empty() {
        Ok(name)
    } else if name.is_empty() {
        Ok(prefix)
    } else {
        Ok(format!("{prefix}/{name}"))
    }
}

fn parse_octal(bytes: &[u8]) -> Option<usize> {
    let mut value = 0_usize;
    let mut found = false;
    for byte in bytes
        .iter()
        .copied()
        .filter(|byte| *byte != 0 && *byte != b' ')
    {
        if !(b'0'..=b'7').contains(&byte) {
            return None;
        }
        found = true;
        value = value
            .checked_mul(8)?
            .checked_add(usize::from(byte - b'0'))?;
    }
    found.then_some(value)
}

/// A package root and its manifest, borrowed from the caller's member set.
#[derive(Debug)]
pub struct LocatedPackage<'a> {
    pub root: String,
    pub manifest: &'a TarballMember,
    pub subpath: Option<&'a TarballMember>,
}

/// Finds a matching package manifest across all archive members.
pub fn locate_package<'a>(
    members: &'a [TarballMember],
    purl: &PackagePurl,
) -> Result<LocatedPackage<'a>, PackageError> {
    #[derive(Deserialize)]
    struct Manifest {
        name: Option<String>,
        version: Option<String>,
    }
    let mut matches: Vec<(String, &'a TarballMember)> = Vec::new();
    for member in members
        .iter()
        .filter(|member| member.name.ends_with("package.json"))
    {
        let manifest: Manifest =
            serde_json::from_slice(&member.bytes).map_err(|source| PackageError::ManifestJson {
                path: member.name.clone(),
                source,
            })?;
        if manifest.name.as_deref() == Some(purl.name())
            && manifest.version.as_deref() == Some(purl.version())
        {
            let root = member
                .name
                .strip_suffix("package.json")
                .unwrap_or_default()
                .trim_end_matches('/')
                .to_owned();
            matches.push((root, member));
        }
    }
    matches.sort_by(|left, right| left.0.cmp(&right.0));
    matches.dedup_by(|left, right| left.0 == right.0);
    let (root, manifest) = match matches.as_slice() {
        [] => {
            return Err(PackageError::NoPackageRoot {
                name: purl.name().to_owned(),
                version: purl.version().to_owned(),
            });
        }
        [one] => one,
        many => {
            return Err(PackageError::MultiplePackageRoots {
                name: purl.name().to_owned(),
                version: purl.version().to_owned(),
                roots: many.iter().map(|entry| entry.0.clone()).collect(),
            });
        }
    };
    let subpath = purl
        .subpath()
        .map(|subpath| {
            let path = if root.is_empty() {
                subpath.to_owned()
            } else {
                format!("{root}/{subpath}")
            };
            members
                .iter()
                .find(|member| member.name == path)
                .ok_or(PackageError::MissingSubpath { path })
        })
        .transpose()?;
    Ok(LocatedPackage {
        root: root.clone(),
        manifest,
        subpath,
    })
}
