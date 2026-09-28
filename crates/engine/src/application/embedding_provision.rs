//! Owner-private persisted embedding runtime provisioning shared by compiler owners.
//!
//! Portable model and invocation identity is owned by `backend-compile`'s
//! `EmbeddingRuntimeSpecV1`. This module stores only the owner-local launch
//! paths, explicit environment and limits, and bounded content-addressed model
//! and tokenizer bytes. Activation always goes through the shared BEM1/BEC1
//! executable boundary and its real self-test.

use super::compiler::{EmbeddingProvisioningFailure, EmbeddingRequirement};
use backend_compile::{
    EmbeddingArtifact, EmbeddingExecutable, EmbeddingNormalization, EmbeddingRuntimeSpecV1,
    ExecutableIdentity, MAX_EMBEDDING_MODEL_BYTES, MAX_EMBEDDING_TOKENIZER_BYTES,
    ProcessEnvironment, ProcessLimits, ToolchainArtifact,
};
use std::fs::{self, File};
use std::io::Read;
use std::num::{NonZeroU16, NonZeroU32};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

const CONFIG_MAGIC: &[u8; 8] = b"BKCWEMB1";
const CONFIG_VERSION: u8 = 1;
const CONFIG_CHECKSUM_BYTES: usize = 32;
const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_ARGUMENTS: usize = 64;
const MAX_DEPENDENCIES: usize = 64;
const MAX_ENVIRONMENT: usize = 128;
const MAX_PATH_BYTES: usize = 4096;
const MAX_ARGUMENT_BYTES: usize = 4096;
const MAX_ENVIRONMENT_VALUE_BYTES: usize = 16 * 1024;
const MAX_TEXT_BYTES: u32 = 4 * 1024 * 1024;
const EMBEDDING_SPEC_BYTES: usize = 4 + 1 + (32 * 4) + 2 + 1 + 4 + 32;
const BEM1_REQUEST_HEADER_BYTES: u64 = 108;
const BEC1_RESPONSE_HEADER_BYTES: u64 = 6;
const MAX_STDIO_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PROCESS_INPUT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_RUNTIME_WORKSPACE_BYTES: u64 = 512 * 1024 * 1024;

/// Explicit process limits and owner memory reservation.
///
/// `memory_bytes` is an OS-enforced process limit where the platform supports
/// it. `resident_credit_bytes` separately reserves owner scheduler credits
/// for resident artifacts and one inference's bounded scratch. The latter is
/// never described as an OS memory ceiling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmbeddingRuntimeLimits {
    /// Maximum captured stdout bytes.
    pub stdout_bytes: u64,
    /// Maximum captured stderr bytes.
    pub stderr_bytes: u64,
    /// Maximum total captured process output bytes.
    pub output_bytes: u64,
    /// Maximum BEM1 request bytes sent to the process.
    pub input_bytes: u64,
    /// Maximum wall-clock time for one inference.
    pub wall_time_ms: u64,
    /// Maximum new workspace bytes below the private runtime directory.
    pub workspace_bytes: u64,
    /// Maximum process-tree count, when supported by the platform.
    pub process_count: u64,
    /// Optional OS-enforced per-process memory ceiling.
    pub memory_bytes: Option<u64>,
    /// Optional OS-enforced CPU-time limit.
    pub cpu_time_ms: Option<u64>,
    /// Scheduler memory credits reserved for resident artifacts and inference scratch.
    pub resident_credit_bytes: u64,
}

impl EmbeddingRuntimeLimits {
    fn validate(
        self,
        model_bytes: u64,
        tokenizer_bytes: u64,
        dimension: NonZeroU16,
        maximum_text_bytes: NonZeroU32,
    ) -> Result<(), EmbeddingRuntimeError> {
        if self.stdout_bytes == 0
            || self.stdout_bytes > MAX_STDIO_BYTES
            || self.stderr_bytes == 0
            || self.stderr_bytes > MAX_STDIO_BYTES
            || self.output_bytes == 0
            || self.output_bytes > MAX_STDIO_BYTES
            || self.input_bytes == 0
            || self.input_bytes > MAX_PROCESS_INPUT_BYTES
            || self.wall_time_ms == 0
            || self.workspace_bytes == 0
            || self.workspace_bytes > MAX_RUNTIME_WORKSPACE_BYTES
            || self.process_count == 0
            || self.process_count > 64
            || self.memory_bytes == Some(0)
            || self
                .memory_bytes
                .is_some_and(|bytes| bytes > 64 * 1024 * 1024 * 1024)
            || self.cpu_time_ms == Some(0)
            || self
                .cpu_time_ms
                .is_some_and(|millis| millis > 5 * 60 * 1000)
            || self.wall_time_ms > 5 * 60 * 1000
            || self.input_bytes < BEM1_REQUEST_HEADER_BYTES + u64::from(maximum_text_bytes.get())
            || self.stdout_bytes < BEC1_RESPONSE_HEADER_BYTES + u64::from(dimension.get()) * 4
            || self.output_bytes < BEC1_RESPONSE_HEADER_BYTES + u64::from(dimension.get()) * 4
        {
            return Err(EmbeddingRuntimeError::InvalidConfig);
        }
        let minimum_reservation = model_bytes
            .checked_add(tokenizer_bytes)
            .and_then(|bytes| bytes.checked_add(self.input_bytes))
            .and_then(|bytes| bytes.checked_add(self.output_bytes))
            .and_then(|bytes| bytes.checked_add(self.memory_bytes.unwrap_or(0)))
            .and_then(|bytes| bytes.checked_add(u64::from(dimension.get()) * 4))
            .ok_or(EmbeddingRuntimeError::InvalidConfig)?;
        if self.resident_credit_bytes < minimum_reservation {
            return Err(EmbeddingRuntimeError::InvalidConfig);
        }
        Ok(())
    }

