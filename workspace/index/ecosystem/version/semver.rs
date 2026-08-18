//! SemVer grammar (crates.io / npm).

use super::{AnyVersion, VersionGrammar};

/// SemVer — a thin wrapper adding the grammar trait to [`semver::Version`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SemverVersion(pub semver::Version);

impl VersionGrammar for SemverVersion {
    fn parse(raw: &str) -> Option<Self> {
        raw.parse().ok().map(SemverVersion)
    }

    fn is_prerelease(&self) -> bool {
        !self.0.pre.is_empty()
    }

    fn range_matches(spec: &str, candidate: &Self) -> bool {
        semver::VersionReq::parse(spec).is_ok_and(|range| range.matches(&candidate.0))
    }

    fn spec_is_valid(spec: &str) -> bool {
        semver::VersionReq::parse(spec).is_ok()
    }

    fn erase(self) -> AnyVersion {
        AnyVersion::Semver(self)
    }
}
