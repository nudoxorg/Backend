//! Root-scoped source addresses. Only an owner read admits their bytes.

use super::CargoSourcePath;
use backend_library::{CargoPackageReadmeLinkTargetV1, CargoPackageReadmeOriginV1};
use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// An exact README-relative address, including the owner's original scope.
/// A saved instance remains an address; the owner revalidates its origin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CargoReadmeLinkAddress {
    origin: CargoPackageReadmeOriginV1,
    href: Arc<str>,
    path: CargoSourcePath,
    fragment: Option<Arc<str>>,
}

impl CargoReadmeLinkAddress {
    /// Projects a file address with the producer's exact relative resolver.
    /// Same-document anchors stay on the README and never become source reads.
    pub(crate) fn new(origin: CargoPackageReadmeOriginV1, href: &str) -> Option<Self> {
        let CargoPackageReadmeLinkTargetV1::File { path, fragment } =
            origin.resolve_relative_href(href).ok()? else { return None; };
        Some(Self { origin, href: Arc::from(href), path: CargoSourcePath::new(path.as_str())?,
            fragment: fragment.map(Arc::from) })
    }

    #[must_use]
    /// The complete owner origin, including README selection and root scope.
    pub fn origin(&self) -> &CargoPackageReadmeOriginV1 { &self.origin }
    #[must_use]
    /// Exact authored href resubmitted to the owner.
    pub fn href(&self) -> &str { &self.href }
    #[must_use]
    /// Relative display address beneath the origin's retained scope.
    pub fn path(&self) -> &CargoSourcePath { &self.path }
    #[must_use]
    /// Optional decoded target fragment, with no declaration inference.
    pub fn fragment(&self) -> Option<&str> { self.fragment.as_deref() }

    /// A line fragment is an address hint, checked against fresh byte bounds.
    #[must_use]
    pub fn source_line(&self) -> Option<u32> {
        let fragment = self.fragment()?.strip_prefix('L')?;
        let (first, end) = fragment.split_once('-').map_or((fragment, None), |(first, end)| (first, Some(end)));
        let first = first.parse::<u32>().ok().filter(|line| *line > 0)?;
        if end.is_some_and(|end| end.strip_prefix('L').and_then(|end| end.parse::<u32>().ok()).is_none_or(|last| last < first)) { return None; }
        Some(first)
    }

    fn identity(&self) -> (&str, u16, [u8; 32], [u8; 32], backend_library::CargoPackageReadmeRootScopeV1, &str, backend_library::CargoPackageReadmeSelectionV1, [u8; 32], &str) {
        let binding = self.origin.request_binding;
        (self.origin.package.as_str(), binding.schema, binding.requested_root_digest,
            binding.effective_workspace_root_digest, self.origin.root_scope, self.origin.path.as_str(),
            self.origin.selection, self.origin.content_digest, &self.href)
    }
}

impl Hash for CargoReadmeLinkAddress {
    fn hash<H: Hasher>(&self, state: &mut H) { self.identity().hash(state); }
}
impl PartialOrd for CargoReadmeLinkAddress {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}
impl Ord for CargoReadmeLinkAddress {
    fn cmp(&self, other: &Self) -> Ordering { self.identity().cmp(&other.identity()) }
}

/// Package inventory paths and README-relative paths have different scopes.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CargoSourceTarget {
    /// A canonical path beneath the exact Cargo package source root.
    PackageFile(CargoSourcePath),
    /// A link beneath the owner-selected README's package or workspace root.
    ReadmeLink(CargoReadmeLinkAddress),
}

impl CargoSourceTarget {
    /// Relative display spelling; the enclosing target retains its root scope.
    #[must_use]
    pub fn path(&self) -> &CargoSourcePath {
        match self { Self::PackageFile(path) => path, Self::ReadmeLink(link) => link.path() }
    }
    #[must_use]
    /// A package inventory may select only an actual package-file target.
    pub fn package_file(&self) -> Option<&CargoSourcePath> {
        match self { Self::PackageFile(path) => Some(path), Self::ReadmeLink(_) => None }
    }
}
