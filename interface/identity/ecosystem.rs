//! Defines ecosystem behavior for `interface-identity`, whose purpose is to spell, parse, and abbreviate every identity a person or agent can name.
//! This module owns the ecosystem invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Short readable tags for the seven supported package ecosystems.

use interface_core::PackageEcosystem;

/// One readable ecosystem tag as it appears at the front of a coordinate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EcosystemTag(&'static str);

impl EcosystemTag {
    /// Returns the exact tag text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// Returns the canonical readable tag for one ecosystem.
///
/// The tag is the package-url type except where that type is an implementation detail rather
/// than a name people use: Go modules are spelled `go` and local C/C++ packages `cpp`.
#[must_use]
pub const fn ecosystem_tag(ecosystem: PackageEcosystem) -> EcosystemTag {
    EcosystemTag(match ecosystem {
        PackageEcosystem::Cargo => "cargo",
        PackageEcosystem::Npm => "npm",
        PackageEcosystem::Pypi => "pypi",
        PackageEcosystem::Golang => "go",
        PackageEcosystem::Maven => "maven",
        PackageEcosystem::Nuget => "nuget",
        PackageEcosystem::Generic => "cpp",
    })
}

/// Parses one readable or package-url ecosystem tag.
///
/// Both spellings are accepted on input so a package-url copied from a manifest and an address
/// copied from a transcript resolve to the same ecosystem. Only the readable tag is ever emitted.
#[must_use]
pub fn parse_ecosystem_tag(text: &str) -> Option<PackageEcosystem> {
    Some(match text {
        "cargo" => PackageEcosystem::Cargo,
        "npm" => PackageEcosystem::Npm,
        "pypi" => PackageEcosystem::Pypi,
        "go" | "golang" => PackageEcosystem::Golang,
        "maven" => PackageEcosystem::Maven,
        "nuget" => PackageEcosystem::Nuget,
        "cpp" | "generic" => PackageEcosystem::Generic,
        _ => return None,
    })
}

/// Every supported ecosystem in stable display order.
pub const ALL_ECOSYSTEMS: [PackageEcosystem; 7] = [
    PackageEcosystem::Cargo,
    PackageEcosystem::Npm,
    PackageEcosystem::Pypi,
    PackageEcosystem::Golang,
    PackageEcosystem::Maven,
    PackageEcosystem::Nuget,
    PackageEcosystem::Generic,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tag_round_trips_and_purl_aliases_normalize() {
        for ecosystem in ALL_ECOSYSTEMS {
            assert_eq!(
                parse_ecosystem_tag(ecosystem_tag(ecosystem).as_str()),
                Some(ecosystem)
            );
        }
        assert_eq!(parse_ecosystem_tag("golang"), Some(PackageEcosystem::Golang));
        assert_eq!(parse_ecosystem_tag("generic"), Some(PackageEcosystem::Generic));
        assert_eq!(parse_ecosystem_tag("hex"), None);
    }
}
