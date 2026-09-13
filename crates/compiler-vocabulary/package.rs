//! Canonical package URL admission shared by interfaces, registries, and compilers.
//!
//! A [`PackageUrl`] owns the exact admitted spelling and stores validated byte
//! ranges into that allocation. Downstream code borrows typed components; it
//! never reparses or allocates normalized copies.

use alloc::{boxed::Box, string::String};
use core::{fmt, ops::Deref, ops::Index};

use heart_identity::{CompilationTargetDomain, ContentId};
use serde::{Deserialize, Serialize};

use crate::{Language, RegistryEcosystem};

/// Maximum canonical package URL bytes accepted at a public boundary.
pub const MAX_PACKAGE_URL_BYTES: usize = 2_048;

/// Canonical package URL `type`.
///
/// This is deliberately distinct from [`RegistryEcosystem`]. A package URL
/// describes package identity while a registry ecosystem selects an
/// acquisition protocol. In particular, local C/C++ input uses `generic`
/// while a configured C/C++ registry uses the product's `cpp` protocol.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PackageType {
    /// Rust crates.io or Cargo-vendored package.
    Cargo,
    /// TypeScript/JavaScript npm package.
    Npm,
    /// Python Package Index distribution.
    Pypi,
    /// Go module coordinate.
    Golang,
    /// Java Maven coordinate.
    Maven,
    /// C# NuGet package.
    Nuget,
    /// Explicit local C/C++ source package.
    Generic,
}

impl PackageType {
    /// Parses the exact canonical type token retained in a package URL.
    #[must_use]
    pub const fn parse(value: &str) -> Option<Self> {
        match value.as_bytes() {
            b"cargo" => Some(Self::Cargo),
            b"npm" => Some(Self::Npm),
            b"pypi" => Some(Self::Pypi),
            b"golang" => Some(Self::Golang),
            b"maven" => Some(Self::Maven),
            b"nuget" => Some(Self::Nuget),
            b"generic" => Some(Self::Generic),
            _ => None,
        }
    }

    /// Returns the canonical package URL type token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Npm => "npm",
            Self::Pypi => "pypi",
            Self::Golang => "golang",
            Self::Maven => "maven",
            Self::Nuget => "nuget",
            Self::Generic => "generic",
        }
    }

    /// Returns the semantic compiler family for this package type.
    #[must_use]
    pub const fn language(self) -> Language {
        match self {
            Self::Cargo => Language::Rust,
            Self::Npm => Language::TypeScript,
            Self::Pypi => Language::Python,
            Self::Golang => Language::Go,
            Self::Maven => Language::Java,
            Self::Nuget => Language::CSharp,
            Self::Generic => Language::Clang,
        }
    }

    /// Returns the acquisition protocol when this URL type is remotely resolvable.
    #[must_use]
    pub const fn registry(self) -> Option<RegistryEcosystem> {
        match self {
            Self::Cargo => Some(RegistryEcosystem::Cargo),
            Self::Npm => Some(RegistryEcosystem::Npm),
            Self::Pypi => Some(RegistryEcosystem::Pypi),
            Self::Golang => Some(RegistryEcosystem::Golang),
            Self::Maven => Some(RegistryEcosystem::Maven),
            Self::Nuget => Some(RegistryEcosystem::Nuget),
            Self::Generic => Some(RegistryEcosystem::Cpp),
        }
    }
}

impl RegistryEcosystem {
    /// Returns the package URL type admitted by this acquisition protocol.
    #[must_use]
    pub const fn package_type(self) -> PackageType {
        match self {
            Self::Cargo => PackageType::Cargo,
            Self::Npm => PackageType::Npm,
            Self::Pypi => PackageType::Pypi,
            Self::Maven => PackageType::Maven,
            Self::Nuget => PackageType::Nuget,
            Self::Golang => PackageType::Golang,
            Self::Cpp => PackageType::Generic,
        }
    }
}

/// One validated half-open byte range in the retained package URL spelling.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageTextRange {
    /// First included UTF-8 byte.
    pub start: u16,
    /// First excluded UTF-8 byte.
    pub end: u16,
}

/// Immutable parsed facts exposed by [`PackageUrl`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageUrlFacts {
    /// Domain-separated identity of this exact canonical package URL.
    pub identity: ContentId<CompilationTargetDomain>,
    /// Canonical package URL type.
    pub ecosystem: PackageType,
    /// Optional namespace path without its trailing slash.
    pub namespace: Option<PackageTextRange>,
    /// Required package name.
    pub name: PackageTextRange,
    /// Required pinned version.
    pub version: PackageTextRange,
    /// Canonically ordered qualifier text without the leading question mark.
    pub qualifiers: Option<PackageTextRange>,
    /// Optional package-relative subpath without the leading hash.
    pub subpath: Option<PackageTextRange>,
}

/// One exact package URL accepted only after complete structural validation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PackageUrl {
    text: Box<str>,
    facts: PackageUrlFacts,
}

impl PackageUrl {
    /// Parses and retains an exact canonical, version-pinned package URL.
    pub fn parse(text: impl Into<String>) -> Result<Self, RejectedPackageUrl> {
        Self::try_from(text.into())
    }

