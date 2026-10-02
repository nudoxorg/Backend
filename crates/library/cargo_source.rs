//! Exact, owner-observed source identity for Cargo packages.
//!
//! A package name and version do not identify a Cargo source. The same
//! package can be supplied by different registry protocols, Git revisions,
//! or local path roots. This receipt keeps those facts together with the
//! Cargo resolution that observed them. Its short digest can be carried in a
//! package coordinate; the owner must still match the receipt against its
//! current Cargo observation before it reads from the source.

use crate::browse::ProjectTreeRequestBindingV1;
use crate::surface::{PackageCoordinate, PackageReference, ProductText};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Wire version of [`CargoPackageSourceAuthorityV1`].
pub const CARGO_PACKAGE_SOURCE_AUTHORITY_SCHEMA: u16 = 1;
/// Maximum Cargo package-name and version bytes retained in this receipt.
pub const MAX_CARGO_SOURCE_COORDINATE_BYTES: usize = 256;
/// Maximum exact source URL, query, or effective-target bytes retained.
pub const MAX_CARGO_SOURCE_DETAIL_BYTES: usize = 2_048;
/// Maximum bytes read from one package-relative source file.
pub const MAX_CARGO_PACKAGE_SOURCE_FILE_BYTES: usize = 1024 * 1024;
/// Maximum UTF-8 Markdown bytes admitted from one Cargo package README.
pub const MAX_CARGO_PACKAGE_README_BYTES: usize = 512 * 1024;
/// Maximum bytes in one relative Markdown link submitted for owner resolution.
pub const MAX_CARGO_PACKAGE_README_LINK_BYTES: usize = 2_048;
/// Maximum bytes in a package-relative source path.
pub const MAX_CARGO_PACKAGE_SOURCE_PATH_BYTES: usize = 1_024;
/// Maximum source addresses returned by one inventory read.
pub const MAX_CARGO_PACKAGE_SOURCE_INVENTORY_PATHS: usize = 512;
/// Maximum filesystem entries inspected by one inventory read.
pub const MAX_CARGO_PACKAGE_SOURCE_INVENTORY_SCAN_ENTRIES: usize = 8_192;

/// How Cargo names an admitted registry source.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CargoRegistrySourceSchemeV1 {
    /// Cargo's `registry+` source scheme.
    Registry,
    /// Cargo's `sparse+` source scheme.
    Sparse,
}

/// Exact source authority Cargo reported for one package.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CargoPackageSourceV1 {
    /// A registry index. `index_url` is retained byte-for-byte after Cargo's
    /// source-scheme prefix; registry and sparse sources remain distinct.
    Registry {
        /// Exact Cargo source scheme.
        scheme: CargoRegistrySourceSchemeV1,
        /// Exact index URL from Cargo's package source identity.
        index_url: ProductText,
    },
    /// A Git checkout. `requested_query` retains the exact Cargo query text
    /// (for example `branch=main` or `rev=...`), while `resolved_commit` is
    /// the exact checkout commit reported by Cargo.
    Git {
        /// Exact repository URL before the Cargo query.
        repository_url: ProductText,
        /// Exact requested ref/query, if the source specified one.
        requested_query: Option<ProductText>,
        /// Exact resolved commit identifier.
        resolved_commit: ProductText,
    },
    /// A local path source. The actual filesystem path remains with the
    /// owner; the receipt carries commitments to the observed roots.
    Path,
}

/// Identities for the exact local roots Cargo observed, over Cargo's exact
/// UTF-8 path spellings. These are hash commitments, not encryption; low-
/// entropy inputs may be guessable.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CargoPackageRootIdentityV1 {
    /// Commitment to the admitted workspace root.
    pub workspace_root: [u8; 32],
    /// Commitment to the directory containing this package's Cargo manifest.
    pub package_root: [u8; 32],
    /// Commitment to the exact Cargo manifest path.
    pub manifest_path: [u8; 32],
}

/// A Cargo package source receipt derived from one owner-observed metadata
/// row.
///
/// This value is a selector, not a filesystem capability. A source owner must
/// resolve its `authority_digest` against the exact current Cargo observation
/// and must never open a path copied from a client request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CargoPackageSourceAuthorityV1 {
    schema: u16,
    name: ProductText,
    version: ProductText,
    package_id_digest: [u8; 32],
    source: CargoPackageSourceV1,
    roots: CargoPackageRootIdentityV1,
    /// Commitment to the exact relative path spelling used by the tree for a
    /// local path package. It ties the display row to the root commitment
    /// without exposing the canonical absolute path.
    path_display_digest: Option<[u8; 32]>,
    source_revision: [u8; 32],
    effective_target: ProductText,
    resolved_features_digest: [u8; 32],
    authority_digest: [u8; 32],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CargoPackageSourceAuthorityWireV1 {
    schema: u16,
    name: ProductText,
    version: ProductText,
    package_id_digest: [u8; 32],
    source: CargoPackageSourceV1,
    roots: CargoPackageRootIdentityV1,
    path_display_digest: Option<[u8; 32]>,
    source_revision: [u8; 32],
    effective_target: ProductText,
    resolved_features_digest: [u8; 32],
    authority_digest: [u8; 32],
}

impl<'de> Deserialize<'de> for CargoPackageSourceAuthorityV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = CargoPackageSourceAuthorityWireV1::deserialize(deserializer)?;
        Self::try_from(wire).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<CargoPackageSourceAuthorityWireV1> for CargoPackageSourceAuthorityV1 {
    type Error = &'static str;

    fn try_from(wire: CargoPackageSourceAuthorityWireV1) -> Result<Self, Self::Error> {
        let authority = Self {
            schema: wire.schema,
            name: wire.name,
            version: wire.version,
            package_id_digest: wire.package_id_digest,
            source: wire.source,
            roots: wire.roots,
            path_display_digest: wire.path_display_digest,
            source_revision: wire.source_revision,
            effective_target: wire.effective_target,
            resolved_features_digest: wire.resolved_features_digest,
            authority_digest: wire.authority_digest,
        };
        authority
            .has_admissible_shape()
            .then_some(authority)
            .ok_or("Cargo package source authority is malformed or has a mismatched digest")
    }
}

/// Why Cargo source identity could not be admitted for a package row.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CargoPackageSourceAuthorityFailureV1 {
    /// No Cargo owner observation supplied a source receipt.
    NotObserved,
    /// A lockfile-only answer does not prove the active source and roots.
    LockfileOnly,
    /// Cargo did not resolve this listed package into the target graph.
    NotInResolvedGraph,
    /// Cargo returned a source spelling this version does not recognize.
    UnsupportedSource,
    /// Cargo omitted the exact package identifier.
    MissingPackageIdentity,
    /// Cargo omitted or malformed the package manifest path.
    MissingManifestRoot,
    /// Cargo omitted or malformed the workspace root.
    MissingWorkspaceRoot,
    /// The metadata did not provide an effective target.
    MissingEffectiveTarget,
    /// Cargo's resolved node omitted its exact selected feature set.
    MissingResolvedFeatures,
    /// The source observation did not have a revision commitment.
    MissingSourceRevision,
    /// Source fields exceeded the bounded receipt shape.
    SourceDetailsOutOfBounds,
}

/// Admitted Cargo source authority, or the typed reason it is unavailable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    content = "detail",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum CargoPackageSourceAuthorityStateV1 {
    /// Complete source and root facts from an admitted Cargo metadata row.
    Admitted(CargoPackageSourceAuthorityV1),
    /// The owner did not have sufficient exact facts to issue a receipt.
    Unavailable(CargoPackageSourceAuthorityFailureV1),
}

/// Canonical package-relative path requested from an admitted Cargo source.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CargoPackageSourcePathV1(Box<str>);

impl CargoPackageSourcePathV1 {
    /// Admits one slash-separated relative path without empty, dot, or parent
    /// components. A platform path separator is never accepted on the wire.
    pub fn new(value: impl Into<String>) -> Result<Self, CargoPackageSourceReadFailureV1> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= MAX_CARGO_PACKAGE_SOURCE_PATH_BYTES
            && !value.starts_with('/')
            && !value.contains('\\')
            && !value.contains(':')
            && !value.chars().any(char::is_control)
            && value.split('/').count() <= 128
            && value.split('/').all(|segment| {
                !segment.is_empty()
                    && segment != "."
                    && segment != ".."
                    && ![".git", ".hg", ".svn", ".cargo", "target", "build"]
                        .iter()
                        .any(|internal| segment.eq_ignore_ascii_case(internal))
            });
        if !valid {
            return Err(CargoPackageSourceReadFailureV1::InvalidRelativePath);
        }
        Ok(Self(value.into_boxed_str()))
    }

    /// The canonical slash-separated relative path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether a decoded path still satisfies the wire bounds.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        Self::new(self.0.to_string()).is_ok()
    }
}

