//! Package identity: the coordinates that name a package and the deterministic
//! [`PackageId`] derived from them.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use thiserror::Error;

use super::Id;
use crate::ecosystem::Language;

/// Identity tag for packages. Zero-variant: it exists only to brand [`Id`], never
/// to be constructed.
pub enum Package {}

/// The stable, deterministic identity of a package across the whole system.
pub type PackageId = Id<Package>;

/// Why a raw package name was rejected.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum NameError {
    #[error("package name is empty")]
    NameEmpty,
    #[error("package name for {ecosystem} is too long (len={len}, max={max})")]
    NameTooLong { ecosystem: Language, len: usize, max: usize },
    #[error("package name for {ecosystem} contains invalid characters: {invalid_chars:?}")]
    NameHasInvalidChars { ecosystem: Language, invalid_chars: Vec<char> },
}

/// Wrapper carrying raw input + source for Cargo/npm (both use semver but
/// kept distinct so ecosystem is traceable in the error chain).
#[derive(Debug, Error)]
#[error("invalid cargo version {raw:?}: {source}")]
pub struct CargoVersionError {
    raw: String,
    #[from]
    source: semver::Error,
}

#[derive(Debug, Error)]
#[error("invalid npm version {raw:?}: {source}")]
pub struct NpmVersionError {
    raw: String,
    #[from]
    source: semver::Error,
}

/// Wrapper for PEP 440.
#[derive(Debug, Error)]
#[error("invalid python version {raw:?}: {source}")]
pub struct PythonVersionError {
    raw: String,
    #[from]
    source: uv_pep440::VersionParseError,
}

/// Why a raw version failed to parse. Each variant wraps the concrete parser
/// error (with raw for context) and uses #[from] for direct ? conversion.
#[derive(Debug, Error)]
pub enum VersionError {
    #[error(transparent)]
    Cargo(#[from] CargoVersionError),
    #[error(transparent)]
    Npm(#[from] NpmVersionError),
    #[error(transparent)]
    Python(#[from] PythonVersionError),
}

/// An ecosystem-appropriate, typed package version.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PackageVersion {
    /// crates.io SemVer.
    Cargo(semver::Version),
    /// npm SemVer.
    Npm(semver::Version),
    /// Python PEP 440.
    Python(uv_pep440::Version),
    /// Go module version (tag or pseudo-version).
    Go(String),
    /// Java/Maven version string.
    Java(String),
}

impl TryFrom<(Language, &str)> for PackageVersion {
    type Error = VersionError;

    fn try_from((ecosystem, raw): (Language, &str)) -> Result<Self, Self::Error> {
        match ecosystem {
            Language::Rust => {
                let v = semver::Version::parse(raw)
                    .map_err(|source| CargoVersionError { raw: raw.to_owned(), source })?;
                Ok(Self::Cargo(v))
            }
            Language::Typescript => {
                let v = semver::Version::parse(raw)
                    .map_err(|source| NpmVersionError { raw: raw.to_owned(), source })?;
                Ok(Self::Npm(v))
            }
            Language::Python => {
                let v = raw
                    .parse::<uv_pep440::Version>()
                    .map_err(|source| PythonVersionError { raw: raw.to_owned(), source })?;
                Ok(Self::Python(v))
            }
            Language::Go => Ok(Self::Go(raw.to_owned())),
            Language::Java => Ok(Self::Java(raw.to_owned())),
        }
    }
}

impl PackageVersion {
    /// The canonical string form under this version's grammar.
    pub fn canonical(&self) -> String {
        match self {
            PackageVersion::Cargo(v) | PackageVersion::Npm(v) => v.to_string(),
            PackageVersion::Python(v) => v.to_string(),
            PackageVersion::Go(v) | PackageVersion::Java(v) => v.clone(),
        }
    }
}

impl From<&PackageVersion> for Language {
    fn from(package_version: &PackageVersion) -> Self {
        match package_version {
            PackageVersion::Cargo(_) => Language::Rust,
            PackageVersion::Npm(_) => Language::Typescript,
            PackageVersion::Python(_) => Language::Python,
            PackageVersion::Go(_) => Language::Go,
            PackageVersion::Java(_) => Language::Java,
        }
    }
}

/// Which registry a package was sourced from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RegistryOrigin {
    CratesIo,
    NpmPublic,
    PyPi,
    Custom { name: SmolStr, url: url::Url },
}

impl RegistryOrigin {
    pub fn token(&self) -> Cow<'static, str> {
        match self {
            RegistryOrigin::CratesIo => Cow::Borrowed("crates.io"),
            RegistryOrigin::NpmPublic => Cow::Borrowed("npm"),
            RegistryOrigin::PyPi => Cow::Borrowed("pypi"),
            RegistryOrigin::Custom { name, .. } => Cow::Owned(name.to_string()),
        }
    }
}
