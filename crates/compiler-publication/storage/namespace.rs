//! Defines storage namespace behavior for `compiler-publication`, whose purpose is to publish verified compiler fragments as immutable generations.
//! This module owns the storage namespace invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Closed sibling artifact namespaces and their fixed identity-path grammar.

use heart_identity::{ArtifactId, Domain, Encoding, HASH_BYTES};

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
const HEX_NAME_BYTES: usize = HASH_BYTES * 2;
const ARTIFACT_EXTENSION_BYTES: usize = 7;
const ARTIFACT_NAME_BYTES: usize = HEX_NAME_BYTES + ARTIFACT_EXTENSION_BYTES;

/// Closed immutable artifact sibling directories owned by compiler publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StorageNamespace {
    /// Complete canonical IR fragments.
    Fragments,
    /// Complete portable semantic images.
    SemanticImages,
    /// Complete canonical compiler package manifests.
    Manifests,
}

impl StorageNamespace {
    pub(crate) const fn directory(self) -> &'static str {
        match self {
            Self::Fragments => "fragments",
            Self::SemanticImages => "semantic-images",
            Self::Manifests => "manifests",
        }
    }

    const fn extension(self) -> &'static [u8; ARTIFACT_EXTENSION_BYTES] {
        match self {
            Self::Fragments => b".irfrag",
            Self::SemanticImages => b".semimg",
            Self::Manifests => b".irmani",
        }
    }
}

/// Renders one typed identity in this namespace's fixed immutable path grammar.
pub(super) fn artifact_name<EncodingTag: Encoding, DomainTag: Domain>(
    identity: ArtifactId<EncodingTag, DomainTag>,
    namespace: StorageNamespace,
) -> [u8; ARTIFACT_NAME_BYTES] {
    let raw: &[u8; HASH_BYTES] = identity.as_ref();
    let mut name = [0_u8; ARTIFACT_NAME_BYTES];
    for (index, byte) in raw.iter().copied().enumerate() {
        name[index * 2] = HEX_DIGITS[usize::from(byte >> 4)];
        name[index * 2 + 1] = HEX_DIGITS[usize::from(byte & 0x0f)];
    }
    name[HEX_NAME_BYTES..].copy_from_slice(namespace.extension());
    name
}
