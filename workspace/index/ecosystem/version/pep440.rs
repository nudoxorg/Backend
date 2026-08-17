//! PEP 440 grammar (PyPI).

use super::{AnyVersion, VersionGrammar};

/// PEP 440 — wrapper over [`uv_pep440::Version`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pep440Version(pub uv_pep440::Version);

impl VersionGrammar for Pep440Version {
    fn parse(raw: &str) -> Option<Self> {
        raw.parse().ok().map(Pep440Version)
    }

    fn is_prerelease(&self) -> bool {
        self.0.any_prerelease()
    }

    fn range_matches(spec: &str, candidate: &Self) -> bool {
        spec.parse::<uv_pep440::VersionSpecifiers>()
            .is_ok_and(|specifiers| specifiers.contains(&candidate.0))
    }

    fn spec_is_valid(spec: &str) -> bool {
        spec.parse::<uv_pep440::VersionSpecifiers>().is_ok()
    }

    fn erase(self) -> AnyVersion {
        AnyVersion::Pep440(self)
    }
}
