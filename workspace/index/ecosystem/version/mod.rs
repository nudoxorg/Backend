//! Version grammars behind one trait — parse, prerelease, range match.

mod go;
mod maven;
mod nuget;
mod pep440;
mod semver;

pub use go::GoVersion;
pub use maven::MavenVersion;
pub use nuget::NuGetVersion;
pub use pep440::Pep440Version;
pub use semver::SemverVersion;

use core::cmp::Ordering;

use crate::ecosystem::cpp::version::CppVersion;

/// One ecosystem's version semantics: parse, prerelease classification, and
/// native range matching (semver ranges, PEP 440 specifiers, NuGet intervals,
/// Maven brackets, Go module rules).
pub trait VersionGrammar: Sized + Ord + Clone + Send + Sync + 'static {
    fn parse(raw: &str) -> Option<Self>;

    fn is_prerelease(&self) -> bool;

    /// Whether `candidate` satisfies the ecosystem-native range `spec`.
    fn range_matches(spec: &str, candidate: &Self) -> bool;

    /// Whether `spec` parses as a well-formed range (malformed request vs.
    /// nothing-matched stay distinct errors).
    fn spec_is_valid(spec: &str) -> bool;

    /// Fold into the type-erased [`AnyVersion`] for `DynSpec` call sites.
    fn erase(self) -> AnyVersion;
}

/// The type-erased version — one variant per concrete grammar. Exists only so
/// [`crate::ecosystem::DynSpec`] can talk about versions without generics; do not leak it
/// into `heart`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnyVersion {
    Semver(SemverVersion),
    Pep440(Pep440Version),
    NuGet(NuGetVersion),
    Go(GoVersion),
    Maven(MavenVersion),
    /// C/C++ registry-less four-kind version (`Tag ▸ Date ▸ Pseudo ▸ Raw`).
    Cpp(CppVersion),
}

impl AnyVersion {
    /// Order two erased versions; `None` across grammars (a candidate set is
    /// always single-ecosystem, so a cross-grammar compare is caller error).
    pub fn compare(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (AnyVersion::Semver(a), AnyVersion::Semver(b)) => Some(a.cmp(b)),
            (AnyVersion::Pep440(a), AnyVersion::Pep440(b)) => Some(a.cmp(b)),
            (AnyVersion::NuGet(a), AnyVersion::NuGet(b)) => Some(a.cmp(b)),
            (AnyVersion::Go(a), AnyVersion::Go(b)) => Some(a.cmp(b)),
            (AnyVersion::Maven(a), AnyVersion::Maven(b)) => Some(a.cmp(b)),
            (AnyVersion::Cpp(a), AnyVersion::Cpp(b)) => Some(a.cmp(b)),
            _ => None,
        }
    }

    pub fn is_prerelease(&self) -> bool {
        match self {
            AnyVersion::Semver(v) => v.is_prerelease(),
            AnyVersion::Pep440(v) => v.is_prerelease(),
            AnyVersion::NuGet(v) => v.is_prerelease(),
            AnyVersion::Go(v) => v.is_prerelease(),
            AnyVersion::Maven(v) => v.is_prerelease(),
            AnyVersion::Cpp(v) => v.is_prerelease(),
        }
    }
}