impl TryFrom<String> for CargoPackageSourcePathV1 {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value).map_err(|_| "Cargo source path is not a bounded relative path")
    }
}

impl From<CargoPackageSourcePathV1> for String {
    fn from(value: CargoPackageSourcePathV1) -> Self {
        value.0.into()
    }
}

/// Why a source-file request could not return current UTF-8 contents.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CargoPackageSourceReadFailureV1 {
    /// The package reference did not contain one exact Cargo authority digest.
    InvalidPackageReference,
    /// The relative path was not canonical or exceeded its bound.
    InvalidRelativePath,
    /// The owner has no current Cargo metadata observation for this authority.
    AuthorityUnavailable,
    /// The bounded manifest read set could not be completely revalidated.
    SourceObservationUnavailable,
    /// The cached Cargo observation changed since the package was displayed.
    StaleAuthority,
    /// The exact metadata row did not retain a local package root.
    PackageRootUnavailable,
    /// The requested path is not a source, documentation, or package manifest file.
    UnsupportedFileKind,
    /// The file was absent, non-regular, unreadable, or crossed a symlink.
    FileUnavailable,
    /// The file exceeded the per-file byte bound.
    FileTooLarge,
    /// The file bytes were not bounded valid UTF-8 source text.
    NotUtf8Text,
}

/// Semantic relationship of a source-only Cargo file to the indexed corpus.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CargoPackageSourceSemanticStatusV1 {
    /// No exact source-to-IR lineage receipt was available for this file.
    NotIndexed,
}

/// Exact package route and project-tree request required for a source read.
/// A package authority binds its effective workspace but not the requested
/// directory that admitted the tree, so source reads carry both commitments.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CargoPackageSourceRequestV1 {
    /// Exact source-qualified package route from the tree row.
    pub package: PackageReference,
    /// Exact requested and effective roots from that same tree response.
    pub request_binding: ProjectTreeRequestBindingV1,
}

impl CargoPackageSourceRequestV1 {
    /// Makes a request from one exact ProjectTree row and its owner binding.
    #[must_use]
    pub fn from_tree(
        package: PackageReference,
        request_binding: ProjectTreeRequestBindingV1,
    ) -> Self {
        Self {
            package,
            request_binding,
        }
    }

    /// Validates the package authority selector and both root commitments.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        CargoPackageSourceAuthorityV1::digest_from_package_reference(&self.package).is_some()
            && self.request_binding.has_admissible_shape()
    }
}

/// Result of reading one file under an owner-admitted Cargo package root.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CargoPackageSourceFileResultV1 {
    /// Current bytes, hashed independently from the Cargo metadata revision.
    Read {
        /// Exact source-qualified package route requested.
        package: PackageReference,
        /// Current owner-revalidated Cargo source receipt.
        authority: CargoPackageSourceAuthorityV1,
        /// Exact tree request that admitted this source route.
        request_binding: ProjectTreeRequestBindingV1,
        /// Exact relative path read.
        path: CargoPackageSourcePathV1,
        /// BLAKE3 of the exact returned UTF-8 bytes.
        content_digest: [u8; 32],
        /// Bounded text read through the owner-held directory capability.
        contents: Box<str>,
        /// Source text does not itself prove semantic indexing.
        semantic: CargoPackageSourceSemanticStatusV1,
    },
    /// The owner observation no longer matches the route's source receipt.
    Stale {
        /// Exact source-qualified package route requested.
        package: PackageReference,
        /// Exact request selector which failed to match current owner state.
        request_binding: ProjectTreeRequestBindingV1,
    },
    /// No file was returned; the failure remains typed.
    Unavailable {
        /// Exact source-qualified package route requested when it was valid.
        package: Option<PackageReference>,
        /// Exact request selector, when it was structurally valid.
        request_binding: Option<ProjectTreeRequestBindingV1>,
        /// Stable reason the source-file authority could not answer.
        reason: CargoPackageSourceReadFailureV1,
    },
}

impl CargoPackageSourceFileResultV1 {
    /// Validates an untrusted reply, including content, path, and route binding.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        match self {
            Self::Read {
                package,
                authority,
                request_binding,
                path,
                content_digest,
                contents,
                semantic: CargoPackageSourceSemanticStatusV1::NotIndexed,
            } => {
                authority.matches_package_reference(package)
                    && authority.has_admissible_shape()
                    && request_binding.has_admissible_shape()
                    && request_binding
                        .matches_workspace_root_identity(authority.roots().workspace_root)
                    && path.has_admissible_shape()
                    && contents.len() <= MAX_CARGO_PACKAGE_SOURCE_FILE_BYTES
                    && !contents.as_bytes().contains(&0)
                    && blake3::hash(contents.as_bytes()).as_bytes() == content_digest
            }
            Self::Stale {
                package,
                request_binding,
            } => {
                CargoPackageSourceAuthorityV1::digest_from_package_reference(package).is_some()
                    && request_binding.has_admissible_shape()
            }
            Self::Unavailable {
                package,
                request_binding,
                ..
            } => {
                package.as_ref().is_none_or(|package| {
                    CargoPackageSourceAuthorityV1::digest_from_package_reference(package).is_some()
                }) && request_binding.is_none_or(|binding| binding.has_admissible_shape())
            }
        }
    }
}

/// README declaration parsed from the exact package manifest admitted by the
/// current Cargo metadata observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CargoPackageReadmeManifestV1 {
    /// The exact package manifest omits a README declaration; apply Cargo's
    /// supported conventional package-root lookup.
    Unspecified,
    /// The manifest declares an exact relative README path.
    Path(String),
    /// The manifest declares `readme = true`, selecting `README.md`.
    Enabled,
    /// The manifest explicitly disables its README.
    Disabled,
    /// The manifest inherits its README from `[workspace.package]`.
    WorkspaceInherited,
}

impl Default for CargoPackageReadmeManifestV1 {
    fn default() -> Self {
        Self::Unspecified
    }
}

/// How the owner resolved Cargo's README path for one exact package release.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CargoPackageReadmeSelectionV1 {
    /// The package manifest supplied an explicit path.
    ManifestPath,
    /// The package manifest set `readme = true`, selecting `README.md`.
    ManifestTrueDefault,
    /// Cargo's conventional README lookup selected a package-root file.
    CargoConventionalDefault,
    /// A package manifest explicitly inherited its workspace package README.
    WorkspaceInherited,
}

/// Which owner-held root anchors a selected package README and its relative
/// links. Workspace scope is admitted only for an exact manifest inheritance.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CargoPackageReadmeRootScopeV1 {
    /// The exact package source root from Cargo's package row.
    Package,
    /// The effective local Cargo workspace root bound by the tree request.
    EffectiveWorkspace,
}

/// Exact package and project binding for a README read.
///
/// GUI callers should send the full binding from the same `ProjectTree`.
/// Command-line callers can bind the requested directory and leave the
/// effective-root expectation empty; the owner still requires its current
/// cached tree binding and returns that complete binding in the result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CargoPackageReadmeRequestV1 {
    /// Exact source-qualified package route from the current tree.
    pub package: PackageReference,
    /// Commitment to the exact requested local project directory.
    pub requested_root_digest: [u8; 32],
    /// Optional expected effective-workspace commitment from a prior tree.
    pub expected_workspace_root_digest: Option<[u8; 32]>,
}

impl CargoPackageReadmeRequestV1 {
    /// Makes a GUI request bound to the complete exact tree receipt.
    #[must_use]
    pub fn from_tree(package: PackageReference, binding: ProjectTreeRequestBindingV1) -> Self {
        Self {
            package,
            requested_root_digest: binding.requested_root_digest,
            expected_workspace_root_digest: Some(binding.effective_workspace_root_digest),
        }
    }

    /// Makes a CLI request bound to its submitted project directory. The
    /// owner resolves and returns the effective workspace root.
    #[must_use]
    pub fn for_requested_root(package: PackageReference, requested_root: &Path) -> Option<Self> {
        Some(Self {
            package,
            requested_root_digest: ProjectTreeRequestBindingV1::requested_root_digest_for(
                requested_root,
            )?,
            expected_workspace_root_digest: None,
        })
    }

    /// Checks bounded source and binding selectors.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        CargoPackageSourceAuthorityV1::digest_from_package_reference(&self.package).is_some()
            && self.requested_root_digest != [0; 32]
            && self
                .expected_workspace_root_digest
                .is_none_or(|digest| digest != [0; 32])
    }
}

/// Why the exact package manifest declares no README.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CargoPackageReadmeAbsenceV1 {
    /// The package manifest explicitly disabled its README.
    ManifestDisabled,
    /// The manifest omitted a README and Cargo's supported default files were absent.
    NoCargoDefault,
}

