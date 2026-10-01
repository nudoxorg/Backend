//! Exact, owner-observed source identity for Cargo packages.
//!
//! A package name and version do not identify a Cargo source. The same
//! package can be supplied by different registry protocols, Git revisions,
//! or local path roots. This receipt keeps those facts together with the
//! Cargo resolution that observed them. Its short digest can be carried in a
//! package coordinate; the owner must still match the receipt against its
//! current Cargo observation before it reads from the source.

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
/// Maximum bytes in a package-relative source path.
pub const MAX_CARGO_PACKAGE_SOURCE_PATH_BYTES: usize = 1_024;

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
    },
    /// No file was returned; the failure remains typed.
    Unavailable {
        /// Exact source-qualified package route requested when it was valid.
        package: Option<PackageReference>,
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
                path,
                content_digest,
                contents,
                semantic: CargoPackageSourceSemanticStatusV1::NotIndexed,
            } => {
                authority.matches_package_reference(package)
                    && authority.has_admissible_shape()
                    && path.has_admissible_shape()
                    && contents.len() <= MAX_CARGO_PACKAGE_SOURCE_FILE_BYTES
                    && !contents.as_bytes().contains(&0)
                    && blake3::hash(contents.as_bytes()).as_bytes() == content_digest
            }
            Self::Stale { package } => {
                CargoPackageSourceAuthorityV1::digest_from_package_reference(package).is_some()
            }
            Self::Unavailable { package, .. } => package.as_ref().is_none_or(|package| {
                CargoPackageSourceAuthorityV1::digest_from_package_reference(package).is_some()
            }),
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
        value["authority_digest"] = serde_json::json!([0; 32]);
        assert!(serde_json::from_value::<CargoPackageSourceAuthorityV1>(value).is_err());
    }
}
