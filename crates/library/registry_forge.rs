//! Versioned registry-to-forge lineage facts.
//!
//! A registry coordinate is not enough to recover the source that produced a
//! release.  This module carries the admitted relationship explicitly so the
//! desktop, CLI, and MCP surfaces can request a forge record by typed
//! identity instead of guessing from package names or URLs.  The association
//! is deliberately smaller than [`crate::ForgePackageRecord`]: immutable
//! forge objects and content IDs are references that let the detailed forge
//! projection be fetched and merged without duplicating blobs in every
//! registry row.

use crate::surface::{PackageReference, ProductText};
use crate::{ForgeCoordinate, ForgeObjectId, ForgeUnavailableReason};
#[cfg(test)]
use crate::{ForgeRefName, ForgeRevision};
use serde::{Deserialize, Serialize};

mod blob;
use blob::admit_blobs;
pub use blob::{
    RegistryForgeBlobFrontier, RegistryForgeBlobKind, RegistryForgeBlobPage, RegistryForgeBlobRef,
};

/// Schema version for [`RegistryForgeAssociation`].
pub const REGISTRY_FORGE_ASSOCIATION_VERSION: u16 = 2;
/// Maximum number of forge associations retained for one registry release.
pub const MAX_REGISTRY_FORGE_ASSOCIATIONS: usize = 8;
/// Maximum number of candidate forge identities retained by one association.
pub const MAX_REGISTRY_FORGE_CANDIDATES: usize = 8;
/// Maximum number of immutable forge objects or content blobs referenced by one association.
pub const MAX_REGISTRY_FORGE_BLOBS: usize = 8;
/// Maximum number of content-addressed pages named by one source-file frontier.
pub const MAX_REGISTRY_FORGE_PAGES: usize = 1024;
/// Schema version for [`RegistryForgeBlobFrontier`].
pub const REGISTRY_FORGE_BLOB_FRONTIER_VERSION: u16 = 1;
/// Maximum canonical JSON identity payload for one forge association.
pub const MAX_REGISTRY_FORGE_ASSOCIATION_BYTES: usize = 512 * 1024;

/// One versioned relationship between a registry release lineage and forge source identities.
///
/// `facts_version` is a content identity over every field except itself.  It
/// makes an association safe to cache and lets a consumer merge a detailed
/// forge response by exact facts rather than by display strings.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryForgeAssociation {
    /// Association schema version.
    pub version: u16,
    /// Content identity of the association facts.
    pub facts_version: [u8; 32],
    /// Exact registry package lineage this association belongs to.
    pub registry: PackageReference,
    /// One candidate for a resolved relationship, or several candidates when ambiguous.
    pub candidates: Box<[RegistryForgeCandidate]>,
    /// Whether the forge source was resolved, ambiguous, or unavailable.
    pub state: RegistryForgeAssociationState,
    /// Typed source of the relationship evidence.
    pub provenance: RegistryForgeProvenance,
    /// Optional paged source-file frontier. The inline blob list is only a
    /// bounded hot head; this root and its CAS pages can represent a complete
    /// source corpus without copying every file into every release row.
    #[serde(default)]
    pub source_files: Option<RegistryForgeBlobFrontier>,
}

impl RegistryForgeAssociation {
    /// Builds and identities a structurally valid association.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryForgeAssociationError`] when any bounded field or
    /// state invariant is invalid.
    pub fn new(
        registry: PackageReference,
        candidates: Box<[RegistryForgeCandidate]>,
        state: RegistryForgeAssociationState,
        provenance: RegistryForgeProvenance,
    ) -> Result<Self, RegistryForgeAssociationError> {
        let mut association = Self {
            version: REGISTRY_FORGE_ASSOCIATION_VERSION,
            facts_version: [0; 32],
            registry,
            candidates,
            state,
            provenance,
            source_files: None,
        };
        association.admit_payload()?;
        association.facts_version = association.identity()?;
        association.admit().map(|()| association)
    }

    /// Adds a paged source-file frontier and re-identifies the association.
    pub fn with_source_files(
        mut self,
        source_files: RegistryForgeBlobFrontier,
    ) -> Result<Self, RegistryForgeAssociationError> {
        source_files.admit()?;
        self.source_files = Some(source_files);
        self.facts_version = [0; 32];
        self.facts_version = self.identity()?;
        self.admit().map(|()| self)
    }