/// Why a selected README could not be returned as bounded UTF-8 Markdown.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CargoPackageReadmeFailureV1 {
    /// The package route did not carry one exact Cargo source authority.
    InvalidPackageReference,
    /// No current Cargo owner observation can revalidate this source.
    AuthorityUnavailable,
    /// The watched Cargo metadata/manifests could not be re-observed safely.
    SourceObservationUnavailable,
    /// The exact package metadata row has no retained local package root.
    PackageRootUnavailable,
    /// The manifest's selected README path is not a canonical package-relative path.
    InvalidReadmePath,
    /// The manifest selected a path outside its exact owner-held root.
    ReadmeOutsideAuthorizedRoot,
    /// Workspace-inherited README selection could not be resolved from the admitted manifests.
    WorkspaceReadmeUnresolved,
    /// The exact package manifest could not be read safely from its held root.
    PackageManifestUnavailable,
    /// The exact package manifest was too large for README admission.
    PackageManifestTooLarge,
    /// The exact package manifest was invalid or had a malformed README value.
    PackageManifestMalformed,
    /// Cargo selected a README path that is absent, non-regular, unreadable, or linked.
    SelectedFileUnavailable,
    /// The selected README exceeded the fixed content bound; no prefix was returned.
    ContentTooLarge,
    /// The selected bytes were not bounded UTF-8 Markdown text.
    NotUtf8Text,
}

/// Bounded Markdown content selected by the exact Cargo package manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CargoPackageReadmeV1 {
    /// Owner-held root used for this exact package README and its relative
    /// links. This is derived from the package manifest, never caller input.
    pub root_scope: CargoPackageReadmeRootScopeV1,
    /// Exact path relative to `root_scope` selected by Cargo semantics.
    pub path: CargoPackageSourcePathV1,
    /// Which Cargo/manifest rule selected this path.
    pub selection: CargoPackageReadmeSelectionV1,
    /// BLAKE3 of the exact returned UTF-8 Markdown bytes.
    pub content_digest: [u8; 32],
    /// Bounded Markdown; links are hints until each target is owner-read.
    pub contents: Box<str>,
    /// README text does not itself prove semantic indexing.
    pub semantic: CargoPackageSourceSemanticStatusV1,
}

impl CargoPackageReadmeV1 {
    /// Validates the path, content bound, semantic state, and byte digest.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        self.path.has_admissible_shape()
            && self.contents.len() <= MAX_CARGO_PACKAGE_README_BYTES
            && !self.contents.as_bytes().contains(&0)
            && blake3::hash(self.contents.as_bytes()).as_bytes() == &self.content_digest
            && self.semantic == CargoPackageSourceSemanticStatusV1::NotIndexed
    }

    /// Checks that the root scope is consistent with the admitted selection.
    #[must_use]
    pub fn has_admissible_scope(&self) -> bool {
        self.has_admissible_shape()
            && match (self.root_scope, self.selection) {
                (
                    CargoPackageReadmeRootScopeV1::Package,
                    CargoPackageReadmeSelectionV1::WorkspaceInherited,
                )
                | (
                    CargoPackageReadmeRootScopeV1::EffectiveWorkspace,
                    CargoPackageReadmeSelectionV1::ManifestPath
                    | CargoPackageReadmeSelectionV1::ManifestTrueDefault
                    | CargoPackageReadmeSelectionV1::CargoConventionalDefault,
                ) => false,
                _ => true,
            }
    }

    /// Checks that this README came from the exact owner-held root identified
    /// by the source receipt and the caller's tree request.
    #[must_use]
    pub fn matches_authority_scope(
        &self,
        authority: &CargoPackageSourceAuthorityV1,
        request_binding: &ProjectTreeRequestBindingV1,
    ) -> bool {
        if !self.has_admissible_scope()
            || !request_binding.matches_workspace_root_identity(authority.roots().workspace_root)
        {
            return false;
        }
        match self.root_scope {
            CargoPackageReadmeRootScopeV1::Package => authority.roots().package_root != [0; 32],
            CargoPackageReadmeRootScopeV1::EffectiveWorkspace => {
                self.selection == CargoPackageReadmeSelectionV1::WorkspaceInherited
                    && authority.roots().workspace_root
                        == request_binding.effective_workspace_root_digest
            }
        }
    }
}

/// Small origin receipt used to resolve links from one exact owner-returned
/// README without resending its Markdown body.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CargoPackageReadmeOriginV1 {
    /// Exact source-qualified package release that supplied the README.
    pub package: PackageReference,
    /// Exact request roots that admitted the source package.
    pub request_binding: ProjectTreeRequestBindingV1,
    /// Owner-held root in which the README path is relative.
    pub root_scope: CargoPackageReadmeRootScopeV1,
    /// Exact selected path relative to the owner-held root.
    pub path: CargoPackageSourcePathV1,
    /// Cargo rule that selected the README.
    pub selection: CargoPackageReadmeSelectionV1,
    /// Digest of the exact README bytes returned by the owner.
    pub content_digest: [u8; 32],
}

/// One locally resolved target of a README Markdown href.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CargoPackageReadmeLinkTargetV1 {
    /// A fragment on the current README page.
    Anchor { fragment: Box<str> },
    /// A normalized same-root file path and optional fragment.
    File {
        path: CargoPackageSourcePathV1,
        fragment: Option<Box<str>>,
    },
}

impl CargoPackageReadmeOriginV1 {
    /// Extracts the small owner origin from one admitted README reply.
    #[must_use]
    pub fn from_result(result: &CargoPackageReadmeResultV1) -> Option<Self> {
        if !result.has_admissible_shape() {
            return None;
        }
        let CargoPackageReadmeResultV1::Read {
            package,
            request_binding,
            readme,
            ..
        } = result
        else {
            return None;
        };
        let origin = Self {
            package: package.clone(),
            request_binding: *request_binding,
            root_scope: readme.root_scope,
            path: readme.path.clone(),
            selection: readme.selection,
            content_digest: readme.content_digest,
        };
        origin.has_admissible_shape().then_some(origin)
    }

    /// Checks the compact owner-issued route and scope selector.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        CargoPackageSourceAuthorityV1::digest_from_package_reference(&self.package).is_some()
            && self.request_binding.has_admissible_shape()
            && self.path.has_admissible_shape()
            && self.content_digest != [0; 32]
            && match (self.root_scope, self.selection) {
                (
                    CargoPackageReadmeRootScopeV1::Package,
                    CargoPackageReadmeSelectionV1::WorkspaceInherited,
                )
                | (
                    CargoPackageReadmeRootScopeV1::EffectiveWorkspace,
                    CargoPackageReadmeSelectionV1::ManifestPath
                    | CargoPackageReadmeSelectionV1::ManifestTrueDefault
                    | CargoPackageReadmeSelectionV1::CargoConventionalDefault,
                ) => false,
                _ => true,
            }
    }

    /// Resolves a relative Markdown href against the admitted README path.
    /// The returned path remains relative to this origin's typed root scope;
    /// parent segments may not escape that root.
    pub fn resolve_relative_href(
        &self,
        href: &str,
    ) -> Result<CargoPackageReadmeLinkTargetV1, CargoPackageReadmeLinkFailureV1> {
        if !self.has_admissible_shape()
            || href.is_empty()
            || href.len() > MAX_CARGO_PACKAGE_README_LINK_BYTES
            || href.chars().any(char::is_control)
            || href.trim() != href
        {
            return Err(CargoPackageReadmeLinkFailureV1::InvalidRequest);
        }
        let (before_fragment, raw_fragment) = href
            .split_once('#')
            .map_or((href, None), |(path, fragment)| (path, Some(fragment)));
        let raw_path = before_fragment
            .split_once('?')
            .map_or(before_fragment, |(path, _)| path);
        let fragment = raw_fragment
            .filter(|fragment| !fragment.is_empty())
            .map(decode_markdown_uri_component)
            .transpose()?
            .map(String::into_boxed_str);
        if raw_path.is_empty() {
            return fragment
                .map(|fragment| CargoPackageReadmeLinkTargetV1::Anchor { fragment })
                .ok_or(CargoPackageReadmeLinkFailureV1::InvalidRequest);
        }
        let decoded = decode_markdown_uri_component(raw_path)?;
        if decoded.starts_with('/')
            || decoded.starts_with('\\')
            || decoded.contains('\\')
            || decoded
                .find(':')
                .is_some_and(|colon| decoded[..colon].find('/').is_none())
        {
            return Err(CargoPackageReadmeLinkFailureV1::NotRelative);
        }
        let mut segments = self.path.as_str().split('/').collect::<Vec<_>>();
        segments.pop();
        for segment in decoded.split('/') {
            match segment {
                "" => return Err(CargoPackageReadmeLinkFailureV1::NotRelative),
                "." => {}
                ".." => {
                    if segments.pop().is_none() {
                        return Err(CargoPackageReadmeLinkFailureV1::OutsideScope);
                    }
                }
                value => segments.push(value),
            }
        }
        let joined = segments.join("/");
        let path = CargoPackageSourcePathV1::new(joined)
            .map_err(|_| CargoPackageReadmeLinkFailureV1::NotRelative)?;
        Ok(CargoPackageReadmeLinkTargetV1::File { path, fragment })
    }
}

