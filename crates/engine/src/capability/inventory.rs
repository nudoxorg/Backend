//! Lossless client projection of verified lifecycle owners.

use super::{
    ActiveCapability, ActiveState, CapabilityKind, CapabilityManifest, CapabilityRuntime,
    CapabilityTarget, EmbeddingModel, ExecutionReady, InstalledCapability, LanguageOracle,
    ResidentCapability, RevokedCapability,
};

/// Capability families that have a lossless client inventory projection.
pub trait CapabilityInventoryKind: CapabilityKind {
    /// Projects the family and its compatibility identity.
    fn inventory_family(manifest: &CapabilityManifest<Self>) -> backend_library::CapabilityFamily;
    /// Projects the complete producer authority retained by the manifest.
    fn inventory_authority(
        manifest: &CapabilityManifest<Self>,
    ) -> backend_library::CapabilityAuthority;
}

impl CapabilityInventoryKind for LanguageOracle {
    fn inventory_family(manifest: &CapabilityManifest<Self>) -> backend_library::CapabilityFamily {
        let recipe = manifest.recipe();
        backend_library::CapabilityFamily::LanguageOracle {
            profile: recipe.profile,
            task: recipe.task,
        }
    }

    fn inventory_authority(
        manifest: &CapabilityManifest<Self>,
    ) -> backend_library::CapabilityAuthority {
        let recipe = manifest.recipe();
        let package_authority =
            backend_library::PackageAuthorityIdentity::ImmutableClosure(recipe.package_authority);
        let toolchain_identity = recipe.toolchain.to_bytes();
        backend_library::CapabilityAuthority::Compiler {
            recipe: backend_library::compiler_authority_recipe(
                manifest.identity.to_bytes(),
                recipe.profile,
                recipe.task,
                manifest.protocol_abi,
                recipe.native_tool,
                toolchain_identity,
                package_authority,
            ),
            toolchain: recipe.native_tool,
            toolchain_identity,
            package_authority,
        }
    }
}

impl CapabilityInventoryKind for EmbeddingModel {
    fn inventory_family(manifest: &CapabilityManifest<Self>) -> backend_library::CapabilityFamily {
        let recipe = embedding_capability_recipe(manifest);
        backend_library::CapabilityFamily::Embedding {
            recipe: Some(recipe.identity()),
        }
    }

    fn inventory_authority(
        manifest: &CapabilityManifest<Self>,
    ) -> backend_library::CapabilityAuthority {
        backend_library::CapabilityAuthority::Embedding(embedding_capability_recipe(manifest))
    }
}

fn embedding_capability_recipe(
    manifest: &CapabilityManifest<EmbeddingModel>,
) -> backend_library::EmbeddingCapabilityRecipe {
    let recipe = manifest.recipe();
    let treatment = |side: &[u8], value: Option<super::TreatmentId>| {
        value.map_or_else(
            || {
                let mut hasher = blake3::Hasher::new();
                hasher.update(b"backend.embedding.plain-treatment.v1\0");
                hasher.update(side);
                *hasher.finalize().as_bytes()
            },
            super::TreatmentId::as_bytes,
        )
    };
    let query = match recipe.query {
        super::QueryTreatment::Plain => None,
        super::QueryTreatment::ModelPrefix(identity) => Some(identity),
    };
    let document = match recipe.document {
        super::DocumentTreatment::Plain => None,
        super::DocumentTreatment::ModelPrefix(identity) => Some(identity),
    };
    backend_library::EmbeddingCapabilityRecipe::new(
        manifest.artifact().to_bytes(),
        recipe.tokenizer.as_bytes(),
        match recipe.extraction {
            super::SourceExtraction::RawUtf8 => backend_library::EmbeddingSource::RawUtf8,
            super::SourceExtraction::SemanticDeclaration => {
                backend_library::EmbeddingSource::SemanticDeclaration
            }
            super::SourceExtraction::Documentation => {
                backend_library::EmbeddingSource::Documentation
            }
        },
        None,
        Some(recipe.chunking.maximum_tokens.get()),
        recipe.chunking.overlap_tokens,
        u32::from(recipe.dimensions.get()),
        match recipe.metric {
            super::EmbeddingMetric::Cosine => backend_library::EmbeddingMetric::Cosine,
            super::EmbeddingMetric::Dot => backend_library::EmbeddingMetric::Dot,
            super::EmbeddingMetric::Euclidean => backend_library::EmbeddingMetric::Euclidean,
        },
        match recipe.pooling {
            super::Pooling::Cls => backend_library::EmbeddingPooling::Cls,
            super::Pooling::Mean => backend_library::EmbeddingPooling::Mean,
            super::Pooling::LastToken => backend_library::EmbeddingPooling::LastToken,
        },
        match recipe.normalization {
            super::Normalization::None => backend_library::EmbeddingNormalization::None,
            super::Normalization::L2 => backend_library::EmbeddingNormalization::UnitL2,
        },
        match recipe.numeric {
            super::NumericRepresentation::F32 => backend_library::EmbeddingEncoding::Float32,
            super::NumericRepresentation::F16 => backend_library::EmbeddingEncoding::Float16,
            super::NumericRepresentation::I8 => backend_library::EmbeddingEncoding::Signed8,
        },
        treatment(b"query", query),
        treatment(b"document", document),
    )
}