    /// Returns the stable content identity for the association's payload.
    ///
    /// The schema version is included in the digest domain so a future schema
    /// cannot accidentally alias this association.
    pub fn identity(&self) -> Result<[u8; 32], RegistryForgeAssociationError> {
        let payload = RegistryForgeAssociationIdentity {
            version: self.version,
            registry: &self.registry,
            candidates: &self.candidates,
            state: &self.state,
            provenance: &self.provenance,
            source_files: &self.source_files,
        };
        let encoded = canonical_json(&payload)?;
        if encoded.len() > MAX_REGISTRY_FORGE_ASSOCIATION_BYTES {
            return Err(RegistryForgeAssociationError::BytesBound);
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.registry-forge-association\0");
        hasher.update(&encoded);
        Ok(*hasher.finalize().as_bytes())
    }

    /// Returns canonical versioned wire bytes for this association.
    pub fn encode_canonical(&self) -> Result<Vec<u8>, RegistryForgeAssociationError> {
        let encoded = canonical_json(self)?;
        if encoded.len() > MAX_REGISTRY_FORGE_ASSOCIATION_BYTES {
            return Err(RegistryForgeAssociationError::BytesBound);
        }
        Ok(encoded)
    }

    /// Decodes and strictly validates canonical association wire bytes.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, RegistryForgeAssociationError> {
        if bytes.len() > MAX_REGISTRY_FORGE_ASSOCIATION_BYTES {
            return Err(RegistryForgeAssociationError::BytesBound);
        }
        let association: Self = serde_json::from_slice(bytes)
            .map_err(|_| RegistryForgeAssociationError::IdentityEncoding)?;
        if association.encode_canonical()?.as_slice() != bytes {
            return Err(RegistryForgeAssociationError::NonCanonicalEncoding);
        }
        association.admit()?;
        Ok(association)
    }

    /// Admits the association and verifies its content identity.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryForgeAssociationError`] when the association is
    /// malformed, exceeds a bound, or carries a stale `facts_version`.
    pub fn admit(&self) -> Result<(), RegistryForgeAssociationError> {
        self.admit_payload()?;
        if self.facts_version != self.identity()? {
            return Err(RegistryForgeAssociationError::IdentityMismatch);
        }
        Ok(())
    }

    /// Admits this association and verifies that it belongs to one registry row.
    pub fn admit_for_registry(
        &self,
        registry: &PackageReference,
    ) -> Result<(), RegistryForgeAssociationError> {
        self.admit()?;
        if &self.registry != registry {
            return Err(RegistryForgeAssociationError::RegistryMismatch);
        }
        Ok(())
    }

    /// Returns whether the relationship has one immutable forge identity.
    #[must_use]
    pub const fn is_resolved(&self) -> bool {
        matches!(&self.state, RegistryForgeAssociationState::Resolved { .. })
    }

    fn admit_payload(&self) -> Result<(), RegistryForgeAssociationError> {
        if self.version != REGISTRY_FORGE_ASSOCIATION_VERSION {
            return Err(RegistryForgeAssociationError::Version);
        }
        if !matches!(self.registry, PackageReference::Purl(_)) {
            return Err(RegistryForgeAssociationError::RegistryPackage);
        }
        if self.candidates.len() > MAX_REGISTRY_FORGE_CANDIDATES {
            return Err(RegistryForgeAssociationError::CandidateBound);
        }
        for candidate in &self.candidates {
            candidate.admit()?;
        }
        for (index, candidate) in self.candidates.iter().enumerate() {
            if self.candidates[index + 1..]
                .iter()
                .any(|other| other.source == candidate.source)
            {
                return Err(RegistryForgeAssociationError::DuplicateCandidate);
            }
        }
        self.state.admit(&self.candidates)?;
        if let Some(source_files) = &self.source_files {
            source_files.admit()?;
        }
        Ok(())
    }
}

/// One candidate forge identity attached to a registry lineage.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryForgeCandidate {
    /// Structured canonical forge identity.
    pub source: RegistryForgeSourceIdentity,
    /// Confidence assigned by the evidence authority.
    pub confidence: RegistryForgeConfidence,
}

impl RegistryForgeCandidate {
    fn admit(&self) -> Result<(), RegistryForgeAssociationError> {
        self.source.admit()
    }
}

/// Canonical forge identity retained beside a registry release.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryForgeSourceIdentity {
    /// Canonical typed forge coordinate used as the exact lookup key.
    pub coordinate: ForgeCoordinate,
}

impl RegistryForgeSourceIdentity {
    fn admit(&self) -> Result<(), RegistryForgeAssociationError> {
        if !self.coordinate.identity_is_valid() {
            return Err(RegistryForgeAssociationError::SourceIdentity);
        }
        Ok(())
    }
}