fn decode_markdown_uri_component(
    component: &str,
) -> Result<String, CargoPackageReadmeLinkFailureV1> {
    let bytes = component.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let Some(pair) = bytes.get(index + 1..index + 3) else {
                return Err(CargoPackageReadmeLinkFailureV1::NotRelative);
            };
            let high = markdown_hex(pair[0]).ok_or(CargoPackageReadmeLinkFailureV1::NotRelative)?;
            let low = markdown_hex(pair[1]).ok_or(CargoPackageReadmeLinkFailureV1::NotRelative)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    let decoded =
        String::from_utf8(decoded).map_err(|_| CargoPackageReadmeLinkFailureV1::NotRelative)?;
    if decoded.chars().any(char::is_control) {
        return Err(CargoPackageReadmeLinkFailureV1::NotRelative);
    }
    Ok(decoded)
}

fn markdown_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Request to follow one relative link from an owner-admitted README.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CargoPackageReadmeLinkRequestV1 {
    /// Exact README origin returned by the owner.
    pub origin: CargoPackageReadmeOriginV1,
    /// Relative Markdown target; the owner resolves it against the exact
    /// README directory and refuses schemes, absolute paths, and root escape.
    pub href: String,
}

impl CargoPackageReadmeLinkRequestV1 {
    /// Checks bounded origin and link spelling before owner dispatch.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        self.origin.has_admissible_shape()
            && !self.href.is_empty()
            && self.href.len() <= MAX_CARGO_PACKAGE_README_LINK_BYTES
            && !self.href.chars().any(char::is_control)
            && self.href.trim() == self.href
    }
}

/// Stable failure when a README link cannot be read under its exact root.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CargoPackageReadmeLinkFailureV1 {
    /// The caller supplied a malformed README origin or href.
    InvalidRequest,
    /// The origin no longer matches the current manifest-selected README.
    StaleOrigin,
    /// The href is a scheme, absolute address, or otherwise not a local link.
    NotRelative,
    /// Dot segments would escape the exact package/workspace root.
    OutsideScope,
    /// The selected path is not an admitted source/document text kind.
    UnsupportedFileKind,
    /// The exact held-root target is absent, linked, or unreadable.
    TargetUnavailable,
    /// The target is larger than the bounded source-file limit.
    TargetTooLarge,
    /// The target is not valid bounded UTF-8 text.
    NotUtf8Text,
    /// The owner could not revalidate the current project/source observation.
    ObservationUnavailable,
}

/// Result of following one relative README link through its same held scope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CargoPackageReadmeLinkResultV1 {
    /// A same-README fragment; no filesystem read was needed.
    Anchor {
        /// Exact origin whose Markdown contained this fragment.
        origin: CargoPackageReadmeOriginV1,
        /// Bounded decoded fragment without the leading hash.
        fragment: Box<str>,
    },
    /// Current UTF-8 target bytes under the origin's exact held root.
    Read {
        /// Exact origin whose link was followed.
        origin: CargoPackageReadmeOriginV1,
        /// Current owner source authority for the package release.
        authority: CargoPackageSourceAuthorityV1,
        /// Same package/workspace scope as the README origin.
        root_scope: CargoPackageReadmeRootScopeV1,
        /// Resolved path relative to the admitted root scope.
        path: CargoPackageSourcePathV1,
        /// Optional target fragment without the leading hash.
        fragment: Option<Box<str>>,
        /// Digest of the exact returned UTF-8 target bytes.
        content_digest: [u8; 32],
        /// Bounded text read through the owner-held directory capability.
        contents: Box<str>,
        /// Source text does not itself prove semantic indexing.
        semantic: CargoPackageSourceSemanticStatusV1,
    },
    /// The README or project source authority changed after the origin was issued.
    Stale {
        /// Exact origin supplied by the caller.
        origin: CargoPackageReadmeOriginV1,
    },
    /// No target bytes were returned; the failure remains typed.
    Unavailable {
        /// Exact origin if its shape was valid.
        origin: Option<CargoPackageReadmeOriginV1>,
        /// Stable reason the owner could not read this relative target.
        reason: CargoPackageReadmeLinkFailureV1,
    },
}

impl CargoPackageReadmeLinkResultV1 {
    /// Validates target content and its exact package/project/scope binding.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        match self {
            Self::Anchor { origin, fragment } => {
                origin.has_admissible_shape()
                    && !fragment.is_empty()
                    && fragment.len() <= MAX_CARGO_PACKAGE_README_LINK_BYTES
                    && !fragment.chars().any(char::is_control)
            }
            Self::Read {
                origin,
                authority,
                root_scope,
                path,
                fragment,
                content_digest,
                contents,
                semantic,
            } => {
                origin.has_admissible_shape()
                    && authority.matches_package_reference(&origin.package)
                    && authority.has_admissible_shape()
                    && authority.roots().workspace_root
                        == origin.request_binding.effective_workspace_root_digest
                    && *root_scope == origin.root_scope
                    && path.has_admissible_shape()
                    && fragment.as_ref().is_none_or(|fragment| {
                        !fragment.is_empty()
                            && fragment.len() <= MAX_CARGO_PACKAGE_README_LINK_BYTES
                            && !fragment.chars().any(char::is_control)
                    })
                    && contents.len() <= MAX_CARGO_PACKAGE_SOURCE_FILE_BYTES
                    && !contents.as_bytes().contains(&0)
                    && blake3::hash(contents.as_bytes()).as_bytes() == content_digest
                    && *semantic == CargoPackageSourceSemanticStatusV1::NotIndexed
            }
            Self::Stale { origin } => origin.has_admissible_shape(),
            Self::Unavailable { origin, .. } => origin
                .as_ref()
                .is_none_or(CargoPackageReadmeOriginV1::has_admissible_shape),
        }
    }
}

/// Result of reading the README chosen by one currently admitted Cargo source.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CargoPackageReadmeResultV1 {
    /// Current README bytes selected under this exact source receipt.
    Read {
        /// Exact source-qualified package route requested.
        package: PackageReference,
        /// Current Cargo authority revalidated by the owner.
        authority: CargoPackageSourceAuthorityV1,
        /// Exact local project tree request that admitted this source.
        request_binding: ProjectTreeRequestBindingV1,
        /// Selected path, rule, bytes, and digest.
        readme: CargoPackageReadmeV1,
    },
    /// The exact current package manifest establishes that no README applies.
    Absent {
        /// Exact source-qualified package route requested.
        package: PackageReference,
        /// Current Cargo authority revalidated by the owner.
        authority: CargoPackageSourceAuthorityV1,
        /// Exact local project tree request that admitted this source.
        request_binding: ProjectTreeRequestBindingV1,
        /// Whether disabled explicitly or absent by Cargo's defaults.
        reason: CargoPackageReadmeAbsenceV1,
    },
    /// The owner observation no longer matches the route's source receipt.
    Stale {
        /// Exact source-qualified package reference requested.
        package: PackageReference,
        /// Exact request context when the owner had an admitted tree.
        request_binding: Option<ProjectTreeRequestBindingV1>,
    },
    /// No README was returned; the reason remains typed and no partial body is exposed.
    Unavailable {
        /// Exact source-qualified package reference when it was valid.
        package: Option<PackageReference>,
        /// Exact request context when available.
        request_binding: Option<ProjectTreeRequestBindingV1>,
        /// Stable reason the owner could not produce the README.
        reason: CargoPackageReadmeFailureV1,
    },
}

impl CargoPackageReadmeResultV1 {
    /// Validates route/authority association and bounded README content.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        match self {
            Self::Read {
                package,
                authority,
                request_binding,
                readme,
            } => {
                authority.matches_package_reference(package)
                    && authority.has_admissible_shape()
                    && request_binding.has_admissible_shape()
                    && request_binding
                        .matches_workspace_root_identity(authority.roots().workspace_root)
                    && readme.matches_authority_scope(authority, request_binding)
            }
            Self::Absent {
                package,
                authority,
                request_binding,
                ..
            } => {
                authority.matches_package_reference(package)
                    && authority.has_admissible_shape()
                    && request_binding.has_admissible_shape()
                    && request_binding
                        .matches_workspace_root_identity(authority.roots().workspace_root)
            }
            Self::Stale {
                package,
                request_binding,
            } => {
                CargoPackageSourceAuthorityV1::digest_from_package_reference(package).is_some()
                    && request_binding.is_none_or(|binding| binding.has_admissible_shape())
            }
            Self::Unavailable {
                package,
                request_binding,
                ..
            } => {
                package.as_ref().is_none_or(|package| {
                    CargoPackageSourceAuthorityV1::digest_from_package_reference(package).is_some()
                }) && request_binding.is_none_or(|binding| binding.has_admissible_shape())
            }
        }
    }
}

