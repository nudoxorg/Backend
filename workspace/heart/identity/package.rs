//! Package identity: the coordinates that name a package and the deterministic
//! [`PackageId`] derived from them.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use thiserror::Error;

use super::{namespace, Id};
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
    Empty,
    #[error("package name {raw:?} is invalid for {ecosystem}")]
    Invalid { ecosystem: Language, raw: String },
    #[error("package name {raw:?} exceeds the {ecosystem} length limit")]
    TooLong { ecosystem: Language, raw: String },
}

/// Why a raw version failed to parse.
#[derive(Debug, Error)]
#[error("invalid {ecosystem} version {raw:?}: {message}")]
pub struct VersionError {
    pub ecosystem: Language,
    pub raw: String,
    pub message: String,
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
}

impl TryFrom<(Language, &str)> for PackageVersion {
    type Error = VersionError;

    fn try_from((ecosystem, raw): (Language, &str)) -> Result<Self, Self::Error> {
        let _ = (ecosystem, raw);
        todo!("dispatch to semver::Version::parse or uv_pep440::Version::from_str")
    }
}

impl PackageVersion {
    /// The canonical string form under this version's grammar.
    pub fn canonical(&self) -> String {
        match self {
            PackageVersion::Cargo(v) | PackageVersion::Npm(v) => v.to_string(),
            PackageVersion::Python(v) => v.to_string(),
        }
    }
}

impl From<&PackageVersion> for Language {
    fn from(package_version: &PackageVersion) -> Self {
        match package_version {
            PackageVersion::Cargo(_) => Language::Rust,
            PackageVersion::Npm(_) => Language::Typescript,
            PackageVersion::Python(_) => Language::Python,
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
