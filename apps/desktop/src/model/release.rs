//! A registry release: a crate name and one exact version, where its source
//! is, and what the registry says about it (W-Acquire).
//!
//! These are the words every surface uses for a release: Find's offer, the
//! package page's header and the version comb all say the same thing about
//! the same release. Where a release's source *comes from* is
//! `host::registry`'s business.

use facet::marks::semver;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

/// A crate's name as the registry spells it.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct CrateName(Arc<str>);

impl CrateName {
    /// Admits a crate name: ASCII letters, digits, `-` and `_`, starting with
    /// a letter, at most 64 bytes (the registry's own rule).
    ///
    /// # Errors
    /// [`ReleaseError::Name`] for anything else.
    pub(crate) fn new(name: &str) -> Result<Self, ReleaseError> {
        let admitted = !name.is_empty()
            && name.len() <= 64
            && name.starts_with(|c: char| c.is_ascii_alphabetic())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if admitted { Ok(Self(Arc::from(name))) } else { Err(ReleaseError::Name(name.to_owned())) }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CrateName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One exact version as the registry spells it, build metadata kept
/// (`1.1.6+spec-1.1.0`): the directory and archive names carry it.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct Version(Arc<str>);

impl Version {
    /// Admits `MAJOR.MINOR.PATCH`, with an optional `-pre` and `+build`.
    ///
    /// # Errors
    /// [`ReleaseError::Version`] for anything else.
    pub(crate) fn new(version: &str) -> Result<Self, ReleaseError> {
        let (rest, build) = version.split_once('+').map_or((version, None), |(rest, build)| (rest, Some(build)));
        let (core, pre) = rest.split_once('-').map_or((rest, None), |(core, pre)| (core, Some(pre)));
        let numeric = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        let tag = |part: Option<&str>| part.is_none_or(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'));
        let parts = core.split('.').collect::<Vec<_>>();
        if parts.len() == 3 && parts.iter().all(|part| numeric(part)) && tag(pre) && tag(build) {
            Ok(Self(Arc::from(version)))
        } else {
            Err(ReleaseError::Version(version.to_owned()))
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// Without build metadata, as people read it (`1.1.6`).
    pub(crate) fn short(&self) -> &str {
        semver::short(&self.0)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One registry release: a crate at one exact version.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct Release {
    pub(crate) name: CrateName,
    pub(crate) version: Version,
}

impl Release {
    pub(crate) fn new(name: &str, version: &str) -> Result<Self, ReleaseError> {
        Ok(Self { name: CrateName::new(name)?, version: Version::new(version)? })
    }

    /// `name-version`, the registry's directory and archive stem.
    pub(crate) fn stem(&self) -> String {
        format!("{}-{}", self.name, self.version)
    }

    /// Reads a `name-version` stem back. The version is the longest suffix
    /// after a `-` that is a version, so `md-5-0.10.6` is `md-5` at `0.10.6`
    /// and `toml-1.1.6+spec-1.1.0` is `toml` at `1.1.6+spec-1.1.0`.
    pub(crate) fn from_stem(stem: &str) -> Option<Self> {
        stem.match_indices('-').find_map(|(at, _)| Self::new(&stem[..at], &stem[at + 1..]).ok())
    }

    /// The registry's package URL for this release.
    pub(crate) fn purl(&self) -> String {
        format!("pkg:cargo/{}@{}", self.name, self.version)
    }

    /// Reads a package URL back (`pkg:cargo/NAME@VERSION`, nothing more).
    pub(crate) fn from_purl(purl: &str) -> Option<Self> {
        let (name, version) = purl.strip_prefix("pkg:cargo/")?.split_once('@')?;
        Self::new(name, version).ok()
    }
}

impl fmt::Display for Release {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.name, self.version.short())
    }
}

/// Why a name or version was not admitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReleaseError {
    Name(String),
    Version(String),
}

impl fmt::Display for ReleaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(name) => write!(formatter, "`{name}` is not a crate name"),
            Self::Version(version) => write!(formatter, "`{version}` is not a version"),
        }
    }
}

/// Where a release's source is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Availability {
    /// An unpacked source tree is on this machine (cargo's, or one this app
    /// unpacked): indexing reads nothing else.
    Unpacked(PathBuf),
    /// Only the registry archive is on this machine: it is verified and
    /// unpacked first, still without the network.
    Archive(PathBuf),
    /// Only published: reading it needs a download.
    Download,
}

/// One published release and where its source is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Published {
    pub(crate) release: Release,
    /// `YYYY-MM-DD`, when the registry index recorded it.
    pub(crate) date: Option<Arc<str>>,
    /// Withdrawn by its publisher.
    pub(crate) yanked: bool,
    pub(crate) availability: Availability,
}

/// A registry release browsing shows: where its source is and, when it is
/// already in the library, its page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Offer {
    pub(crate) release: Release,
    pub(crate) availability: Availability,
    /// Its package in the library, once the owner has indexed it.
    pub(crate) library: Option<crate::model::pages::PackageRef>,
}