/// Why a bounded package-relative source inventory is incomplete.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CargoPackageSourceInventoryGapV1 {
    /// A directory has more direct entries than its bounded listing permits.
    DirectoryEntryLimit,
    /// The whole traversal reached its entry-count bound.
    ScanEntryLimit,
    /// The package tree is deeper than the supported traversal bound.
    DepthLimit,
    /// A path could not be represented by the canonical UTF-8 route type.
    UnaddressablePath,
    /// A directory changed or could not be opened without following links.
    DirectoryUnavailable,
}

/// Whether the listed addresses cover every supported safe source file.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "coverage", content = "detail", rename_all = "kebab-case")]
pub enum CargoPackageSourceInventoryCoverageV1 {
    /// The complete set of supported, regular, UTF-8-addressable files was
    /// enumerated. Internal build/VCS/config directories and links are
    /// deliberately excluded.
    Complete,
    /// There were more paths than one reply admits; only the bounded prefix
    /// is included and this fixed cap has no continuation cursor.
    Truncated {
        /// Maximum number of addresses in this inventory contract.
        limit: u16,
    },
    /// Some paths could not be observed safely. The listed rows are only an
    /// exact partial set, with no claim that omitted rows do not exist.
    Partial {
        /// The first bounded reason the traversal could not complete.
        reason: CargoPackageSourceInventoryGapV1,
    },
}

/// Bounded source and documentation addresses under an exact Cargo receipt.
/// These paths are navigation hints only; they do not prove indexed content.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CargoPackageSourceInventoryV1 {
    /// Exact source-qualified package route requested.
    pub package: PackageReference,
    /// Current Cargo authority revalidated by the owner.
    pub authority: CargoPackageSourceAuthorityV1,
    /// Exact tree request that admitted this source route.
    pub request_binding: ProjectTreeRequestBindingV1,
    /// Sorted canonical relative file addresses, each requiring an individual
    /// digest-checked read before its bytes are displayed.
    pub paths: Box<[CargoPackageSourcePathV1]>,
    /// Explicit completeness state of the bounded traversal.
    pub coverage: CargoPackageSourceInventoryCoverageV1,
}

impl CargoPackageSourceInventoryV1 {
    /// Validates route binding, path order, and explicit truncation shape.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        self.authority.matches_package_reference(&self.package)
            && self.authority.has_admissible_shape()
            && self.request_binding.has_admissible_shape()
            && self
                .request_binding
                .matches_workspace_root_identity(self.authority.roots().workspace_root)
            && self.paths.len() <= MAX_CARGO_PACKAGE_SOURCE_INVENTORY_PATHS
            && self
                .paths
                .iter()
                .all(CargoPackageSourcePathV1::has_admissible_shape)
            && self.paths.windows(2).all(|pair| pair[0] < pair[1])
            && match self.coverage {
                CargoPackageSourceInventoryCoverageV1::Complete
                | CargoPackageSourceInventoryCoverageV1::Partial { .. } => true,
                CargoPackageSourceInventoryCoverageV1::Truncated { limit } => {
                    usize::from(limit) == MAX_CARGO_PACKAGE_SOURCE_INVENTORY_PATHS
                        && self.paths.len() == MAX_CARGO_PACKAGE_SOURCE_INVENTORY_PATHS
                }
            }
    }
}

/// Why an owner could not produce a package source inventory.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CargoPackageSourceInventoryFailureV1 {
    /// The owner has no current Cargo metadata observation for this authority.
    AuthorityUnavailable,
    /// The Cargo metadata input set changed since the package was displayed.
    StaleAuthority,
    /// The exact Cargo row has no retained local root.
    PackageRootUnavailable,
    /// The owner could not hold or enumerate the exact package root.
    DirectoryUnavailable,
}

/// Result of reading one bounded inventory under an exact source authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CargoPackageSourceInventoryResultV1 {
    /// Exact observed file addresses with explicit completeness.
    Listed(CargoPackageSourceInventoryV1),
    /// The authority changed between the route and current owner lookup.
    Stale {
        /// Exact source-qualified package reference requested.
        package: PackageReference,
        /// Exact request selector which failed to match current owner state.
        request_binding: ProjectTreeRequestBindingV1,
    },
    /// No inventory was returned; the failure remains typed.
    Unavailable {
        /// Exact source-qualified package reference when it was valid.
        package: Option<PackageReference>,
        /// Exact request selector, when it was structurally valid.
        request_binding: Option<ProjectTreeRequestBindingV1>,
        /// Stable reason the source inventory could not answer.
        reason: CargoPackageSourceInventoryFailureV1,
    },
}

impl CargoPackageSourceInventoryResultV1 {
    /// Validates the bounded result and any exact source route it carries.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        match self {
            Self::Listed(inventory) => inventory.has_admissible_shape(),
            Self::Stale {
                package,
                request_binding,
            } => {
                CargoPackageSourceAuthorityV1::digest_from_package_reference(package).is_some()
                    && request_binding.has_admissible_shape()
            }
            Self::Unavailable {
                package,
                request_binding,
                ..
            } => {
                package.as_ref().is_none_or(|package| {
                    CargoPackageSourceAuthorityV1::digest_from_package_reference(package).is_some()
                }) && request_binding.is_none_or(|binding| binding.has_admissible_shape())
            }
        }
    }
}

impl Default for CargoPackageSourceAuthorityStateV1 {
    fn default() -> Self {
        Self::Unavailable(CargoPackageSourceAuthorityFailureV1::NotObserved)
    }
}