    fn process_limits(self) -> Result<ProcessLimits, EmbeddingRuntimeError> {
        let mut limits = ProcessLimits::new(
            to_usize(self.stdout_bytes)?,
            to_usize(self.stderr_bytes)?,
            Duration::from_millis(self.wall_time_ms),
            to_usize(self.output_bytes)?,
        )?
        .with_input_bytes_limit(to_usize(self.input_bytes)?)?
        .with_workspace_limit(to_usize(self.workspace_bytes)?)?
        .with_process_count_limit(to_usize(self.process_count)?)?;
        if let Some(memory_bytes) = self.memory_bytes {
            limits = limits.with_memory_bytes_limit(to_usize(memory_bytes)?)?;
        }
        if let Some(cpu_time_ms) = self.cpu_time_ms {
            limits = limits.with_cpu_time_limit(Duration::from_millis(cpu_time_ms))?;
        }
        Ok(limits)
    }
}

/// Owner-supplied values used to install or replace an embedding runtime.
///
/// Model and tokenizer buffers are bounded by the shared compile crate before
/// they are persisted. The buffers are not included in diagnostics or `Debug`.
pub struct EmbeddingRuntimeInstall {
    /// Whether indexing may continue with an explicit unavailable embedding plane.
    pub requirement: EmbeddingRequirement,
    /// Absolute external model-runtime executable.
    pub program: PathBuf,
    /// Explicit argument vector; ambient shell parsing is not used.
    pub arguments: Vec<String>,
    /// Exact executable dependencies included in the portable toolchain identity.
    pub dependencies: Vec<PathBuf>,
    /// Explicit child environment, never inherited from the owner process.
    pub environment: Vec<(String, String)>,
    /// Bounded inference resources and scheduler credit reservation.
    pub limits: EmbeddingRuntimeLimits,
    /// Exact model artifact bytes.
    pub model_bytes: Vec<u8>,
    /// Exact tokenizer artifact bytes.
    pub tokenizer_bytes: Vec<u8>,
    /// Fixed coordinate dimension.
    pub dimension: NonZeroU16,
    /// Required output normalization.
    pub normalization: EmbeddingNormalization,
    /// Maximum UTF-8 bytes accepted by one inference request.
    pub maximum_text_bytes: NonZeroU32,
    /// Digest of model-side options and task treatment.
    pub options_digest: [u8; 32],
}

impl std::fmt::Debug for EmbeddingRuntimeInstall {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmbeddingRuntimeInstall")
            .field("requirement", &self.requirement)
            .field("program", &"<owner-private path>")
            .field("arguments", &self.arguments.len())
            .field("dependencies", &self.dependencies.len())
            .field("environment", &self.environment.len())
            .field("limits", &self.limits)
            .field("model_bytes", &self.model_bytes.len())
            .field("tokenizer_bytes", &self.tokenizer_bytes.len())
            .field("dimension", &self.dimension)
            .field("normalization", &self.normalization)
            .field("maximum_text_bytes", &self.maximum_text_bytes)
            .finish_non_exhaustive()
    }
}

/// Closed reason why an optional, previously configured runtime cannot activate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmbeddingUnavailableReason {
    /// The model blob is missing or does not match the configured digest.
    ModelArtifactUnavailable,
    /// The tokenizer blob is missing or does not match the configured digest.
    TokenizerArtifactUnavailable,
    /// Model, tokenizer, executable, or declared dependency bytes changed from the pinned hash.
    ArtifactIdentityMismatch,
    /// The configured executable or one of its declared dependencies changed.
    ExecutableIdentityChanged,
    /// The configured executable cannot be opened or is no longer present.
    ExecutableUnavailable,
    /// This platform cannot enforce one of the configured process resource limits.
    UnsupportedResourceLimit,
    /// This platform cannot create or protect the private inference workspace.
    PlatformUnsupported,
    /// The external program failed the bounded BEM1/BEC1 activation self-test.
    ActivationFailed,
}

/// Observable embedding state after loading the owner-private config.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmbeddingRuntimeStatus {
    /// No embedding runtime has been provisioned; Optional behavior is used.
    NotConfigured,
    /// The exact configured runtime passed artifact checks and a real process self-test.
    Available { recipe: [u8; 32] },
    /// A configured runtime could not activate; Optional keeps IR and Required fails staging.
    Unavailable(EmbeddingUnavailableReason),
}

/// Activated runtime and its typed Optional/Required provisioning status.
pub struct EmbeddingRuntimeProvision {
    runtime: Option<Arc<EmbeddingExecutable>>,
    requirement: EmbeddingRequirement,
    status: EmbeddingRuntimeStatus,
    resident_credit_bytes: u64,
}

impl std::fmt::Debug for EmbeddingRuntimeProvision {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmbeddingRuntimeProvision")
            .field("active", &self.runtime.is_some())
            .field("requirement", &self.requirement)
            .field("status", &self.status)
            .field("resident_credit_bytes", &self.resident_credit_bytes)
            .finish()
    }
}

impl EmbeddingRuntimeProvision {
    /// Loads and activates a persisted runtime, or returns no-config/unavailable state.
    pub fn open(config_path: impl AsRef<Path>) -> Result<Self, EmbeddingRuntimeError> {
        let path = config_path.as_ref();
        let config = match read_private_config(path) {
            Ok(Some(config)) => config,
            Ok(None) => {
                return Ok(Self {
                    runtime: None,
                    requirement: EmbeddingRequirement::Optional,
                    status: EmbeddingRuntimeStatus::NotConfigured,
                    resident_credit_bytes: 0,
                });
            }
            Err(error) => return Err(error),
        };
        config.limits.validate(
            config.model_bytes,
            config.tokenizer_bytes,
            config.spec.dimension(),
            config.spec.maximum_text_bytes(),
        )?;
        let resident_credit_bytes = config.limits.resident_credit_bytes;
        match activate_config(path, &config) {
            Ok(runtime) => Ok(Self {
                runtime: Some(Arc::new(runtime)),
                requirement: config.requirement,
                status: EmbeddingRuntimeStatus::Available {
                    recipe: config.spec.recipe_identity(),
                },
                resident_credit_bytes,
            }),
            Err(reason) => Ok(Self {
                runtime: None,
                requirement: config.requirement,
                status: EmbeddingRuntimeStatus::Unavailable(reason),
                resident_credit_bytes,
            }),
        }
    }

    /// Returns the active shared embedding executable, if activation succeeded.
    #[must_use]
    pub fn runtime(&self) -> Option<Arc<EmbeddingExecutable>> {
        self.runtime.as_ref().map(Arc::clone)
    }

