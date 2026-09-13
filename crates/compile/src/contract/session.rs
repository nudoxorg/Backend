//! Session keys bound to authority and discovery inputs.

use super::{
    AuthorityIdentity, FlowId, InputManifest, InputManifestId, ProfileId, SemanticBasisId,
    SessionId, SessionSchema, typed_of,
};

/// A session key invalidated by any authority, input, profile, flow, or
/// semantic-basis change.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionKey {
    digest: SessionId,
    authority: SessionId,
    manifest: InputManifestId,
}

impl SessionKey {
    /// Constructs a key from all participating identities.
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
        let mut bytes = Vec::with_capacity(160);
        bytes.extend_from_slice(authority_digest.as_bytes());
        bytes.extend_from_slice(manifest_digest.as_bytes());
        bytes.extend_from_slice(profile.as_bytes());
        bytes.extend_from_slice(flow.as_bytes());
        bytes.extend_from_slice(semantic.as_bytes());
        Self {
            digest: typed_of::<SessionSchema>(&bytes),
            authority: authority_digest,
            manifest: manifest_digest,
        }
    }

    /// Returns the raw session identity.
    #[must_use]
    pub const fn digest(self) -> SessionId {
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

    /// Checks the authority and manifest portion of a key before extraction.
    #[must_use]
    pub fn matches(&self, authority: &AuthorityIdentity, manifest: &InputManifest) -> bool {
        self.authority == authority.digest() && self.manifest == manifest.digest()
    }
}
