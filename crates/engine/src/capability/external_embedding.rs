use std::{fmt, sync::Arc};

use backend_version::ObjectVersion;

use super::{
    CapabilityArtifactSchema, CapabilityRuntime, DependencyRole, EmbeddingModel, Normalization,
    NumericRepresentation, ResidentCapability,
};

/// Configuration and verified sidecars used to activate the built-in external embedding runtime.
pub struct ExternalEmbeddingLoader {
    /// Verified executable path.
    pub program: std::path::PathBuf,
    /// Explicit executable arguments.
    pub arguments: Vec<String>,
    /// Absolute bounded scratch workspace.
    pub workspace: std::path::PathBuf,
    /// Exact environment; ambient variables are not inherited.
    pub environment: backend_compile::ProcessEnvironment,
    /// Process input/output/deadline/workspace/process/memory bounds.
    pub process_limits: backend_compile::ProcessLimits,
    /// Independent UTF-8 request bound.
    pub maximum_text_bytes: usize,
    /// Content-checked executable closure.
    pub executable: backend_compile::ToolchainArtifact,
    /// Immutable tokenizer bytes matching the manifest's tokenizer dependency.
    pub tokenizer: Arc<[u8]>,
    /// Immutable helper bytes matching the manifest's runtime dependency and executable path.
    pub helper: Arc<[u8]>,
}

/// Active external model process adapter. Calls remain available only through an engine
/// [`ExecutionReady`] borrow.
pub struct ExternalEmbeddingCapability {
    runtime: backend_compile::EmbeddingExecutable,
}

impl ExternalEmbeddingCapability {
    /// Runs one bounded query/document inference on the active external model.
    ///
    /// # Errors
    ///
    /// Returns a protocol, process-bound, identity, dimension, normalization, or revocation
    /// failure reported by the verified executable adapter.
    pub fn infer(
        &self,
        invocation: backend_compile::EmbeddingInvocation<'_>,
    ) -> Result<backend_compile::EmbeddingCoordinates, backend_compile::EmbeddingExecutableError>
    {
        self.runtime.infer(invocation)
    }
}

/// Failure while binding a verified resident model and tokenizer closure to the external runtime.
#[derive(Debug)]
pub enum ExternalEmbeddingActivationError {
    /// Recipe requests a coordinate representation not spoken by the f32 protocol.
    NumericRepresentation,
    /// Manifest has no single tokenizer dependency.
    MissingTokenizer,
    /// Manifest has no single embedding-helper runtime dependency.
    MissingRuntime,
    /// Loader tokenizer bytes differ from the manifest dependency identity.
    TokenizerIdentity,
    /// Loader helper bytes, manifest dependency, and executable path do not share one identity.
    RuntimeIdentity,
    /// External executable activation, self-test, inference, or revocation failed.
    Runtime(backend_compile::EmbeddingExecutableError),
}

impl fmt::Display for ExternalEmbeddingActivationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "external embedding activation failed: {self:?}")
    }
}

impl std::error::Error for ExternalEmbeddingActivationError {}

impl CapabilityRuntime<EmbeddingModel> for ExternalEmbeddingCapability {
    type Loader = ExternalEmbeddingLoader;
    type Error = ExternalEmbeddingActivationError;

    fn activate(
        resident: &ResidentCapability<EmbeddingModel>,
        loader: &Self::Loader,
    ) -> Result<Self, Self::Error> {
        let recipe = resident.manifest().recipe();
        if recipe.numeric != NumericRepresentation::F32 {
            return Err(ExternalEmbeddingActivationError::NumericRepresentation);
        }
        let mut tokenizers = resident
            .manifest()
            .dependencies()
            .iter()
            .filter(|dependency| dependency.role == DependencyRole::Tokenizer);
        let tokenizer = tokenizers
            .next()
            .ok_or(ExternalEmbeddingActivationError::MissingTokenizer)?;
        if tokenizers.next().is_some() {
            return Err(ExternalEmbeddingActivationError::MissingTokenizer);
        }
        let observed_tokenizer =
            ObjectVersion::<CapabilityArtifactSchema>::from_value(loader.tokenizer.as_ref());
        if observed_tokenizer != tokenizer.artifact {
            return Err(ExternalEmbeddingActivationError::TokenizerIdentity);
        }
        let mut runtimes = resident
            .manifest()
            .dependencies()
            .iter()
            .filter(|dependency| dependency.role == DependencyRole::Runtime);
        let helper = runtimes
            .next()
            .ok_or(ExternalEmbeddingActivationError::MissingRuntime)?;
        if runtimes.next().is_some() {
            return Err(ExternalEmbeddingActivationError::MissingRuntime);
        }
        let observed_helper =
            ObjectVersion::<CapabilityArtifactSchema>::from_value(loader.helper.as_ref());
        if observed_helper != helper.artifact
            || backend_compile::typed_of::<backend_compile::ToolchainSchema>(loader.helper.as_ref())
                != loader.executable.executable().digest()
        {
            return Err(ExternalEmbeddingActivationError::RuntimeIdentity);
        }
        let model = backend_compile::EmbeddingArtifact::new(Arc::from(resident.bytes()));
        let tokenizer = backend_compile::EmbeddingArtifact::new(Arc::clone(&loader.tokenizer));
        let normalization = match recipe.normalization {
            Normalization::None => backend_compile::EmbeddingNormalization::None,
            Normalization::L2 => backend_compile::EmbeddingNormalization::L2,
        };
        let runtime = backend_compile::EmbeddingExecutable::activate(
            loader.program.clone(),
            loader.arguments.clone(),
            loader.workspace.clone(),
            loader.environment.clone(),
            loader.process_limits,
            loader.maximum_text_bytes,
            recipe.dimensions,
            normalization,
            recipe.identity().to_bytes(),
            loader.executable.clone(),
            model,
            tokenizer,
        )
        .map_err(ExternalEmbeddingActivationError::Runtime)?;
        Ok(Self { runtime })
    }

    fn probe_ready(
        &mut self,
        _resident: &ResidentCapability<EmbeddingModel>,
    ) -> Result<(), Self::Error> {
        self.runtime
            .probe_ready()
            .map_err(ExternalEmbeddingActivationError::Runtime)
    }

    fn revoke(&mut self) -> Result<(), Self::Error> {
        self.runtime.revoke();
        Ok(())
    }
}