impl CargoPackageSourceAuthorityV1 {
    /// Builds the receipt from the exact fields returned by one Cargo metadata
    /// observation. Callers should only pass values read by the Cargo owner.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_metadata_observation(
        name: &str,
        version: &str,
        package_id: &str,
        source: Option<&str>,
        manifest_path: &str,
        workspace_root: &str,
        source_revision: [u8; 32],
        effective_target: &str,
        resolved_features: &[String],
    ) -> Result<Self, CargoPackageSourceAuthorityFailureV1> {
        if source_revision == [0; 32] {
            return Err(CargoPackageSourceAuthorityFailureV1::MissingSourceRevision);
        }
        if package_id.is_empty() || package_id.len() > MAX_CARGO_SOURCE_DETAIL_BYTES {
            return Err(CargoPackageSourceAuthorityFailureV1::MissingPackageIdentity);
        }
        if effective_target.trim().is_empty() {
            return Err(CargoPackageSourceAuthorityFailureV1::MissingEffectiveTarget);
        }
        if name.len() > MAX_CARGO_SOURCE_COORDINATE_BYTES
            || version.len() > MAX_CARGO_SOURCE_COORDINATE_BYTES
            || effective_target.len() > MAX_CARGO_SOURCE_DETAIL_BYTES
        {
            return Err(CargoPackageSourceAuthorityFailureV1::SourceDetailsOutOfBounds);
        }
        let name = admitted_exact_text(name, MAX_CARGO_SOURCE_COORDINATE_BYTES)?;
        let version = admitted_exact_text(version, MAX_CARGO_SOURCE_COORDINATE_BYTES)?;
        let effective_target =
            admitted_exact_text(effective_target, MAX_CARGO_SOURCE_DETAIL_BYTES)?;
        let _workspace_path = admitted_absolute_path(workspace_root)
            .ok_or(CargoPackageSourceAuthorityFailureV1::MissingWorkspaceRoot)?;
        let workspace_path = _workspace_path;
        let manifest = admitted_absolute_path(manifest_path)
            .ok_or(CargoPackageSourceAuthorityFailureV1::MissingManifestRoot)?;
        let package_root = manifest
            .parent()
            .ok_or(CargoPackageSourceAuthorityFailureV1::MissingManifestRoot)?;
        let source = parse_cargo_source(source)?;
        let path_display_digest = if matches!(&source, CargoPackageSourceV1::Path) {
            let display_path = Self::vendored_display_path(package_root, workspace_path)
                .ok_or(CargoPackageSourceAuthorityFailureV1::MissingManifestRoot)?;
            Some(digest_field(
                b"cargo-vendored-display-path.v1",
                display_path.as_bytes(),
            ))
        } else {
            None
        };

        let mut features = resolved_features.to_vec();
        if features.iter().any(|feature| {
            feature.is_empty()
                || feature.len() > MAX_CARGO_SOURCE_DETAIL_BYTES
                || feature.chars().any(char::is_control)
        }) {
            return Err(CargoPackageSourceAuthorityFailureV1::SourceDetailsOutOfBounds);
        }
        features.sort_unstable();
        if features.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(CargoPackageSourceAuthorityFailureV1::SourceDetailsOutOfBounds);
        }

        let authority = Self {
            schema: CARGO_PACKAGE_SOURCE_AUTHORITY_SCHEMA,
            name,
            version,
            package_id_digest: digest_field(b"cargo-package-id.v1", package_id.as_bytes()),
            source,
            roots: CargoPackageRootIdentityV1 {
                workspace_root: digest_path("cargo-workspace-root.v1", workspace_root),
                package_root: digest_path(
                    "cargo-package-root.v1",
                    package_root
                        .to_str()
                        .ok_or(CargoPackageSourceAuthorityFailureV1::MissingManifestRoot)?
                        .as_bytes(),
                ),
                manifest_path: digest_path("cargo-manifest-path.v1", manifest_path),
            },
            path_display_digest,
            source_revision,
            effective_target,
            resolved_features_digest: feature_digest(&features),
            authority_digest: [0; 32],
        }
        .with_computed_digest();
        if authority.has_admissible_shape() {
            Ok(authority)
        } else {
            Err(CargoPackageSourceAuthorityFailureV1::SourceDetailsOutOfBounds)
        }
    }

    /// The package's Cargo name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    /// The package's exact Cargo version spelling.
    #[must_use]
    pub fn version(&self) -> &str {
        self.version.as_str()
    }

    /// The exact source variant and source values.
    #[must_use]
    pub const fn source(&self) -> &CargoPackageSourceV1 {
        &self.source
    }

    /// Opaque exact identity for the Cargo package metadata row.
    #[must_use]
    pub const fn package_id_digest(&self) -> [u8; 32] {
        self.package_id_digest
    }

    /// Exact workspace, manifest, and package-root path commitments.
    #[must_use]
    pub const fn roots(&self) -> CargoPackageRootIdentityV1 {
        self.roots
    }

    /// Checks that a local owner path is the exact package root observed by
    /// Cargo metadata. The path remains local and is never serialized here.
    #[must_use]
    pub fn matches_package_root_path(&self, path: &Path) -> bool {
        path.to_str().is_some_and(|path| {
            self.roots.package_root == digest_path("cargo-package-root.v1", path.as_bytes())
        })
    }

    /// Checks that a local owner path is the workspace root observed by Cargo.
    #[must_use]
    pub fn matches_workspace_root_path(&self, path: &Path) -> bool {
        path.to_str().is_some_and(|path| {
            self.roots.workspace_root == digest_path("cargo-workspace-root.v1", path.as_bytes())
        })
    }

    /// Checks whether a rendered local path agrees with the owner-observed
    /// package root commitment.
    #[must_use]
    pub fn matches_vendored_display_path(&self, path: &str) -> bool {
        self.path_display_digest.is_some_and(|expected| {
            expected == digest_field(b"cargo-vendored-display-path.v1", path.as_bytes())
        })
    }

    /// Makes a bounded, non-path-leaking label for a local Cargo package.
    /// Roots outside the workspace receive an opaque stable label; this is a
    /// display identity only and must never be opened as a filesystem path.
    pub(crate) fn vendored_display_path(
        package_root: &Path,
        workspace_root: &Path,
    ) -> Option<String> {
        if let Ok(relative) = package_root.strip_prefix(workspace_root) {
            let relative = relative.to_str()?;
            if !relative.chars().any(char::is_control) {
                return Some(if relative.is_empty() {
                    ".".to_owned()
                } else {
                    relative.replace('\\', "/")
                });
            }
            return None;
        }
        let root = package_root.to_str()?;
        let digest = digest_path("cargo-package-root.v1", root.as_bytes());
        Some(format!("external/{}", lower_hex(&digest)[..16].to_owned()))
    }

    /// Cargo metadata and package-source revision observed by the owner.
    #[must_use]
    pub const fn source_revision(&self) -> [u8; 32] {
        self.source_revision
    }

    /// The effective target used for this Cargo metadata observation.
    #[must_use]
    pub fn effective_target(&self) -> &str {
        self.effective_target.as_str()
    }

    /// Heap capacity retained by this receipt's owned text fields.
    #[doc(hidden)]
    #[must_use]
    pub fn retained_text_capacity_bytes(&self) -> usize {
        let mut bytes = self
            .name
            .retained_capacity()
            .saturating_add(self.version.retained_capacity())
            .saturating_add(self.effective_target.retained_capacity());
        bytes = bytes.saturating_add(match &self.source {
            CargoPackageSourceV1::Registry { index_url, .. } => index_url.retained_capacity(),
            CargoPackageSourceV1::Git {
                repository_url,
                requested_query,
                resolved_commit,
            } => repository_url
                .retained_capacity()
                .saturating_add(
                    requested_query
                        .as_ref()
                        .map_or(0, ProductText::retained_capacity),
                )
                .saturating_add(resolved_commit.retained_capacity()),
            CargoPackageSourceV1::Path => 0,
        });
        bytes
    }

    /// Hash of the exact sorted features Cargo resolved for this package.
    #[must_use]
    pub const fn resolved_features_digest(&self) -> [u8; 32] {
        self.resolved_features_digest
    }

    /// Canonical identity of all source, root, and resolution facts.
    #[must_use]
    pub const fn authority_digest(&self) -> [u8; 32] {
        self.authority_digest
    }

    /// Checks the decoded DTO's shape and recomputes its identity.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        self.schema == CARGO_PACKAGE_SOURCE_AUTHORITY_SCHEMA
            && self.package_id_digest != [0; 32]
            && self.source_revision != [0; 32]
            && self.roots.workspace_root != [0; 32]
            && self.roots.package_root != [0; 32]
            && self.roots.manifest_path != [0; 32]
            && (matches!(&self.source, CargoPackageSourceV1::Path)
                == self.path_display_digest.is_some())
            && self
                .path_display_digest
                .is_none_or(|digest| digest != [0; 32])
            && self.resolved_features_digest != [0; 32]
            && !self.name.as_str().is_empty()
            && exact_text_is_valid(self.name.as_str(), MAX_CARGO_SOURCE_COORDINATE_BYTES)
            && !self.version.as_str().is_empty()
            && exact_text_is_valid(self.version.as_str(), MAX_CARGO_SOURCE_COORDINATE_BYTES)
            && exact_text_is_valid(
                self.effective_target.as_str(),
                MAX_CARGO_SOURCE_DETAIL_BYTES,
            )
            && source_has_admissible_shape(&self.source)
            && self.authority_digest == self.compute_digest()
    }

    /// Constructs the source-qualified Cargo package coordinate used by
    /// route, page-cache, and search identities. The opaque qualifier binds
    /// this coordinate to the full receipt; the owner must revalidate it.
    pub fn package_reference(
        &self,
    ) -> Result<PackageReference, CargoPackageSourceAuthorityFailureV1> {
        if !self.has_admissible_shape() {
            return Err(CargoPackageSourceAuthorityFailureV1::SourceDetailsOutOfBounds);
        }
        let spelling = format!(
            "pkg:cargo/{}@{}?cargo-authority={}",
            self.name.as_str(),
            self.version.as_str(),
            lower_hex(&self.authority_digest),
        );
        PackageCoordinate::parse(spelling)
            .map(PackageReference::Purl)
            .map_err(|_| CargoPackageSourceAuthorityFailureV1::SourceDetailsOutOfBounds)
    }

    /// Returns the authority digest only for a source-qualified Cargo PURL.
    /// No URL, Git ref, or local filesystem path is parsed by the caller.
    #[must_use]
    pub fn digest_from_package_reference(reference: &PackageReference) -> Option<[u8; 32]> {
        let PackageReference::Purl(coordinate) = reference else {
            return None;
        };
        if coordinate.package_type() != backend_semantic::vocabulary::PackageType::Cargo {
            return None;
        }
        if coordinate.namespace().is_some() || coordinate.subpath().is_some() {
            return None;
        }
        let digest_text = coordinate.qualifiers()?.strip_prefix("cargo-authority=")?;
        let digest = parse_lower_hex(digest_text)?;
        let canonical = format!(
            "pkg:cargo/{}@{}?cargo-authority={}",
            coordinate.name(),
            coordinate.version(),
            lower_hex(&digest),
        );
        (coordinate.as_str() == canonical).then_some(digest)
    }

    /// Confirms that this receipt is the exact source authority encoded in a
    /// package reference, including its name and version.
    #[must_use]
    pub fn matches_package_reference(&self, reference: &PackageReference) -> bool {
        self.package_reference().as_ref() == Ok(reference)
    }

    fn with_computed_digest(mut self) -> Self {
        self.authority_digest = self.compute_digest();
        self
    }

    fn compute_digest(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.cargo-package-source-authority.v1\0");
        hasher.update(&self.schema.to_le_bytes());
        hash_text(&mut hasher, self.name.as_str());
        hash_text(&mut hasher, self.version.as_str());
        hasher.update(&self.package_id_digest);
        match &self.source {
            CargoPackageSourceV1::Registry { scheme, index_url } => {
                hasher.update(&[0]);
                hasher.update(&[*scheme as u8]);
                hash_text(&mut hasher, index_url.as_str());
            }
            CargoPackageSourceV1::Git {
                repository_url,
                requested_query,
                resolved_commit,
            } => {
                hasher.update(&[1]);
                hash_text(&mut hasher, repository_url.as_str());
                match requested_query {
                    Some(query) => {
                        hasher.update(&[1]);
                        hash_text(&mut hasher, query.as_str());
                    }
                    None => {
                        hasher.update(&[0]);
                    }
                }
                hash_text(&mut hasher, resolved_commit.as_str());
            }
            CargoPackageSourceV1::Path => {
                hasher.update(&[2]);
            }
        };
        hasher.update(&self.roots.workspace_root);
        hasher.update(&self.roots.package_root);
        hasher.update(&self.roots.manifest_path);
        match self.path_display_digest {
            Some(digest) => {
                hasher.update(&[1]);
                hasher.update(&digest);
            }
            None => {
                hasher.update(&[0]);
            }
        }
        hasher.update(&self.source_revision);
        hash_text(&mut hasher, self.effective_target.as_str());
        hasher.update(&self.resolved_features_digest);
        *hasher.finalize().as_bytes()
    }
}