    /// Returns the retained canonical spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Returns the canonical package URL type.
    #[must_use]
    pub const fn package_type(&self) -> PackageType {
        self.facts.ecosystem
    }

    /// Returns the optional namespace.
    #[must_use]
    pub fn namespace(&self) -> Option<&str> {
        self.facts.namespace.map(|range| &self[range])
    }

    /// Returns the registry-native package name without its namespace.
    #[must_use]
    pub fn name(&self) -> &str {
        &self[self.facts.name]
    }

    /// Returns the registry-native package path used as semantic lineage.
    ///
    /// Namespace and name are one contiguous slice of the admitted URL. This
    /// keeps declaration identity construction on the original parse result
    /// instead of making compiler adapters rebuild or reparse coordinates.
    #[must_use]
    pub fn lineage_name(&self) -> &str {
        let start = self
            .facts
            .namespace
            .map_or(self.facts.name.start, |namespace| namespace.start);
        &self.text[usize::from(start)..usize::from(self.facts.name.end)]
    }

    /// Returns the immutable package version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self[self.facts.version]
    }

    /// Returns the canonical qualifier list without `?`.
    #[must_use]
    pub fn qualifiers(&self) -> Option<&str> {
        self.facts.qualifiers.map(|range| &self[range])
    }

    /// Returns the package-relative subpath without `#`.
    #[must_use]
    pub fn subpath(&self) -> Option<&str> {
        self.facts.subpath.map(|range| &self[range])
    }
}

impl Deref for PackageUrl {
    type Target = PackageUrlFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl AsRef<str> for PackageUrl {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Index<PackageTextRange> for PackageUrl {
    type Output = str;

    fn index(&self, range: PackageTextRange) -> &Self::Output {
        &self.text[usize::from(range.start)..usize::from(range.end)]
    }
}

impl fmt::Display for PackageUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<String> for PackageUrl {
    type Error = RejectedPackageUrl;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let facts = match parse(&text) {
            Ok(facts) => facts,
            Err(error) => return Err(RejectedPackageUrl { text, error }),
        };
        Ok(Self {
            text: text.into_boxed_str(),
            facts,
        })
    }
}

impl From<PackageUrl> for String {
    fn from(value: PackageUrl) -> Self {
        value.text.into()
    }
}

/// Rejected package URL retaining both the exact input owner and typed cause.
#[derive(Debug, Eq, PartialEq)]
pub struct RejectedPackageUrl {
    /// Exact input spelling returned without a second copy.
    pub text: String,
    /// Closed structural rejection.
    pub error: PackageUrlError,
}

impl fmt::Display for RejectedPackageUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "rejected package URL: {}", self.error)
    }
}

/// Closed package URL structural rejection with exact byte coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageUrlError {
    /// The input is empty.
    Empty,
    /// The input exceeds the package URL budget.
    TooLong { observed: usize, maximum: usize },
    /// The URL did not begin with the canonical `pkg:` scheme.
    Scheme,
    /// The package type was absent or unsupported.
    PackageType { range: PackageTextRange },
    /// No nonempty package name followed the type and optional namespace.
    Name,
    /// A nonempty pinned version was absent.
    Version,
    /// Query, fragment, or coordinate punctuation appeared repeatedly or out of order.
    Delimiter { offset: u16 },
    /// A component contains a byte outside canonical package URL spelling.
    Character { offset: u16, observed: u8 },
    /// A percent escape was truncated, non-hexadecimal, or lower-case.
    Escape { offset: u16 },
    /// Qualifier keys were empty, repeated, or not strictly ascending.
    QualifierOrder { offset: u16 },
    /// A qualifier omitted its `=` separator or nonempty value.
    QualifierValue { offset: u16 },
}

impl fmt::Display for PackageUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