    /// Returns the exact Optional/Required engine policy.
    #[must_use]
    pub const fn requirement(&self) -> EmbeddingRequirement {
        self.requirement
    }

    /// Returns the cold-open result without revealing private host paths or environment values.
    #[must_use]
    pub const fn status(&self) -> EmbeddingRuntimeStatus {
        self.status
    }

    /// Returns the configured scheduler memory reservation for the runtime and one inference.
    #[must_use]
    pub const fn resident_credit_bytes(&self) -> u64 {
        self.resident_credit_bytes
    }

    /// Returns the engine's closed provisioning cause when configured activation failed.
    #[must_use]
    pub const fn provisioning_failure(&self) -> Option<EmbeddingProvisioningFailure> {
        match self.status {
            EmbeddingRuntimeStatus::Unavailable(
                EmbeddingUnavailableReason::ModelArtifactUnavailable,
            ) => Some(EmbeddingProvisioningFailure::ModelUnavailable),
            EmbeddingRuntimeStatus::Unavailable(
                EmbeddingUnavailableReason::TokenizerArtifactUnavailable,
            ) => Some(EmbeddingProvisioningFailure::TokenizerUnavailable),
            EmbeddingRuntimeStatus::Unavailable(
                EmbeddingUnavailableReason::ArtifactIdentityMismatch,
            ) => Some(EmbeddingProvisioningFailure::ArtifactIdentityMismatch),
            EmbeddingRuntimeStatus::Unavailable(
                EmbeddingUnavailableReason::ExecutableIdentityChanged,
            ) => Some(EmbeddingProvisioningFailure::ArtifactIdentityMismatch),
            EmbeddingRuntimeStatus::Unavailable(
                EmbeddingUnavailableReason::ExecutableUnavailable,
            ) => Some(EmbeddingProvisioningFailure::ExecutableUnavailable),
            EmbeddingRuntimeStatus::Unavailable(
                EmbeddingUnavailableReason::UnsupportedResourceLimit,
            ) => Some(EmbeddingProvisioningFailure::ResourceLimitUnavailable),
            EmbeddingRuntimeStatus::Unavailable(
                EmbeddingUnavailableReason::PlatformUnsupported,
            ) => Some(EmbeddingProvisioningFailure::PlatformUnsupported),
            EmbeddingRuntimeStatus::Unavailable(EmbeddingUnavailableReason::ActivationFailed) => {
                Some(EmbeddingProvisioningFailure::ActivationRejected)
            }
            EmbeddingRuntimeStatus::NotConfigured | EmbeddingRuntimeStatus::Available { .. } => {
                None
            }
        }
    }
}

/// Installs exact model/tokenizer bytes and an owner-private launch policy atomically.
///
/// Blobs are content-addressed and published before the config pointer. A crash can leave only
/// an unreachable blob; it cannot make an older config refer to partially written bytes.
pub fn install_embedding_runtime(
    config_path: impl AsRef<Path>,
    request: EmbeddingRuntimeInstall,
) -> Result<EmbeddingRuntimeSpecV1, EmbeddingRuntimeError> {
    let config_path = config_path.as_ref();
    validate_config_path(config_path)?;
    if let Some(parent) = config_path.parent() {
        backend_platform::durable::ensure_private_directory(parent)?;
    }
    if request.model_bytes.is_empty()
        || request.model_bytes.len() > MAX_EMBEDDING_MODEL_BYTES
        || request.tokenizer_bytes.is_empty()
        || request.tokenizer_bytes.len() > MAX_EMBEDDING_TOKENIZER_BYTES
        || request.arguments.len() > MAX_ARGUMENTS
        || request.maximum_text_bytes.get() > MAX_TEXT_BYTES
    {
        return Err(EmbeddingRuntimeError::InvalidConfig);
    }
    let program = canonical_regular_file(&request.program)?;
    let (dependencies, dependency_ids) = canonical_dependencies(&program, &request.dependencies)?;
    let executable = ToolchainArtifact::from_path(&program, dependency_ids)?;
    let model_hash = *blake3::hash(&request.model_bytes).as_bytes();
    let tokenizer_hash = *blake3::hash(&request.tokenizer_bytes).as_bytes();
    let spec = EmbeddingRuntimeSpecV1::new(
        model_hash,
        model_hash,
        tokenizer_hash,
        executable.identity().to_bytes(),
        request.dimension,
        request.normalization,
        request.maximum_text_bytes,
        request.options_digest,
    );
    let environment = ProcessEnvironment::new(request.environment)?;
    let config = PersistedEmbeddingRuntimeConfig {
        spec,
        requirement: request.requirement,
        program,
        arguments: request.arguments,
        dependencies,
        environment: environment.variables().to_vec(),
        limits: request.limits,
        model_bytes: u64::try_from(request.model_bytes.len())
            .map_err(|_| EmbeddingRuntimeError::InvalidConfig)?,
        tokenizer_bytes: u64::try_from(request.tokenizer_bytes.len())
            .map_err(|_| EmbeddingRuntimeError::InvalidConfig)?,
    };
    config.limits.validate(
        config.model_bytes,
        config.tokenizer_bytes,
        config.spec.dimension(),
        config.spec.maximum_text_bytes(),
    )?;
    write_private_artifact(config_path, model_hash, "model", &request.model_bytes)?;
    write_private_artifact(
        config_path,
        tokenizer_hash,
        "tokenizer",
        &request.tokenizer_bytes,
    )?;
    let bytes = encode_config(&config)?;
    backend_platform::durable::write_private_atomic(config_path, &bytes)?;
    Ok(spec)
}

/// Read-only summary of the persisted portable spec and resource policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmbeddingRuntimeSummary {
    /// Shared portable model/invocation identity.
    pub spec: EmbeddingRuntimeSpecV1,
    /// Whether output may proceed without an available embedding runtime.
    pub requirement: EmbeddingRequirement,
    /// Explicit local process limits and scheduler reservation.
    pub limits: EmbeddingRuntimeLimits,
}

