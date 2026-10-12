//! Admission of an explicitly declared query recipe for a shared compiler runtime.
//!
//! Runtime activation proves executable/artifact identity and bounded protocol behavior.
//! It cannot infer pooling or text treatment from opaque options. The owner must declare
//! those semantics when installing the runtime, committing this contract's digest to BERS.
//! Admission does not attest that an arbitrary executable implements its declaration.

use backend_compile::{
    EmbeddingExecutable, EmbeddingExecutionIdentity, EmbeddingNormalization,
    EmbeddingRuntimeSpecError, EmbeddingRuntimeSpecV1,
};
use backend_extension_qdrant as vector;
use std::fmt;
use std::num::NonZeroU32;
use std::sync::Arc;

const DESCRIPTOR_MAGIC: &[u8; 5] = b"BLQR\x01";
const CONTRACT_BYTES: usize = 5 + 32;

/// The selected runtime pointer chooses its exact immutable descriptor object.
pub(super) fn recipe_descriptor_path(
    parent: &std::path::Path,
    recipe: [u8; 32],
) -> std::path::PathBuf {
    let hex: String = recipe.iter().map(|byte| format!("{byte:02x}")).collect();
    parent.join(format!("embedding-query-{hex}.config"))
}

/// Text handling declared by the local owner before passing a purpose-tagged request.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalEmbeddingTreatment {
    /// Pass the exact UTF-8 text without adding instructions or a prefix.
    UnmodifiedUtf8 = 1,
}

/// Explicit coordinate-producing semantics. These are declarations, not inference results.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalQueryRecipeContract {
    /// Distance applied to the admitted document and query coordinates.
    pub metric: vector::Metric,
    /// Pooling implemented by the selected, owner-trusted embedding executable.
    pub pooling: vector::EmbeddingPooling,
    /// Canonical stored coordinate representation; only float32 is supported locally.
    pub encoding: vector::EmbeddingEncoding,
    /// Query-side text handling, independent of the protocol's query purpose tag.
    pub query_treatment: LocalEmbeddingTreatment,
    /// Document-side text handling, independent of the document purpose tag.
    pub document_treatment: LocalEmbeddingTreatment,
    /// Digest of remaining output-affecting provider options, including tokenizer
    /// truncation, special-token and padding settings. The owner must declare it.
    pub provider_options: [u8; 32],
}

impl LocalQueryRecipeContract {
    /// Digest that the installer must commit as the runtime's BERS options_digest.
    /// Existing opaque digests are never reinterpreted as this contract.
    #[must_use]
    pub fn options_digest(self) -> [u8; 32] {
        let mut digest = blake3::Hasher::new_derive_key("backend.local-query.recipe-options.v1");
        digest.update(&self.canonical_bytes());
        *digest.finalize().as_bytes()
    }

    fn canonical_bytes(self) -> [u8; CONTRACT_BYTES] {
        let mut bytes = [0; CONTRACT_BYTES];
        bytes[..5].copy_from_slice(&[
            self.metric as u8,
            self.pooling as u8,
            self.encoding as u8,
            self.query_treatment as u8,
            self.document_treatment as u8,
        ]);
        bytes[5..].copy_from_slice(&self.provider_options);
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Self, LocalQueryRecipeError> {
        if bytes.len() != CONTRACT_BYTES {
            return Err(LocalQueryRecipeError::DescriptorFormat);
        }
        let metric = match bytes[0] {
            0 => vector::Metric::EuclideanSquared,
            1 => vector::Metric::CosineDistance,
            2 => vector::Metric::NegativeDot,
            _ => return Err(LocalQueryRecipeError::DescriptorFormat),
        };
        let pooling = match bytes[1] {
            1 => vector::EmbeddingPooling::ClassificationToken,
            2 => vector::EmbeddingPooling::Mean,
            3 => vector::EmbeddingPooling::LastToken,
            _ => return Err(LocalQueryRecipeError::DescriptorFormat),
        };
        if bytes[2] != vector::EmbeddingEncoding::Float32 as u8 {
            return Err(LocalQueryRecipeError::UnsupportedEncoding);
        }
        if bytes[3] != LocalEmbeddingTreatment::UnmodifiedUtf8 as u8
            || bytes[4] != LocalEmbeddingTreatment::UnmodifiedUtf8 as u8
        {
            return Err(LocalQueryRecipeError::UnsupportedTreatment);
        }
        Ok(Self {
            metric,
            pooling,
            encoding: vector::EmbeddingEncoding::Float32,
            query_treatment: LocalEmbeddingTreatment::UnmodifiedUtf8,
            document_treatment: LocalEmbeddingTreatment::UnmodifiedUtf8,
            provider_options: bytes[5..]
                .try_into()
                .map_err(|_| LocalQueryRecipeError::DescriptorFormat)?,
        })
    }
}

/// Bounded portable declaration paired with the exact runtime spec it describes.
/// Decoding this value creates no executable authority or available capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalQueryRecipeDescriptor {
    runtime: EmbeddingRuntimeSpecV1,
    contract: LocalQueryRecipeContract,
}

