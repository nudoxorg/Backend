//! Defines coordinate behavior for `interface-identity`, whose purpose is to spell, parse, and abbreviate every identity a person or agent can name.
//! This module owns the coordinate invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The package coordinate: `ecosystem:name@version`, the shortest thing that names one shelf entry.

use core::fmt;

use interface_core::{PackageEcosystem, PackageUrl};

use crate::ecosystem::{ecosystem_tag, parse_ecosystem_tag};

/// Longest coordinate spelling accepted on any input surface.
pub const MAX_COORDINATE_BYTES: usize = 512;

/// Exact package name, including any registry namespace (`@types/node`, `github.com/x/y`).
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageName(Box<str>);

impl PackageName {
    /// Admits one registry package name on its own, under the same byte rules as a coordinate.
    ///
    /// # Errors
    ///
    /// Rejects an empty name, a name that would overflow the coordinate budget on its own, or a
    /// byte that can never appear inside a coordinate.
    pub fn new(name: &str) -> Result<Self, CoordinateParseError> {
        if name.is_empty() {
            return Err(CoordinateParseError::EmptyName);
        }
        if name.len() > MAX_COORDINATE_BYTES {
            return Err(CoordinateParseError::TooLong {
                observed: name.len(),
                maximum: MAX_COORDINATE_BYTES,
            });
        }
        reject_coordinate_bytes(name.bytes(), 0)?;
        Ok(Self(name.into()))
    }

    /// Returns the exact name text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Exact pinned version spelling as the registry publishes it.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageVersion(Box<str>);

impl PackageVersion {
    /// Admits one version spelling on its own, under the same byte rules as a coordinate.
    ///
    /// # Errors
    ///
    /// Rejects an empty version, one that would overflow the coordinate budget on its own, or a
    /// byte that can never appear inside a coordinate.
    pub fn new(version: &str) -> Result<Self, CoordinateParseError> {
        if version.is_empty() {
            return Err(CoordinateParseError::EmptyVersion);
        }
        if version.len() > MAX_COORDINATE_BYTES {
            return Err(CoordinateParseError::TooLong {
                observed: version.len(),
                maximum: MAX_COORDINATE_BYTES,
            });
        }
        reject_coordinate_bytes(version.bytes(), 0)?;
        Ok(Self(version.into()))
    }

    /// Whether the spelling carries a pre-release tag (`1.0.0-alpha.1`), by the semver rule of a
    /// hyphen before any build metadata.
    #[must_use]
    pub fn is_prerelease(&self) -> bool {
        let core = self.0.split('+').next().unwrap_or_default();
        core.contains('-')
    }

    /// Returns the exact version text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Rejects any byte a coordinate can never carry, reporting its offset from `base`.
fn reject_coordinate_bytes(
    bytes: impl Iterator<Item = u8>,
    base: usize,
) -> Result<(), CoordinateParseError> {
    for (offset, byte) in bytes.enumerate() {
        if matches!(byte, b':' | b'#' | b'?' | b' ' | b'\n' | b'\t') || byte == 0 {
            return Err(CoordinateParseError::Character {
                offset: base + offset,
                observed: byte,
            });
        }
    }
    Ok(())
}

/// One pinned package as every surface names it.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PackageCoordinate {
    /// Closed ecosystem.
    pub ecosystem: PackageEcosystem,
    /// Exact package name including namespace.
    pub name: PackageName,
    /// Exact pinned version.
    pub version: PackageVersion,
}

impl PackageCoordinate {
    /// Builds a coordinate from already-validated parts.
    ///
    /// # Errors
    ///
    /// Rejects an empty name or version, or a spelling that would exceed the coordinate budget.
    pub fn new(
        ecosystem: PackageEcosystem,
        name: &str,
        version: &str,
    ) -> Result<Self, CoordinateParseError> {
        if name.is_empty() {
            return Err(CoordinateParseError::EmptyName);
        }
        if version.is_empty() {
            return Err(CoordinateParseError::EmptyVersion);
        }
        let rendered = ecosystem_tag(ecosystem).as_str().len() + 2 + name.len() + version.len();
        if rendered > MAX_COORDINATE_BYTES {
            return Err(CoordinateParseError::TooLong {
                observed: rendered,
                maximum: MAX_COORDINATE_BYTES,
            });
        }
        reject_coordinate_bytes(name.bytes().chain(version.bytes()), 0)?;
        Ok(Self {
            ecosystem,
            name: PackageName(name.into()),
            version: PackageVersion(version.into()),
        })
    }