/// Reads the owner-private configuration without executing its external program.
pub fn inspect_embedding_runtime(
    config_path: impl AsRef<Path>,
) -> Result<Option<EmbeddingRuntimeSummary>, EmbeddingRuntimeError> {
    Ok(
        read_private_config(config_path.as_ref())?.map(|config| EmbeddingRuntimeSummary {
            spec: config.spec,
            requirement: config.requirement,
            limits: config.limits,
        }),
    )
}

/// Reads only the persisted scheduler reservation without activating model code.
pub fn embedding_runtime_resident_credit_bytes(
    config_path: impl AsRef<Path>,
) -> Result<u64, EmbeddingRuntimeError> {
    Ok(inspect_embedding_runtime(config_path)?
        .map_or(0, |summary| summary.limits.resident_credit_bytes))
}

/// Removes only the config pointer. Content-addressed blobs remain for normal owner cleanup.
pub fn remove_embedding_runtime(
    config_path: impl AsRef<Path>,
) -> Result<(), EmbeddingRuntimeError> {
    match backend_platform::durable::remove_private(config_path.as_ref()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Failure while provisioning, cold-opening, or activating embedding state.
#[derive(Debug, Error)]
pub enum EmbeddingRuntimeError {
    /// A path, bound, canonical encoding, or persisted claim was invalid.
    #[error("embedding runtime configuration is invalid")]
    InvalidConfig,
    /// Persisted runtime bytes or artifact metadata are malformed or corrupt.
    #[error("embedding runtime state is corrupt")]
    Corrupt,
    /// File-system persistence or reading failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Process environment or resource limits failed validation.
    #[error(transparent)]
    Process(#[from] backend_compile::ProcessError),
    /// The external process did not pass the exact bounded embedding protocol.
    #[error(transparent)]
    Activation(#[from] backend_compile::EmbeddingExecutableError),
}

struct PersistedEmbeddingRuntimeConfig {
    spec: EmbeddingRuntimeSpecV1,
    requirement: EmbeddingRequirement,
    program: PathBuf,
    arguments: Vec<String>,
    dependencies: Vec<(PathBuf, [u8; 32])>,
    environment: Vec<(String, String)>,
    limits: EmbeddingRuntimeLimits,
    model_bytes: u64,
    tokenizer_bytes: u64,
}

fn activate_config(
    config_path: &Path,
    config: &PersistedEmbeddingRuntimeConfig,
) -> Result<EmbeddingExecutable, EmbeddingUnavailableReason> {
    let model_path = artifact_path(config_path, config.spec.model(), "model");
    let tokenizer_path = artifact_path(config_path, config.spec.tokenizer(), "tokenizer");
    let model_bytes =
        read_private_artifact(&model_path, config.model_bytes, MAX_EMBEDDING_MODEL_BYTES).map_err(
            |error| {
                artifact_read_failure(error, EmbeddingUnavailableReason::ModelArtifactUnavailable)
            },
        )?;
    let tokenizer_bytes = read_private_artifact(
        &tokenizer_path,
        config.tokenizer_bytes,
        MAX_EMBEDDING_TOKENIZER_BYTES,
    )
    .map_err(|error| {
        artifact_read_failure(
            error,
            EmbeddingUnavailableReason::TokenizerArtifactUnavailable,
        )
    })?;
    let model = EmbeddingArtifact::new(Arc::from(model_bytes));
    let tokenizer = EmbeddingArtifact::new(Arc::from(tokenizer_bytes));
    if model.identity().as_bytes() != config.spec.model()
        || tokenizer.identity().as_bytes() != config.spec.tokenizer()
    {
        return Err(EmbeddingUnavailableReason::ArtifactIdentityMismatch);
    }
    let mut dependency_ids = Vec::with_capacity(config.dependencies.len());
    for (path, expected) in &config.dependencies {
        let identity = ExecutableIdentity::from_path(path).map_err(|error| match error {
            backend_compile::ProcessError::ExecutableUnavailable => {
                EmbeddingUnavailableReason::ExecutableUnavailable
            }
            _ => EmbeddingUnavailableReason::ExecutableIdentityChanged,
        })?;
        let observed = identity.digest().to_bytes();
        if observed != *expected {
            return Err(EmbeddingUnavailableReason::ArtifactIdentityMismatch);
        }
        dependency_ids.push(identity.digest());
    }
    let executable_identity =
        ExecutableIdentity::from_path(&config.program).map_err(|error| match error {
            backend_compile::ProcessError::ExecutableUnavailable => {
                EmbeddingUnavailableReason::ExecutableUnavailable
            }
            _ => EmbeddingUnavailableReason::ExecutableIdentityChanged,
        })?;
    let executable = ToolchainArtifact::new(executable_identity, dependency_ids);
    if executable.identity().to_bytes() != config.spec.executable() {
        return Err(EmbeddingUnavailableReason::ArtifactIdentityMismatch);
    }
    let process_limits = config
        .limits
        .process_limits()
        .map_err(|error| match error {
            EmbeddingRuntimeError::Process(backend_compile::ProcessError::UnsupportedLimit(_)) => {
                EmbeddingUnavailableReason::UnsupportedResourceLimit
            }
            _ => EmbeddingUnavailableReason::ActivationFailed,
        })?;
    if process_limits.unsupported_limit().is_some() {
        return Err(EmbeddingUnavailableReason::UnsupportedResourceLimit);
    }
    let environment = ProcessEnvironment::new(config.environment.clone())
        .map_err(|_| EmbeddingUnavailableReason::ActivationFailed)?;
    let workspace = config_path
        .parent()
        .ok_or(EmbeddingUnavailableReason::ActivationFailed)?
        .join("embedding-runtime-work");
    backend_platform::durable::ensure_private_directory(&workspace).map_err(|error| {
        if error.kind() == std::io::ErrorKind::Unsupported {
            EmbeddingUnavailableReason::PlatformUnsupported
        } else {
            EmbeddingUnavailableReason::ActivationFailed
        }
    })?;
    let runtime = EmbeddingExecutable::activate_with_spec(
        config.spec,
        config.program.clone(),
        config.arguments.clone(),
        workspace,
        environment,
        process_limits,
        executable,
        model,
        tokenizer,
    )
    .map_err(|_| EmbeddingUnavailableReason::ActivationFailed)?;
    config
        .spec
        .validate_execution_identity(runtime.execution_identity())
        .map_err(|_| EmbeddingUnavailableReason::ActivationFailed)?;
    let resident_bytes = config
        .model_bytes
        .checked_add(config.tokenizer_bytes)
        .and_then(|bytes| bytes.checked_add(runtime.maximum_inference_scratch_bytes() as u64))
        .ok_or(EmbeddingUnavailableReason::ActivationFailed)?;
    if resident_bytes > config.limits.resident_credit_bytes {
        return Err(EmbeddingUnavailableReason::ActivationFailed);
    }
    Ok(runtime)
}

fn artifact_read_failure(
    error: EmbeddingRuntimeError,
    missing: EmbeddingUnavailableReason,
) -> EmbeddingUnavailableReason {
    match error {
        EmbeddingRuntimeError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => missing,
        _ => EmbeddingUnavailableReason::ArtifactIdentityMismatch,
    }
}

fn encode_config(
    config: &PersistedEmbeddingRuntimeConfig,
) -> Result<Vec<u8>, EmbeddingRuntimeError> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(CONFIG_MAGIC);
    bytes.push(CONFIG_VERSION);
    bytes.extend_from_slice(&config.spec.canonical_bytes());
    bytes.push(match config.requirement {
        EmbeddingRequirement::Optional => 0,
        EmbeddingRequirement::Required => 1,
    });
    bytes.extend_from_slice(&config.model_bytes.to_be_bytes());
    bytes.extend_from_slice(&config.tokenizer_bytes.to_be_bytes());
    encode_limits(&mut bytes, config.limits);
    put_string(&mut bytes, &config.program)?;
    put_count(&mut bytes, config.arguments.len(), MAX_ARGUMENTS)?;
    for argument in &config.arguments {
        put_utf8(&mut bytes, argument, MAX_ARGUMENT_BYTES)?;
    }
    put_count(&mut bytes, config.dependencies.len(), MAX_DEPENDENCIES)?;
    for (path, digest) in &config.dependencies {
        put_string(&mut bytes, path)?;
        bytes.extend_from_slice(digest);
    }
    put_count(&mut bytes, config.environment.len(), MAX_ENVIRONMENT)?;
    for (key, value) in &config.environment {
        put_utf8(&mut bytes, key, MAX_ARGUMENT_BYTES)?;
        put_utf8(&mut bytes, value, MAX_ENVIRONMENT_VALUE_BYTES)?;
    }
    if bytes.len().saturating_add(CONFIG_CHECKSUM_BYTES) > MAX_CONFIG_BYTES {
        return Err(EmbeddingRuntimeError::InvalidConfig);
    }
    let checksum = blake3::hash(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    Ok(bytes)
}

fn decode_config(bytes: &[u8]) -> Result<PersistedEmbeddingRuntimeConfig, EmbeddingRuntimeError> {
    if bytes.len() < CONFIG_MAGIC.len() + 1 + CONFIG_CHECKSUM_BYTES
        || bytes.len() > MAX_CONFIG_BYTES
    {
        return Err(EmbeddingRuntimeError::Corrupt);
    }
    let checksum_at = bytes.len() - CONFIG_CHECKSUM_BYTES;
    if blake3::hash(&bytes[..checksum_at]).as_bytes() != &bytes[checksum_at..]
        || &bytes[..CONFIG_MAGIC.len()] != CONFIG_MAGIC
    {
        return Err(EmbeddingRuntimeError::Corrupt);
    }
    let mut reader = Reader::new(&bytes[CONFIG_MAGIC.len()..checksum_at]);
    if reader.u8()? != CONFIG_VERSION {
        return Err(EmbeddingRuntimeError::Corrupt);
    }
    let spec_bytes = reader.take(EMBEDDING_SPEC_BYTES)?;
    let spec =
        EmbeddingRuntimeSpecV1::decode(spec_bytes).map_err(|_| EmbeddingRuntimeError::Corrupt)?;
    let requirement = match reader.u8()? {
        0 => EmbeddingRequirement::Optional,
        1 => EmbeddingRequirement::Required,
        _ => return Err(EmbeddingRuntimeError::Corrupt),
    };
    let model_bytes = reader.u64()?;
    let tokenizer_bytes = reader.u64()?;
    if model_bytes == 0
        || model_bytes > MAX_EMBEDDING_MODEL_BYTES as u64
        || tokenizer_bytes == 0
        || tokenizer_bytes > MAX_EMBEDDING_TOKENIZER_BYTES as u64
    {
        return Err(EmbeddingRuntimeError::Corrupt);
    }
    let limits = decode_limits(&mut reader)?;
    let program = PathBuf::from(reader.string(MAX_PATH_BYTES)?);
    if !program.is_absolute() {
        return Err(EmbeddingRuntimeError::Corrupt);
    }
    let argument_count = reader.count(MAX_ARGUMENTS)?;
    let mut arguments = Vec::new();
    arguments
        .try_reserve_exact(argument_count)
        .map_err(|_| EmbeddingRuntimeError::Corrupt)?;
    for _ in 0..argument_count {
        let value = reader.string(MAX_ARGUMENT_BYTES)?.to_owned();
        if value.as_bytes().contains(&0) {
            return Err(EmbeddingRuntimeError::Corrupt);
        }
        arguments.push(value);
    }
    let dependency_count = reader.count(MAX_DEPENDENCIES)?;
    let mut dependencies = Vec::new();
    dependencies
        .try_reserve_exact(dependency_count)
        .map_err(|_| EmbeddingRuntimeError::Corrupt)?;
    let mut previous_dependency: Option<String> = None;
    for _ in 0..dependency_count {
        let path_text = reader.string(MAX_PATH_BYTES)?.to_owned();
        if previous_dependency
            .as_ref()
            .is_some_and(|previous| previous >= &path_text)
        {
            return Err(EmbeddingRuntimeError::Corrupt);
        }
        previous_dependency = Some(path_text.clone());
        let path = PathBuf::from(path_text);
        if !path.is_absolute() {
            return Err(EmbeddingRuntimeError::Corrupt);
        }
        dependencies.push((path, reader.array()?));
    }
    let environment_count = reader.count(MAX_ENVIRONMENT)?;
    let mut environment = Vec::new();
    environment
        .try_reserve_exact(environment_count)
        .map_err(|_| EmbeddingRuntimeError::Corrupt)?;
    let mut previous_key: Option<String> = None;
    for _ in 0..environment_count {
        let key = reader.string(MAX_ARGUMENT_BYTES)?.to_owned();
        let value = reader.string(MAX_ENVIRONMENT_VALUE_BYTES)?.to_owned();
        if key.is_empty()
            || key.contains('=')
            || key.as_bytes().contains(&0)
            || value.as_bytes().contains(&0)
            || previous_key
                .as_ref()
                .is_some_and(|previous| previous >= &key)
        {
            return Err(EmbeddingRuntimeError::Corrupt);
        }
        previous_key = Some(key.clone());
        environment.push((key, value));
    }
    reader.finish()?;
    limits.validate(
        model_bytes,
        tokenizer_bytes,
        spec.dimension(),
        spec.maximum_text_bytes(),
    )?;
    Ok(PersistedEmbeddingRuntimeConfig {
        spec,
        requirement,
        program,
        arguments,
        dependencies,
        environment,
        limits,
        model_bytes,
        tokenizer_bytes,
    })
}

fn read_private_config(
    path: &Path,
) -> Result<Option<PersistedEmbeddingRuntimeConfig>, EmbeddingRuntimeError> {
    let mut file = match backend_platform::durable::open_private_read(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES as u64 {
        return Err(EmbeddingRuntimeError::Corrupt);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(metadata.len() as usize)
        .map_err(|_| EmbeddingRuntimeError::Corrupt)?;
    file.by_ref()
        .take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() {
        return Err(EmbeddingRuntimeError::Corrupt);
    }
    decode_config(&bytes).map(Some)
}

fn read_private_artifact(
    path: &Path,
    expected_length: u64,
    maximum: usize,
) -> Result<Vec<u8>, EmbeddingRuntimeError> {
    let mut file = backend_platform::durable::open_private_read(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() != expected_length || metadata.len() > maximum as u64 {
        return Err(EmbeddingRuntimeError::Corrupt);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(
            usize::try_from(expected_length).map_err(|_| EmbeddingRuntimeError::Corrupt)?,
        )
        .map_err(|_| EmbeddingRuntimeError::Corrupt)?;
    file.by_ref()
        .take(expected_length.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != expected_length {
        return Err(EmbeddingRuntimeError::Corrupt);
    }
    Ok(bytes)
}

fn write_private_artifact(
    config_path: &Path,
    digest: [u8; 32],
    kind: &str,
    bytes: &[u8],
) -> Result<(), EmbeddingRuntimeError> {
    let path = artifact_path(config_path, digest, kind);
    if let Ok(existing) = read_private_artifact(
        &path,
        u64::try_from(bytes.len()).map_err(|_| EmbeddingRuntimeError::InvalidConfig)?,
        bytes.len(),
    ) {
        if existing == bytes {
            return Ok(());
        }
    }
    backend_platform::durable::write_private_atomic(&path, bytes)?;
    Ok(())
}

fn artifact_path(config_path: &Path, digest: [u8; 32], kind: &str) -> PathBuf {
    config_path.with_file_name(format!("embedding-{}-{kind}.bin", hex(&digest)))
}

fn canonical_dependencies(
    program: &Path,
    dependencies: &[PathBuf],
) -> Result<(Vec<(PathBuf, [u8; 32])>, Vec<backend_compile::ToolchainId>), EmbeddingRuntimeError> {
    if dependencies.len() > MAX_DEPENDENCIES {
        return Err(EmbeddingRuntimeError::InvalidConfig);
    }
    let mut values = Vec::new();
    for dependency in dependencies {
        let path = canonical_regular_file(dependency)?;
        if path == program {
            return Err(EmbeddingRuntimeError::InvalidConfig);
        }
        let identity = ExecutableIdentity::from_path(&path)?;
        values.push((path, identity.digest().to_bytes(), identity.digest()));
    }
    values.sort_by(|left, right| path_string(&left.0).cmp(&path_string(&right.0)));
    if values.windows(2).any(|window| window[0].0 == window[1].0) {
        return Err(EmbeddingRuntimeError::InvalidConfig);
    }
    let mut persisted = Vec::with_capacity(values.len());
    let mut identities = Vec::with_capacity(values.len());
    for (path, digest, identity) in values {
        persisted.push((path, digest));
        identities.push(identity);
    }
    Ok((persisted, identities))
}

fn canonical_regular_file(path: &Path) -> Result<PathBuf, EmbeddingRuntimeError> {
    if !path.is_absolute() {
        return Err(EmbeddingRuntimeError::InvalidConfig);
    }
    let canonical = fs::canonicalize(path)?;
    if !canonical.is_absolute()
        || !canonical.is_file()
        || canonical.to_str().is_none()
        || path_string(&canonical).len() > MAX_PATH_BYTES
    {
        return Err(EmbeddingRuntimeError::InvalidConfig);
    }
    Ok(canonical)
}

fn validate_config_path(path: &Path) -> Result<(), EmbeddingRuntimeError> {
    if !path.is_absolute() || path.file_name().is_none() || path.parent().is_none() {
        return Err(EmbeddingRuntimeError::InvalidConfig);
    }
    if path.parent().is_some_and(|parent| !parent.is_dir()) {
        return Err(EmbeddingRuntimeError::InvalidConfig);
    }
    Ok(())
}

fn encode_limits(bytes: &mut Vec<u8>, limits: EmbeddingRuntimeLimits) {
    for value in [
        limits.stdout_bytes,
        limits.stderr_bytes,
        limits.output_bytes,
        limits.input_bytes,
        limits.wall_time_ms,
        limits.workspace_bytes,
        limits.process_count,
        limits.memory_bytes.unwrap_or(0),
        limits.cpu_time_ms.unwrap_or(0),
        limits.resident_credit_bytes,
    ] {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
}

fn decode_limits(reader: &mut Reader<'_>) -> Result<EmbeddingRuntimeLimits, EmbeddingRuntimeError> {
    let stdout_bytes = reader.u64()?;
    let stderr_bytes = reader.u64()?;
    let output_bytes = reader.u64()?;
    let input_bytes = reader.u64()?;
    let wall_time_ms = reader.u64()?;
    let workspace_bytes = reader.u64()?;
    let process_count = reader.u64()?;
    let memory_bytes = match reader.u64()? {
        0 => None,
        value => Some(value),
    };
    let cpu_time_ms = match reader.u64()? {
        0 => None,
        value => Some(value),
    };
    let resident_credit_bytes = reader.u64()?;
    Ok(EmbeddingRuntimeLimits {
        stdout_bytes,
        stderr_bytes,
        output_bytes,
        input_bytes,
        wall_time_ms,
        workspace_bytes,
        process_count,
        memory_bytes,
        cpu_time_ms,
        resident_credit_bytes,
    })
}

fn to_usize(value: u64) -> Result<usize, EmbeddingRuntimeError> {
    usize::try_from(value).map_err(|_| EmbeddingRuntimeError::InvalidConfig)
}

fn put_count(
    bytes: &mut Vec<u8>,
    count: usize,
    maximum: usize,
) -> Result<(), EmbeddingRuntimeError> {
    if count > maximum {
        return Err(EmbeddingRuntimeError::InvalidConfig);
    }
    bytes.extend_from_slice(
        &u16::try_from(count)
            .map_err(|_| EmbeddingRuntimeError::InvalidConfig)?
            .to_be_bytes(),
    );
    Ok(())
}

fn put_string(bytes: &mut Vec<u8>, path: &Path) -> Result<(), EmbeddingRuntimeError> {
    let value = path.to_str().ok_or(EmbeddingRuntimeError::InvalidConfig)?;
    put_utf8(bytes, value, MAX_PATH_BYTES)
}

fn put_utf8(bytes: &mut Vec<u8>, value: &str, maximum: usize) -> Result<(), EmbeddingRuntimeError> {
    if value.len() > maximum || value.as_bytes().contains(&0) {
        return Err(EmbeddingRuntimeError::InvalidConfig);
    }
    bytes.extend_from_slice(
        &u16::try_from(value.len())
            .map_err(|_| EmbeddingRuntimeError::InvalidConfig)?
            .to_be_bytes(),
    );
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

fn path_string(path: &Path) -> &str {
    path.to_str().unwrap_or("")
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], EmbeddingRuntimeError> {
        let end = self
            .cursor
            .checked_add(length)
            .ok_or(EmbeddingRuntimeError::Corrupt)?;
        let value = self
            .bytes
            .get(self.cursor..end)
            .ok_or(EmbeddingRuntimeError::Corrupt)?;
        self.cursor = end;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], EmbeddingRuntimeError> {
        self.take(N)?
            .try_into()
            .map_err(|_| EmbeddingRuntimeError::Corrupt)
    }

    fn u8(&mut self) -> Result<u8, EmbeddingRuntimeError> {
        Ok(self.take(1)?[0])
    }

    fn u64(&mut self) -> Result<u64, EmbeddingRuntimeError> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    fn count(&mut self, maximum: usize) -> Result<usize, EmbeddingRuntimeError> {
        let count = usize::from(u16::from_be_bytes(self.array()?));
        if count > maximum {
            return Err(EmbeddingRuntimeError::Corrupt);
        }
        Ok(count)
    }

    fn string(&mut self, maximum: usize) -> Result<&'a str, EmbeddingRuntimeError> {
        let length = usize::from(u16::from_be_bytes(self.array()?));
        if length > maximum {
            return Err(EmbeddingRuntimeError::Corrupt);
        }
        let value =
            std::str::from_utf8(self.take(length)?).map_err(|_| EmbeddingRuntimeError::Corrupt)?;
        if value.as_bytes().contains(&0) {
            return Err(EmbeddingRuntimeError::Corrupt);
        }
        Ok(value)
    }

    fn finish(self) -> Result<(), EmbeddingRuntimeError> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(EmbeddingRuntimeError::Corrupt)
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    use std::os::unix::fs::PermissionsExt;

    fn private_dir(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "embedding-runtime-{name}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create private fixture directory");
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("restrict fixture directory");
        path
    }

    fn install_protocol_fixture(
        directory: &Path,
        requirement: EmbeddingRequirement,
    ) -> Result<(PathBuf, EmbeddingRuntimeSpecV1), EmbeddingRuntimeError> {
        // This executable verifies the persisted-artifact paths and BEM1/BEC1 activation
        // boundary. Its fixed vector is a protocol fixture, not a pretrained model.
        let program = directory.join("embedding-protocol-fixture.sh");
        fs::write(
            &program,
            b"#!/bin/sh\ntest -f \"$BACKEND_EMBEDDING_MODEL_FILE\" || exit 4\ntest -f \"$BACKEND_EMBEDDING_TOKENIZER_FILE\" || exit 5\nwhile IFS= read -r line || [ -n \"$line\" ]; do :; done\nprintf '\\102\\105\\103\\061\\000\\002\\000\\000\\200\\077\\000\\000\\000\\000'\n",
        )?;
        #[cfg(unix)]
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
        let limits = EmbeddingRuntimeLimits {
            stdout_bytes: 64,
            stderr_bytes: 64,
            output_bytes: 128,
            input_bytes: 512,
            wall_time_ms: 2_000,
            workspace_bytes: 1024 * 1024,
            process_count: 2,
            memory_bytes: None,
            cpu_time_ms: Some(2_000),
            resident_credit_bytes: 1024 * 1024,
        };
        let config = directory.join("embedding.config");
        let spec = install_embedding_runtime(
            &config,
            EmbeddingRuntimeInstall {
                requirement,
                program,
                arguments: Vec::new(),
                dependencies: Vec::new(),
                environment: Vec::new(),
                limits,
                model_bytes: b"exact-model-artifact".to_vec(),
                tokenizer_bytes: b"exact-tokenizer-artifact".to_vec(),
                dimension: NonZeroU16::new(2).expect("nonzero dimension"),
                normalization: EmbeddingNormalization::L2,
                maximum_text_bytes: NonZeroU32::new(128).expect("nonzero text bound"),
                options_digest: [7; 32],
            },
        )?;
        Ok((config, spec))
    }

    #[test]
    fn absent_config_is_unconfigured_optional() -> Result<(), Box<dyn std::error::Error>> {
        let directory = private_dir("unconfigured");
        let provision = EmbeddingRuntimeProvision::open(directory.join("embedding.config"))?;
        assert_eq!(provision.requirement(), EmbeddingRequirement::Optional);
        assert_eq!(provision.status(), EmbeddingRuntimeStatus::NotConfigured);
        assert!(provision.runtime().is_none());
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn protocol_fixture_cold_reopens_exact_spec_and_artifacts()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = private_dir("reopen");
        let (config, expected) =
            install_protocol_fixture(&directory, EmbeddingRequirement::Optional)?;
        let loaded = EmbeddingRuntimeProvision::open(&config)?;
        assert_eq!(loaded.requirement(), EmbeddingRequirement::Optional);
        assert!(
            matches!(loaded.status(), EmbeddingRuntimeStatus::Available { recipe } if recipe == expected.recipe_identity()),
            "cold-open activation status: {:?}",
            loaded.status()
        );
        let runtime = loaded.runtime().ok_or("active executable missing")?;
        assert_eq!(runtime.execution_identity().model(), expected.model());
        assert_eq!(
            runtime.execution_identity().tokenizer(),
            expected.tokenizer()
        );
        assert_eq!(runtime.execution_identity().dimension(), 2);
        assert_eq!(
            runtime.execution_identity().recipe(),
            expected.recipe_identity()
        );
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn optional_missing_or_changed_artifacts_are_typed_unavailable_after_cold_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let missing_dir = private_dir("missing");
        let (missing_config, spec) =
            install_protocol_fixture(&missing_dir, EmbeddingRequirement::Optional)?;
        backend_platform::durable::remove_private(&artifact_path(
            &missing_config,
            spec.model(),
            "model",
        ))?;
        let missing = EmbeddingRuntimeProvision::open(&missing_config)?;
        assert_eq!(
            missing.status(),
            EmbeddingRuntimeStatus::Unavailable(
                EmbeddingUnavailableReason::ModelArtifactUnavailable
            )
        );
        fs::remove_dir_all(missing_dir)?;

        let changed_dir = private_dir("changed");
        let (changed_config, spec) =
            install_protocol_fixture(&changed_dir, EmbeddingRequirement::Optional)?;
        backend_platform::durable::write_private_atomic(
            &artifact_path(&changed_config, spec.tokenizer(), "tokenizer"),
            b"Exact-tokenizer-artifact",
        )?;
        let changed = EmbeddingRuntimeProvision::open(&changed_config)?;
        assert_eq!(
            changed.status(),
            EmbeddingRuntimeStatus::Unavailable(
                EmbeddingUnavailableReason::ArtifactIdentityMismatch
            )
        );
        fs::remove_dir_all(changed_dir)?;
        Ok(())
    }

    #[test]
    fn required_changed_executable_is_rejected_after_cold_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = private_dir("executable-drift");
        let (config, _) = install_protocol_fixture(&directory, EmbeddingRequirement::Required)?;
        let decoded = read_private_config(&config)?.ok_or("config missing")?;
        fs::write(&decoded.program, b"#!/bin/sh\nexit 9\n")?;
        let reopened = EmbeddingRuntimeProvision::open(&config)?;
        assert_eq!(reopened.requirement(), EmbeddingRequirement::Required);
        assert_eq!(
            reopened.status(),
            EmbeddingRuntimeStatus::Unavailable(
                EmbeddingUnavailableReason::ArtifactIdentityMismatch
            )
        );
        assert_eq!(
            reopened.provisioning_failure(),
            Some(EmbeddingProvisioningFailure::ArtifactIdentityMismatch)
        );
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn required_tampered_tokenizer_is_rejected_after_cold_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = private_dir("required-tokenizer-drift");
        let (config, spec) = install_protocol_fixture(&directory, EmbeddingRequirement::Required)?;
        backend_platform::durable::write_private_atomic(
            &artifact_path(&config, spec.tokenizer(), "tokenizer"),
            b"Exact-tokenizer-artifact",
        )?;
        let reopened = EmbeddingRuntimeProvision::open(&config)?;
        assert_eq!(reopened.requirement(), EmbeddingRequirement::Required);
        assert_eq!(
            reopened.status(),
            EmbeddingRuntimeStatus::Unavailable(
                EmbeddingUnavailableReason::ArtifactIdentityMismatch
            )
        );
        assert_eq!(
            reopened.provisioning_failure(),
            Some(EmbeddingProvisioningFailure::ArtifactIdentityMismatch)
        );
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn same_length_config_corruption_is_rejected_on_cold_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = private_dir("corrupt-config");
        let (config, _) = install_protocol_fixture(&directory, EmbeddingRequirement::Optional)?;
        let mut bytes = fs::read(&config)?;
        let changed_at = CONFIG_MAGIC.len() + 8;
        bytes[changed_at] ^= 1;
        backend_platform::durable::write_private_atomic(&config, &bytes)?;
        assert!(matches!(
            EmbeddingRuntimeProvision::open(&config),
            Err(EmbeddingRuntimeError::Corrupt)
        ));
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn required_missing_artifact_preserves_typed_required_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = private_dir("required-missing");
        let (config, spec) = install_protocol_fixture(&directory, EmbeddingRequirement::Required)?;
        backend_platform::durable::remove_private(&artifact_path(&config, spec.model(), "model"))?;
        let reopened = EmbeddingRuntimeProvision::open(&config)?;
        assert_eq!(reopened.requirement(), EmbeddingRequirement::Required);
        assert_eq!(
            reopened.status(),
            EmbeddingRuntimeStatus::Unavailable(
                EmbeddingUnavailableReason::ModelArtifactUnavailable
            )
        );
        assert_eq!(
            reopened.provisioning_failure(),
            Some(EmbeddingProvisioningFailure::ModelUnavailable)
        );
        fs::remove_dir_all(directory)?;
        Ok(())
    }
}