fn parse_cargo_source(
    source: Option<&str>,
) -> Result<CargoPackageSourceV1, CargoPackageSourceAuthorityFailureV1> {
    let Some(source) = source else {
        return Ok(CargoPackageSourceV1::Path);
    };
    if let Some(index_url) = source.strip_prefix("registry+") {
        return Ok(CargoPackageSourceV1::Registry {
            scheme: CargoRegistrySourceSchemeV1::Registry,
            index_url: admitted_exact_text(index_url, MAX_CARGO_SOURCE_DETAIL_BYTES)?,
        });
    }
    if let Some(index_url) = source.strip_prefix("sparse+") {
        return Ok(CargoPackageSourceV1::Registry {
            scheme: CargoRegistrySourceSchemeV1::Sparse,
            index_url: admitted_exact_text(index_url, MAX_CARGO_SOURCE_DETAIL_BYTES)?,
        });
    }
    if let Some(git) = source.strip_prefix("git+") {
        let (requested_and_repo, resolved_commit) = git
            .rsplit_once('#')
            .ok_or(CargoPackageSourceAuthorityFailureV1::UnsupportedSource)?;
        if !valid_git_commit(resolved_commit) {
            return Err(CargoPackageSourceAuthorityFailureV1::UnsupportedSource);
        }
        let (repository_url, requested_query) = requested_and_repo
            .split_once('?')
            .map_or((requested_and_repo, None), |(repository, query)| {
                (repository, Some(query))
            });
        let repository_url = admitted_exact_text(repository_url, MAX_CARGO_SOURCE_DETAIL_BYTES)?;
        let requested_query = requested_query
            .map(|query| admitted_exact_text(query, MAX_CARGO_SOURCE_DETAIL_BYTES))
            .transpose()?;
        let resolved_commit = admitted_exact_text(resolved_commit, 64)?;
        return Ok(CargoPackageSourceV1::Git {
            repository_url,
            requested_query,
            resolved_commit,
        });
    }
    Err(CargoPackageSourceAuthorityFailureV1::UnsupportedSource)
}

fn source_has_admissible_shape(source: &CargoPackageSourceV1) -> bool {
    match source {
        CargoPackageSourceV1::Registry { index_url, .. } => {
            exact_text_is_valid(index_url.as_str(), MAX_CARGO_SOURCE_DETAIL_BYTES)
        }
        CargoPackageSourceV1::Git {
            repository_url,
            requested_query,
            resolved_commit,
        } => {
            exact_text_is_valid(repository_url.as_str(), MAX_CARGO_SOURCE_DETAIL_BYTES)
                && requested_query.as_ref().is_none_or(|query| {
                    exact_text_is_valid(query.as_str(), MAX_CARGO_SOURCE_DETAIL_BYTES)
                })
                && valid_git_commit(resolved_commit.as_str())
        }
        CargoPackageSourceV1::Path => true,
    }
}

fn admitted_exact_text(
    value: &str,
    maximum: usize,
) -> Result<ProductText, CargoPackageSourceAuthorityFailureV1> {
    if !exact_text_is_valid(value, maximum) {
        return Err(CargoPackageSourceAuthorityFailureV1::SourceDetailsOutOfBounds);
    }
    ProductText::new(value.to_owned())
        .map_err(|_| CargoPackageSourceAuthorityFailureV1::SourceDetailsOutOfBounds)
}

fn exact_text_is_valid(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn admitted_absolute_path(value: &str) -> Option<&Path> {
    let path = Path::new(value);
    (path.is_absolute()
        && !value.is_empty()
        && value.len() <= MAX_CARGO_SOURCE_DETAIL_BYTES
        && !value.chars().any(char::is_control))
    .then_some(path)
}

fn valid_git_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn feature_digest(features: &[String]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.cargo-resolved-features.v1\0");
    hasher.update(&(features.len() as u64).to_le_bytes());
    for feature in features {
        hash_text(&mut hasher, feature);
    }
    *hasher.finalize().as_bytes()
}

fn digest_path(domain: &str, value: &[u8]) -> [u8; 32] {
    digest_field(domain.as_bytes(), value)
}

fn digest_field(domain: &[u8], value: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&(domain.len() as u64).to_le_bytes());
    hasher.update(domain);
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
    *hasher.finalize().as_bytes()
}