    /// Parses `ecosystem:name@version`. The version separator is the last `@`, so scoped npm
    /// names such as `npm:@types/node@20.11.0` parse.
    ///
    /// # Errors
    ///
    /// Returns the exact missing or malformed part.
    pub fn parse(text: &str) -> Result<Self, CoordinateParseError> {
        if text.is_empty() {
            return Err(CoordinateParseError::Empty);
        }
        if text.len() > MAX_COORDINATE_BYTES {
            return Err(CoordinateParseError::TooLong {
                observed: text.len(),
                maximum: MAX_COORDINATE_BYTES,
            });
        }
        let Some((tag, rest)) = text.split_once(':') else {
            return Err(CoordinateParseError::MissingEcosystem);
        };
        let ecosystem =
            parse_ecosystem_tag(tag).ok_or(CoordinateParseError::UnknownEcosystem {
                length: tag.len(),
            })?;
        let Some((name, version)) = rest.rsplit_once('@') else {
            return Err(CoordinateParseError::MissingVersion);
        };
        Self::new(ecosystem, name, version)
    }

    /// Projects an already-validated package URL onto its coordinate.
    #[must_use]
    pub fn from_package_url(url: &PackageUrl) -> Option<Self> {
        let name = match url.namespace {
            Some(namespace) => {
                let mut joined = String::with_capacity(
                    usize::from(namespace.end - namespace.start) + 1 + usize::from(url.name.end - url.name.start),
                );
                joined.push_str(&url[namespace]);
                joined.push('/');
                joined.push_str(&url[url.name]);
                joined
            }
            None => url[url.name].to_owned(),
        };
        Self::new(url.ecosystem, &name, &url[url.version]).ok()
    }

    /// Renders the canonical package URL that admits this coordinate to the compiler.
    #[must_use]
    pub fn package_url_text(&self) -> String {
        let type_name = match self.ecosystem {
            PackageEcosystem::Cargo => "cargo",
            PackageEcosystem::Npm => "npm",
            PackageEcosystem::Pypi => "pypi",
            PackageEcosystem::Golang => "golang",
            PackageEcosystem::Maven => "maven",
            PackageEcosystem::Nuget => "nuget",
            PackageEcosystem::Generic => "generic",
        };
        let mut text = String::with_capacity(
            4 + type_name.len() + 1 + self.name.0.len() + 1 + self.version.0.len(),
        );
        text.push_str("pkg:");
        text.push_str(type_name);
        text.push('/');
        text.push_str(&self.name.0);
        text.push('@');
        text.push_str(&self.version.0);
        text
    }
}

impl fmt::Display for PackageCoordinate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}@{}",
            ecosystem_tag(self.ecosystem).as_str(),
            self.name.0,
            self.version.0
        )
    }
}

/// Exact coordinate spelling rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoordinateParseError {
    /// The input was empty.
    Empty,
    /// The spelling exceeds the fixed coordinate budget.
    TooLong {
        /// Observed UTF-8 bytes.
        observed: usize,
        /// Accepted UTF-8 bytes.
        maximum: usize,
    },
    /// No `ecosystem:` prefix was present.
    MissingEcosystem,
    /// The ecosystem tag is not one of the seven supported spellings.
    UnknownEcosystem {
        /// Length of the rejected tag.
        length: usize,
    },
    /// No `@version` suffix was present.
    MissingVersion,
    /// The name between the tag and the version was empty.
    EmptyName,
    /// The version after the last `@` was empty.
    EmptyVersion,
    /// A byte cannot appear inside a coordinate name or version.
    Character {
        /// Byte offset inside the joined name and version text.
        offset: usize,
        /// Exact rejected byte.
        observed: u8,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_round_trip_including_scoped_names() -> Result<(), CoordinateParseError> {
        let scoped = PackageCoordinate::parse("npm:@types/node@20.11.0")?;
        assert_eq!(scoped.name.as_str(), "@types/node");
        assert_eq!(scoped.version.as_str(), "20.11.0");
        assert_eq!(scoped.to_string(), "npm:@types/node@20.11.0");
        assert_eq!(scoped.package_url_text(), "pkg:npm/@types/node@20.11.0");
        assert_eq!(
            PackageCoordinate::parse("cargo:serde"),
            Err(CoordinateParseError::MissingVersion)
        );
        assert_eq!(
            PackageCoordinate::parse("hex:phoenix@1.0"),
            Err(CoordinateParseError::UnknownEcosystem { length: 3 })
        );
        Ok(())
    }

    #[test]
    fn package_url_projects_onto_the_same_coordinate() -> Result<(), &'static str> {
        let url = PackageUrl::try_from("pkg:golang/github.com/x/y@v1.2.3".to_owned())
            .map_err(|_| "the fixture package URL is well formed")?;
        let coordinate = PackageCoordinate::from_package_url(&url)
            .ok_or("a well-formed package URL projects onto a coordinate")?;
        assert_eq!(coordinate.to_string(), "go:github.com/x/y@v1.2.3");
        assert_eq!(
            coordinate.package_url_text(),
            "pkg:golang/github.com/x/y@v1.2.3"
        );
        Ok(())
    }
}
