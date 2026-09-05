//! Typed package-url admission for local package compilation.
//!
//! The parser retains one exact owned spelling and validated component ranges.
//! No package resolver receives an unclassified ecosystem string or an
//! unpinned coordinate.

use core::{ops::Deref, ops::Index};

use compiler_vocabulary::{Language, LanguageProfile};

use crate::GenerateTarget;

/// Maximum canonical package-url bytes accepted by application transports.
pub const MAX_PACKAGE_URL_BYTES: usize = 2_048;

/// One supported package ecosystem with exactly one compiler language family.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PackageEcosystem {
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
    /// Explicit local C/C++ package coordinate.
    Generic,
}

impl PackageEcosystem {
    /// The only compiler language family this ecosystem can enter.
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
}

/// One validated half-open byte range in the retained package-url spelling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageTextRange {
    /// First included UTF-8 byte.
    pub start: u16,
    /// First excluded UTF-8 byte.
    pub end: u16,
}

/// Immutable parsed facts exposed by [`PackageUrl`] through validated dereferencing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageUrlFacts {
    /// Closed package ecosystem.
    pub ecosystem: PackageEcosystem,
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
#[derive(Debug, Eq, PartialEq)]
pub struct PackageUrl {
    text: Box<str>,
    facts: PackageUrlFacts,
}

impl Deref for PackageUrl {
    type Target = PackageUrlFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl AsRef<str> for PackageUrl {
    fn as_ref(&self) -> &str {
        &self.text
    }
}

impl Index<PackageTextRange> for PackageUrl {
    type Output = str;

    fn index(&self, range: PackageTextRange) -> &Self::Output {
        &self.text[usize::from(range.start)..usize::from(range.end)]
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

/// Rejected package URL retaining both the exact input owner and typed cause.
#[derive(Debug, Eq, PartialEq)]
pub struct RejectedPackageUrl {
    /// Exact input spelling returned to the caller without a second copy.
    pub text: String,
    /// Closed structural rejection.
    pub error: PackageUrlError,
}

/// Closed package-url structural rejection with exact byte coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageUrlError {
    /// The input is empty.
    Empty,
    /// The input exceeds the application package-url budget.
    TooLong {
        /// Observed UTF-8 bytes.
        observed: usize,
        /// Maximum accepted bytes.
        maximum: usize,
    },
    /// The URL did not begin with the canonical `pkg:` scheme.
    Scheme,
    /// The package type was absent or outside the seven supported ecosystems.
    Ecosystem {
        /// Exact rejected type bytes.
        range: PackageTextRange,
    },
    /// No nonempty package name followed the type and optional namespace.
    Name,
    /// A nonempty pinned version was absent.
    Version,
    /// Query, fragment, or coordinate punctuation appeared more than once or out of order.
    Delimiter {
        /// First offending byte.
        offset: u16,
    },
    /// A component contains a byte outside canonical package-url spelling.
    Character {
        /// Offending byte coordinate.
        offset: u16,
        /// Exact rejected byte.
        observed: u8,
    },
    /// A percent escape was truncated, non-hexadecimal, or lower-case.
    Escape {
        /// Percent byte coordinate.
        offset: u16,
    },
    /// Qualifier keys were empty, repeated, or not strictly ascending.
    QualifierOrder {
        /// First byte of the offending qualifier.
        offset: u16,
    },
    /// A qualifier omitted its `=` separator or nonempty value.
    QualifierValue {
        /// First byte of the offending qualifier.
        offset: u16,
    },
}

/// Immutable package compilation facts exposed by [`PackageCompileRequest`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageCompileFacts {
    /// Compiler profile, stage, and caller correlation.
    pub target: GenerateTarget,
    /// Ecosystem already proven compatible with `target.profile`.
    pub ecosystem: PackageEcosystem,
}

/// One profile-compatible, pinned package request.
#[derive(Debug, Eq, PartialEq)]
pub struct PackageCompileRequest {
    facts: PackageCompileFacts,
    package: PackageUrl,
}

impl PackageCompileRequest {
    /// Binds a validated package URL to its one compatible language profile.
    ///
    /// # Errors
    ///
    /// Returns both closed families when a caller attempts to route a package
    /// through a foreign language adapter.
    pub fn new(
        target: GenerateTarget,
        package: PackageUrl,
    ) -> Result<Self, PackageProfileMismatch> {
        let ecosystem = package.ecosystem;
        let profile_language = target.profile.language();
        let package_language = ecosystem.language();
        if profile_language != package_language {
            return Err(PackageProfileMismatch {
                profile: target.profile,
                ecosystem,
            });
        }
        Ok(Self {
            facts: PackageCompileFacts { target, ecosystem },
            package,
        })
    }

    pub(crate) const fn correlation(&self) -> crate::CorrelationId {
        self.facts.target.correlation
    }
}

impl Deref for PackageCompileRequest {
    type Target = PackageCompileFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl AsRef<PackageUrl> for PackageCompileRequest {
    fn as_ref(&self) -> &PackageUrl {
        &self.package
    }
}

/// Exact profile/ecosystem mismatch rejected before package resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageProfileMismatch {
    /// Requested compiler profile.
    pub profile: LanguageProfile,
    /// Package ecosystem incompatible with the profile.
    pub ecosystem: PackageEcosystem,
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
    let Some(body) = text.strip_prefix("pkg:") else {
        return Err(PackageUrlError::Scheme);
    };
    let body_start = text.len() - body.len();
    let (before_fragment, subpath) = split_once(body, '#', body_start)?;
    let (coordinates, qualifiers) = split_once(before_fragment, '?', body_start)?;
    let Some(type_end) = coordinates.find('/') else {
        return Err(PackageUrlError::Name);
    };
    let ecosystem_range = range(body_start, body_start + type_end);
    let ecosystem =
        ecosystem(&text[usize::from(ecosystem_range.start)..usize::from(ecosystem_range.end)])
            .ok_or(PackageUrlError::Ecosystem {
                range: ecosystem_range,
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
        ecosystem,
        namespace,
        name: range(name_start, name_start + name.len()),
        version: range(version_start, version_start + version_text.len()),
        qualifiers,
        subpath,
    })
}

fn split_once(
    value: &str,
    delimiter: char,
    global_start: usize,
) -> Result<(&str, Option<(usize, &str)>), PackageUrlError> {
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

fn ecosystem(value: &str) -> Option<PackageEcosystem> {
    match value {
        "cargo" => Some(PackageEcosystem::Cargo),
        "npm" => Some(PackageEcosystem::Npm),
        "pypi" => Some(PackageEcosystem::Pypi),
        "golang" => Some(PackageEcosystem::Golang),
        "maven" => Some(PackageEcosystem::Maven),
        "nuget" => Some(PackageEcosystem::Nuget),
        "generic" => Some(PackageEcosystem::Generic),
        _ => None,
    }
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
            if !escaped.iter().all(u8::is_ascii_digit)
                && !escaped.iter().all(|byte| (b'A'..=b'F').contains(byte))
                && !(escaped[0].is_ascii_digit() && (b'A'..=b'F').contains(&escaped[1]))
                && !((b'A'..=b'F').contains(&escaped[0]) && escaped[1].is_ascii_digit())
            {
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
