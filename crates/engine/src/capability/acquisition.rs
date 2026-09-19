//! Source-authorized acquisition and verified installation transitions.

use std::{fmt, sync::Arc};

use backend_store::TypedObject;
use backend_version::{Schema, SchemaIdentity};

use super::{
    ArtifactSourceId, AuthorizationScopeId, AvailableCapability, CapabilityArtifactId,
    CapabilityArtifactSchema, CapabilityKind, CapabilityManifest, InstalledCapability,
};

/// Untrusted transfer/store result presented for acquisition verification.
pub struct AcquiredCapabilityArtifact {
    artifact: Arc<TypedObject>,
    source: ArtifactSourceId,
    authorization: Option<AuthorizationScopeId>,
}

impl AcquiredCapabilityArtifact {
    /// Groups exact immutable bytes with the source and credential policy used by the adapter.
    #[must_use]
    pub fn new(
        artifact: Arc<TypedObject>,
        source: ArtifactSourceId,
        authorization: Option<AuthorizationScopeId>,
    ) -> Self {
        Self {
            artifact,
            source,
            authorization,
        }
    }

    /// Exact immutable object presented by transfer/storage.
    #[must_use]
    pub fn artifact(&self) -> &TypedObject {
        &self.artifact
    }
}

/// Owner verification of the transfer commit receipt and authenticated source session.
pub trait CapabilityAcquisitionVerifier<K: CapabilityKind> {
    /// Exact source/receipt verification failure.
    type Error;
    /// Verifies that the acquired immutable object came from the manifest-authorized transfer.
    ///
    /// # Errors
    ///
    /// Returns the adapter's precise receipt, authority, revocation, or source-session rejection.
    fn verify(
        &self,
        manifest: &CapabilityManifest<K>,
        acquired: &AcquiredCapabilityArtifact,
    ) -> Result<(), Self::Error>;
}

/// Exact reason untrusted acquisition evidence was not promoted.
#[derive(Debug)]
pub enum AcquisitionVerificationError<K: CapabilityKind, E> {
    /// Source identity differs from the authenticated manifest.
    Source {
        /// Manifest owner retained for another source.
        available: AvailableCapability<K>,
    },
    /// Credential policy differs from the authenticated manifest.
    Authorization {
        /// Manifest owner retained for a correctly scoped source session.
        available: AvailableCapability<K>,
    },
    /// Store object schema, identity, or extent mismatched.
    Artifact {
        /// Manifest owner retained for another transfer.
        available: AvailableCapability<K>,
    },
    /// Transfer receipt or source authority was rejected.
    Authority {
        /// Manifest owner retained after rejection.
        available: AvailableCapability<K>,
        /// Adapter-specific authority failure.
        error: E,
    },
}

/// Source-authorized, byte-verified acquisition. Only this type can install a capability.
pub struct VerifiedAcquisition<K: CapabilityKind> {
    manifest: Arc<CapabilityManifest<K>>,
    artifact: Arc<TypedObject>,
}

impl<K: CapabilityKind> AvailableCapability<K> {
    /// Verifies source/authentication evidence and exact immutable bytes returned by transfer.
    ///
    /// # Errors
    ///
    /// Returns the still-available manifest with the exact source, authorization, byte, or
    /// transfer-authority failure.
    pub fn verify_acquisition<V: CapabilityAcquisitionVerifier<K>>(
        self,
        acquired: AcquiredCapabilityArtifact,
        verifier: &V,
    ) -> Result<VerifiedAcquisition<K>, AcquisitionVerificationError<K, V::Error>> {
        if acquired.source != self.manifest.authority.source {
            return Err(AcquisitionVerificationError::Source { available: self });
        }
        if acquired.authorization != self.manifest.authority.authorization {
            return Err(AcquisitionVerificationError::Authorization { available: self });
        }
        if acquired.artifact.schema()
            != SchemaIdentity::new(
                CapabilityArtifactSchema::DOMAIN,
                CapabilityArtifactSchema::TYPE,
                CapabilityArtifactSchema::VERSION,
            )
            || acquired.artifact.version().as_slice() != self.manifest.artifact.as_bytes()
            || u64::try_from(acquired.artifact.bytes().len()).ok()
                != Some(self.manifest.bytes.get())
        {
            return Err(AcquisitionVerificationError::Artifact { available: self });
        }
        if let Err(error) = verifier.verify(&self.manifest, &acquired) {
            return Err(AcquisitionVerificationError::Authority {
                available: self,
                error,
            });
        }
        Ok(VerifiedAcquisition {
            manifest: self.manifest,
            artifact: acquired.artifact,
        })
    }
}

impl<K: CapabilityKind> VerifiedAcquisition<K> {
    /// Installs the verified artifact after proving its complete dependency closure is resident.
    ///
    /// # Errors
    ///
    /// Returns this verified acquisition when any exact dependency is not resident.
    pub fn install(
        self,
        resident_dependencies: &[CapabilityArtifactId],
    ) -> Result<InstalledCapability<K>, InstallError<K>> {
        if self
            .manifest
            .dependencies
            .iter()
            .any(|dependency| !resident_dependencies.contains(&dependency.artifact))
        {
            return Err(InstallError::Dependency { acquisition: self });
        }
        Ok(InstalledCapability {
            manifest: self.manifest,
            artifact: self.artifact,
        })
    }
}

/// Installation failure retains the already verified primary acquisition.
#[derive(Debug)]
pub enum InstallError<K: CapabilityKind> {
    /// At least one exact dependency is not resident.
    Dependency {
        /// Verified primary artifact retained while dependencies are acquired.
        acquisition: VerifiedAcquisition<K>,
    },
}

impl<K: CapabilityKind> fmt::Debug for VerifiedAcquisition<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedAcquisition")
            .field("manifest", &self.manifest.identity)
            .field("artifact", &self.artifact.id())
            .finish()
    }
}