impl LocalQueryRecipeDescriptor {
    /// Creates a declaration only when BERS commits its complete semantic options.
    ///
    /// # Errors
    /// Returns a closed mismatch or unsupported-encoding cause; no runtime is started.
    pub fn new(
        runtime: EmbeddingRuntimeSpecV1,
        contract: LocalQueryRecipeContract,
    ) -> Result<Self, LocalQueryRecipeError> {
        if contract.encoding != vector::EmbeddingEncoding::Float32 {
            return Err(LocalQueryRecipeError::UnsupportedEncoding);
        }
        if runtime.options_digest() != contract.options_digest() {
            return Err(LocalQueryRecipeError::SemanticOptionsMismatch);
        }
        Ok(Self { runtime, contract })
    }

    /// Exact bounded descriptor encoding, independent of runtime activation.
    #[must_use]
    pub fn canonical_bytes(self) -> Vec<u8> {
        let runtime = self.runtime.canonical_bytes();
        let mut bytes = Vec::with_capacity(DESCRIPTOR_MAGIC.len() + runtime.len() + CONTRACT_BYTES);
        bytes.extend_from_slice(DESCRIPTOR_MAGIC);
        bytes.extend_from_slice(&runtime);
        bytes.extend_from_slice(&self.contract.canonical_bytes());
        bytes
    }

    /// Decodes the exact descriptor grammar; extensions and trailing bytes are refused.
    ///
    /// # Errors
    /// Returns a typed format, runtime-spec, options, encoding or treatment rejection.
    pub fn decode(bytes: &[u8]) -> Result<Self, LocalQueryRecipeError> {
        if bytes.get(..DESCRIPTOR_MAGIC.len()) != Some(DESCRIPTOR_MAGIC.as_slice()) {
            return Err(LocalQueryRecipeError::DescriptorFormat);
        }
        let runtime_end = bytes
            .len()
            .checked_sub(CONTRACT_BYTES)
            .filter(|end| *end >= DESCRIPTOR_MAGIC.len())
            .ok_or(LocalQueryRecipeError::DescriptorFormat)?;
        let runtime = EmbeddingRuntimeSpecV1::decode(&bytes[DESCRIPTOR_MAGIC.len()..runtime_end])
            .map_err(LocalQueryRecipeError::RuntimeSpec)?;
        let contract = LocalQueryRecipeContract::decode(&bytes[runtime_end..])?;
        Self::new(runtime, contract)
    }