/// Relationship state retained even when a forge authority cannot be reached.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "kebab-case")]
pub enum RegistryForgeAssociationState {
    /// Exactly one candidate resolved to immutable forge objects.
    Resolved {
        /// Immutable commit object.
        commit: ForgeObjectId,
        /// Immutable tree object, when the authority reported one.
        tree: Option<ForgeObjectId>,
        /// Content-addressed archive, tree, snapshot, or source-file blobs.
        blobs: Box<[RegistryForgeBlobRef]>,
    },
    /// More than one candidate remains possible; no string-based choice is implied.
    Ambiguous,
    /// Resolution is unavailable, with optional cached content references.
    Unavailable {
        /// Typed reason for the unavailable state.
        reason: ForgeUnavailableReason,
        /// Previously shared immutable content that remains usable, if any.
        blobs: Box<[RegistryForgeBlobRef]>,
    },
}

impl RegistryForgeAssociationState {
    fn admit(
        &self,
        candidates: &[RegistryForgeCandidate],
    ) -> Result<(), RegistryForgeAssociationError> {
        match self {
            Self::Resolved {
                commit,
                tree,
                blobs,
            } => {
                if candidates.len() != 1 {
                    return Err(RegistryForgeAssociationError::StateShape);
                }
                admit_object_id(commit)?;
                if let Some(tree) = tree {
                    admit_object_id(tree)?;
                }
                admit_blobs(blobs)
            }
            Self::Ambiguous => {
                if candidates.len() < 2 {
                    Err(RegistryForgeAssociationError::StateShape)
                } else {
                    Ok(())
                }
            }
            Self::Unavailable { blobs, .. } => admit_blobs(blobs),
        }
    }
}

/// Evidence strength assigned to a candidate forge identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryForgeConfidence {
    /// The registry or immutable manifest explicitly names this source.
    Exact,
    /// Multiple independent facts agree on the source.
    High,
    /// The source is plausible but not independently confirmed.
    Medium,
    /// The source is weakly suggested and must not be selected implicitly.
    Low,
}

/// Typed authority that supplied the relationship evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum RegistryForgeProvenance {
    /// Repository/homepage metadata published by the registry.
    RegistryMetadata,
    /// A package manifest or package metadata file.
    PackageManifest,
    /// A lockfile or resolved dependency record.
    Lockfile,
    /// A maintainer-supplied source declaration.
    Maintainer,
    /// An explicit user-provided source override.
    UserProvided,
    /// A bounded derived fact, retaining the exact derivation label.
    Derived(ProductText),
}

/// Why a registry-to-forge association was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistryForgeAssociationError {
    /// Association schema version is unsupported.
    Version,
    /// Registry lineage is not a versioned package URL.
    RegistryPackage,
    /// Candidate collection exceeded its bound.
    CandidateBound,
    /// Canonical association identity exceeded its byte bound.
    BytesBound,
    /// Candidate identities are duplicated.
    DuplicateCandidate,
    /// Candidate source fields are malformed.
    SourceIdentity,
    /// Association state does not match candidate cardinality.
    StateShape,
    /// Immutable forge object identity is malformed.
    ObjectId,
    /// Content reference is malformed.
    BlobShape,
    /// Content reference collection exceeded its bound.
    BlobBound,
    /// Association content identity does not match its payload.
    IdentityMismatch,
    /// Association content identity could not be encoded.
    IdentityEncoding,
    /// JSON bytes were valid but not in the canonical field order/encoding.
    NonCanonicalEncoding,
    /// Association points at a different registry lineage than its row.
    RegistryMismatch,
    /// Paged source-file frontier is malformed.
    FrontierShape,
}

impl core::fmt::Display for RegistryForgeAssociationError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(match self {
            Self::Version => "registry-forge association version is unsupported",
            Self::RegistryPackage => "registry-forge association package is not a purl",
            Self::CandidateBound => "registry-forge association candidate count exceeds its bound",
            Self::BytesBound => "registry-forge association identity exceeds its byte bound",
            Self::DuplicateCandidate => "registry-forge association candidates are duplicated",
            Self::SourceIdentity => "registry-forge source identity is malformed",
            Self::StateShape => "registry-forge association state has an invalid candidate shape",
            Self::ObjectId => "registry-forge object identity is malformed",
            Self::BlobShape => "registry-forge content reference is malformed",
            Self::BlobBound => "registry-forge content reference count exceeds its bound",
            Self::IdentityMismatch => "registry-forge association has a stale content identity",
            Self::IdentityEncoding => "registry-forge association identity could not be encoded",
            Self::NonCanonicalEncoding => "registry-forge association bytes are not canonical",
            Self::RegistryMismatch => {
                "registry-forge association belongs to another registry package"
            }
            Self::FrontierShape => "registry-forge source-file frontier is malformed",
        })
    }
}

fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, RegistryForgeAssociationError> {
    // The identity/wire structs have a fixed declaration order. Decoding
    // always re-encodes and byte-compares the value, so a future field-order
    // or serializer change is rejected as a new wire contract instead of
    // silently aliasing an existing content ID.
    serde_json::to_vec(value).map_err(|_| RegistryForgeAssociationError::IdentityEncoding)
}

fn admit_object_id(object: &ForgeObjectId) -> Result<(), RegistryForgeAssociationError> {
    ForgeObjectId::parse(&object.as_hex())
        .map(|_| ())
        .map_err(|_| RegistryForgeAssociationError::ObjectId)
}

#[derive(Serialize)]
struct RegistryForgeAssociationIdentity<'a> {
    version: u16,
    registry: &'a PackageReference,
    candidates: &'a [RegistryForgeCandidate],
    state: &'a RegistryForgeAssociationState,
    provenance: &'a RegistryForgeProvenance,
    source_files: &'a Option<RegistryForgeBlobFrontier>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package() -> PackageReference {
        PackageReference::parse("pkg:cargo/demo@1.0.0").expect("package")
    }

    fn source(name: &str) -> RegistryForgeSourceIdentity {
        RegistryForgeSourceIdentity {
            coordinate: ForgeCoordinate::new(
                format!("https://github.com/acme/{name}"),
                ForgeRevision::Tag(ForgeRefName::new("v1.0.0").expect("tag")),
                None::<String>,
            )
            .expect("coordinate"),
        }
    }

    fn candidate(name: &str) -> RegistryForgeCandidate {
        RegistryForgeCandidate {
            source: source(name),
            confidence: RegistryForgeConfidence::Exact,
        }
    }

    fn resolved() -> RegistryForgeAssociation {
        let blob =
            RegistryForgeBlobRef::new(RegistryForgeBlobKind::SourceSnapshot, [7; 32], 42, None)
                .expect("blob");
        RegistryForgeAssociation::new(
            package(),
            vec![candidate("demo")].into_boxed_slice(),
            RegistryForgeAssociationState::Resolved {
                commit: ForgeObjectId::parse("0123456789012345678901234567890123456789")
                    .expect("commit"),
                tree: None,
                blobs: vec![blob].into_boxed_slice(),
            },
            RegistryForgeProvenance::RegistryMetadata,
        )
        .expect("association")
    }

    #[test]
    fn resolved_association_is_content_identified_and_round_trips() {
        let association = resolved();
        association.admit().expect("admission");
        let encoded = serde_json::to_vec(&association).expect("encoding");
        let decoded: RegistryForgeAssociation = serde_json::from_slice(&encoded).expect("decoding");
        assert_eq!(decoded, association);
        assert!(decoded.is_resolved());
    }

    #[test]
    fn canonical_wire_rejects_reordered_or_whitespace_json() {
        let association = resolved();
        let encoded = association.encode_canonical().expect("canonical encoding");
        assert_eq!(
            RegistryForgeAssociation::decode_canonical(&encoded).expect("canonical decoding"),
            association
        );
        let mut padded = Vec::with_capacity(encoded.len() + 2);
        padded.push(b' ');
        padded.extend_from_slice(&encoded);
        padded.push(b' ');
        assert!(matches!(
            RegistryForgeAssociation::decode_canonical(&padded),
            Err(RegistryForgeAssociationError::NonCanonicalEncoding)
        ));
    }

    #[test]
    fn source_file_frontier_keeps_inline_head_bounded_but_commits_complete_root() {
        let pages = vec![RegistryForgeBlobPage {
            index: 0,
            content_id: [7; 32],
            files: 2,
            bytes: 42,
        }]
        .into_boxed_slice();
        let frontier = RegistryForgeBlobFrontier::new(pages, 2, 42).expect("frontier");
        let association = resolved()
            .with_source_files(frontier.clone())
            .expect("source frontier");
        assert_eq!(association.source_files, Some(frontier.clone()));
        assert_ne!(association.facts_version, resolved().facts_version);
        association.admit().expect("frontier admission");

        let mut tampered = frontier;
        tampered.root = [6; 32];
        assert!(matches!(
            tampered.admit(),
            Err(RegistryForgeAssociationError::FrontierShape)
        ));

        let duplicate_pages = vec![
            RegistryForgeBlobPage {
                index: 0,
                content_id: [11; 32],
                files: 1,
                bytes: 7,
            },
            RegistryForgeBlobPage {
                index: 1,
                content_id: [11; 32],
                files: 1,
                bytes: 8,
            },
        ]
        .into_boxed_slice();
        assert!(matches!(
            RegistryForgeBlobFrontier::new(duplicate_pages, 2, 15),
            Err(RegistryForgeAssociationError::FrontierShape)
        ));
    }

    #[test]
    fn unavailable_private_and_auth_failed_states_remain_typed() {
        for reason in [
            ForgeUnavailableReason::Private,
            ForgeUnavailableReason::AuthFailed,
            ForgeUnavailableReason::Offline,
        ] {
            let association = RegistryForgeAssociation::new(
                package(),
                Box::new([]),
                RegistryForgeAssociationState::Unavailable {
                    reason,
                    blobs: Box::new([]),
                },
                RegistryForgeProvenance::RegistryMetadata,
            )
            .expect("unavailable association");
            assert!(!association.is_resolved());
            assert!(association.admit().is_ok());
        }
    }

    #[test]
    fn ambiguous_association_requires_distinct_candidates() {
        let association = RegistryForgeAssociation::new(
            package(),
            vec![candidate("first"), candidate("second")].into_boxed_slice(),
            RegistryForgeAssociationState::Ambiguous,
            RegistryForgeProvenance::Derived(ProductText::from_static("registry-homepage")),
        )
        .expect("ambiguous association");
        assert!(!association.is_resolved());
        assert!(matches!(
            RegistryForgeAssociation::new(
                package(),
                vec![candidate("only")].into_boxed_slice(),
                RegistryForgeAssociationState::Ambiguous,
                RegistryForgeProvenance::RegistryMetadata,
            ),
            Err(RegistryForgeAssociationError::StateShape)
        ));
    }

    #[test]
    fn source_file_blobs_allow_distinct_paths_but_reject_duplicate_paths_or_ids() {
        let first = RegistryForgeBlobRef::new(
            RegistryForgeBlobKind::SourceFile,
            [8; 32],
            10,
            Some(ProductText::from_static("src/lib.rs")),
        )
        .expect("first source file");
        let second = RegistryForgeBlobRef::new(
            RegistryForgeBlobKind::SourceFile,
            [9; 32],
            11,
            Some(ProductText::from_static("src/main.rs")),
        )
        .expect("second source file");
        let association = RegistryForgeAssociation::new(
            package(),
            vec![candidate("demo")].into_boxed_slice(),
            RegistryForgeAssociationState::Unavailable {
                reason: ForgeUnavailableReason::Stale,
                blobs: vec![first.clone(), second].into_boxed_slice(),
            },
            RegistryForgeProvenance::RegistryMetadata,
        )
        .expect("source file association");
        assert!(association.admit().is_ok());

        let duplicate_path = RegistryForgeBlobRef::new(
            RegistryForgeBlobKind::SourceFile,
            [10; 32],
            12,
            Some(ProductText::from_static("src/lib.rs")),
        )
        .expect("duplicate path fixture");
        assert!(matches!(
            RegistryForgeAssociation::new(
                package(),
                vec![candidate("demo")].into_boxed_slice(),
                RegistryForgeAssociationState::Unavailable {
                    reason: ForgeUnavailableReason::Stale,
                    blobs: vec![first.clone(), duplicate_path].into_boxed_slice(),
                },
                RegistryForgeProvenance::RegistryMetadata,
            ),
            Err(RegistryForgeAssociationError::BlobShape)
        ));

        let duplicate_id = RegistryForgeBlobRef::new(
            RegistryForgeBlobKind::SourceFile,
            [8; 32],
            12,
            Some(ProductText::from_static("src/other.rs")),
        )
        .expect("duplicate identity fixture");
        assert!(matches!(
            RegistryForgeAssociation::new(
                package(),
                vec![candidate("demo")].into_boxed_slice(),
                RegistryForgeAssociationState::Unavailable {
                    reason: ForgeUnavailableReason::Stale,
                    blobs: vec![first, duplicate_id].into_boxed_slice(),
                },
                RegistryForgeProvenance::RegistryMetadata,
            ),
            Err(RegistryForgeAssociationError::BlobShape)
        ));
    }
}
