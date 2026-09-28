//! Session keys bound to authority and discovery inputs.

use super::{
    AuthorityIdentity, FlowId, InputManifest, InputManifestId, ProfileId, SemanticBasisId,
    SessionId, SessionSchema, typed_of,
};

/// Stable process lineage for one authority, profile, flow, and semantic
/// basis. The exact manifest remains attached to each key so requests can
/// advance through this lineage while retaining their input fence.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionKey {
    digest: SessionId,
    authority: SessionId,
    manifest: InputManifestId,
}

impl SessionKey {
    /// Constructs an exact-input key attached to one stable process lineage.
    #[must_use]
    pub fn new(
        authority: AuthorityIdentity,
        manifest: &InputManifest,
        profile: ProfileId,
        flow: FlowId,
        semantic: SemanticBasisId,
    ) -> Self {
        let authority_digest = authority.digest();
        let manifest_digest = manifest.digest();
        let mut bytes = Vec::with_capacity(128);
        bytes.extend_from_slice(b"backend.compile.session-lineage.v1\0");
        bytes.extend_from_slice(authority_digest.as_bytes());
        bytes.extend_from_slice(profile.as_bytes());
        bytes.extend_from_slice(flow.as_bytes());
        bytes.extend_from_slice(semantic.as_bytes());
        Self {
            digest: typed_of::<SessionSchema>(&bytes),
            authority: authority_digest,
            manifest: manifest_digest,
        }
    }

    /// Returns the stable process-lineage identity.
    #[must_use]
    pub const fn digest(self) -> SessionId {
        self.digest
    }

    /// Returns the stable process-lineage identity, independent of input
    /// manifest and discovery revision.
    #[must_use]
    pub const fn lineage(self) -> SessionId {
        self.digest
    }

    /// Returns the authority identity embedded in this key.
    #[must_use]
    pub const fn authority(self) -> SessionId {
        self.authority
    }

    /// Returns the manifest identity embedded in this key.
    #[must_use]
    pub const fn manifest(self) -> InputManifestId {
        self.manifest
    }

    /// Advances the exact-input fence while retaining this stable process
    /// lineage. Callers must validate the new manifest before invoking it.
    #[must_use]
    pub(crate) const fn with_manifest(self, manifest: InputManifestId) -> Self {
        Self { manifest, ..self }
    }

    /// Checks the authority and manifest portion of a key before extraction.
    #[must_use]
    pub fn matches(&self, authority: &AuthorityIdentity, manifest: &InputManifest) -> bool {
        self.authority == authority.digest() && self.manifest == manifest.digest()
    }
}