    /// Admits this declaration against the already activated compiler runtime.
    /// The returned proof retains the same Arc; it performs no probes or activation.
    ///
    /// # Errors
    /// Returns the actual runtime identity mismatch, preserving its typed field.
    pub fn admit(
        self,
        runtime: Arc<EmbeddingExecutable>,
    ) -> Result<AdmittedLocalQueryRecipe, LocalQueryRecipeError> {
        let execution = runtime.execution_identity();
        self.runtime
            .validate_execution_identity(execution)
            .map_err(LocalQueryRecipeError::RuntimeSpec)?;
        let normalization = match execution.normalization() {
            EmbeddingNormalization::None => vector::EmbeddingNormalization::None,
            EmbeddingNormalization::L2 => vector::EmbeddingNormalization::UnitL2,
        };
        let recipe = vector::EmbeddingRecipe {
            model: local_model_version(self.runtime, execution.launch_configuration()),
            tokenizer: vector::TokenizerVersion::from_value(&execution.tokenizer()),
            dimensions: NonZeroU32::new(execution.dimension()).ok_or(
                LocalQueryRecipeError::RuntimeSpec(EmbeddingRuntimeSpecError::ZeroDimension),
            )?,
            metric: self.contract.metric,
            pooling: self.contract.pooling,
            normalization,
            encoding: self.contract.encoding,
            query_treatment: vector::TreatmentVersion::from_value(&[
                self.contract.query_treatment as u8,
                backend_compile::EmbeddingPurpose::Query as u8,
            ]),
            document_treatment: vector::TreatmentVersion::from_value(&[
                self.contract.document_treatment as u8,
                backend_compile::EmbeddingPurpose::Document as u8,
            ]),
        };
        Ok(AdmittedLocalQueryRecipe {
            runtime,
            execution,
            recipe,
        })
    }
}

fn local_model_version(
    runtime: EmbeddingRuntimeSpecV1,
    launch_configuration: [u8; 32],
) -> vector::ModelVersion {
    let mut model = blake3::Hasher::new_derive_key("backend.local-query.activated-model.v1");
    model.update(&runtime.canonical_bytes());
    model.update(&launch_configuration);
    vector::ModelVersion::from_value(model.finalize().as_bytes())
}

/// An activated runtime and explicitly declared semantics admitted together.
/// Its private fields prevent callers from manufacturing this proof from a label.
#[derive(Debug)]
pub struct AdmittedLocalQueryRecipe {
    runtime: Arc<EmbeddingExecutable>,
    execution: EmbeddingExecutionIdentity,
    recipe: vector::EmbeddingRecipe,
}

impl AdmittedLocalQueryRecipe {
    /// Shared compiler/runtime owner, with no second activation or model instance.
    #[must_use]
    pub const fn runtime(&self) -> &Arc<EmbeddingExecutable> {
        &self.runtime
    }

    /// Complete admitted vector recipe; its model revision also binds launch settings.
    #[must_use]
    pub const fn recipe(&self) -> vector::EmbeddingRecipe {
        self.recipe
    }

    /// Real activated execution identity retained independently of the vector recipe hash.
    #[must_use]
    pub const fn execution_identity(&self) -> EmbeddingExecutionIdentity {
        self.execution
    }
}

/// Closed recipe admission causes; no substring or model-name classification is used.
#[derive(Debug, Eq, PartialEq)]
pub enum LocalQueryRecipeError {
    /// Descriptor magic, tags or exact extent are not canonical.
    DescriptorFormat,
    /// Complete declared options do not match the installed runtime recipe.
    SemanticOptionsMismatch,
    /// Canonical local vector facts do not implement this coordinate encoding.
    UnsupportedEncoding,
    /// Query text transformation has no implemented local execution path.
    UnsupportedTreatment,
    /// Existing BERS decoding or activated identity comparison refused the claim.
    RuntimeSpec(EmbeddingRuntimeSpecError),
}

impl fmt::Display for LocalQueryRecipeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DescriptorFormat => {
                formatter.write_str("local query recipe descriptor is invalid")
            }
            Self::SemanticOptionsMismatch => formatter.write_str(
                "local query recipe options differ from the installed embedding runtime",
            ),
            Self::UnsupportedEncoding => {
                formatter.write_str("local query embeddings require float32 encoding")
            }
            Self::UnsupportedTreatment => {
                formatter.write_str("local query embedding text treatment is unsupported")
            }
            Self::RuntimeSpec(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for LocalQueryRecipeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RuntimeSpec(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "local_recipe_tests.rs"]
mod tests;