fn hash_text(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn lower_hex(value: &[u8; 32]) -> String {
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn parse_lower_hex(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 || value.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    let mut digest = [0; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        digest[index] = (high << 4) | low;
    }
    Some(digest)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority(source: Option<&str>) -> CargoPackageSourceAuthorityV1 {
        CargoPackageSourceAuthorityV1::from_metadata_observation(
            "serde",
            "1.0.219",
            "cargo-package-id-exact",
            source,
            "/workspace/.cargo/registry/src/serde-1.0.219/Cargo.toml",
            "/workspace",
            [7; 32],
            "x86_64-unknown-linux-gnu",
            &["derive".to_owned(), "std".to_owned()],
        )
        .expect("observed Cargo source")
    }

    #[test]
    fn registry_sparse_git_query_commit_and_path_root_are_distinct() {
        let registry = authority(Some("registry+https://example.test/index"));
        let sparse = authority(Some("sparse+https://example.test/index"));
        let git_branch = authority(Some(
            "git+https://example.test/serde?branch=main#0123456789abcdef0123456789abcdef01234567",
        ));
        let git_tag = authority(Some(
            "git+https://example.test/serde?tag=main#0123456789abcdef0123456789abcdef01234567",
        ));
        let git_commit = authority(Some(
            "git+https://example.test/serde?branch=main#1123456789abcdef0123456789abcdef01234567",
        ));
        let path = authority(None);

        assert_ne!(registry.authority_digest(), sparse.authority_digest());
        assert_ne!(git_branch.authority_digest(), git_tag.authority_digest());
        assert_ne!(git_branch.authority_digest(), git_commit.authority_digest());
        assert_ne!(path.authority_digest(), registry.authority_digest());
    }

    #[test]
    fn qualified_package_reference_round_trips_the_exact_authority_digest() {
        let receipt = authority(Some(
            "git+https://example.test/serde?branch=main#0123456789abcdef0123456789abcdef01234567",
        ));
        let reference = receipt.package_reference().expect("qualified route key");
        assert!(receipt.matches_package_reference(&reference));
        assert_eq!(
            CargoPackageSourceAuthorityV1::digest_from_package_reference(&reference),
            Some(receipt.authority_digest())
        );
        let reparsed = PackageReference::parse(reference.as_str()).expect("route survives text");
        assert!(receipt.matches_package_reference(&reparsed));
    }

    #[test]
    fn source_qualified_reference_rejects_uncommitted_extra_qualifiers() {
        let receipt = authority(Some("registry+https://example.test/index"));
        let mut spelling = receipt
            .package_reference()
            .expect("qualified route key")
            .as_str()
            .to_owned();
        spelling.push_str("&repository_url=https%3A%2F%2Fother.example");
        let altered = PackageReference::parse(spelling).expect("valid but altered purl");
        assert!(CargoPackageSourceAuthorityV1::digest_from_package_reference(&altered).is_none());
        assert!(
            !receipt.matches_package_reference(&altered),
            "a receipt admits only the exact canonical route it minted"
        );
    }

    #[test]
    fn package_source_paths_reject_ambiguous_or_traversing_spellings() {
        for invalid in [
            "",
            "/etc/passwd",
            "src/../Cargo.toml",
            "src//lib.rs",
            "src/./lib.rs",
            "src\\lib.rs",
            "src/C:\\Cargo.toml",
            ".git/config",
        ] {
            assert!(
                CargoPackageSourcePathV1::new(invalid).is_err(),
                "accepted unsafe path {invalid:?}"
            );
        }
        let path = CargoPackageSourcePathV1::new("src/lib.rs").expect("relative Rust source");
        let serialized = serde_json::to_string(&path).expect("serialize path");
        assert_eq!(serialized, "\"src/lib.rs\"");
        assert!(serde_json::from_str::<CargoPackageSourcePathV1>("\"src/../Cargo.toml\"").is_err());
    }

    #[test]
    fn malformed_or_noncanonical_authorities_fail_closed() {
        assert!(
            CargoPackageSourceAuthorityV1::from_metadata_observation(
                "serde",
                "1.0.219",
                "cargo-package-id-exact",
                Some("git+https://example.test/serde?branch=main#not-a-commit"),
                "/workspace/serde/Cargo.toml",
                "/workspace",
                [7; 32],
                "x86_64-unknown-linux-gnu",
                &[],
            )
            .is_err()
        );
        assert!(
            CargoPackageSourceAuthorityV1::from_metadata_observation(
                "serde",
                "1.0.219",
                "cargo-package-id-exact",
                Some("registry+https://example.test/index"),
                "relative/Cargo.toml",
                "/workspace",
                [7; 32],
                "x86_64-unknown-linux-gnu",
                &[],
            )
            .is_err()
        );
    }

    #[test]
    fn external_path_sources_use_opaque_display_labels() {
        let outside = Path::new("/private/cargo/sources/helper");
        let workspace = Path::new("/private/project");
        let label = CargoPackageSourceAuthorityV1::vendored_display_path(outside, workspace)
            .expect("bounded opaque label");
        assert!(label.starts_with("external/"));
        assert!(!label.contains("/private/"));
        let receipt = CargoPackageSourceAuthorityV1::from_metadata_observation(
            "helper",
            "1.0.0",
            "path+file:///private/cargo/sources/helper#helper@1.0.0",
            None,
            "/private/cargo/sources/helper/Cargo.toml",
            "/private/project",
            [9; 32],
            "x86_64-unknown-linux-gnu",
            &[],
        )
        .expect("path source receipt");
        assert!(receipt.matches_vendored_display_path(&label));
        assert!(receipt.package_reference().is_ok());
    }

    #[test]
    fn serde_rejects_a_tampered_authority_digest() {
        let receipt = authority(Some("registry+https://example.test/index"));
        let mut value = serde_json::to_value(receipt).expect("serialize");
        value["authority_digest"] = serde_json::json!(vec![0_u8; 32]);
        assert!(serde_json::from_value::<CargoPackageSourceAuthorityV1>(value).is_err());
    }

    #[test]
    fn readme_result_binds_exact_package_authority_path_and_content_digest() {
        let authority = authority(Some("registry+https://example.test/index"));
        let package = authority.package_reference().expect("exact package route");
        let request_binding = ProjectTreeRequestBindingV1::for_paths(
            Path::new("/workspace/crates/app"),
            "/workspace",
        )
        .expect("tree request binding");
        assert_eq!(
            request_binding.effective_workspace_root_digest,
            authority.roots().workspace_root,
            "the tree and exact source receipt must bind the same workspace root"
        );
        let contents = "# serde\n\nA real release README.\n";
        let result = CargoPackageReadmeResultV1::Read {
            package: package.clone(),
            authority: authority.clone(),
            request_binding,
            readme: CargoPackageReadmeV1 {
                root_scope: CargoPackageReadmeRootScopeV1::Package,
                path: CargoPackageSourcePathV1::new("crates-io.md").expect("canonical README path"),
                selection: CargoPackageReadmeSelectionV1::ManifestPath,
                content_digest: *blake3::hash(contents.as_bytes()).as_bytes(),
                contents: contents.into(),
                semantic: CargoPackageSourceSemanticStatusV1::NotIndexed,
            },
        };
        assert!(result.has_admissible_shape());

        let mut mismatched_scope = result.clone();
        let CargoPackageReadmeResultV1::Read { readme, .. } = &mut mismatched_scope else {
            unreachable!()
        };
        readme.root_scope = CargoPackageReadmeRootScopeV1::EffectiveWorkspace;
        assert!(!mismatched_scope.has_admissible_shape());

        let mut mismatched_project = result.clone();
        let CargoPackageReadmeResultV1::Read {
            request_binding, ..
        } = &mut mismatched_project
        else {
            unreachable!()
        };
        request_binding.effective_workspace_root_digest = [42; 32];
        assert!(!mismatched_project.has_admissible_shape());

        let absent = CargoPackageReadmeResultV1::Absent {
            package: package.clone(),
            authority: authority.clone(),
            request_binding,
            reason: CargoPackageReadmeAbsenceV1::NoCargoDefault,
        };
        assert!(absent.has_admissible_shape());

        let stale = CargoPackageReadmeResultV1::Stale {
            package,
            request_binding: Some(request_binding),
        };
        assert!(stale.has_admissible_shape());

        let mut tampered = result;
        let CargoPackageReadmeResultV1::Read { readme, .. } = &mut tampered else {
            unreachable!()
        };
        readme.content_digest = [0; 32];
        assert!(!tampered.has_admissible_shape());
    }

    #[test]
    fn readme_link_resolution_is_relative_bounded_and_root_scoped() {
        let authority = authority(Some("registry+https://example.test/index"));
        let package = authority.package_reference().expect("exact package route");
        let request_binding = ProjectTreeRequestBindingV1::for_paths(
            Path::new("/workspace/crates/app"),
            "/workspace",
        )
        .expect("tree request binding");
        let contents = "# app\n";
        let readme = CargoPackageReadmeResultV1::Read {
            package,
            authority,
            request_binding,
            readme: CargoPackageReadmeV1 {
                root_scope: CargoPackageReadmeRootScopeV1::Package,
                path: CargoPackageSourcePathV1::new("docs/README.md")
                    .expect("canonical README path"),
                selection: CargoPackageReadmeSelectionV1::ManifestPath,
                content_digest: *blake3::hash(contents.as_bytes()).as_bytes(),
                contents: contents.into(),
                semantic: CargoPackageSourceSemanticStatusV1::NotIndexed,
            },
        };
        let origin = CargoPackageReadmeOriginV1::from_result(&readme).expect("owner origin");
        assert_eq!(
            origin
                .resolve_relative_href("../guide.md#start")
                .expect("scoped link"),
            CargoPackageReadmeLinkTargetV1::File {
                path: CargoPackageSourcePathV1::new("guide.md").expect("normalized path"),
                fragment: Some("start".into()),
            }
        );
        assert_eq!(
            origin.resolve_relative_href("#overview").expect("anchor"),
            CargoPackageReadmeLinkTargetV1::Anchor {
                fragment: "overview".into(),
            }
        );
        assert_eq!(
            origin
                .resolve_relative_href("guide.md?view=1#api")
                .expect("query and fragment"),
            CargoPackageReadmeLinkTargetV1::File {
                path: CargoPackageSourcePathV1::new("docs/guide.md").expect("normalized path"),
                fragment: Some("api".into()),
            }
        );
        for href in [
            "https://example.test/guide.md",
            "//example.test/guide.md",
            "/absolute/guide.md",
            "../../outside.md",
            "%2e%2e/%2e%2e/outside.md",
            "bad\\path.md",
            "guide.md%00",
        ] {
            assert!(
                origin.resolve_relative_href(href).is_err(),
                "unexpectedly admitted {href:?}"
            );
        }

        let request = CargoPackageReadmeLinkRequestV1 {
            origin: origin.clone(),
            href: "../guide.md".to_owned(),
        };
        assert!(request.has_admissible_shape());
        let mut foreign_origin = request.clone();
        foreign_origin.origin.request_binding.requested_root_digest = [7; 32];
        assert!(foreign_origin.has_admissible_shape());
        assert_ne!(
            foreign_origin.origin.request_binding.requested_root_digest,
            origin.request_binding.requested_root_digest,
            "the owner must compare the request binding against its retained current tree"
        );
    }
}