impl<K: CapabilityInventoryKind> CapabilityManifest<K> {
    fn inventory_status(
        &self,
        id: backend_library::CapabilityId,
        lifecycle: backend_library::CapabilityLifecycle,
    ) -> backend_library::CapabilityStatus {
        let target = match self.target {
            CapabilityTarget::Native { os, architecture } => {
                backend_library::CapabilityTarget::Native {
                    os: os as u8,
                    architecture: architecture as u8,
                }
            }
            CapabilityTarget::Managed(runtime) => {
                backend_library::CapabilityTarget::Managed(runtime as u8)
            }
        };
        let authority = K::inventory_authority(self);
        backend_library::CapabilityStatus::observed(
            id,
            K::inventory_family(self),
            self.identity.to_bytes(),
            target,
            self.protocol_abi,
            authority,
            lifecycle,
        )
    }
}

impl<K: CapabilityInventoryKind> InstalledCapability<K> {
    /// Projects the exact installed state for bounded client health reporting.
    #[must_use]
    pub fn inventory_status(
        &self,
        id: backend_library::CapabilityId,
    ) -> backend_library::CapabilityStatus {
        self.manifest
            .inventory_status(id, backend_library::CapabilityLifecycle::Installed)
    }
}

impl<K: CapabilityInventoryKind> ResidentCapability<K> {
    /// Projects the exact resident state for bounded client health reporting.
    #[must_use]
    pub fn inventory_status(
        &self,
        id: backend_library::CapabilityId,
    ) -> backend_library::CapabilityStatus {
        self.manifest
            .inventory_status(id, backend_library::CapabilityLifecycle::Resident)
    }
}

impl<K: CapabilityInventoryKind, R: CapabilityRuntime<K>> ActiveCapability<K, R> {
    /// Projects active ownership without claiming a fresh execution probe.
    #[must_use]
    pub fn inventory_status(
        &self,
        id: backend_library::CapabilityId,
    ) -> Option<backend_library::CapabilityStatus> {
        match &self.state {
            ActiveState::Live { resident, .. } => Some(
                resident
                    .manifest
                    .inventory_status(id, backend_library::CapabilityLifecycle::Active),
            ),
            ActiveState::Vacant => None,
        }
    }
}

impl<K: CapabilityInventoryKind, R: CapabilityRuntime<K>> ExecutionReady<'_, K, R> {
    /// Projects readiness backed by this fresh live probe borrow.
    #[must_use]
    pub fn inventory_status(
        &self,
        id: backend_library::CapabilityId,
    ) -> backend_library::CapabilityStatus {
        self.resident
            .manifest
            .inventory_status(id, backend_library::CapabilityLifecycle::Ready)
    }
}

impl<K: CapabilityInventoryKind> RevokedCapability<K> {
    /// Projects the explicit revoked state while retaining verified resident bytes.
    #[must_use]
    pub fn inventory_status(
        &self,
        id: backend_library::CapabilityId,
    ) -> backend_library::CapabilityStatus {
        self.resident
            .manifest
            .inventory_status(id, backend_library::CapabilityLifecycle::Revoked)
    }
}