fn parse(text: &str) -> Result<PackageUrlFacts, PackageUrlError> {
    if text.is_empty() {
        return Err(PackageUrlError::Empty);
    }
    if text.len() > MAX_PACKAGE_URL_BYTES || text.len() > usize::from(u16::MAX) {
        return Err(PackageUrlError::TooLong {
            observed: text.len(),
            maximum: MAX_PACKAGE_URL_BYTES,
        });
    }
    validate_bytes(text)?;
    if let Some(fragment) = text.find('#')
        && let Some(query) = text[fragment + 1..].find('?')
    {
        return Err(PackageUrlError::Delimiter {
            offset: wire_offset(fragment + 1 + query),
        });
    }
    let Some(body) = text.strip_prefix("pkg:") else {
        return Err(PackageUrlError::Scheme);
    };
    let body_start = text.len() - body.len();
    let (before_fragment, subpath) = split_once(body, '#', body_start)?;
    let (coordinates, qualifiers) = split_once(before_fragment, '?', body_start)?;
    let Some(type_end) = coordinates.find('/') else {
        return Err(PackageUrlError::Name);
    };
    let package_type_range = range(body_start, body_start + type_end);
    let package_type =
        PackageType::parse(&text[package_type_range.start.into()..package_type_range.end.into()])
            .ok_or(PackageUrlError::PackageType {
            range: package_type_range,
        })?;
    let path_start = body_start + type_end + 1;
    let package_with_version = &coordinates[type_end + 1..];
    let Some(version_separator) = package_with_version.rfind('@') else {
        return Err(PackageUrlError::Version);
    };
    let package_path = &package_with_version[..version_separator];
    let version_start = path_start + version_separator + 1;
    let version_text = &package_with_version[version_separator + 1..];
    if version_text.is_empty() {
        return Err(PackageUrlError::Version);
    }
    let (namespace, name_start, name) = match package_path.rfind('/') {
        Some(separator) => (
            Some(range(path_start, path_start + separator)),
            path_start + separator + 1,
            &package_path[separator + 1..],
        ),
        None => (None, path_start, package_path),
    };
    if name.is_empty() || namespace.is_some_and(|value| value.start == value.end) {
        return Err(PackageUrlError::Name);
    }
    let qualifiers = qualifiers
        .map(|(start, value)| {
            validate_qualifiers(value, start)?;
            Ok(range(start, start + value.len()))
        })
        .transpose()?;
    let subpath = subpath
        .map(|(start, value)| {
            if value.is_empty() {
                return Err(PackageUrlError::Delimiter {
                    offset: wire_offset(start.saturating_sub(1)),
                });
            }
            Ok(range(start, start + value.len()))
        })
        .transpose()?;
    Ok(PackageUrlFacts {
        identity: ContentId::<CompilationTargetDomain>::from_canonical_bytes(text.as_bytes()),
        ecosystem: package_type,
        namespace,
        name: range(name_start, name_start + name.len()),
        version: range(version_start, version_start + version_text.len()),
        qualifiers,
        subpath,
    })
}

type LocatedSuffix<'text> = (usize, &'text str);
type SplitText<'text> = (&'text str, Option<LocatedSuffix<'text>>);

fn split_once(
    value: &str,
    delimiter: char,
    global_start: usize,
) -> Result<SplitText<'_>, PackageUrlError> {
    let Some(offset) = value.find(delimiter) else {
        return Ok((value, None));
    };
    let rest = &value[offset + delimiter.len_utf8()..];
    if let Some(repeated) = rest.find(delimiter) {
        return Err(PackageUrlError::Delimiter {
            offset: wire_offset(global_start + offset + 1 + repeated),
        });
    }
    Ok((
        &value[..offset],
        Some((global_start + offset + delimiter.len_utf8(), rest)),
    ))
}

fn validate_bytes(value: &str) -> Result<(), PackageUrlError> {
    let bytes = value.as_bytes();
    let mut offset = 0_usize;
    while offset < bytes.len() {
        let observed = bytes[offset];
        if observed == b'%' {
            let Some(escaped) = bytes.get(offset + 1..offset + 3) else {
                return Err(PackageUrlError::Escape {
                    offset: wire_offset(offset),
                });
            };
            if !escaped.iter().all(u8::is_ascii_hexdigit)
                || escaped.iter().any(u8::is_ascii_lowercase)
            {
                return Err(PackageUrlError::Escape {
                    offset: wire_offset(offset),
                });
            }
            let decoded = (hex_nibble(escaped[0]) << 4) | hex_nibble(escaped[1]);
            if decoded.is_ascii_alphanumeric() || matches!(decoded, b'-' | b'.' | b'_' | b'~') {
                return Err(PackageUrlError::Escape {
                    offset: wire_offset(offset),
                });
            }
            offset += 3;
            continue;
        }
        if !observed.is_ascii()
            || observed.is_ascii_control()
            || observed.is_ascii_whitespace()
            || observed == b'\\'
        {
            return Err(PackageUrlError::Character {
                offset: wire_offset(offset),
                observed,
            });
        }
        offset += 1;
    }
    Ok(())
}

fn validate_qualifiers(value: &str, global_start: usize) -> Result<(), PackageUrlError> {
    if value.is_empty() {
        return Err(PackageUrlError::QualifierValue {
            offset: wire_offset(global_start),
        });
    }
    let mut previous = None;
    let mut consumed = 0_usize;
    for qualifier in value.split('&') {
        let qualifier_start = global_start + consumed;
        let Some((key, assigned)) = qualifier.split_once('=') else {
            return Err(PackageUrlError::QualifierValue {
                offset: wire_offset(qualifier_start),
            });
        };
        if key.is_empty() || assigned.is_empty() {
            return Err(PackageUrlError::QualifierValue {
                offset: wire_offset(qualifier_start),
            });
        }
        if key.bytes().any(|byte| byte.is_ascii_uppercase())
            || previous.is_some_and(|preceding| preceding >= key)
        {
            return Err(PackageUrlError::QualifierOrder {
                offset: wire_offset(qualifier_start),
            });
        }
        previous = Some(key);
        consumed += qualifier.len() + 1;
    }
    Ok(())
}

fn range(start: usize, end: usize) -> PackageTextRange {
    PackageTextRange {
        start: wire_offset(start),
        end: wire_offset(end),
    }
}

fn wire_offset(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

const fn hex_nibble(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'A'..=b'F' => value - b'A' + 10,
        _ => 0,
    }
}
