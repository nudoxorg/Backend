//! Bounded cold-process protocol for an external embedding model runtime.
//!
//! This adapter owns immutable model and tokenizer bytes plus a content-checked executable. Every
//! inference is supervised under explicit input, output, deadline, process, memory, and workspace
//! limits. A runtime becomes active only after the executable answers a real self-test request.

use crate::embedding_cache::EmbeddingCacheSession;
use crate::supervisor::RunningByteSession;
use crate::{
    ProcessEnvironment, ProcessError, ProcessLimits, ProcessStdin, ProcessSupervisor,
    ProcessTerminal, ProtocolDescriptor, SupervisedCommand, ToolchainArtifact,
};
use std::{
    collections::{HashMap, VecDeque},
    fmt,
    fs::File,
    io::{self, Read},
    num::{NonZeroU16, NonZeroU32},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, TryLockError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

const REQUEST_MAGIC: &[u8; 4] = b"BEM1";
const RESPONSE_MAGIC: &[u8; 4] = b"BEC1";
const BATCH_REQUEST_MAGIC: &[u8; 4] = b"BEM2";
const BATCH_RESPONSE_MAGIC: &[u8; 4] = b"BEC2";
const REQUEST_HEADER_BYTES: usize = 4 + 1 + 1 + 2 + 32 + 32 + 32 + 4;
const RESPONSE_HEADER_BYTES: usize = 4 + 2;
const BATCH_RESPONSE_HEADER_BYTES: usize = 4 + 2 + 4;
const BATCH_ITEM_HEADER_BYTES: usize = 32 + 4;
const BATCH_RESPONSE_ITEM_HEADER_BYTES: usize = 32;
/// Maximum unique texts admitted to one external embedding batch.
pub const MAX_EMBEDDING_BATCH_ITEMS: usize = 256;
/// Maximum original inputs accepted by one call; duplicates are folded before dispatch.
pub const MAX_EMBEDDING_BATCH_INPUTS: usize = 65_536;
/// Maximum aggregate UTF-8 payload bytes admitted to one API batch before identity hashing.
pub const MAX_EMBEDDING_BATCH_INPUT_BYTES: usize = 512 * 1024 * 1024;
/// Maximum decoded float32 coordinates retained by one batch call.
pub const MAX_EMBEDDING_BATCH_COORDINATE_BYTES: usize = 64 * 1024 * 1024;
/// Conservative allowance for bounded per-input batch and output metadata.
pub const MAX_EMBEDDING_BATCH_METADATA_BYTES: usize = 32 * 1024 * 1024;
/// Maximum float32 coordinate storage retained by the exact-input cache.
pub const MAX_EMBEDDING_CACHE_COORDINATE_BYTES: usize = 8 * 1024 * 1024;
const RUNTIME_SPEC_MAGIC: &[u8; 4] = b"BERS";
const RUNTIME_SPEC_VERSION: u8 = 1;
const RUNTIME_SPEC_BYTES: usize = 4 + 1 + 32 + 32 + 32 + 32 + 2 + 1 + 4 + 32;
const MODEL_FILE_ENV: &str = "BACKEND_EMBEDDING_MODEL_FILE";
const TOKENIZER_FILE_ENV: &str = "BACKEND_EMBEDDING_TOKENIZER_FILE";
const MODEL_FILE_NAME: &str = "model.bin";
const TOKENIZER_FILE_NAME: &str = "tokenizer.bin";

/// Maximum model bytes materialized into the private inference workspace.
pub const MAX_EMBEDDING_MODEL_BYTES: usize = 256 * 1024 * 1024;
/// Maximum tokenizer bytes materialized into the private inference workspace.
pub const MAX_EMBEDDING_TOKENIZER_BYTES: usize = 64 * 1024 * 1024;

static ARTIFACT_WORKSPACE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// One shared, finite-wait admission gate for all callers of one activated executable.
///
/// The compiler may have multiple native lanes, but one heavy embedding model/process runs at a
/// time by default. The lock protects only this permit bit; it is never held while a child runs.
struct InferenceAdmissionGate {
    occupied: Mutex<bool>,
    available: Condvar,
}

impl InferenceAdmissionGate {
    fn new() -> Self {
        Self {
            occupied: Mutex::new(false),
            available: Condvar::new(),
        }
    }

    fn acquire(
        &self,
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<InferenceAdmissionPermit<'_>, EmbeddingExecutableError> {
        check_request_deadline(cancelled, deadline)?;
        let mut occupied = lock_mutex_until(&self.occupied, cancelled, deadline)?;
        while *occupied {
            check_request_deadline(cancelled, deadline)?;
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(EmbeddingExecutableError::Process(ProcessError::Deadline));
            };
            if remaining.is_zero() {
                return Err(EmbeddingExecutableError::Process(ProcessError::Deadline));
            }
            let (next, timeout) = self
                .available
                .wait_timeout(occupied, remaining.min(Duration::from_millis(10)))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            occupied = next;
            check_request_deadline(cancelled, deadline)?;
            if timeout.timed_out() && Instant::now() >= deadline {
                return Err(EmbeddingExecutableError::Process(ProcessError::Deadline));
            }
        }
        *occupied = true;
        Ok(InferenceAdmissionPermit { gate: self })
    }
}

struct InferenceAdmissionPermit<'gate> {
    gate: &'gate InferenceAdmissionGate,
}

impl Drop for InferenceAdmissionPermit<'_> {
    fn drop(&mut self) {
        let mut occupied = self
            .gate
            .occupied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *occupied = false;
        self.gate.available.notify_one();
    }
}

/// Identity of exact immutable model or tokenizer bytes.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EmbeddingArtifactId([u8; 32]);

impl EmbeddingArtifactId {
    /// Returns the full artifact digest.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Immutable resident model/tokenizer artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbeddingArtifact {
    identity: EmbeddingArtifactId,
    bytes: Arc<[u8]>,
}

impl EmbeddingArtifact {
    /// Owns immutable bytes and derives their exact artifact identity.
    #[must_use]
    pub fn new(bytes: Arc<[u8]>) -> Self {
        let identity = EmbeddingArtifactId(*blake3::hash(&bytes).as_bytes());
        Self { identity, bytes }
    }

    /// Exact byte identity.
    #[must_use]
    pub const fn identity(&self) -> EmbeddingArtifactId {
        self.identity
    }

    /// Immutable bytes retained for the active runtime lifetime.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Portable, untrusted claim describing one embedding runtime configuration.
///
/// Decoding this value does not admit model or executable authority. Owners must activate the
/// runtime from their verified artifacts and compare the resulting
/// [`EmbeddingExecutionIdentity`] with [`Self::validate_execution_identity`]. Local paths,
/// temporary artifact paths, arguments, and process environment values are deliberately excluded
/// from this portable claim. The activated execution identity hashes executable/cwd paths, exact
/// arguments, explicit environment (excluding injected private artifact paths), and process limits
/// so changed launch settings cannot alias in input caches or compiler embedding-plane identities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmbeddingRuntimeSpecV1 {
    model: [u8; 32],
    model_version: [u8; 32],
    tokenizer: [u8; 32],
    executable: [u8; 32],
    dimension: NonZeroU16,
    normalization: EmbeddingNormalization,
    maximum_text_bytes: NonZeroU32,
    options_digest: [u8; 32],
}

impl EmbeddingRuntimeSpecV1 {
    /// Creates a portable runtime claim from exact artifact identities and explicit options.
    #[must_use]
    pub const fn new(
        model: [u8; 32],
        model_version: [u8; 32],
        tokenizer: [u8; 32],
        executable: [u8; 32],
        dimension: NonZeroU16,
        normalization: EmbeddingNormalization,
        maximum_text_bytes: NonZeroU32,
        options_digest: [u8; 32],
    ) -> Self {
        Self {
            model,
            model_version,
            tokenizer,
            executable,
            dimension,
            normalization,
            maximum_text_bytes,
            options_digest,
        }
    }

    /// Returns the claimed model payload identity.
    #[must_use]
    pub const fn model(self) -> [u8; 32] {
        self.model
    }

    /// Returns the declared immutable model version.
    #[must_use]
    pub const fn model_version(self) -> [u8; 32] {
        self.model_version
    }

    /// Returns the claimed tokenizer payload identity.
    #[must_use]
    pub const fn tokenizer(self) -> [u8; 32] {
        self.tokenizer
    }

    /// Returns the claimed executable and declared toolchain-closure identity.
    #[must_use]
    pub const fn executable(self) -> [u8; 32] {
        self.executable
    }

    /// Returns the fixed vector dimension.
    #[must_use]
    pub const fn dimension(self) -> NonZeroU16 {
        self.dimension
    }

    /// Returns the required coordinate normalization.
    #[must_use]
    pub const fn normalization(self) -> EmbeddingNormalization {
        self.normalization
    }

    /// Returns the maximum UTF-8 text bytes accepted by one request.
    #[must_use]
    pub const fn maximum_text_bytes(self) -> NonZeroU32 {
        self.maximum_text_bytes
    }

    /// Returns the digest of portable model options and treatments.
    #[must_use]
    pub const fn options_digest(self) -> [u8; 32] {
        self.options_digest
    }

    /// Returns the recipe identity derived from these exact canonical fields.
    #[must_use]
    pub fn recipe_identity(self) -> [u8; 32] {
        let mut hasher =
            blake3::Hasher::new_derive_key("backend.compile.embedding-runtime-recipe.v1");
        hasher.update(&self.canonical_bytes());
        *hasher.finalize().as_bytes()
    }

    /// Encodes the exact fixed-width `BERS` version-one wire representation.
    #[must_use]
    pub fn canonical_bytes(self) -> [u8; RUNTIME_SPEC_BYTES] {
        let mut bytes = [0; RUNTIME_SPEC_BYTES];
        bytes[..4].copy_from_slice(RUNTIME_SPEC_MAGIC);
        bytes[4] = RUNTIME_SPEC_VERSION;
        let mut offset = 5;
        for identity in [
            self.model,
            self.model_version,
            self.tokenizer,
            self.executable,
        ] {
            bytes[offset..offset + 32].copy_from_slice(&identity);
            offset += 32;
        }
        bytes[offset..offset + 2].copy_from_slice(&self.dimension.get().to_be_bytes());
        offset += 2;
        bytes[offset] = match self.normalization {
            EmbeddingNormalization::None => 0,
            EmbeddingNormalization::L2 => 1,
        };
        offset += 1;
        bytes[offset..offset + 4].copy_from_slice(&self.maximum_text_bytes.get().to_be_bytes());
        offset += 4;
        bytes[offset..offset + 32].copy_from_slice(&self.options_digest);
        bytes
    }

    /// Decodes a canonical claim without treating its identities as admitted artifacts.
    ///
    /// # Errors
    ///
    /// Returns a precise framing or field error for noncanonical bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, EmbeddingRuntimeSpecError> {
        if bytes.len() != RUNTIME_SPEC_BYTES {
            return Err(EmbeddingRuntimeSpecError::Length {
                expected: RUNTIME_SPEC_BYTES,
                observed: bytes.len(),
            });
        }
        if &bytes[..4] != RUNTIME_SPEC_MAGIC {
            return Err(EmbeddingRuntimeSpecError::Magic);
        }
        if bytes[4] != RUNTIME_SPEC_VERSION {
            return Err(EmbeddingRuntimeSpecError::Version(bytes[4]));
        }
        let mut offset = 5;
        let mut read_identity = || {
            let identity = bytes[offset..offset + 32].try_into().map_err(|_| {
                EmbeddingRuntimeSpecError::Length {
                    expected: RUNTIME_SPEC_BYTES,
                    observed: bytes.len(),
                }
            })?;
            offset += 32;
            Ok::<[u8; 32], EmbeddingRuntimeSpecError>(identity)
        };
        let model = read_identity()?;
        let model_version = read_identity()?;
        let tokenizer = read_identity()?;
        let executable = read_identity()?;
        let dimension = NonZeroU16::new(u16::from_be_bytes([bytes[offset], bytes[offset + 1]]))
            .ok_or(EmbeddingRuntimeSpecError::ZeroDimension)?;
        offset += 2;
        let normalization = match bytes[offset] {
            0 => EmbeddingNormalization::None,
            1 => EmbeddingNormalization::L2,
            value => return Err(EmbeddingRuntimeSpecError::Normalization(value)),
        };
        offset += 1;
        let maximum_text_bytes = NonZeroU32::new(u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]))
        .ok_or(EmbeddingRuntimeSpecError::ZeroTextLimit)?;
        offset += 4;
        let options_digest = bytes[offset..offset + 32].try_into().map_err(|_| {
            EmbeddingRuntimeSpecError::Length {
                expected: RUNTIME_SPEC_BYTES,
                observed: bytes.len(),
            }
        })?;
        Ok(Self::new(
            model,
            model_version,
            tokenizer,
            executable,
            dimension,
            normalization,
            maximum_text_bytes,
            options_digest,
        ))
    }

    /// Checks every claim against an identity observed from an activated executable.
    ///
    /// # Errors
    ///
    /// Returns the first exact identity field that differs.
    pub fn validate_execution_identity(
        self,
        observed: EmbeddingExecutionIdentity,
    ) -> Result<(), EmbeddingRuntimeSpecError> {
        for (field, matches) in [
            ("model", self.model == observed.model),
            (
                "model_version",
                self.model_version == observed.model_version,
            ),
            ("tokenizer", self.tokenizer == observed.tokenizer),
            ("executable", self.executable == observed.executable),
            (
                "dimension",
                u32::from(self.dimension.get()) == observed.dimension,
            ),
            (
                "normalization",
                self.normalization == observed.normalization,
            ),
            (
                "maximum_text_bytes",
                self.maximum_text_bytes.get() == observed.maximum_text_bytes,
            ),
            (
                "options_digest",
                self.options_digest == observed.options_digest,
            ),
            ("recipe", self.recipe_identity() == observed.recipe),
        ] {
            if !matches {
                return Err(EmbeddingRuntimeSpecError::ObservedMismatch { field });
            }
        }
        Ok(())
    }
}

/// Rejection while decoding or matching an embedding runtime claim.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum EmbeddingRuntimeSpecError {
    /// Canonical byte extent is not exact.
    #[error("embedding runtime spec length is {observed}, expected {expected}")]
    Length {
        /// Required fixed-width extent.
        expected: usize,
        /// Observed input extent.
        observed: usize,
    },
    /// Magic bytes do not identify this spec format.
    #[error("embedding runtime spec magic is invalid")]
    Magic,
    /// Wire version is unsupported.
    #[error("embedding runtime spec version {0} is unsupported")]
    Version(u8),
    /// Dimension must be nonzero.
    #[error("embedding runtime spec dimension is zero")]
    ZeroDimension,
    /// Maximum UTF-8 request bytes must be nonzero.
    #[error("embedding runtime spec text bound is zero")]
    ZeroTextLimit,
    /// Normalization tag is not canonical.
    #[error("embedding runtime spec normalization tag {0} is invalid")]
    Normalization(u8),
    /// One field differs from the activated runtime.
    #[error("activated embedding runtime differs from spec field {field}")]
    ObservedMismatch {
        /// Exact field that did not match.
        field: &'static str,
    },
}

/// Query and indexed-document inputs are different model tasks.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EmbeddingPurpose {
    /// Search query embedding.
    Query,
    /// Indexed document/code embedding.
    Document,
}

/// Output normalization required by the recipe.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EmbeddingNormalization {
    /// Preserve finite model output.
    None,
    /// Require a unit-length vector within a fixed numeric tolerance.
    L2,
}

/// One bounded external inference request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmbeddingInvocation<'text> {
    /// Query/document task treatment.
    pub purpose: EmbeddingPurpose,
    /// UTF-8 input bytes.
    pub text: &'text str,
}

/// Content identity for one exact model task and input payload.
///
/// The digest commits the complete activated execution identity, query/document purpose, and
/// exact UTF-8 input bytes. It is suitable for bounded in-memory reuse; durable cache entries
/// still need to be published as versioned semantic objects.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EmbeddingInputIdentity([u8; 32]);

impl EmbeddingInputIdentity {
    /// Derives identity from the full runtime configuration and exact invocation payload.
    #[must_use]
    pub fn new(execution: EmbeddingExecutionIdentity, invocation: EmbeddingInvocation<'_>) -> Self {
        let mut configuration =
            blake3::Hasher::new_derive_key("backend.compile.embedding-configuration.v1");
        configuration.update(&execution.model);
        configuration.update(&execution.model_version);
        configuration.update(&execution.tokenizer);
        configuration.update(&execution.executable);
        configuration.update(&execution.recipe);
        configuration.update(&execution.dimension.to_be_bytes());
        configuration.update(&execution.maximum_text_bytes.to_be_bytes());
        configuration.update(&execution.options_digest);
        configuration.update(&execution.launch_configuration);
        configuration.update(&[match execution.normalization {
            EmbeddingNormalization::None => 0,
            EmbeddingNormalization::L2 => 1,
        }]);
        Self::for_configuration(*configuration.finalize().as_bytes(), invocation)
    }

    fn new_until(
        execution: EmbeddingExecutionIdentity,
        invocation: EmbeddingInvocation<'_>,
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<Self, EmbeddingExecutableError> {
        let mut configuration =
            blake3::Hasher::new_derive_key("backend.compile.embedding-configuration.v1");
        configuration.update(&execution.model);
        configuration.update(&execution.model_version);
        configuration.update(&execution.tokenizer);
        configuration.update(&execution.executable);
        configuration.update(&execution.recipe);
        configuration.update(&execution.dimension.to_be_bytes());
        configuration.update(&execution.maximum_text_bytes.to_be_bytes());
        configuration.update(&execution.options_digest);
        configuration.update(&execution.launch_configuration);
        configuration.update(&[match execution.normalization {
            EmbeddingNormalization::None => 0,
            EmbeddingNormalization::L2 => 1,
        }]);
        Self::for_configuration_until(
            *configuration.finalize().as_bytes(),
            invocation,
            cancelled,
            deadline,
        )
    }

    fn for_configuration_until(
        configuration: [u8; 32],
        invocation: EmbeddingInvocation<'_>,
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<Self, EmbeddingExecutableError> {
        let mut hasher = blake3::Hasher::new_derive_key("backend.compile.embedding-input.v1");
        hasher.update(&configuration);
        hasher.update(&[match invocation.purpose {
            EmbeddingPurpose::Query => 1,
            EmbeddingPurpose::Document => 2,
        }]);
        for chunk in invocation.text.as_bytes().chunks(64 * 1024) {
            check_request_deadline(cancelled, deadline)?;
            hasher.update(chunk);
        }
        check_request_deadline(cancelled, deadline)?;
        Ok(Self(*hasher.finalize().as_bytes()))
    }

    /// Derives identity for another owner that has already committed its exact producer recipe.
    #[must_use]
    pub fn for_configuration(configuration: [u8; 32], invocation: EmbeddingInvocation<'_>) -> Self {
        let mut hasher = blake3::Hasher::new_derive_key("backend.compile.embedding-input.v1");
        hasher.update(&configuration);
        hasher.update(&[match invocation.purpose {
            EmbeddingPurpose::Query => 1,
            EmbeddingPurpose::Document => 2,
        }]);
        hasher.update(invocation.text.as_bytes());
        Self(*hasher.finalize().as_bytes())
    }

    /// Full input identity digest.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Protocol selected after the activation self-test.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmbeddingBatchProtocol {
    /// BEM2/BEC2 accepted a real bounded two-item batch self-test.
    BatchV2,
    /// BEM2/BEC2 passed a two-turn self-test on one retained supervised process.
    PersistentBatchV2,
    /// The executable supports only the original BEM1/BEC1 one-input protocol.
    SingleV1,
}

/// Typed coordinates tied to the exact recipe, model, tokenizer, and task.
#[derive(Clone, Debug, PartialEq)]
pub struct EmbeddingCoordinates {
    recipe: [u8; 32],
    model: EmbeddingArtifactId,
    tokenizer: EmbeddingArtifactId,
    purpose: EmbeddingPurpose,
    normalization: EmbeddingNormalization,
    values: Arc<[f32]>,
}

#[derive(Clone)]
pub(crate) struct ValidatedProducerVector {
    identity: EmbeddingInputIdentity,
    values: Arc<[f32]>,
}

impl ValidatedProducerVector {
    fn from_decoded(identity: EmbeddingInputIdentity, coordinates: &EmbeddingCoordinates) -> Self {
        Self {
            identity,
            values: Arc::clone(&coordinates.values),
        }
    }

    pub(crate) const fn identity(&self) -> EmbeddingInputIdentity {
        self.identity
    }

    pub(crate) fn values(&self) -> &[f32] {
        &self.values
    }
}

/// Identity of one activated embedding invocation recipe and its exact launch policy.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EmbeddingExecutionIdentity {
    model: [u8; 32],
    model_version: [u8; 32],
    tokenizer: [u8; 32],
    dimension: u32,
    normalization: EmbeddingNormalization,
    executable: [u8; 32],
    recipe: [u8; 32],
    maximum_text_bytes: u32,
    options_digest: [u8; 32],
    launch_configuration: [u8; 32],
}

impl EmbeddingExecutionIdentity {
    /// Exact resident model bytes.
    #[must_use]
    pub const fn model(self) -> [u8; 32] {
        self.model
    }

    /// Configured model release/version identity bound to the exact resident model bytes by the
    /// activated runtime recipe.
    #[must_use]
    pub const fn model_version(self) -> [u8; 32] {
        self.model_version
    }

    /// Exact resident tokenizer bytes.
    #[must_use]
    pub const fn tokenizer(self) -> [u8; 32] {
        self.tokenizer
    }

    /// Fixed output dimension.
    #[must_use]
    pub const fn dimension(self) -> u32 {
        self.dimension
    }

    /// Validated vector normalization.
    #[must_use]
    pub const fn normalization(self) -> EmbeddingNormalization {
        self.normalization
    }

    /// Exact executable plus declared dependency closure.
    #[must_use]
    pub const fn executable(self) -> [u8; 32] {
        self.executable
    }

    /// Portable recipe identity supplied at activation.
    #[must_use]
    pub const fn recipe(self) -> [u8; 32] {
        self.recipe
    }

    /// Maximum request text bound observed from the activated runtime.
    #[must_use]
    pub const fn maximum_text_bytes(self) -> u32 {
        self.maximum_text_bytes
    }

    /// Portable semantic options bound observed from the activated runtime.
    #[must_use]
    pub const fn options_digest(self) -> [u8; 32] {
        self.options_digest
    }

    /// Digest of executable/cwd paths, exact arguments, explicit output-affecting environment
    /// values, and process limits.
    ///
    /// The owner-private model and tokenizer paths injected into the child environment are
    /// excluded; their immutable bytes already have separate identities.
    #[must_use]
    pub const fn launch_configuration(self) -> [u8; 32] {
        self.launch_configuration
    }
}

struct EmbeddingArtifactWorkspace {
    name: String,
    model_path: PathBuf,
    tokenizer_path: PathBuf,
    model_length: usize,
    tokenizer_length: usize,
    parent: backend_platform::DirectoryCapability,
    directory_handle: backend_platform::DirectoryCapability,
}

impl EmbeddingArtifactWorkspace {
    fn create(
        workspace: &Path,
        model: &EmbeddingArtifact,
        tokenizer: &EmbeddingArtifact,
        deadline: Instant,
    ) -> Result<Self, EmbeddingExecutableError> {
        check_request_deadline(None, deadline)?;
        if model.bytes().len() > MAX_EMBEDDING_MODEL_BYTES {
            return Err(EmbeddingExecutableError::ArtifactLimit {
                artifact: "model",
                observed: model.bytes().len(),
                maximum: MAX_EMBEDDING_MODEL_BYTES,
            });
        }
        if tokenizer.bytes().len() > MAX_EMBEDDING_TOKENIZER_BYTES {
            return Err(EmbeddingExecutableError::ArtifactLimit {
                artifact: "tokenizer",
                observed: tokenizer.bytes().len(),
                maximum: MAX_EMBEDDING_TOKENIZER_BYTES,
            });
        }

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| {
                EmbeddingExecutableError::ArtifactWorkspace(io::Error::other(
                    "system time precedes Unix epoch",
                ))
            })?
            .as_nanos();
        let sequence = ARTIFACT_WORKSPACE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            ".embedding-runtime-{}-{stamp}-{sequence}",
            std::process::id()
        );
        let directory = workspace.join(&name);

        check_request_deadline(None, deadline)?;
        let parent = backend_platform::DirectoryCapability::open(workspace)
            .map_err(EmbeddingExecutableError::ArtifactWorkspace)?;
        let directory_handle = parent
            .create_private_dir(&name)
            .map_err(EmbeddingExecutableError::ArtifactWorkspace)?;

        let artifacts = Self {
            name,
            model_path: directory.join(MODEL_FILE_NAME),
            tokenizer_path: directory.join(TOKENIZER_FILE_NAME),
            model_length: model.bytes().len(),
            tokenizer_length: tokenizer.bytes().len(),
            parent,
            directory_handle,
        };
        check_request_deadline(None, deadline)?;
        artifacts.write_artifact(MODEL_FILE_NAME, model.bytes(), deadline)?;
        check_request_deadline(None, deadline)?;
        artifacts.write_artifact(TOKENIZER_FILE_NAME, tokenizer.bytes(), deadline)?;
        artifacts.verify_artifact(
            MODEL_FILE_NAME,
            model.identity,
            model.bytes().len(),
            None,
            deadline,
        )?;
        artifacts.verify_artifact(
            TOKENIZER_FILE_NAME,
            tokenizer.identity,
            tokenizer.bytes().len(),
            None,
            deadline,
        )?;
        Ok(artifacts)
    }

    fn write_artifact(
        &self,
        name: &str,
        bytes: &[u8],
        deadline: Instant,
    ) -> Result<(), EmbeddingExecutableError> {
        write_private_artifact_atomic(
            &self.directory_handle,
            name,
            bytes,
            deadline,
            |file, chunk| {
                use std::io::Write as _;
                file.write_all(chunk)
            },
        )
    }

    fn verify_until(
        &self,
        model: EmbeddingArtifactId,
        tokenizer: EmbeddingArtifactId,
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<(), EmbeddingExecutableError> {
        check_request_deadline(cancelled, deadline)?;
        self.verify_artifact(
            MODEL_FILE_NAME,
            model,
            self.model_length,
            cancelled,
            deadline,
        )?;
        self.verify_artifact(
            TOKENIZER_FILE_NAME,
            tokenizer,
            self.tokenizer_length,
            cancelled,
            deadline,
        )
    }

    fn verify_artifact(
        &self,
        name: &str,
        expected: EmbeddingArtifactId,
        expected_length: usize,
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<(), EmbeddingExecutableError> {
        check_request_deadline(cancelled, deadline)?;
        let mut file = self
            .directory_handle
            .open_private_file(name)
            .map_err(EmbeddingExecutableError::ArtifactWorkspace)?;
        if file
            .metadata()
            .map_err(EmbeddingExecutableError::ArtifactWorkspace)?
            .len()
            != u64::try_from(expected_length).unwrap_or(u64::MAX)
        {
            return Err(EmbeddingExecutableError::ArtifactDrift {
                artifact: if name == MODEL_FILE_NAME {
                    "model"
                } else {
                    "tokenizer"
                },
            });
        }
        let mut hasher = blake3::Hasher::new();
        let mut scratch = [0; 64 * 1024];
        let mut remaining = expected_length;
        while remaining > 0 {
            check_request_deadline(cancelled, deadline)?;
            let limit = remaining.min(scratch.len());
            let observed = file
                .read(&mut scratch[..limit])
                .map_err(EmbeddingExecutableError::ArtifactWorkspace)?;
            if observed == 0 {
                return Err(EmbeddingExecutableError::ArtifactDrift {
                    artifact: if name == MODEL_FILE_NAME {
                        "model"
                    } else {
                        "tokenizer"
                    },
                });
            }
            hasher.update(&scratch[..observed]);
            remaining -= observed;
        }
        let mut extra = [0; 1];
        if file
            .read(&mut extra)
            .map_err(EmbeddingExecutableError::ArtifactWorkspace)?
            != 0
            || file
                .metadata()
                .map_err(EmbeddingExecutableError::ArtifactWorkspace)?
                .len()
                != u64::try_from(expected_length).unwrap_or(u64::MAX)
        {
            return Err(EmbeddingExecutableError::ArtifactDrift {
                artifact: if name == MODEL_FILE_NAME {
                    "model"
                } else {
                    "tokenizer"
                },
            });
        }
        if hasher.finalize().as_bytes() != &expected.0 {
            return Err(EmbeddingExecutableError::ArtifactDrift {
                artifact: if name == MODEL_FILE_NAME {
                    "model"
                } else {
                    "tokenizer"
                },
            });
        }
        check_request_deadline(cancelled, deadline)?;
        Ok(())
    }

    fn model_path(&self) -> &Path {
        &self.model_path
    }

    fn tokenizer_path(&self) -> &Path {
        &self.tokenizer_path
    }
}

fn write_private_artifact_atomic(
    directory: &backend_platform::DirectoryCapability,
    name: &str,
    bytes: &[u8],
    deadline: Instant,
    mut write_chunk: impl FnMut(&mut File, &[u8]) -> io::Result<()>,
) -> Result<(), EmbeddingExecutableError> {
    check_request_deadline(None, deadline)?;
    let sequence = ARTIFACT_WORKSPACE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = format!(".{name}-{sequence}.tmp");
    let mut file = directory
        .create_file_exclusive(&temporary)
        .map_err(EmbeddingExecutableError::ArtifactWorkspace)?;
    let write_result = (|| {
        for chunk in bytes.chunks(64 * 1024) {
            check_request_deadline(None, deadline)?;
            write_chunk(&mut file, chunk).map_err(EmbeddingExecutableError::ArtifactWorkspace)?;
        }
        check_request_deadline(None, deadline)?;
        file.sync_all()
            .map_err(EmbeddingExecutableError::ArtifactWorkspace)?;
        check_request_deadline(None, deadline)?;
        directory
            .rename_with_outcome(&temporary, name, true)
            .map_err(|error| EmbeddingExecutableError::ArtifactWorkspace(error.into_io_error()))
    })();
    drop(file);
    if write_result.is_err() {
        let _ = directory.remove_file(&temporary);
    }
    write_result
}

impl Drop for EmbeddingArtifactWorkspace {
    fn drop(&mut self) {
        let _ = self.parent.remove_dir_all(&self.name, 8);
    }
}

fn embedding_environment(
    source: &ProcessEnvironment,
    artifacts: &EmbeddingArtifactWorkspace,
) -> Result<ProcessEnvironment, EmbeddingExecutableError> {
    let model_path = artifacts
        .model_path()
        .to_str()
        .ok_or(EmbeddingExecutableError::ArtifactPath)?
        .to_owned();
    let tokenizer_path = artifacts
        .tokenizer_path()
        .to_str()
        .ok_or(EmbeddingExecutableError::ArtifactPath)?
        .to_owned();
    let mut variables = source
        .variables()
        .iter()
        .filter(|(key, _)| key != MODEL_FILE_ENV && key != TOKENIZER_FILE_ENV)
        .cloned()
        .collect::<Vec<_>>();
    variables.push((MODEL_FILE_ENV.to_owned(), model_path));
    variables.push((TOKENIZER_FILE_ENV.to_owned(), tokenizer_path));
    ProcessEnvironment::new(variables).map_err(EmbeddingExecutableError::Process)
}

fn embedding_launch_configuration(
    program: &Path,
    workspace: &Path,
    arguments: &[String],
    environment: &ProcessEnvironment,
    process_limits: ProcessLimits,
) -> [u8; 32] {
    let mut hasher =
        blake3::Hasher::new_derive_key("backend.compile.embedding-launch-configuration.v1");
    update_launch_configuration_field(&mut hasher, program.as_os_str().as_encoded_bytes());
    update_launch_configuration_field(&mut hasher, workspace.as_os_str().as_encoded_bytes());
    hasher.update(
        &u64::try_from(arguments.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for argument in arguments {
        update_launch_configuration_field(&mut hasher, argument.as_bytes());
    }
    let variable_count = environment
        .variables()
        .iter()
        .filter(|(key, _)| key != MODEL_FILE_ENV && key != TOKENIZER_FILE_ENV)
        .count();
    hasher.update(
        &u64::try_from(variable_count)
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for (key, value) in environment.variables() {
        if key == MODEL_FILE_ENV || key == TOKENIZER_FILE_ENV {
            continue;
        }
        update_launch_configuration_field(&mut hasher, key.as_bytes());
        update_launch_configuration_field(&mut hasher, value.as_bytes());
    }
    for limit in [
        process_limits.stdout(),
        process_limits.stderr(),
        process_limits.output_bytes(),
        process_limits.input_bytes(),
    ] {
        hasher.update(&u64::try_from(limit).unwrap_or(u64::MAX).to_be_bytes());
    }
    update_launch_duration(&mut hasher, process_limits.wall_time());
    update_launch_optional_usize(&mut hasher, process_limits.workspace_limit());
    update_launch_optional_usize(&mut hasher, process_limits.process_count_limit());
    update_launch_optional_usize(&mut hasher, process_limits.memory_bytes_limit());
    match process_limits.cpu_time_limit() {
        Some(duration) => {
            hasher.update(&[1]);
            update_launch_duration(&mut hasher, duration);
        }
        None => {
            hasher.update(&[0]);
        }
    }
    *hasher.finalize().as_bytes()
}

fn update_launch_configuration_field(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
}

fn update_launch_optional_usize(hasher: &mut blake3::Hasher, value: Option<usize>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hasher.update(&u64::try_from(value).unwrap_or(u64::MAX).to_be_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn update_launch_duration(hasher: &mut blake3::Hasher, duration: Duration) {
    hasher.update(&duration.as_secs().to_be_bytes());
    hasher.update(&duration.subsec_nanos().to_be_bytes());
}

/// Failure to encode a vector into the canonical bounded segment payload.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum EmbeddingPayloadError {
    /// The caller-provided slice is not exactly the canonical payload extent.
    #[error("embedding payload buffer has an inexact extent")]
    BufferExtent,
    /// The vector dimension cannot be represented by the payload format.
    #[error("embedding dimension exceeds the payload format")]
    DimensionOverflow,
}

const EMBEDDING_PAYLOAD_MAGIC: &[u8; 4] = b"BVE1";
const EMBEDDING_PAYLOAD_HEADER_BYTES: usize = 4 + 2;

impl EmbeddingCoordinates {
    /// Exact recipe identity supplied by the admitted model manifest.
    #[must_use]
    pub const fn recipe(&self) -> [u8; 32] {
        self.recipe
    }
    /// Exact model artifact.
    #[must_use]
    pub const fn model(&self) -> EmbeddingArtifactId {
        self.model
    }
    /// Exact tokenizer artifact.
    #[must_use]
    pub const fn tokenizer(&self) -> EmbeddingArtifactId {
        self.tokenizer
    }
    /// Query/document treatment used by inference.
    #[must_use]
    pub const fn purpose(&self) -> EmbeddingPurpose {
        self.purpose
    }
    /// Admitted output normalization.
    #[must_use]
    pub const fn normalization(&self) -> EmbeddingNormalization {
        self.normalization
    }
    /// Exact fixed-dimension coordinates.
    #[must_use]
    pub fn values(&self) -> &[f32] {
        &self.values
    }

    /// Clones the shared immutable coordinate storage without copying its values.
    #[must_use]
    pub fn shared_values(&self) -> Arc<[f32]> {
        Arc::clone(&self.values)
    }

    /// Returns the exact length of the canonical versioned vector payload.
    #[must_use]
    pub fn canonical_payload_len(&self) -> usize {
        EMBEDDING_PAYLOAD_HEADER_BYTES + self.values.len() * size_of::<f32>()
    }

    /// Writes exact, bounded vector bytes to a caller-reserved buffer without allocating.
    ///
    /// The format is `BVE1`, a big-endian `u16` dimension, then finite `f32` values in
    /// little-endian order. Inference validates finiteness before coordinates are constructed.
    pub fn encode_canonical_payload(&self, output: &mut [u8]) -> Result<(), EmbeddingPayloadError> {
        let dimensions = u16::try_from(self.values.len())
            .map_err(|_| EmbeddingPayloadError::DimensionOverflow)?;
        if output.len() != self.canonical_payload_len() {
            return Err(EmbeddingPayloadError::BufferExtent);
        }
        output[..4].copy_from_slice(EMBEDDING_PAYLOAD_MAGIC);
        output[4..6].copy_from_slice(&dimensions.to_be_bytes());
        for (value, bytes) in self.values.iter().zip(output[6..].chunks_exact_mut(4)) {
            bytes.copy_from_slice(&value.to_le_bytes());
        }
        Ok(())
    }
}

/// Bounded external embedding executable and its resident artifact closure.
pub struct EmbeddingExecutable {
    program: PathBuf,
    arguments: Vec<String>,
    workspace: PathBuf,
    environment: ProcessEnvironment,
    process_limits: ProcessLimits,
    maximum_text_bytes: usize,
    dimensions: NonZeroU16,
    normalization: EmbeddingNormalization,
    recipe: [u8; 32],
    model_version: [u8; 32],
    options_digest: [u8; 32],
    executable: ToolchainArtifact,
    model: EmbeddingArtifact,
    tokenizer: EmbeddingArtifact,
    artifact_workspace: EmbeddingArtifactWorkspace,
    launch_configuration: [u8; 32],
    inference_gate: InferenceAdmissionGate,
    inference_cache: Mutex<EmbeddingInferenceCache>,
    persistent_session: Mutex<Option<RunningByteSession>>,
    batch_protocol: EmbeddingBatchProtocol,
    active: bool,
}

const MAX_EMBEDDING_CACHE_ENTRIES: usize = 256;

#[derive(Default)]
struct EmbeddingInferenceCache {
    coordinates: HashMap<EmbeddingInputIdentity, EmbeddingCoordinates>,
    insertion_order: VecDeque<EmbeddingInputIdentity>,
    coordinate_bytes: usize,
}

impl EmbeddingInferenceCache {
    fn get(&self, identity: EmbeddingInputIdentity) -> Option<EmbeddingCoordinates> {
        self.coordinates.get(&identity).cloned()
    }

    fn insert(&mut self, identity: EmbeddingInputIdentity, coordinates: &EmbeddingCoordinates) {
        let bytes = coordinates.values.len().saturating_mul(size_of::<f32>());
        if bytes > MAX_EMBEDDING_CACHE_COORDINATE_BYTES
            || self.coordinates.contains_key(&identity)
            || self.coordinates.try_reserve(1).is_err()
            || self.insertion_order.try_reserve(1).is_err()
        {
            return;
        }
        while self.coordinates.len() >= MAX_EMBEDDING_CACHE_ENTRIES
            || self.coordinate_bytes.saturating_add(bytes) > MAX_EMBEDDING_CACHE_COORDINATE_BYTES
        {
            let Some(oldest) = self.insertion_order.pop_front() else {
                break;
            };
            if let Some(removed) = self.coordinates.remove(&oldest) {
                self.coordinate_bytes = self
                    .coordinate_bytes
                    .saturating_sub(removed.values.len().saturating_mul(size_of::<f32>()));
            }
        }
        self.coordinates.insert(identity, coordinates.clone());
        self.insertion_order.push_back(identity);
        self.coordinate_bytes = self.coordinate_bytes.saturating_add(bytes);
    }
}

impl fmt::Debug for EmbeddingExecutable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EmbeddingExecutable")
            .field("program", &self.program)
            .field("dimensions", &self.dimensions)
            .field("normalization", &self.normalization)
            .field("recipe", &self.recipe)
            .field("model", &self.model.identity)
            .field("tokenizer", &self.tokenizer.identity)
            .field("model_version", &self.model_version)
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

impl EmbeddingExecutable {
    /// Returns the exact activated model runtime identity, including launch arguments and
    /// output-affecting explicit environment values.
    #[must_use]
    pub fn execution_identity(&self) -> EmbeddingExecutionIdentity {
        let model = self.model.identity.as_bytes();
        EmbeddingExecutionIdentity {
            model,
            model_version: self.model_version,
            tokenizer: self.tokenizer.identity.as_bytes(),
            dimension: u32::from(self.dimensions.get()),
            normalization: self.normalization,
            executable: self.executable.identity().to_bytes(),
            recipe: self.recipe,
            maximum_text_bytes: u32::try_from(self.maximum_text_bytes).unwrap_or(u32::MAX),
            options_digest: self.options_digest,
            launch_configuration: self.launch_configuration,
        }
    }

    /// Opens a bounded durable-cache session for a held workspace directory capability.
    ///
    /// The returned non-cloneable session owns its pinned directory and separate kernel lock. It
    /// is passed by the workspace compiler to each inference and is never attached to this shared
    /// runtime. Persistence is available only for unit-L2 output and remains an optimization.
    pub fn open_durable_cache_session(
        &self,
        directory: backend_platform::DirectoryCapability,
    ) -> Option<EmbeddingCacheSession> {
        if !self.active || self.normalization != EmbeddingNormalization::L2 {
            return None;
        }
        let identity = self.execution_identity();
        EmbeddingCacheSession::open(directory, identity)
    }

    /// Upper bound for one vector's canonical output bytes.
    #[must_use]
    pub const fn canonical_payload_max_bytes(&self) -> usize {
        EMBEDDING_PAYLOAD_HEADER_BYTES + self.dimensions.get() as usize * size_of::<f32>()
    }

    /// Maximum source-text bytes copied into one supervised request.
    #[must_use]
    pub const fn maximum_text_bytes(&self) -> usize {
        self.maximum_text_bytes
    }

    /// Per-call upper bound for cache storage, batch request/output scratch, decoded coordinates,
    /// and per-input batch metadata. Returned vectors retained by callers and concurrent calls
    /// can aggregate beyond this per-call budget. Compiler staging reserves encoded BVE1 payloads
    /// separately.
    #[must_use]
    pub const fn maximum_inference_scratch_bytes(&self) -> usize {
        self.process_limits
            .input_bytes()
            .saturating_add(self.process_limits.output_bytes())
            .saturating_add(MAX_EMBEDDING_BATCH_COORDINATE_BYTES)
            .saturating_add(MAX_EMBEDDING_BATCH_METADATA_BYTES)
            .saturating_add(MAX_EMBEDDING_CACHE_COORDINATE_BYTES)
    }

    /// Maximum simultaneous child processes admitted through this activated model runtime.
    #[must_use]
    pub const fn maximum_concurrent_inferences(&self) -> usize {
        1
    }

    /// Protocol admitted by the activation self-test.
    #[must_use]
    pub const fn batch_protocol(&self) -> EmbeddingBatchProtocol {
        self.batch_protocol
    }

    /// Verifies the executable and performs one supervised inference self-test before returning an
    /// active runtime. Activation construction and its readiness self-test share one deadline
    /// derived from the configured process wall-time limit; later inference calls each receive a
    /// fresh call-wide deadline of the same duration.
    ///
    /// # Errors
    ///
    /// Returns the exact artifact, configuration, process-bound, protocol, or self-test failure.
    #[allow(
        clippy::too_many_arguments,
        reason = "runtime authority and every resource bound remain explicit"
    )]
    pub fn activate(
        program: PathBuf,
        arguments: Vec<String>,
        workspace: PathBuf,
        environment: ProcessEnvironment,
        process_limits: ProcessLimits,
        maximum_text_bytes: usize,
        dimensions: NonZeroU16,
        normalization: EmbeddingNormalization,
        recipe: [u8; 32],
        executable: ToolchainArtifact,
        model: EmbeddingArtifact,
        tokenizer: EmbeddingArtifact,
    ) -> Result<Self, EmbeddingExecutableError> {
        let model_version = model.identity.as_bytes();
        Self::activate_inner(
            program,
            arguments,
            workspace,
            environment,
            process_limits,
            maximum_text_bytes,
            dimensions,
            normalization,
            recipe,
            [0; 32],
            model_version,
            executable,
            model,
            tokenizer,
        )
    }

    /// Activates exactly the portable spec against the supplied verified executable and bytes.
    ///
    /// The model and tokenizer are durably materialized into a private child workspace. Their
    /// paths replace any caller-supplied artifact path variables before the first self-test.
    /// Activation construction and readiness use one deadline derived from the configured
    /// process wall-time limit.
    ///
    /// # Errors
    ///
    /// Returns a field mismatch, private-workspace failure, artifact drift, process-bound,
    /// protocol, or self-test failure.
    #[allow(
        clippy::too_many_arguments,
        reason = "the local launch boundary and verified artifact closure remain explicit"
    )]
    pub fn activate_with_spec(
        spec: EmbeddingRuntimeSpecV1,
        program: PathBuf,
        arguments: Vec<String>,
        workspace: PathBuf,
        environment: ProcessEnvironment,
        process_limits: ProcessLimits,
        executable: ToolchainArtifact,
        model: EmbeddingArtifact,
        tokenizer: EmbeddingArtifact,
    ) -> Result<Self, EmbeddingExecutableError> {
        let observed_static = [
            ("model", spec.model == model.identity.as_bytes()),
            ("tokenizer", spec.tokenizer == tokenizer.identity.as_bytes()),
            (
                "executable",
                spec.executable == executable.identity().to_bytes(),
            ),
        ];
        for (field, matches) in observed_static {
            if !matches {
                return Err(EmbeddingExecutableError::Spec(
                    EmbeddingRuntimeSpecError::ObservedMismatch { field },
                ));
            }
        }
        let maximum_text_bytes = usize::try_from(spec.maximum_text_bytes.get())
            .map_err(|_| EmbeddingExecutableError::ZeroTextLimit)?;
        let runtime = Self::activate_inner(
            program,
            arguments,
            workspace,
            environment,
            process_limits,
            maximum_text_bytes,
            spec.dimension,
            spec.normalization,
            spec.recipe_identity(),
            spec.options_digest,
            spec.model_version,
            executable,
            model,
            tokenizer,
        )?;
        spec.validate_execution_identity(runtime.execution_identity())
            .map_err(EmbeddingExecutableError::Spec)?;
        Ok(runtime)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "activation must bind every local process and artifact input"
    )]
    fn activate_inner(
        program: PathBuf,
        arguments: Vec<String>,
        workspace: PathBuf,
        environment: ProcessEnvironment,
        process_limits: ProcessLimits,
        maximum_text_bytes: usize,
        dimensions: NonZeroU16,
        normalization: EmbeddingNormalization,
        recipe: [u8; 32],
        options_digest: [u8; 32],
        model_version: [u8; 32],
        executable: ToolchainArtifact,
        model: EmbeddingArtifact,
        tokenizer: EmbeddingArtifact,
    ) -> Result<Self, EmbeddingExecutableError> {
        // Activation construction, artifact verification, and the initial readiness self-test
        // share one explicit budget. This is separate from later inference-call deadlines.
        let activation_deadline = Instant::now()
            .checked_add(process_limits.wall_time())
            .ok_or(EmbeddingExecutableError::Process(ProcessError::Deadline))?;
        check_request_deadline(None, activation_deadline)?;
        SupervisedCommand::validate_launch_inputs(&program, &arguments, &workspace)
            .map_err(EmbeddingExecutableError::Process)?;
        if maximum_text_bytes == 0 {
            return Err(EmbeddingExecutableError::ZeroTextLimit);
        }
        let _maximum_text_bytes = u32::try_from(maximum_text_bytes)
            .map_err(|_| EmbeddingExecutableError::RequestExtent)?;
        let uncancelled = AtomicBool::new(false);
        executable
            .verify_path_until(&program, &uncancelled, activation_deadline)
            .map_err(EmbeddingExecutableError::Process)?;
        check_request_deadline(None, activation_deadline)?;
        if model.bytes().is_empty() || tokenizer.bytes().is_empty() {
            return Err(EmbeddingExecutableError::EmptyArtifact);
        }
        let request_bound = REQUEST_HEADER_BYTES
            .checked_add(maximum_text_bytes)
            .ok_or(EmbeddingExecutableError::RequestExtent)?;
        if request_bound > process_limits.input_bytes() {
            return Err(EmbeddingExecutableError::RequestExtent);
        }
        let response_bound = usize::from(dimensions.get())
            .checked_mul(size_of::<f32>())
            .and_then(|bytes| bytes.checked_add(RESPONSE_HEADER_BYTES))
            .ok_or(EmbeddingExecutableError::ResponseExtent)?;
        if response_bound > process_limits.stdout()
            || response_bound > process_limits.output_bytes()
        {
            return Err(EmbeddingExecutableError::ResponseBound);
        }
        let launch_configuration = embedding_launch_configuration(
            &program,
            &workspace,
            &arguments,
            &environment,
            process_limits,
        );
        let artifact_workspace = EmbeddingArtifactWorkspace::create(
            &workspace,
            &model,
            &tokenizer,
            activation_deadline,
        )?;
        check_request_deadline(None, activation_deadline)?;
        let environment = embedding_environment(&environment, &artifact_workspace)?;
        let mut runtime = Self {
            program,
            arguments,
            workspace,
            environment,
            process_limits,
            maximum_text_bytes,
            dimensions,
            normalization,
            recipe,
            model_version,
            options_digest,
            executable,
            model,
            tokenizer,
            artifact_workspace,
            launch_configuration,
            inference_gate: InferenceAdmissionGate::new(),
            inference_cache: Mutex::new(EmbeddingInferenceCache::default()),
            persistent_session: Mutex::new(None),
            batch_protocol: EmbeddingBatchProtocol::SingleV1,
            active: true,
        };
        if let Err(error) = runtime.probe_ready_until(activation_deadline) {
            runtime.active = false;
            return Err(error);
        }
        Ok(runtime)
    }

    /// Runs a real bounded self-test against the currently verified executable.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingExecutableError::Revoked`] after revocation, or the exact bounded
    /// process/protocol/output validation failure.
    pub fn probe_ready(&mut self) -> Result<(), EmbeddingExecutableError> {
        let deadline = self.request_deadline()?;
        self.probe_ready_until(deadline)
    }

    fn probe_ready_until(&mut self, deadline: Instant) -> Result<(), EmbeddingExecutableError> {
        check_request_deadline(None, deadline)?;
        if !self.active {
            return Err(EmbeddingExecutableError::Revoked);
        }
        let _permit = self.inference_gate.acquire(None, deadline)?;
        let probe = EmbeddingInvocation {
            purpose: EmbeddingPurpose::Query,
            text: "backend embedding readiness",
        };
        let second_probe = EmbeddingInvocation {
            purpose: EmbeddingPurpose::Query,
            text: "backend embedding batch check",
        };
        let probes = [probe, second_probe];
        if self.persistent_requested() {
            let persistent_probes = [
                probe,
                EmbeddingInvocation {
                    purpose: EmbeddingPurpose::Document,
                    text: "backend embedding document readiness",
                },
            ];
            for invocation in persistent_probes {
                check_request_deadline(None, deadline)?;
                let batch = [(
                    EmbeddingInputIdentity::new_until(
                        self.execution_identity(),
                        invocation,
                        None,
                        deadline,
                    )?,
                    invocation.text,
                )];
                if !self.batch_request_fits_until(&batch, None, deadline)? {
                    return Err(EmbeddingExecutableError::BatchRequestExtent);
                }
                let coordinates = self.run_persistent_batch_v2_admitted(
                    &batch,
                    invocation.purpose,
                    None,
                    deadline,
                )?;
                if coordinates.len() != 1 || coordinates[0].purpose() != invocation.purpose {
                    return Err(EmbeddingExecutableError::Protocol);
                }
            }
            self.batch_protocol = EmbeddingBatchProtocol::PersistentBatchV2;
            return Ok(());
        }
        let mut batch = Vec::with_capacity(probes.len());
        for invocation in probes {
            check_request_deadline(None, deadline)?;
            batch.push((
                EmbeddingInputIdentity::new_until(
                    self.execution_identity(),
                    invocation,
                    None,
                    deadline,
                )?,
                invocation.text,
            ));
        }
        if batch_single_request_fits(self.maximum_text_bytes, self.process_limits.input_bytes()) {
            match self.run_batch_v2_admitted(&batch, probe.purpose, None, deadline) {
                Ok(coordinates) if coordinates.len() == probes.len() => {
                    self.batch_protocol = EmbeddingBatchProtocol::BatchV2;
                    return Ok(());
                }
                Err(error)
                    if matches!(
                        error,
                        EmbeddingExecutableError::Process(
                            ProcessError::Deadline | ProcessError::Cancelled
                        )
                    ) =>
                {
                    return Err(error);
                }
                _ => {}
            }
        }
        self.batch_protocol = EmbeddingBatchProtocol::SingleV1;
        self.infer_inner_admitted(probe, false, None, deadline)
            .map(|_| ())
    }

    /// Executes one supervised, recipe-bound embedding request.
    ///
    /// # Errors
    ///
    /// Returns the exact preflight, process resource, protocol, dimension, numeric, or
    /// normalization rejection. No coordinates are returned on partial validation.
    pub fn infer(
        &self,
        invocation: EmbeddingInvocation<'_>,
    ) -> Result<EmbeddingCoordinates, EmbeddingExecutableError> {
        let deadline = self.request_deadline()?;
        if matches!(
            self.batch_protocol,
            EmbeddingBatchProtocol::BatchV2 | EmbeddingBatchProtocol::PersistentBatchV2
        ) {
            return self
                .infer_batch_inner(invocation.purpose, &[invocation.text], None, deadline, None)?
                .into_iter()
                .next()
                .ok_or(EmbeddingExecutableError::Protocol);
        }
        self.infer_inner_with_deadline(invocation, true, None, deadline)
    }

    /// Embeds many exact inputs, deduplicating by model recipe, task, and payload.
    ///
    /// BEM2-capable workers receive bounded microbatches through one supervised child per
    /// microbatch. A BEM1-only worker remains supported and receives one supervised request per
    /// unique cache miss. No coordinates from a malformed or partial batch are returned or cached.
    ///
    /// # Errors
    ///
    /// Returns the first input, cancellation, process, protocol, dimension, numeric, or
    /// normalization failure. Callers must discard the whole returned batch on error.
    pub fn infer_batch(
        &self,
        purpose: EmbeddingPurpose,
        texts: &[&str],
    ) -> Result<Vec<EmbeddingCoordinates>, EmbeddingExecutableError> {
        let deadline = self.request_deadline()?;
        self.infer_batch_inner(purpose, texts, None, deadline, None)
    }

    /// Cancellation-aware form of [`Self::infer_batch`].
    ///
    /// The shared flag is observed before execution and while the supervised process runs. A
    /// cancelled child is terminated and reaped before this call returns.
    pub fn infer_batch_with_cancellation_flag(
        &self,
        purpose: EmbeddingPurpose,
        texts: &[&str],
        cancelled: &AtomicBool,
    ) -> Result<Vec<EmbeddingCoordinates>, EmbeddingExecutableError> {
        let deadline = self.request_deadline()?;
        self.infer_batch_inner(purpose, texts, Some(cancelled), deadline, None)
    }

    /// Embeds a batch using the workspace cache session borrowed for this owner operation.
    ///
    /// The session belongs to the caller's workspace compiler, not this shareable runtime. A
    /// cache failure behaves as a miss; cancellation and the one absolute call deadline still
    /// apply to lookup, inference, and publication.
    pub fn infer_batch_with_cache_session(
        &self,
        purpose: EmbeddingPurpose,
        texts: &[&str],
        cancelled: &AtomicBool,
        cache_session: &EmbeddingCacheSession,
    ) -> Result<Vec<EmbeddingCoordinates>, EmbeddingExecutableError> {
        let deadline = self.request_deadline()?;
        self.infer_batch_inner(
            purpose,
            texts,
            Some(cancelled),
            deadline,
            Some(cache_session),
        )
    }

    fn request_deadline(&self) -> Result<Instant, EmbeddingExecutableError> {
        Instant::now()
            .checked_add(self.process_limits.wall_time())
            .ok_or(EmbeddingExecutableError::Process(ProcessError::Deadline))
    }

    fn verify_executable_until(
        &self,
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<(), EmbeddingExecutableError> {
        let uncancelled = AtomicBool::new(false);
        let cancellation = cancelled.unwrap_or(&uncancelled);
        self.executable
            .verify_path_until(&self.program, cancellation, deadline)
            .map_err(EmbeddingExecutableError::Process)
    }

    fn infer_batch_inner(
        &self,
        purpose: EmbeddingPurpose,
        texts: &[&str],
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
        cache_session: Option<&EmbeddingCacheSession>,
    ) -> Result<Vec<EmbeddingCoordinates>, EmbeddingExecutableError> {
        check_request_deadline(cancelled, deadline)?;
        if !self.active {
            return Err(EmbeddingExecutableError::Revoked);
        }
        let cache_session =
            cache_session.filter(|session| session.is_bound_to(self.execution_identity()));
        if texts.len() > MAX_EMBEDDING_BATCH_INPUTS {
            return Err(EmbeddingExecutableError::BatchInputLimit {
                observed: texts.len(),
                maximum: MAX_EMBEDDING_BATCH_INPUTS,
            });
        }
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        validate_batch_input_bytes(
            texts,
            self.maximum_text_bytes,
            MAX_EMBEDDING_BATCH_INPUT_BYTES,
            cancelled,
            deadline,
        )?;

        // Cache hits are accepted only after every mutable artifact path has been checked against
        // the activated manifest. Cache residency never becomes executable/model authority.
        self.artifact_workspace.verify_until(
            self.model.identity,
            self.tokenizer.identity,
            cancelled,
            deadline,
        )?;
        self.verify_executable_until(cancelled, deadline)?;
        check_request_deadline(cancelled, deadline)?;
        self.check_persistent_session_health(cancelled, deadline)?;

        let mut unique_inputs = Vec::<(EmbeddingInputIdentity, &str)>::new();
        let mut unique_by_identity = HashMap::<EmbeddingInputIdentity, usize>::new();
        unique_inputs
            .try_reserve(texts.len())
            .map_err(|_| EmbeddingExecutableError::BatchRequestExtent)?;
        unique_by_identity
            .try_reserve(texts.len())
            .map_err(|_| EmbeddingExecutableError::BatchRequestExtent)?;
        let mut input_indices = Vec::new();
        input_indices
            .try_reserve_exact(texts.len())
            .map_err(|_| EmbeddingExecutableError::BatchRequestExtent)?;
        for text in texts {
            check_request_deadline(cancelled, deadline)?;
            let invocation = EmbeddingInvocation { purpose, text };
            let identity = EmbeddingInputIdentity::new_until(
                self.execution_identity(),
                invocation,
                cancelled,
                deadline,
            )?;
            let index = if let Some(index) = unique_by_identity.get(&identity) {
                *index
            } else {
                let index = unique_inputs.len();
                unique_inputs.push((identity, text));
                unique_by_identity.insert(identity, index);
                index
            };
            input_indices.push(index);
        }

        // Bound the complete unique result set before any cache read or coordinate allocation.
        // A warm cache must not bypass the same per-call retained-coordinate ceiling as a miss.
        check_batch_result_limit(unique_inputs.len(), self.dimensions.get())?;
        let mut unique_results = vec![None; unique_inputs.len()];
        let mut misses = Vec::new();
        misses
            .try_reserve_exact(unique_inputs.len())
            .map_err(|_| EmbeddingExecutableError::BatchRequestExtent)?;
        {
            let cache = lock_mutex_until(&self.inference_cache, cancelled, deadline)?;
            for (index, (identity, _)) in unique_inputs.iter().enumerate() {
                if let Some(coordinates) = cache.get(*identity) {
                    unique_results[index] = Some(coordinates);
                } else {
                    misses.push(index);
                }
            }
        }
        self.load_cache_session_results(
            cache_session,
            purpose,
            &unique_inputs,
            &mut misses,
            &mut unique_results,
            cancelled,
            deadline,
        )?;

        // Admit a whole API batch as one owner. Besides bounding external model processes, this
        // lets concurrent callers recheck exact identities after the prior owner's transactional
        // cache commit, so simultaneous duplicate cold requests share one inference batch.
        let _permit = if misses.is_empty() {
            None
        } else {
            Some(self.inference_gate.acquire(cancelled, deadline)?)
        };
        if _permit.is_some() {
            check_request_deadline(cancelled, deadline)?;
            self.artifact_workspace.verify_until(
                self.model.identity,
                self.tokenizer.identity,
                cancelled,
                deadline,
            )?;
            self.verify_executable_until(cancelled, deadline)?;
            check_request_deadline(cancelled, deadline)?;

            let mut still_missing = Vec::new();
            still_missing
                .try_reserve_exact(misses.len())
                .map_err(|_| EmbeddingExecutableError::BatchRequestExtent)?;
            let cache = lock_mutex_until(&self.inference_cache, cancelled, deadline)?;
            for index in misses.drain(..) {
                check_request_deadline(cancelled, deadline)?;
                let (identity, _) = unique_inputs[index];
                if let Some(coordinates) = cache.get(identity) {
                    unique_results[index] = Some(coordinates);
                } else {
                    still_missing.push(index);
                }
            }
            misses = still_missing;
            // Durable lookups can block on the cache actor and filesystem. Do not hold the
            // process-local result-cache mutex while waiting on that independent owner.
            drop(cache);
            self.load_cache_session_results(
                cache_session,
                purpose,
                &unique_inputs,
                &mut misses,
                &mut unique_results,
                cancelled,
                deadline,
            )?;
        }
        let inferred_indices = misses.clone();

        if self.batch_protocol == EmbeddingBatchProtocol::SingleV1 {
            for index in misses {
                check_request_deadline(cancelled, deadline)?;
                let (_, text) = unique_inputs[index];
                unique_results[index] = Some(self.infer_inner_admitted(
                    EmbeddingInvocation { purpose, text },
                    false,
                    cancelled,
                    deadline,
                )?);
            }
        } else {
            let mut cursor = 0;
            while cursor < misses.len() {
                check_request_deadline(cancelled, deadline)?;
                let mut end = cursor;
                let mut request_bytes = REQUEST_HEADER_BYTES;
                while end < misses.len() && end - cursor < MAX_EMBEDDING_BATCH_ITEMS {
                    check_request_deadline(cancelled, deadline)?;
                    let (_, text) = unique_inputs[misses[end]];
                    let Some(candidate_bytes) = self.next_batch_request_extent(request_bytes, text)
                    else {
                        break;
                    };
                    let candidate_count = end - cursor + 1;
                    if !self.batch_response_fits(candidate_count) {
                        break;
                    }
                    request_bytes = candidate_bytes;
                    end += 1;
                }
                if end == cursor {
                    return Err(EmbeddingExecutableError::BatchRequestExtent);
                }
                let batch = misses[cursor..end]
                    .iter()
                    .map(|index| unique_inputs[*index])
                    .collect::<Vec<_>>();
                let coordinates =
                    self.run_batch_v2_admitted(&batch, purpose, cancelled, deadline)?;
                check_request_deadline(cancelled, deadline)?;
                if coordinates.len() != batch.len() {
                    return Err(EmbeddingExecutableError::BatchResponseCount {
                        expected: batch.len(),
                        observed: coordinates.len(),
                    });
                }
                for (index, coordinates) in misses[cursor..end].iter().zip(coordinates) {
                    unique_results[*index] = Some(coordinates);
                }
                cursor = end;
            }
        }

        let mut output = Vec::new();
        output
            .try_reserve_exact(input_indices.len())
            .map_err(|_| EmbeddingExecutableError::BatchRequestExtent)?;
        for index in input_indices {
            output.push(
                unique_results[index]
                    .as_ref()
                    .cloned()
                    .ok_or(EmbeddingExecutableError::Protocol)?,
            );
        }
        check_request_deadline(cancelled, deadline)?;
        // Cache writes are queued only after all misses decode successfully. This keeps one
        // failed later microbatch from publishing a prefix of producer output.
        if !inferred_indices.is_empty()
            && let Some(cache_session) = cache_session
        {
            let mut values = Vec::new();
            values
                .try_reserve_exact(inferred_indices.len())
                .map_err(|_| EmbeddingExecutableError::BatchRequestExtent)?;
            for &index in &inferred_indices {
                check_request_deadline(cancelled, deadline)?;
                if let Some(coordinates) = unique_results[index].as_ref() {
                    values.push(ValidatedProducerVector::from_decoded(
                        unique_inputs[index].0,
                        coordinates,
                    ));
                }
            }
            cache_session
                .store_validated(values, cancelled, deadline)
                .map_err(EmbeddingExecutableError::Process)?;
        }
        check_request_deadline(cancelled, deadline)?;
        let mut cache = lock_mutex_until(&self.inference_cache, cancelled, deadline)?;
        for (index, (identity, _)) in unique_inputs.iter().enumerate() {
            check_request_deadline(cancelled, deadline)?;
            if let Some(coordinates) = unique_results[index].as_ref() {
                cache.insert(*identity, coordinates);
            }
        }
        drop(cache);
        check_request_deadline(cancelled, deadline)?;
        Ok(output)
    }

    fn load_cache_session_results(
        &self,
        cache_session: Option<&EmbeddingCacheSession>,
        purpose: EmbeddingPurpose,
        unique_inputs: &[(EmbeddingInputIdentity, &str)],
        misses: &mut Vec<usize>,
        unique_results: &mut [Option<EmbeddingCoordinates>],
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<(), EmbeddingExecutableError> {
        let Some(cache_session) = cache_session else {
            return Ok(());
        };
        if misses.is_empty() {
            return Ok(());
        }
        check_request_deadline(cancelled, deadline)?;
        let mut identities = Vec::new();
        if identities.try_reserve_exact(misses.len()).is_err() {
            return Ok(());
        }
        identities.extend(misses.iter().map(|index| unique_inputs[*index].0));
        let cached = match cache_session.lookup_batch(&identities, cancelled, deadline) {
            Ok(Some(cached)) if cached.len() == misses.len() => cached,
            Ok(_) | Err(ProcessError::OutputLimit | ProcessError::UnsupportedLimit(_)) => {
                return Ok(());
            }
            Err(error) => return Err(EmbeddingExecutableError::Process(error)),
        };
        let mut still_missing = Vec::new();
        if still_missing.try_reserve_exact(misses.len()).is_err() {
            return Ok(());
        }
        for (&index, values) in misses.iter().zip(cached) {
            check_request_deadline(cancelled, deadline)?;
            if let Some(values) = values {
                unique_results[index] = Some(EmbeddingCoordinates {
                    recipe: self.recipe,
                    model: self.model.identity,
                    tokenizer: self.tokenizer.identity,
                    purpose,
                    normalization: self.normalization,
                    values,
                });
            } else {
                still_missing.push(index);
            }
        }
        *misses = still_missing;
        Ok(())
    }

    fn infer_inner(
        &self,
        invocation: EmbeddingInvocation<'_>,
        use_cache: bool,
    ) -> Result<EmbeddingCoordinates, EmbeddingExecutableError> {
        let deadline = self.request_deadline()?;
        self.infer_inner_with_deadline(invocation, use_cache, None, deadline)
    }

    fn infer_inner_with_cancellation_flag(
        &self,
        invocation: EmbeddingInvocation<'_>,
        use_cache: bool,
        cancelled: Option<&AtomicBool>,
    ) -> Result<EmbeddingCoordinates, EmbeddingExecutableError> {
        let deadline = self.request_deadline()?;
        self.infer_inner_with_deadline(invocation, use_cache, cancelled, deadline)
    }

    fn infer_inner_with_deadline(
        &self,
        invocation: EmbeddingInvocation<'_>,
        use_cache: bool,
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<EmbeddingCoordinates, EmbeddingExecutableError> {
        if !self.active {
            return Err(EmbeddingExecutableError::Revoked);
        }
        check_request_deadline(cancelled, deadline)?;
        let _permit = self.inference_gate.acquire(cancelled, deadline)?;
        self.infer_inner_admitted(invocation, use_cache, cancelled, deadline)
    }

    fn infer_inner_admitted(
        &self,
        invocation: EmbeddingInvocation<'_>,
        use_cache: bool,
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<EmbeddingCoordinates, EmbeddingExecutableError> {
        if !self.active {
            return Err(EmbeddingExecutableError::Revoked);
        }
        check_request_deadline(cancelled, deadline)?;
        self.artifact_workspace.verify_until(
            self.model.identity,
            self.tokenizer.identity,
            cancelled,
            deadline,
        )?;
        self.verify_executable_until(cancelled, deadline)?;
        if invocation.text.len() > self.maximum_text_bytes {
            return Err(EmbeddingExecutableError::TextLimit {
                observed: invocation.text.len(),
                maximum: self.maximum_text_bytes,
            });
        }
        let input_identity = EmbeddingInputIdentity::new_until(
            self.execution_identity(),
            invocation,
            cancelled,
            deadline,
        )?;
        if use_cache {
            let cache = lock_mutex_until(&self.inference_cache, cancelled, deadline)?;
            if let Some(coordinates) = cache.get(input_identity) {
                return Ok(coordinates);
            }
        }
        let request = self.encode_request_until(invocation, cancelled, deadline)?;
        let uncancelled = AtomicBool::new(false);
        let cancellation = cancelled.unwrap_or(&uncancelled);
        let command = SupervisedCommand::for_authority_with_artifact_until(
            self.program.clone(),
            self.arguments.clone(),
            self.environment.clone(),
            self.workspace.clone(),
            ProcessStdin::bytes(request),
            self.executable.clone(),
            None,
            ProtocolDescriptor::cold(),
            self.process_limits,
            cancellation,
            deadline,
        )
        .map_err(EmbeddingExecutableError::Process)?;
        let receipt = run_supervised_command(command, cancelled, deadline)?;
        check_request_deadline(cancelled, deadline)?;
        if receipt.terminal() != ProcessTerminal::Success || !receipt.reaped() {
            return Err(EmbeddingExecutableError::Terminal(receipt.terminal()));
        }
        let coordinates =
            self.decode_response(invocation.purpose, receipt.stdout(), cancelled, deadline)?;
        if use_cache {
            lock_mutex_until(&self.inference_cache, cancelled, deadline)?
                .insert(input_identity, &coordinates);
        }
        Ok(coordinates)
    }

    /// Revokes this active runtime. Further requests fail before process creation.
    pub fn revoke(&mut self) {
        self.active = false;
        let mut session = self
            .persistent_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *session = None;
    }

    fn encode_request_until(
        &self,
        invocation: EmbeddingInvocation<'_>,
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<Vec<u8>, EmbeddingExecutableError> {
        check_request_deadline(cancelled, deadline)?;
        let text_len = u32::try_from(invocation.text.len())
            .map_err(|_| EmbeddingExecutableError::RequestExtent)?;
        let request_extent = REQUEST_HEADER_BYTES
            .checked_add(invocation.text.len())
            .filter(|extent| *extent <= self.process_limits.input_bytes())
            .ok_or(EmbeddingExecutableError::RequestExtent)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(request_extent)
            .map_err(|_| EmbeddingExecutableError::RequestExtent)?;
        output.extend_from_slice(REQUEST_MAGIC);
        output.push(match invocation.purpose {
            EmbeddingPurpose::Query => 1,
            EmbeddingPurpose::Document => 2,
        });
        output.push(match self.normalization {
            EmbeddingNormalization::None => 0,
            EmbeddingNormalization::L2 => 1,
        });
        output.extend_from_slice(&self.dimensions.get().to_be_bytes());
        output.extend_from_slice(&self.recipe);
        output.extend_from_slice(&self.model.identity.as_bytes());
        output.extend_from_slice(&self.tokenizer.identity.as_bytes());
        output.extend_from_slice(&text_len.to_be_bytes());
        for chunk in invocation.text.as_bytes().chunks(64 * 1024) {
            check_request_deadline(cancelled, deadline)?;
            output.extend_from_slice(chunk);
        }
        check_request_deadline(cancelled, deadline)?;
        Ok(output)
    }

    fn batch_request_fits_until(
        &self,
        batch: &[(EmbeddingInputIdentity, &str)],
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<bool, EmbeddingExecutableError> {
        if batch.is_empty() || batch.len() > MAX_EMBEDDING_BATCH_ITEMS {
            return Ok(false);
        }
        let mut request_bytes = REQUEST_HEADER_BYTES;
        for (_, text) in batch {
            check_request_deadline(cancelled, deadline)?;
            let Some(next) = self.next_batch_request_extent(request_bytes, text) else {
                return Ok(false);
            };
            request_bytes = next;
        }
        Ok(request_bytes <= self.process_limits.input_bytes()
            && self.batch_response_fits(batch.len()))
    }

    fn batch_request_fits(&self, batch: &[(EmbeddingInputIdentity, &str)]) -> bool {
        if batch.is_empty() || batch.len() > MAX_EMBEDDING_BATCH_ITEMS {
            return false;
        }
        let Some(request_bytes) = batch
            .iter()
            .try_fold(REQUEST_HEADER_BYTES, |total, (_, text)| {
                self.next_batch_request_extent(total, text)
            })
        else {
            return false;
        };
        request_bytes <= self.process_limits.input_bytes() && self.batch_response_fits(batch.len())
    }

    fn next_batch_request_extent(&self, current: usize, text: &str) -> Option<usize> {
        if text.len() > self.maximum_text_bytes || u32::try_from(text.len()).is_err() {
            return None;
        }
        current
            .checked_add(BATCH_ITEM_HEADER_BYTES)?
            .checked_add(text.len())
            .filter(|bytes| *bytes <= self.process_limits.input_bytes())
    }

    fn batch_response_fits(&self, item_count: usize) -> bool {
        if item_count == 0 || item_count > MAX_EMBEDDING_BATCH_ITEMS {
            return false;
        }
        let Some(vector_bytes) = usize::from(self.dimensions.get())
            .checked_mul(size_of::<f32>())
            .and_then(|bytes| bytes.checked_add(BATCH_RESPONSE_ITEM_HEADER_BYTES))
        else {
            return false;
        };
        let Some(response_bytes) = item_count
            .checked_mul(vector_bytes)
            .and_then(|bytes| bytes.checked_add(BATCH_RESPONSE_HEADER_BYTES))
        else {
            return false;
        };
        response_bytes <= self.process_limits.stdout()
            && response_bytes <= self.process_limits.output_bytes()
    }

    fn encode_batch_request(
        &self,
        batch: &[(EmbeddingInputIdentity, &str)],
        purpose: EmbeddingPurpose,
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<Vec<u8>, EmbeddingExecutableError> {
        check_request_deadline(cancelled, deadline)?;
        if !self.batch_request_fits_until(batch, cancelled, deadline)? {
            return Err(EmbeddingExecutableError::BatchRequestExtent);
        }
        let mut request_bytes = REQUEST_HEADER_BYTES;
        for (_, text) in batch {
            check_request_deadline(cancelled, deadline)?;
            request_bytes = request_bytes
                .checked_add(BATCH_ITEM_HEADER_BYTES)
                .and_then(|bytes| bytes.checked_add(text.len()))
                .ok_or(EmbeddingExecutableError::BatchRequestExtent)?;
        }
        let count =
            u32::try_from(batch.len()).map_err(|_| EmbeddingExecutableError::BatchRequestExtent)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(request_bytes)
            .map_err(|_| EmbeddingExecutableError::BatchRequestExtent)?;
        output.extend_from_slice(BATCH_REQUEST_MAGIC);
        output.push(match purpose {
            EmbeddingPurpose::Query => 1,
            EmbeddingPurpose::Document => 2,
        });
        output.push(match self.normalization {
            EmbeddingNormalization::None => 0,
            EmbeddingNormalization::L2 => 1,
        });
        output.extend_from_slice(&self.dimensions.get().to_be_bytes());
        output.extend_from_slice(&self.recipe);
        output.extend_from_slice(&self.model.identity.as_bytes());
        output.extend_from_slice(&self.tokenizer.identity.as_bytes());
        output.extend_from_slice(&count.to_be_bytes());
        for (identity, text) in batch {
            check_request_deadline(cancelled, deadline)?;
            let text_len = u32::try_from(text.len())
                .map_err(|_| EmbeddingExecutableError::BatchRequestExtent)?;
            output.extend_from_slice(&identity.as_bytes());
            output.extend_from_slice(&text_len.to_be_bytes());
            for chunk in text.as_bytes().chunks(64 * 1024) {
                check_request_deadline(cancelled, deadline)?;
                output.extend_from_slice(chunk);
            }
        }
        check_request_deadline(cancelled, deadline)?;
        Ok(output)
    }

    fn run_batch_v2(
        &self,
        batch: &[(EmbeddingInputIdentity, &str)],
        purpose: EmbeddingPurpose,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Vec<EmbeddingCoordinates>, EmbeddingExecutableError> {
        if !self.active {
            return Err(EmbeddingExecutableError::Revoked);
        }
        let deadline = self.request_deadline()?;
        check_request_deadline(cancelled, deadline)?;
        let _permit = self.inference_gate.acquire(cancelled, deadline)?;
        self.run_batch_v2_admitted(batch, purpose, cancelled, deadline)
    }

    fn run_batch_v2_admitted(
        &self,
        batch: &[(EmbeddingInputIdentity, &str)],
        purpose: EmbeddingPurpose,
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<Vec<EmbeddingCoordinates>, EmbeddingExecutableError> {
        if self.batch_protocol == EmbeddingBatchProtocol::PersistentBatchV2 {
            return self.run_persistent_batch_v2_admitted(batch, purpose, cancelled, deadline);
        }
        if !self.active {
            return Err(EmbeddingExecutableError::Revoked);
        }
        check_request_deadline(cancelled, deadline)?;
        self.artifact_workspace.verify_until(
            self.model.identity,
            self.tokenizer.identity,
            cancelled,
            deadline,
        )?;
        self.verify_executable_until(cancelled, deadline)?;
        check_request_deadline(cancelled, deadline)?;
        let request = self.encode_batch_request(batch, purpose, cancelled, deadline)?;
        let uncancelled = AtomicBool::new(false);
        let cancellation = cancelled.unwrap_or(&uncancelled);
        let command = SupervisedCommand::for_authority_with_artifact_until(
            self.program.clone(),
            self.arguments.clone(),
            self.environment.clone(),
            self.workspace.clone(),
            ProcessStdin::bytes(request),
            self.executable.clone(),
            None,
            ProtocolDescriptor::cold(),
            self.process_limits,
            cancellation,
            deadline,
        )
        .map_err(EmbeddingExecutableError::Process)?;
        let receipt = run_supervised_command(command, cancelled, deadline)?;
        check_request_deadline(cancelled, deadline)?;
        if receipt.terminal() != ProcessTerminal::Success || !receipt.reaped() {
            return Err(EmbeddingExecutableError::Terminal(receipt.terminal()));
        }
        self.decode_batch_response(batch, purpose, receipt.stdout(), cancelled, deadline)
    }

    fn persistent_requested(&self) -> bool {
        self.arguments
            .iter()
            .any(|argument| argument == "--persistent-bem2-v1")
    }

    fn check_persistent_session_health(
        &self,
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<(), EmbeddingExecutableError> {
        if self.batch_protocol != EmbeddingBatchProtocol::PersistentBatchV2 {
            return Ok(());
        }
        let mut session = lock_mutex_until(&self.persistent_session, cancelled, deadline)?;
        let Some(current) = session.as_mut() else {
            return Ok(());
        };
        let uncancelled = AtomicBool::new(false);
        match current.is_alive_until(cancelled.unwrap_or(&uncancelled), deadline) {
            Ok(true) => Ok(()),
            Ok(false) => {
                *session = None;
                Err(EmbeddingExecutableError::Process(ProcessError::Protocol))
            }
            Err(error) => {
                *session = None;
                Err(EmbeddingExecutableError::Process(error))
            }
        }
    }

    #[cfg(test)]
    fn pause_persistent_stdout_reader_for_test(&self) -> bool {
        self.persistent_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(RunningByteSession::pause_stdout_reader_for_test)
    }

    #[cfg(test)]
    fn hold_persistent_probe_for_test(&self) -> Option<crate::supervisor::TestStdoutProbeControl> {
        self.persistent_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(RunningByteSession::hold_stdout_probe_for_test)
    }

    #[cfg(test)]
    fn clear_inference_cache_for_test(&self) {
        *self
            .inference_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            EmbeddingInferenceCache::default();
    }

    fn run_persistent_batch_v2(
        &self,
        batch: &[(EmbeddingInputIdentity, &str)],
        purpose: EmbeddingPurpose,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Vec<EmbeddingCoordinates>, EmbeddingExecutableError> {
        if !self.active {
            return Err(EmbeddingExecutableError::Revoked);
        }
        let deadline = self.request_deadline()?;
        check_request_deadline(cancelled, deadline)?;
        let _permit = self.inference_gate.acquire(cancelled, deadline)?;
        self.run_persistent_batch_v2_admitted(batch, purpose, cancelled, deadline)
    }

    fn run_persistent_batch_v2_admitted(
        &self,
        batch: &[(EmbeddingInputIdentity, &str)],
        purpose: EmbeddingPurpose,
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<Vec<EmbeddingCoordinates>, EmbeddingExecutableError> {
        if !self.active {
            return Err(EmbeddingExecutableError::Revoked);
        }
        check_request_deadline(cancelled, deadline)?;
        if !self.batch_request_fits_until(batch, cancelled, deadline)? {
            return Err(EmbeddingExecutableError::BatchRequestExtent);
        }
        self.artifact_workspace.verify_until(
            self.model.identity,
            self.tokenizer.identity,
            cancelled,
            deadline,
        )?;
        self.verify_executable_until(cancelled, deadline)?;
        check_request_deadline(cancelled, deadline)?;
        let request = self.encode_batch_request(batch, purpose, cancelled, deadline)?;
        let vector_bytes = usize::from(self.dimensions.get())
            .checked_mul(size_of::<f32>())
            .and_then(|extent| extent.checked_add(BATCH_RESPONSE_ITEM_HEADER_BYTES))
            .ok_or(EmbeddingExecutableError::ResponseExtent)?;
        let response_bytes = batch
            .len()
            .checked_mul(vector_bytes)
            .and_then(|extent| extent.checked_add(BATCH_RESPONSE_HEADER_BYTES))
            .ok_or(EmbeddingExecutableError::ResponseExtent)?;
        let uncancelled = AtomicBool::new(false);
        let cancellation_flag = cancelled.unwrap_or(&uncancelled);
        let mut session = lock_mutex_until(&self.persistent_session, cancelled, deadline)?;
        if let Some(current) = session.as_mut() {
            let alive = match current.is_alive_until(cancellation_flag, deadline) {
                Ok(alive) => alive,
                Err(error) => {
                    *session = None;
                    return Err(EmbeddingExecutableError::Process(error));
                }
            };
            if !alive
                || current.should_recycle()
                || !current.can_exchange(request.len(), response_bytes)
            {
                let retirement = current.retire();
                *session = None;
                retirement.map_err(EmbeddingExecutableError::Process)?;
            }
        }
        if session.is_none() {
            let command = SupervisedCommand::for_authority_with_artifact_until(
                self.program.clone(),
                self.arguments.clone(),
                self.environment.clone(),
                self.workspace.clone(),
                ProcessStdin::null(),
                self.executable.clone(),
                None,
                ProtocolDescriptor::persistent(),
                self.process_limits,
                cancellation_flag,
                deadline,
            )
            .map_err(EmbeddingExecutableError::Process)?;
            *session = Some(
                ProcessSupervisor::new(command)
                    .start_byte_session(cancellation_flag, deadline)
                    .map_err(EmbeddingExecutableError::Process)?,
            );
        }
        let response = match session
            .as_mut()
            .ok_or(EmbeddingExecutableError::Protocol)?
            .exchange_with_cancellation_flag_until(
                request,
                response_bytes,
                cancellation_flag,
                deadline,
            ) {
            Ok(response) => response,
            Err(error) => {
                *session = None;
                return Err(EmbeddingExecutableError::Process(error));
            }
        };
        let coordinates =
            self.decode_batch_response(batch, purpose, &response, cancelled, deadline);
        if coordinates.is_err() {
            *session = None;
        }
        coordinates
    }

    fn decode_batch_response(
        &self,
        batch: &[(EmbeddingInputIdentity, &str)],
        purpose: EmbeddingPurpose,
        bytes: &[u8],
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<Vec<EmbeddingCoordinates>, EmbeddingExecutableError> {
        check_request_deadline(cancelled, deadline)?;
        let Some(header) = bytes.get(..BATCH_RESPONSE_HEADER_BYTES) else {
            return Err(EmbeddingExecutableError::Protocol);
        };
        if &header[..4] != BATCH_RESPONSE_MAGIC {
            return Err(EmbeddingExecutableError::Protocol);
        }
        let dimensions = u16::from_be_bytes([header[4], header[5]]);
        if dimensions != self.dimensions.get() {
            return Err(EmbeddingExecutableError::Dimension {
                expected: self.dimensions.get(),
                observed: dimensions,
            });
        }
        let observed_count = u32::from_be_bytes([header[6], header[7], header[8], header[9]]);
        let expected_count =
            u32::try_from(batch.len()).map_err(|_| EmbeddingExecutableError::BatchRequestExtent)?;
        if observed_count != expected_count {
            return Err(EmbeddingExecutableError::BatchResponseCount {
                expected: batch.len(),
                observed: usize::try_from(observed_count).unwrap_or(usize::MAX),
            });
        }
        let vector_bytes = usize::from(dimensions)
            .checked_mul(size_of::<f32>())
            .and_then(|extent| extent.checked_add(BATCH_RESPONSE_ITEM_HEADER_BYTES))
            .ok_or(EmbeddingExecutableError::Protocol)?;
        let expected_bytes = batch
            .len()
            .checked_mul(vector_bytes)
            .and_then(|extent| extent.checked_add(BATCH_RESPONSE_HEADER_BYTES))
            .ok_or(EmbeddingExecutableError::Protocol)?;
        if bytes.len() != expected_bytes {
            return Err(EmbeddingExecutableError::Protocol);
        }
        let mut coordinates = Vec::new();
        coordinates
            .try_reserve_exact(batch.len())
            .map_err(|_| EmbeddingExecutableError::BatchRequestExtent)?;
        let mut offset = BATCH_RESPONSE_HEADER_BYTES;
        for (index, (identity, _)) in batch.iter().enumerate() {
            if index % 16 == 0 {
                check_request_deadline(cancelled, deadline)?;
            }
            let item_header = &bytes[offset..offset + BATCH_RESPONSE_ITEM_HEADER_BYTES];
            if item_header != identity.as_bytes() {
                return Err(EmbeddingExecutableError::BatchResponseIdentity { index });
            }
            offset += BATCH_RESPONSE_ITEM_HEADER_BYTES;
            let values_end = offset
                .checked_add(usize::from(dimensions) * size_of::<f32>())
                .ok_or(EmbeddingExecutableError::Protocol)?;
            let mut values = Vec::new();
            values
                .try_reserve_exact(usize::from(dimensions))
                .map_err(|_| EmbeddingExecutableError::BatchRequestExtent)?;
            for (coordinate_index, encoded) in bytes[offset..values_end]
                .chunks_exact(size_of::<f32>())
                .enumerate()
            {
                if coordinate_index % 1024 == 0 {
                    check_request_deadline(cancelled, deadline)?;
                }
                let value = f32::from_le_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]);
                if !value.is_finite() {
                    return Err(EmbeddingExecutableError::NonFinite);
                }
                values.push(value);
            }
            if self.normalization == EmbeddingNormalization::L2 {
                let norm_squared = values.iter().map(|value| value * value).sum::<f32>();
                if (norm_squared - 1.0).abs() > 0.001 {
                    return Err(EmbeddingExecutableError::Normalization { norm_squared });
                }
            }
            coordinates.push(EmbeddingCoordinates {
                recipe: self.recipe,
                model: self.model.identity,
                tokenizer: self.tokenizer.identity,
                purpose,
                normalization: self.normalization,
                values: Arc::from(values.into_boxed_slice()),
            });
            offset = values_end;
        }
        check_request_deadline(cancelled, deadline)?;
        Ok(coordinates)
    }

    fn decode_response(
        &self,
        purpose: EmbeddingPurpose,
        bytes: &[u8],
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<EmbeddingCoordinates, EmbeddingExecutableError> {
        check_request_deadline(cancelled, deadline)?;
        let Some(header) = bytes.get(..RESPONSE_HEADER_BYTES) else {
            return Err(EmbeddingExecutableError::Protocol);
        };
        if &header[..4] != RESPONSE_MAGIC {
            return Err(EmbeddingExecutableError::Protocol);
        }
        let dimensions = u16::from_be_bytes([header[4], header[5]]);
        if dimensions != self.dimensions.get() {
            return Err(EmbeddingExecutableError::Dimension {
                expected: self.dimensions.get(),
                observed: dimensions,
            });
        }
        let coordinate_bytes = usize::from(dimensions)
            .checked_mul(size_of::<f32>())
            .and_then(|extent| extent.checked_add(RESPONSE_HEADER_BYTES))
            .ok_or(EmbeddingExecutableError::Protocol)?;
        if bytes.len() != coordinate_bytes {
            return Err(EmbeddingExecutableError::Protocol);
        }
        let mut values = Vec::with_capacity(usize::from(dimensions));
        for (index, encoded) in bytes[RESPONSE_HEADER_BYTES..]
            .chunks_exact(size_of::<f32>())
            .enumerate()
        {
            if index % 1024 == 0 {
                check_request_deadline(cancelled, deadline)?;
            }
            let value = f32::from_le_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]);
            if !value.is_finite() {
                return Err(EmbeddingExecutableError::NonFinite);
            }
            values.push(value);
        }
        if self.normalization == EmbeddingNormalization::L2 {
            let norm_squared = values.iter().map(|value| value * value).sum::<f32>();
            if (norm_squared - 1.0).abs() > 0.001 {
                return Err(EmbeddingExecutableError::Normalization { norm_squared });
            }
        }
        check_request_deadline(cancelled, deadline)?;
        Ok(EmbeddingCoordinates {
            recipe: self.recipe,
            model: self.model.identity,
            tokenizer: self.tokenizer.identity,
            purpose,
            normalization: self.normalization,
            values: Arc::from(values.into_boxed_slice()),
        })
    }
}

/// Exact external embedding failure.
#[derive(Debug)]
pub enum EmbeddingExecutableError {
    /// Zero input-text bound is not meaningful.
    ZeroTextLimit,
    /// Model or tokenizer bytes were empty.
    EmptyArtifact,
    /// A model or tokenizer exceeds its separately configured workspace cap.
    ArtifactLimit {
        /// Artifact role that exceeded its cap.
        artifact: &'static str,
        /// Observed bytes.
        observed: usize,
        /// Maximum admitted bytes.
        maximum: usize,
    },
    /// Private artifact workspace creation, verification, or cleanup failed.
    ArtifactWorkspace(io::Error),
    /// A materialized artifact differs from its content identity.
    ArtifactDrift {
        /// Artifact role whose bytes changed.
        artifact: &'static str,
    },
    /// Artifact path could not be represented in the child environment.
    ArtifactPath,
    /// Activated runtime did not match its portable configuration claim.
    Spec(EmbeddingRuntimeSpecError),
    /// Input exceeded its independent bound.
    TextLimit {
        /// UTF-8 bytes in the rejected input.
        observed: usize,
        /// Configured independent text bound.
        maximum: usize,
    },
    /// One batch contained more original inputs than the call bound permits.
    BatchInputLimit {
        /// Number of supplied inputs.
        observed: usize,
        /// Maximum admitted inputs.
        maximum: usize,
    },
    /// Aggregate original UTF-8 payload bytes exceed the hashing-work bound.
    BatchInputBytesLimit {
        /// Aggregate bytes in supplied inputs.
        observed: usize,
        /// Maximum aggregate bytes admitted.
        maximum: usize,
    },
    /// Decoded coordinates for unique misses exceed the bounded batch retention allowance.
    BatchResultLimit {
        /// Required coordinate bytes.
        observed: usize,
        /// Maximum retained coordinate bytes.
        maximum: usize,
    },
    /// Request header/text extent cannot be represented or exceeds the process input bound.
    RequestExtent,
    /// Batch request, response, or scratch extent cannot be represented or exceeds its bound.
    BatchRequestExtent,
    /// Expected response extent overflowed its fixed-dimension representation.
    ResponseExtent,
    /// Process output limits cannot retain one exact-dimension response.
    ResponseBound,
    /// Process admission, resource bound, deadline, or cleanup failed.
    Process(ProcessError),
    /// Process returned a non-success terminal.
    Terminal(ProcessTerminal),
    /// Response was malformed or not exact-length.
    Protocol,
    /// Response dimension differed from the admitted recipe.
    Dimension {
        /// Dimension admitted from the recipe.
        expected: u16,
        /// Dimension declared by the process response.
        observed: u16,
    },
    /// Batch response did not contain exactly the expected number of vectors.
    BatchResponseCount {
        /// Number requested.
        expected: usize,
        /// Number returned.
        observed: usize,
    },
    /// Batch response returned a different content/version identity at this position.
    BatchResponseIdentity {
        /// Zero-based response position.
        index: usize,
    },
    /// Response contained NaN or infinity.
    NonFinite,
    /// L2-normalized recipe returned a non-unit vector.
    Normalization {
        /// Observed squared L2 norm.
        norm_squared: f32,
    },
    /// Runtime was revoked.
    Revoked,
    /// Another inference held the shared process slot past its finite admission wait budget.
    InferenceAdmissionTimeout,
}

impl fmt::Display for EmbeddingExecutableError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "external embedding runtime failed: {self:?}")
    }
}

impl std::error::Error for EmbeddingExecutableError {}

fn check_cancelled(cancelled: Option<&AtomicBool>) -> Result<(), EmbeddingExecutableError> {
    if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(EmbeddingExecutableError::Process(ProcessError::Cancelled));
    }
    Ok(())
}

fn check_request_deadline(
    cancelled: Option<&AtomicBool>,
    deadline: Instant,
) -> Result<(), EmbeddingExecutableError> {
    check_cancelled(cancelled)?;
    if Instant::now() >= deadline {
        return Err(EmbeddingExecutableError::Process(ProcessError::Deadline));
    }
    Ok(())
}

fn lock_mutex_until<'a, T>(
    mutex: &'a Mutex<T>,
    cancelled: Option<&AtomicBool>,
    deadline: Instant,
) -> Result<MutexGuard<'a, T>, EmbeddingExecutableError> {
    loop {
        check_request_deadline(cancelled, deadline)?;
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => {
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(1)),
                );
            }
        }
    }
}

fn validate_batch_input_bytes(
    texts: &[&str],
    maximum_text_bytes: usize,
    maximum_batch_bytes: usize,
    cancelled: Option<&AtomicBool>,
    deadline: Instant,
) -> Result<(), EmbeddingExecutableError> {
    let mut total = 0_usize;
    for (index, text) in texts.iter().enumerate() {
        if index % 256 == 0 {
            check_request_deadline(cancelled, deadline)?;
        }
        if text.len() > maximum_text_bytes {
            return Err(EmbeddingExecutableError::TextLimit {
                observed: text.len(),
                maximum: maximum_text_bytes,
            });
        }
        total = total.saturating_add(text.len());
    }
    if total > maximum_batch_bytes {
        return Err(EmbeddingExecutableError::BatchInputBytesLimit {
            observed: total,
            maximum: maximum_batch_bytes,
        });
    }
    check_request_deadline(cancelled, deadline)?;
    Ok(())
}

fn check_batch_result_limit(
    unique_results: usize,
    dimensions: u16,
) -> Result<(), EmbeddingExecutableError> {
    let coordinate_bytes = unique_results
        .checked_mul(usize::from(dimensions))
        .and_then(|coordinates| coordinates.checked_mul(size_of::<f32>()))
        .ok_or(EmbeddingExecutableError::BatchResultLimit {
            observed: usize::MAX,
            maximum: MAX_EMBEDDING_BATCH_COORDINATE_BYTES,
        })?;
    if coordinate_bytes > MAX_EMBEDDING_BATCH_COORDINATE_BYTES {
        return Err(EmbeddingExecutableError::BatchResultLimit {
            observed: coordinate_bytes,
            maximum: MAX_EMBEDDING_BATCH_COORDINATE_BYTES,
        });
    }
    Ok(())
}

fn batch_single_request_fits(maximum_text_bytes: usize, input_bytes: usize) -> bool {
    REQUEST_HEADER_BYTES
        .checked_add(BATCH_ITEM_HEADER_BYTES)
        .and_then(|bytes| bytes.checked_add(maximum_text_bytes))
        .is_some_and(|request_bytes| request_bytes <= input_bytes)
}

fn run_supervised_command(
    command: SupervisedCommand,
    cancelled: Option<&AtomicBool>,
    deadline: Instant,
) -> Result<crate::ProcessReceipt, EmbeddingExecutableError> {
    let uncancelled = AtomicBool::new(false);
    let cancellation = cancelled.unwrap_or(&uncancelled);
    ProcessSupervisor::new(command)
        .run_with_observer_until(cancellation, deadline)
        .map_err(EmbeddingExecutableError::Process)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    const MODEL_BYTES: &[u8] = br#"{"<unk>":[1.0,0.0],"alpha":[1.0,0.0],"beta":[0.0,1.0]}"#;
    const TOKENIZER_BYTES: &[u8] = br#"{"lowercase":true,"split":"whitespace"}"#;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().fold(String::new(), |mut output, byte| {
            output.push_str(&format!("{byte:02x}"));
            output
        })
    }

    fn fixture() -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "backend-embedding-{}-{stamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let executable = root.join("fixture.py");
        let script = format!(
            r#"#!/usr/bin/env python3
import json, math, os, struct, sys, time
EXPECTED_MODEL = bytes.fromhex("{}")
EXPECTED_TOKENIZER = bytes.fromhex("{}")
EXPECTED_MODEL_ID = "{}"
EXPECTED_TOKENIZER_ID = "{}"
counter = os.environ.get("BACKEND_EMBEDDING_ACTIVE_COUNTER")
call_counter = os.environ.get("BACKEND_EMBEDDING_CALL_COUNTER")
fault_file = os.environ.get("BACKEND_EMBEDDING_FAULT_FILE")
batch_gate = os.environ.get("BACKEND_EMBEDDING_BATCH_GATE")
if counter:
    import fcntl, time
    def update_counter(delta):
        with open(counter + ".lock", "a+") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            try:
                try:
                    active, peak = map(int, open(counter).read().split())
                except (FileNotFoundError, ValueError):
                    active, peak = 0, 0
                active += delta
                peak = max(peak, active)
                with open(counter, "w") as state:
                    state.write(f"{{active}} {{peak}}")
            finally:
                fcntl.flock(lock, fcntl.LOCK_UN)
    update_counter(1)
    time.sleep(0.3)
model_bytes = open(os.environ["BACKEND_EMBEDDING_MODEL_FILE"], "rb").read()
tokenizer_bytes = open(os.environ["BACKEND_EMBEDDING_TOKENIZER_FILE"], "rb").read()
if model_bytes != EXPECTED_MODEL or tokenizer_bytes != EXPECTED_TOKENIZER:
    sys.exit(71)
frame = sys.stdin.buffer.read()
if len(frame) < 108 or frame[:4] not in (b"BEM1", b"BEM2"):
    sys.exit(72)
if os.environ.get("BACKEND_EMBEDDING_REQUIRE_BEM2") and frame[:4] != b"BEM2":
    sys.exit(78)
if frame[40:72].hex() != EXPECTED_MODEL_ID or frame[72:104].hex() != EXPECTED_TOKENIZER_ID:
    sys.exit(73)
batch = frame[:4] == b"BEM2"
items = []
if batch:
    count = struct.unpack(">I", frame[104:108])[0]
    offset = 108
    for _ in range(count):
        identity = frame[offset:offset + 32]
        text_length = struct.unpack(">I", frame[offset + 32:offset + 36])[0]
        offset += 36
        text = frame[offset:offset + text_length]
        if len(text) != text_length:
            sys.exit(74)
        items.append((identity, text))
        offset += text_length
    if offset != len(frame):
        sys.exit(74)
else:
    text_length = struct.unpack(">I", frame[104:108])[0]
    text = frame[108:]
    if len(text) != text_length:
        sys.exit(74)
    items.append((None, text))
if call_counter:
    with open(call_counter, "a") as output:
        output.write("%s %d\n" % (frame[:4].decode(), len(items)))
if batch_gate and batch and call_counter:
    try:
        call_index = len(open(call_counter).read().splitlines())
    except FileNotFoundError:
        call_index = 0
    if call_index == 2:
        with open(batch_gate + ".started", "w") as ready:
            ready.write("ready")
        while not os.path.exists(batch_gate + ".release"):
            time.sleep(0.001)
if frame[6:8] == b"\x00\x00":
    sys.exit(74)
dimension = struct.unpack(">H", frame[6:8])[0]
normalization = frame[5]
model = json.loads(model_bytes)
tokenizer = json.loads(tokenizer_bytes)
encoded_vectors = []
for identity, raw_text in items:
    text = raw_text.decode("utf-8")
    if tokenizer.get("lowercase"):
        text = text.lower()
    tokens = text.split()
    if not tokens:
        sys.exit(77)
    vectors = [model.get(token, model["<unk>"]) for token in tokens]
    if any(len(vector) != dimension for vector in vectors):
        sys.exit(76)
    values = [sum(vector[index] for vector in vectors) / len(vectors) for index in range(dimension)]
    if normalization == 1:
        norm = math.sqrt(sum(value * value for value in values))
        if norm == 0:
            sys.exit(77)
        values = [value / norm for value in values]
    encoded_vectors.append((identity, struct.pack("<" + "f" * dimension, *values)))
if counter:
    update_counter(-1)
if batch:
    fault = open(fault_file).read().strip() if fault_file else ""
    response_count = len(encoded_vectors) - 1 if fault == "partial" and encoded_vectors else len(encoded_vectors)
    sys.stdout.buffer.write(b"BEC2" + struct.pack(">HI", dimension, response_count))
    for index, (identity, vector) in enumerate(encoded_vectors[:response_count]):
        if fault == "identity" and index == 0:
            identity = bytes([identity[0] ^ 1]) + identity[1:]
        sys.stdout.buffer.write(identity + vector)
else:
    sys.stdout.buffer.write(b"BEC1" + struct.pack(">H", dimension) + encoded_vectors[0][1])
"#,
            hex(MODEL_BYTES),
            hex(TOKENIZER_BYTES),
            hex(blake3::hash(MODEL_BYTES).as_bytes()),
            hex(blake3::hash(TOKENIZER_BYTES).as_bytes()),
        );
        fs::write(&executable, script)?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
        Ok((root, executable))
    }

    fn persistent_fixture() -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "backend-embedding-persistent-{}-{stamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let executable = root.join("persistent-fixture.py");
        let script = format!(
            r##"#!/usr/bin/env python3
import os, struct, sys, time
MODEL = bytes.fromhex("{}")
TOKENIZER = bytes.fromhex("{}")
if sys.argv[1:] != ["--persistent-bem2-v1"]:
    sys.exit(70)
if open(os.environ["BACKEND_EMBEDDING_MODEL_FILE"], "rb").read() != MODEL:
    sys.exit(71)
if open(os.environ["BACKEND_EMBEDDING_TOKENIZER_FILE"], "rb").read() != TOKENIZER:
    sys.exit(72)
call_counter = os.environ.get("BACKEND_EMBEDDING_CALL_COUNTER")
fault_file = os.environ.get("BACKEND_EMBEDDING_FAULT_FILE")
delay_file = os.environ.get("BACKEND_EMBEDDING_DELAY_FILE")
fork_sentinel = os.environ.get("BACKEND_EMBEDDING_FORK_SENTINEL")
escaped_pid_file = os.environ.get("BACKEND_EMBEDDING_ESCAPED_PID_FILE")
start_counter = os.environ.get("BACKEND_EMBEDDING_START_COUNTER")
start_id = 0
if start_counter:
    import fcntl
    with open(start_counter + ".lock", "a+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        try:
            try:
                start_id = int(open(start_counter).read()) + 1
            except (FileNotFoundError, ValueError):
                start_id = 1
            with open(start_counter, "w") as state:
                state.write(str(start_id))
        finally:
            fcntl.flock(lock, fcntl.LOCK_UN)
def exact(length):
    output = bytearray()
    while len(output) < length:
        chunk = sys.stdin.buffer.read(length - len(output))
        if not chunk:
            if not output:
                return None
            raise EOFError("truncated persistent frame")
        output.extend(chunk)
    return bytes(output)
while True:
    prefix = exact(4)
    if prefix is None:
        break
    header = prefix + exact(104)
    if header[:4] != b"BEM2" or header[4] not in (1, 2):
        sys.exit(73)
    if header[40:72] != bytes.fromhex("{}"):
        sys.exit(74)
    if header[72:104] != bytes.fromhex("{}"):
        sys.exit(75)
    dimension = struct.unpack(">H", header[6:8])[0]
    count = struct.unpack(">I", header[104:108])[0]
    if dimension != 2 or count < 1 or count > 256:
        sys.exit(76)
    items = []
    for _ in range(count):
        item_header = exact(36)
        identity = item_header[:32]
        length = struct.unpack(">I", item_header[32:])[0]
        if length < 1 or length > 65536:
            sys.exit(77)
        text = exact(length)
        if text is None:
            sys.exit(78)
        text.decode("utf-8")
        items.append(identity)
    if call_counter:
        with open(call_counter, "a") as output:
            output.write("%d %d %d\n" % (os.getpid(), start_id, count))
    delay_mode = open(delay_file).read().strip() if delay_file and os.path.exists(delay_file) else "fast"
    if delay_mode == "slow":
        time.sleep(1)
    elif delay_mode == "multi-batch":
        time.sleep(1.25)
    fault = open(fault_file).read().strip() if fault_file and os.path.exists(fault_file) else ""
    if fault == "partial" and items:
        items = items[:-1]
    output = bytearray(b"BEC2" + struct.pack(">HI", dimension, len(items)))
    for index, identity in enumerate(items):
        if fault == "identity" and index == 0:
            identity = bytes([identity[0] ^ 1]) + identity[1:]
        output.extend(identity)
        output.extend(struct.pack("<ff", 1.0, 0.0))
    if fault == "padding":
        output.extend(b"x")
    sys.stdout.buffer.write(output)
    sys.stdout.buffer.flush()
    if fault == "idle-padding":
        time.sleep(0.05)
        sys.stdout.buffer.write(b"x")
        sys.stdout.buffer.flush()
    if fault == "idle-gate":
        with open(fault_file + ".ready", "w") as marker:
            marker.write("ready")
        while not os.path.exists(fault_file + ".release"):
            time.sleep(0.001)
        sys.stdout.buffer.write(b"x")
        sys.stdout.buffer.flush()
        with open(fault_file + ".written", "w") as marker:
            marker.write("written")
    if fault == "stderr-flood":
        time.sleep(0.05)
        sys.stderr.buffer.write(b"x" * 100000)
        sys.stderr.buffer.flush()
    if fault == "fork":
        descendant = os.fork()
        if descendant == 0:
            time.sleep(0.8)
            if fork_sentinel:
                with open(fork_sentinel, "w") as marker:
                    marker.write("descendant survived")
            os._exit(0)
        time.sleep(0.15)
        sys.exit(0)
    if fault == "setsid-fork":
        descendant = os.fork()
        if descendant == 0:
            os.setsid()
            if escaped_pid_file:
                # Publish the PID only after the complete marker is durable; the
                # parent must never mistake open/truncate's empty window for a
                # ready helper.
                temporary_pid_file = escaped_pid_file + ".tmp"
                with open(temporary_pid_file, "x") as marker:
                    marker.write(str(os.getpid()) + "\n")
                    marker.flush()
                    os.fsync(marker.fileno())
                os.replace(temporary_pid_file, escaped_pid_file)
            time.sleep(30)
            os._exit(0)
        time.sleep(0.15)
        sys.exit(0)
"##,
            hex(MODEL_BYTES),
            hex(TOKENIZER_BYTES),
            hex(blake3::hash(MODEL_BYTES).as_bytes()),
            hex(blake3::hash(TOKENIZER_BYTES).as_bytes()),
        );
        fs::write(&executable, script)?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
        Ok((root, executable))
    }

    fn runtime_with_counter(
        counter: Option<&Path>,
    ) -> Result<(PathBuf, EmbeddingRuntimeSpecV1, EmbeddingExecutable), Box<dyn std::error::Error>>
    {
        runtime_with_counters(counter, None)
    }

    fn runtime_with_call_counter(
        counter: &Path,
    ) -> Result<(PathBuf, EmbeddingRuntimeSpecV1, EmbeddingExecutable), Box<dyn std::error::Error>>
    {
        runtime_with_counters(None, Some(counter))
    }

    fn runtime_with_counters(
        counter: Option<&Path>,
        call_counter: Option<&Path>,
    ) -> Result<(PathBuf, EmbeddingRuntimeSpecV1, EmbeddingExecutable), Box<dyn std::error::Error>>
    {
        runtime_with_fault_file(counter, call_counter, None)
    }

    fn runtime_with_fault_file(
        counter: Option<&Path>,
        call_counter: Option<&Path>,
        fault_file: Option<&Path>,
    ) -> Result<(PathBuf, EmbeddingRuntimeSpecV1, EmbeddingExecutable), Box<dyn std::error::Error>>
    {
        let (root, program) = fixture()?;
        let mut environment = vec![("PATH".into(), "/usr/bin:/bin".into())];
        if let Some(counter) = counter {
            environment.push((
                "BACKEND_EMBEDDING_ACTIVE_COUNTER".into(),
                counter.to_string_lossy().into_owned(),
            ));
        }
        if let Some(call_counter) = call_counter {
            environment.push((
                "BACKEND_EMBEDDING_CALL_COUNTER".into(),
                call_counter.to_string_lossy().into_owned(),
            ));
        }
        if let Some(fault_file) = fault_file {
            environment.push((
                "BACKEND_EMBEDDING_FAULT_FILE".into(),
                fault_file.to_string_lossy().into_owned(),
            ));
        }
        let environment = ProcessEnvironment::new(environment)?;
        activate_fixture(root, program, Vec::new(), environment)
    }

    fn runtime_with_batch_gate(
        call_counter: &Path,
        batch_gate: &Path,
    ) -> Result<(PathBuf, EmbeddingRuntimeSpecV1, EmbeddingExecutable), Box<dyn std::error::Error>>
    {
        let (root, program) = fixture()?;
        let environment = ProcessEnvironment::new(vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            (
                "BACKEND_EMBEDDING_CALL_COUNTER".into(),
                call_counter.to_string_lossy().into_owned(),
            ),
            (
                "BACKEND_EMBEDDING_BATCH_GATE".into(),
                batch_gate.to_string_lossy().into_owned(),
            ),
        ])?;
        activate_fixture(root, program, Vec::new(), environment)
    }

    fn activate_fixture(
        root: PathBuf,
        program: PathBuf,
        arguments: Vec<String>,
        environment: ProcessEnvironment,
    ) -> Result<(PathBuf, EmbeddingRuntimeSpecV1, EmbeddingExecutable), Box<dyn std::error::Error>>
    {
        let limits = ProcessLimits::new(256, 64, Duration::from_secs(2), 256)?
            .with_input_bytes_limit(2_048)?;
        activate_fixture_with_limits(root, program, arguments, environment, limits)
    }

    fn activate_fixture_with_limits(
        root: PathBuf,
        program: PathBuf,
        arguments: Vec<String>,
        environment: ProcessEnvironment,
        limits: ProcessLimits,
    ) -> Result<(PathBuf, EmbeddingRuntimeSpecV1, EmbeddingExecutable), Box<dyn std::error::Error>>
    {
        let artifact = ToolchainArtifact::from_path(&program, Vec::new())?;
        let model = EmbeddingArtifact::new(Arc::from(MODEL_BYTES));
        let tokenizer = EmbeddingArtifact::new(Arc::from(TOKENIZER_BYTES));
        let spec = EmbeddingRuntimeSpecV1::new(
            model.identity().as_bytes(),
            [7; 32],
            tokenizer.identity().as_bytes(),
            artifact.identity().to_bytes(),
            NonZeroU16::new(2).ok_or("dimensions")?,
            EmbeddingNormalization::L2,
            NonZeroU32::new(128).ok_or("text limit")?,
            [0xA5; 32],
        );
        let runtime = EmbeddingExecutable::activate_with_spec(
            spec,
            program,
            arguments,
            root.clone(),
            environment,
            limits,
            artifact,
            model,
            tokenizer,
        )?;
        Ok((root, spec, runtime))
    }

    fn runtime()
    -> Result<(PathBuf, EmbeddingRuntimeSpecV1, EmbeddingExecutable), Box<dyn std::error::Error>>
    {
        runtime_with_counter(None)
    }

    #[test]
    fn active_external_model_returns_typed_query_and_document_coordinates()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, spec, runtime) = runtime()?;
        let query = runtime.infer(EmbeddingInvocation {
            purpose: EmbeddingPurpose::Query,
            text: "ALPHA beta",
        })?;
        let document = runtime.infer(EmbeddingInvocation {
            purpose: EmbeddingPurpose::Document,
            text: "fn parse() {}",
        })?;
        assert!((query.values()[0] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1.0e-6);
        assert!((query.values()[1] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1.0e-6);
        assert_eq!(query.purpose(), EmbeddingPurpose::Query);
        assert_eq!(document.purpose(), EmbeddingPurpose::Document);
        assert_eq!(query.recipe(), spec.recipe_identity());
        let identity = runtime.execution_identity();
        assert_eq!(identity.model(), query.model().as_bytes());
        assert_eq!(identity.model_version(), [7; 32]);
        assert_eq!(identity.tokenizer(), query.tokenizer().as_bytes());
        assert_eq!(identity.dimension(), 2);
        assert_eq!(
            identity.executable(),
            runtime.executable.identity().to_bytes()
        );
        let mut payload = vec![0; query.canonical_payload_len()];
        query.encode_canonical_payload(&mut payload)?;
        assert_eq!(&payload[..6], b"BVE1\0\x02");
        assert_eq!(&payload[..4], b"BVE1");
        let spec_bytes = spec.canonical_bytes();
        assert_eq!(spec_bytes.len(), 172);
        assert_eq!(&spec_bytes[..5], b"BERS\x01");
        assert_eq!(&spec_bytes[133..135], &2_u16.to_be_bytes());
        assert_eq!(spec_bytes[135], 1);
        assert_eq!(&spec_bytes[136..140], &128_u32.to_be_bytes());
        assert_eq!(EmbeddingRuntimeSpecV1::decode(&spec_bytes)?, spec);
        let changed_options = EmbeddingRuntimeSpecV1::new(
            spec.model(),
            spec.model_version(),
            spec.tokenizer(),
            spec.executable(),
            spec.dimension(),
            spec.normalization(),
            spec.maximum_text_bytes(),
            [0xA4; 32],
        );
        assert_ne!(spec.recipe_identity(), changed_options.recipe_identity());
        drop(runtime);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn batch_only_worker_is_activated_with_two_items_and_single_infer_uses_bem2()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, program) = fixture()?;
        let calls = root.join("protocol-calls.txt");
        let environment = ProcessEnvironment::new(vec![
            (
                "BACKEND_EMBEDDING_CALL_COUNTER".into(),
                calls.to_string_lossy().into_owned(),
            ),
            ("BACKEND_EMBEDDING_REQUIRE_BEM2".into(), "1".into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ])?;
        let (root, _, runtime) = activate_fixture(
            root,
            program,
            vec!["--fixture-option=stable".into()],
            environment,
        )?;
        assert_eq!(runtime.batch_protocol(), EmbeddingBatchProtocol::BatchV2);

        let coordinates = runtime.infer(EmbeddingInvocation {
            purpose: EmbeddingPurpose::Document,
            text: "alpha",
        })?;
        assert_eq!(coordinates.purpose(), EmbeddingPurpose::Document);
        assert_eq!(
            fs::read_to_string(&calls)?.lines().collect::<Vec<_>>(),
            ["BEM2 2", "BEM2 1"]
        );

        drop(runtime);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn persistent_worker_is_self_tested_once_and_reuses_one_process_for_exact_cache_misses()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, program) = persistent_fixture()?;
        let calls = root.join("persistent-calls.txt");
        let starts = root.join("persistent-starts.txt");
        let environment = ProcessEnvironment::new(vec![
            (
                "BACKEND_EMBEDDING_CALL_COUNTER".into(),
                calls.to_string_lossy().into_owned(),
            ),
            (
                "BACKEND_EMBEDDING_START_COUNTER".into(),
                starts.to_string_lossy().into_owned(),
            ),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ])?;
        let (root, _, runtime) = activate_fixture(
            root,
            program,
            vec!["--persistent-bem2-v1".into()],
            environment,
        )?;
        assert_eq!(
            runtime.batch_protocol(),
            EmbeddingBatchProtocol::PersistentBatchV2
        );
        assert_eq!(fs::read_to_string(&starts)?.trim(), "1");
        assert_eq!(fs::read_to_string(&calls)?.lines().count(), 2);
        let invocation = EmbeddingInvocation {
            purpose: EmbeddingPurpose::Document,
            text: "persistent exact document",
        };
        let cold = runtime.infer(invocation)?;
        let warm = runtime.infer(invocation)?;
        assert_eq!(cold.values(), warm.values());
        assert_eq!(fs::read_to_string(&calls)?.lines().count(), 3);
        assert_eq!(fs::read_to_string(&starts)?.trim(), "1");
        let process_ids = fs::read_to_string(&calls)?
            .lines()
            .map(|line| {
                let mut fields = line.split_ascii_whitespace();
                let pid = fields.next().ok_or("missing process id")?;
                let start_id = fields.next().ok_or("missing worker start id")?;
                Ok::<_, Box<dyn std::error::Error>>((pid.to_owned(), start_id.to_owned()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        assert!(
            process_ids
                .iter()
                .all(|identity| identity == &process_ids[0])
        );
        drop(runtime);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn persistent_worker_recycles_before_aggregate_process_output_limit()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, program) = persistent_fixture()?;
        let calls = root.join("persistent-calls.txt");
        let starts = root.join("persistent-starts.txt");
        let environment = ProcessEnvironment::new(vec![
            (
                "BACKEND_EMBEDDING_CALL_COUNTER".into(),
                calls.to_string_lossy().into_owned(),
            ),
            (
                "BACKEND_EMBEDDING_START_COUNTER".into(),
                starts.to_string_lossy().into_owned(),
            ),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ])?;
        let (root, _, runtime) = activate_fixture(
            root,
            program,
            vec!["--persistent-bem2-v1".into()],
            environment,
        )?;
        for index in 0..20 {
            let text = format!("bounded page row {index}");
            let coordinates = runtime.infer(EmbeddingInvocation {
                purpose: EmbeddingPurpose::Document,
                text: &text,
            })?;
            assert_eq!(coordinates.purpose(), EmbeddingPurpose::Document);
        }
        assert_eq!(fs::read_to_string(&starts)?.trim(), "5");
        assert_eq!(fs::read_to_string(&calls)?.lines().count(), 22);
        drop(runtime);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn persistent_worker_enforces_cumulative_stdout_quota_and_reuses_exact_cache_hits()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, program) = persistent_fixture()?;
        let calls = root.join("persistent-calls.txt");
        let starts = root.join("persistent-starts.txt");
        let environment = ProcessEnvironment::new(vec![
            (
                "BACKEND_EMBEDDING_CALL_COUNTER".into(),
                calls.to_string_lossy().into_owned(),
            ),
            (
                "BACKEND_EMBEDDING_START_COUNTER".into(),
                starts.to_string_lossy().into_owned(),
            ),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ])?;
        let limits = ProcessLimits::new(150, 64, Duration::from_secs(2), 512)?
            .with_input_bytes_limit(2_048)?;
        let (root, _, runtime) = activate_fixture_with_limits(
            root,
            program,
            vec!["--persistent-bem2-v1".into()],
            environment,
            limits,
        )?;
        let first = runtime.infer(EmbeddingInvocation {
            purpose: EmbeddingPurpose::Document,
            text: "stdout allowance first item",
        })?;
        assert_eq!(fs::read_to_string(&starts)?.trim(), "1");
        let cached = runtime.infer(EmbeddingInvocation {
            purpose: EmbeddingPurpose::Document,
            text: "stdout allowance first item",
        })?;
        assert_eq!(first.values(), cached.values());
        assert_eq!(fs::read_to_string(&starts)?.trim(), "1");

        let second = runtime.infer(EmbeddingInvocation {
            purpose: EmbeddingPurpose::Document,
            text: "stdout allowance second item",
        })?;
        assert_eq!(second.purpose(), EmbeddingPurpose::Document);
        assert_eq!(fs::read_to_string(&starts)?.trim(), "2");
        assert_eq!(fs::read_to_string(&calls)?.lines().count(), 4);
        drop(runtime);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn persistent_worker_cancellation_reaps_child_and_next_call_starts_cleanly()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, program) = persistent_fixture()?;
        let calls = root.join("persistent-calls.txt");
        let starts = root.join("persistent-starts.txt");
        let delay = root.join("persistent-delay.txt");
        fs::write(&delay, "fast")?;
        let environment = ProcessEnvironment::new(vec![
            (
                "BACKEND_EMBEDDING_CALL_COUNTER".into(),
                calls.to_string_lossy().into_owned(),
            ),
            (
                "BACKEND_EMBEDDING_START_COUNTER".into(),
                starts.to_string_lossy().into_owned(),
            ),
            (
                "BACKEND_EMBEDDING_DELAY_FILE".into(),
                delay.to_string_lossy().into_owned(),
            ),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ])?;
        let (root, _, runtime) = activate_fixture(
            root,
            program,
            vec!["--persistent-bem2-v1".into()],
            environment,
        )?;
        let invocation = EmbeddingInvocation {
            purpose: EmbeddingPurpose::Document,
            text: "cancel and restart",
        };
        fs::write(&delay, "slow")?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_handle = Arc::clone(&cancelled);
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            cancel_handle.store(true, Ordering::Release);
        });
        let result = runtime.infer_batch_with_cancellation_flag(
            invocation.purpose,
            &[invocation.text],
            &cancelled,
        );
        canceller
            .join()
            .map_err(|_| "cancellation thread panicked")?;
        assert!(matches!(
            result,
            Err(EmbeddingExecutableError::Process(ProcessError::Cancelled))
        ));
        fs::write(&delay, "fast")?;
        let recovered = runtime.infer(invocation)?;
        assert_eq!(recovered.purpose(), EmbeddingPurpose::Document);
        assert_eq!(fs::read_to_string(&starts)?.trim(), "2");
        let calls = fs::read_to_string(&calls)?;
        let worker_ids = calls
            .lines()
            .map(|line| line.split_ascii_whitespace().nth(1).unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(worker_ids.first(), Some(&"1"));
        assert_eq!(worker_ids.last(), Some(&"2"));
        drop(runtime);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn persistent_microbatches_share_one_absolute_request_deadline_and_publish_atomically()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, program) = persistent_fixture()?;
        let calls = root.join("persistent-calls.txt");
        let delay = root.join("persistent-delay.txt");
        fs::write(&delay, "fast")?;
        let environment = ProcessEnvironment::new(vec![
            (
                "BACKEND_EMBEDDING_CALL_COUNTER".into(),
                calls.to_string_lossy().into_owned(),
            ),
            (
                "BACKEND_EMBEDDING_DELAY_FILE".into(),
                delay.to_string_lossy().into_owned(),
            ),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ])?;
        let (root, _, runtime) = activate_fixture(
            root,
            program,
            vec!["--persistent-bem2-v1".into()],
            environment,
        )?;
        // With a 2,048-byte request cap, eleven 128-byte inputs fit and twelve do not;
        // the model therefore needs exactly two API microbatches.
        let owned_texts = (0..12)
            .map(|index| format!("{index:03} {}", "a".repeat(124)))
            .collect::<Vec<_>>();
        assert!(owned_texts.iter().all(|text| text.len() == 128));
        let texts = owned_texts.iter().map(String::as_str).collect::<Vec<_>>();
        fs::write(&calls, "")?;
        fs::write(&delay, "multi-batch")?;
        let failed = runtime.infer_batch(EmbeddingPurpose::Document, &texts);
        assert!(matches!(
            failed,
            Err(EmbeddingExecutableError::Process(ProcessError::Deadline))
        ));
        assert!(
            runtime
                .inference_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .coordinates
                .is_empty()
        );
        assert_eq!(fs::read_to_string(&calls)?.lines().count(), 2);

        fs::write(&delay, "fast")?;
        let recovered = runtime.infer_batch(EmbeddingPurpose::Document, &texts)?;
        assert_eq!(recovered.len(), texts.len());
        assert_eq!(fs::read_to_string(&calls)?.lines().count(), 4);
        drop(runtime);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn persistent_worker_corrupt_reply_is_not_cached_and_restarts_after_protocol_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, program) = persistent_fixture()?;
        let calls = root.join("persistent-calls.txt");
        let starts = root.join("persistent-starts.txt");
        let fault = root.join("persistent-fault.txt");
        fs::write(&fault, "none")?;
        let environment = ProcessEnvironment::new(vec![
            (
                "BACKEND_EMBEDDING_CALL_COUNTER".into(),
                calls.to_string_lossy().into_owned(),
            ),
            (
                "BACKEND_EMBEDDING_START_COUNTER".into(),
                starts.to_string_lossy().into_owned(),
            ),
            (
                "BACKEND_EMBEDDING_FAULT_FILE".into(),
                fault.to_string_lossy().into_owned(),
            ),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ])?;
        let (root, _, runtime) = activate_fixture(
            root,
            program,
            vec!["--persistent-bem2-v1".into()],
            environment,
        )?;
        let invocation = EmbeddingInvocation {
            purpose: EmbeddingPurpose::Query,
            text: "corrupted response must not publish",
        };
        fs::write(&fault, "identity")?;
        assert!(matches!(
            runtime.infer(invocation),
            Err(EmbeddingExecutableError::BatchResponseIdentity { index: 0 })
        ));
        fs::write(&fault, "none")?;
        let valid = runtime.infer(invocation)?;
        assert_eq!(valid.purpose(), EmbeddingPurpose::Query);
        assert_eq!(fs::read_to_string(&starts)?.trim(), "2");
        assert_eq!(fs::read_to_string(&calls)?.lines().count(), 4);
        drop(runtime);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn persistent_reply_padding_poison_is_observed_before_cache_publication()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, program) = persistent_fixture()?;
        let calls = root.join("persistent-calls.txt");
        let starts = root.join("persistent-starts.txt");
        let fault = root.join("persistent-fault.txt");
        fs::write(&fault, "none")?;
        let environment = ProcessEnvironment::new(vec![
            (
                "BACKEND_EMBEDDING_CALL_COUNTER".into(),
                calls.to_string_lossy().into_owned(),
            ),
            (
                "BACKEND_EMBEDDING_START_COUNTER".into(),
                starts.to_string_lossy().into_owned(),
            ),
            (
                "BACKEND_EMBEDDING_FAULT_FILE".into(),
                fault.to_string_lossy().into_owned(),
            ),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ])?;
        let (root, _, runtime) = activate_fixture(
            root,
            program,
            vec!["--persistent-bem2-v1".into()],
            environment,
        )?;
        let invocation = EmbeddingInvocation {
            purpose: EmbeddingPurpose::Document,
            text: "padding is not a valid response frame",
        };
        fs::write(&fault, "padding")?;
        assert!(matches!(
            runtime.infer(invocation),
            Err(EmbeddingExecutableError::Process(ProcessError::Protocol))
        ));
        fs::write(&fault, "none")?;
        let valid = runtime.infer(invocation)?;
        assert_eq!(valid.purpose(), EmbeddingPurpose::Document);
        assert_eq!(fs::read_to_string(&starts)?.trim(), "2");
        assert_eq!(fs::read_to_string(&calls)?.lines().count(), 4);
        drop(runtime);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn persistent_unsolicited_idle_stdout_invalidates_the_next_cache_only_request()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, program) = persistent_fixture()?;
        let fault = root.join("persistent-fault.txt");
        fs::write(&fault, "none")?;
        let environment = ProcessEnvironment::new(vec![
            (
                "BACKEND_EMBEDDING_FAULT_FILE".into(),
                fault.to_string_lossy().into_owned(),
            ),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ])?;
        let (root, _, runtime) = activate_fixture(
            root,
            program,
            vec!["--persistent-bem2-v1".into()],
            environment,
        )?;
        // Activation performs two BEM2 readiness exchanges. Arm the fault only after those
        // self-tests so the worker injects the idle byte after the user response below.
        fs::write(&fault, "idle-gate")?;
        let invocation = EmbeddingInvocation {
            purpose: EmbeddingPurpose::Document,
            text: "idle stdout must poison a warm cache hit",
        };
        let first = runtime.infer(invocation)?;
        assert_eq!(first.purpose(), EmbeddingPurpose::Document);
        assert!(runtime.pause_persistent_stdout_reader_for_test());
        let ready = PathBuf::from(format!("{}.ready", fault.to_string_lossy()));
        let release = PathBuf::from(format!("{}.release", fault.to_string_lossy()));
        let written = PathBuf::from(format!("{}.written", fault.to_string_lossy()));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "persistent helper did not reach idle-output gate",
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        fs::write(&release, "release")?;
        while !written.exists() {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "persistent helper did not write gated idle output",
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(matches!(
            runtime.infer(invocation),
            Err(EmbeddingExecutableError::Process(ProcessError::Protocol))
        ));
        drop(runtime);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn persistent_health_probe_uses_request_deadline_instead_of_a_short_reader_window()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, program) = persistent_fixture()?;
        let fault = root.join("persistent-fault.txt");
        fs::write(&fault, "none")?;
        let environment = ProcessEnvironment::new(vec![
            (
                "BACKEND_EMBEDDING_FAULT_FILE".into(),
                fault.to_string_lossy().into_owned(),
            ),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ])?;
        let (_, _, runtime) = activate_fixture(
            root.clone(),
            program,
            vec!["--persistent-bem2-v1".into()],
            environment,
        )?;
        let runtime = Arc::new(runtime);
        let invocation = EmbeddingInvocation {
            purpose: EmbeddingPurpose::Document,
            text: "warm cache remains usable when the reader is merely delayed",
        };
        assert!(runtime.infer(invocation).is_ok());
        assert!(runtime.pause_persistent_stdout_reader_for_test());
        let probe = runtime
            .hold_persistent_probe_for_test()
            .ok_or("persistent session was missing")?;
        let (finished, result) = std::sync::mpsc::sync_channel(1);
        let caller_runtime = Arc::clone(&runtime);
        let caller = std::thread::spawn(move || {
            let _ = finished.send(caller_runtime.infer(invocation));
        });

        assert!(probe.wait_until_probe(Instant::now() + Duration::from_secs(1)));
        assert!(matches!(
            result.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        probe.release();
        let coordinates = result.recv_timeout(Duration::from_secs(2))??;
        assert_eq!(coordinates.purpose(), EmbeddingPurpose::Document);
        caller
            .join()
            .map_err(|_| io::Error::other("cache-only caller panicked"))?;
        assert!(
            runtime
                .persistent_session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
        );
        drop(runtime);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn persistent_idle_stderr_overflow_retires_worker_before_cache_only_return()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, program) = persistent_fixture()?;
        let fault = root.join("persistent-fault.txt");
        fs::write(&fault, "none")?;
        let environment = ProcessEnvironment::new(vec![
            (
                "BACKEND_EMBEDDING_FAULT_FILE".into(),
                fault.to_string_lossy().into_owned(),
            ),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ])?;
        let (root, _, runtime) = activate_fixture(
            root,
            program,
            vec!["--persistent-bem2-v1".into()],
            environment,
        )?;
        let invocation = EmbeddingInvocation {
            purpose: EmbeddingPurpose::Query,
            text: "idle stderr must poison a warm cache hit",
        };
        fs::write(&fault, "stderr-flood")?;
        let first = runtime.infer(invocation)?;
        assert_eq!(first.purpose(), EmbeddingPurpose::Query);
        std::thread::sleep(Duration::from_millis(150));
        assert!(matches!(
            runtime.infer(invocation),
            Err(EmbeddingExecutableError::Process(ProcessError::OutputLimit))
        ));
        drop(runtime);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn persistent_retirement_kills_inherited_pipe_descendant_after_leader_exit()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, program) = persistent_fixture()?;
        let calls = root.join("persistent-calls.txt");
        let starts = root.join("persistent-starts.txt");
        let fault = root.join("persistent-fault.txt");
        let sentinel = root.join("fork-survived.txt");
        fs::write(&fault, "none")?;
        let environment = ProcessEnvironment::new(vec![
            (
                "BACKEND_EMBEDDING_CALL_COUNTER".into(),
                calls.to_string_lossy().into_owned(),
            ),
            (
                "BACKEND_EMBEDDING_START_COUNTER".into(),
                starts.to_string_lossy().into_owned(),
            ),
            (
                "BACKEND_EMBEDDING_FAULT_FILE".into(),
                fault.to_string_lossy().into_owned(),
            ),
            (
                "BACKEND_EMBEDDING_FORK_SENTINEL".into(),
                sentinel.to_string_lossy().into_owned(),
            ),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ])?;
        let (root, _, runtime) = activate_fixture(
            root,
            program,
            vec!["--persistent-bem2-v1".into()],
            environment,
        )?;
        let invocation = EmbeddingInvocation {
            purpose: EmbeddingPurpose::Document,
            text: "leader exits while a fork retains the pipes",
        };
        let helper_pid = fs::read_to_string(&calls)?
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().next())
            .ok_or("persistent helper did not record its process id")?
            .parse::<u32>()?;
        let retirements_before = crate::supervisor::group_retirement_attempts(helper_pid);
        fs::write(&fault, "fork")?;
        let valid = runtime.infer(invocation)?;
        assert_eq!(valid.purpose(), EmbeddingPurpose::Document);
        std::thread::sleep(Duration::from_millis(250));
        let started = Instant::now();
        assert!(matches!(
            runtime.infer(invocation),
            Err(EmbeddingExecutableError::Process(ProcessError::Protocol))
        ));
        assert_eq!(
            crate::supervisor::group_retirement_attempts(helper_pid),
            retirements_before + 1,
            "dropping a session after leader-exit retirement must not signal its group twice"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(900));
        assert!(
            !sentinel.exists(),
            "the group descendant survived retirement"
        );
        assert_eq!(fs::read_to_string(&starts)?.trim(), "1");
        drop(runtime);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn persistent_retirement_does_not_join_escaped_child_holding_pipe_fds()
    -> Result<(), Box<dyn std::error::Error>> {
        struct EscapedPidCleanup(Option<rustix::process::Pid>);
        impl Drop for EscapedPidCleanup {
            fn drop(&mut self) {
                if let Some(pid) = self.0.take() {
                    let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
                }
            }
        }

        let (root, program) = persistent_fixture()?;
        let fault = root.join("persistent-fault.txt");
        let escaped_pid_file = root.join("escaped-pid.txt");
        fs::write(&fault, "none")?;
        let environment = ProcessEnvironment::new(vec![
            (
                "BACKEND_EMBEDDING_FAULT_FILE".into(),
                fault.to_string_lossy().into_owned(),
            ),
            (
                "BACKEND_EMBEDDING_ESCAPED_PID_FILE".into(),
                escaped_pid_file.to_string_lossy().into_owned(),
            ),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ])?;
        let (root, _, runtime) = activate_fixture(
            root,
            program,
            vec!["--persistent-bem2-v1".into()],
            environment,
        )?;
        let invocation = EmbeddingInvocation {
            purpose: EmbeddingPurpose::Document,
            text: "escaped descendant retains the session pipes",
        };
        fs::write(&fault, "setsid-fork")?;
        let valid = runtime.infer(invocation)?;
        assert_eq!(valid.purpose(), EmbeddingPurpose::Document);

        let pid_deadline = Instant::now() + Duration::from_secs(2);
        let escaped_pid = loop {
            if let Ok(raw_pid) = fs::read_to_string(&escaped_pid_file) {
                let raw_pid = raw_pid.trim().parse::<i32>()?;
                break rustix::process::Pid::from_raw(raw_pid)
                    .ok_or("escaped helper wrote an invalid pid")?;
            }
            if Instant::now() >= pid_deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "escaped child did not record its pid",
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        let mut cleanup = EscapedPidCleanup(Some(escaped_pid));
        std::thread::sleep(Duration::from_millis(250));
        let started = Instant::now();
        let next = runtime.infer(EmbeddingInvocation {
            purpose: EmbeddingPurpose::Document,
            text: "force a new request after the helper leader exits",
        });
        assert!(
            matches!(
                &next,
                Err(EmbeddingExecutableError::Process(ProcessError::Protocol))
            ) || matches!(
                &next,
                Err(EmbeddingExecutableError::Process(
                    ProcessError::UnsupportedLimit(crate::UnsupportedLimit::ProcessGroup)
                ))
            ),
            "unexpected error after escaped helper leader exit: {next:?}"
        );
        assert!(
            runtime
                .persistent_session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none(),
            "the session with an escaped child must not remain reusable"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "pipe shutdown waited for an escaped descendant to close inherited descriptors"
        );
        assert!(rustix::process::test_kill_process(escaped_pid).is_ok());
        rustix::process::kill_process(escaped_pid, rustix::process::Signal::KILL)?;
        cleanup.0 = None;
        drop(runtime);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn persistent_session_deadline_covers_a_blocked_stdin_write()
    -> Result<(), Box<dyn std::error::Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let root = std::env::temp_dir().join(format!(
            "backend-embedding-blocked-pipe-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let program = root.join("does-not-read-stdin.py");
        fs::write(
            &program,
            "#!/usr/bin/env python3\nimport time\ntime.sleep(30)\n",
        )?;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
        let executable = ToolchainArtifact::from_path(&program, Vec::new())?;
        let environment = ProcessEnvironment::new(vec![("PATH".into(), "/usr/bin:/bin".into())])?;
        let limits = ProcessLimits::new(64, 64, Duration::from_millis(100), 128)?
            .with_input_bytes_limit(2 * 1024 * 1024)?;
        let command = SupervisedCommand::for_authority_with_artifact(
            program,
            Vec::new(),
            environment,
            root.clone(),
            ProcessStdin::null(),
            executable,
            None,
            ProtocolDescriptor::persistent(),
            limits,
        )?;
        let cancelled = AtomicBool::new(false);
        let mut session = ProcessSupervisor::new(command)
            .start_byte_session(&cancelled, Instant::now() + Duration::from_secs(2))?;
        let request = vec![0x5a; 1024 * 1024];
        let started = Instant::now();
        let result = session.exchange_with_cancellation_flag_until(
            request,
            10,
            &AtomicBool::new(false),
            started + Duration::from_millis(100),
        );
        assert_eq!(result, Err(ProcessError::Deadline));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!session.is_alive_until(&cancelled, Instant::now() + Duration::from_secs(1))?);
        drop(session);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn supervised_embedding_rss_kib(supervisor_pid: u32) -> (Option<u64>, Option<u64>) {
        struct ProcessRow {
            pid: u32,
            parent: u32,
            group: u32,
            rss_kib: u64,
        }

        let output = match std::process::Command::new("/bin/ps")
            .args(["-axo", "pid=,ppid=,pgid=,rss=,command="])
            .output()
        {
            Ok(output) if output.status.success() => output,
            _ => return (None, None),
        };
        let Ok(snapshot) = std::str::from_utf8(&output.stdout) else {
            return (None, None);
        };
        let rows = snapshot
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let pid = fields.next()?.parse().ok()?;
                let parent = fields.next()?.parse().ok()?;
                let group = fields.next()?.parse().ok()?;
                let rss_kib = fields.next()?.parse().ok()?;
                Some(ProcessRow {
                    pid,
                    parent,
                    group,
                    rss_kib,
                })
            })
            .collect::<Vec<_>>();
        let supervisor_rss = rows
            .iter()
            .find(|row| row.pid == supervisor_pid)
            .map(|row| row.rss_kib);
        let supervisor_group = rows
            .iter()
            .find(|row| row.pid == supervisor_pid)
            .map(|row| row.group);
        // Resource ceilings add a shell wrapper that execs the helper. Discover the isolated
        // child process group instead of relying on the transient executable's command string.
        let helper_group = rows
            .iter()
            .find(|row| row.parent == supervisor_pid && Some(row.group) != supervisor_group)
            .map(|row| row.group);
        let helper_group_rss = helper_group.map(|group| {
            rows.iter()
                .filter(|row| row.group == group)
                .fold(0_u64, |total, row| total.saturating_add(row.rss_kib))
        });
        (supervisor_rss, helper_group_rss)
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "runs one real 256-item Metal page through the production process supervisor"]
    fn pinned_persistent_candle_max_page_runs_through_process_supervisor()
    -> Result<(), Box<dyn std::error::Error>> {
        let helper = PathBuf::from(std::env::var("BACKEND_EMBEDDING_PERSISTENT_HELPER")?);
        let model_path = PathBuf::from(std::env::var("BACKEND_EMBEDDING_MODEL_FILE")?);
        let tokenizer_path = PathBuf::from(std::env::var("BACKEND_EMBEDDING_TOKENIZER_FILE")?);
        let model_bytes = fs::read(&model_path)?;
        let tokenizer_bytes = fs::read(&tokenizer_path)?;
        assert_eq!(
            blake3::hash(&model_bytes).to_hex().as_str(),
            "8087e9bf97c265f8435ed268733ecf3791825ad24850fd5d84d89e32ee3a589a"
        );
        assert_eq!(
            blake3::hash(&tokenizer_bytes).to_hex().as_str(),
            "82483bb4f0bdb81779f295ecc5a93285d2156834e994a2169f9800e4c8f250c1"
        );
        let executable = ToolchainArtifact::from_path(&helper, Vec::new())?;
        let helper_identity = executable
            .identity()
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let model = EmbeddingArtifact::new(Arc::from(model_bytes));
        let tokenizer = EmbeddingArtifact::new(Arc::from(tokenizer_bytes));
        let dimensions = NonZeroU16::new(384).ok_or("dimensions")?;
        let maximum_text_bytes = NonZeroU32::new(65_536).ok_or("maximum text bytes")?;
        let spec = EmbeddingRuntimeSpecV1::new(
            model.identity().as_bytes(),
            [0x11; 32],
            tokenizer.identity().as_bytes(),
            executable.identity().to_bytes(),
            dimensions,
            EmbeddingNormalization::L2,
            maximum_text_bytes,
            [0x22; 32],
        );
        let root = std::env::temp_dir().join(format!(
            "backend-embedding-real-supervisor-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        fs::create_dir(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let environment = ProcessEnvironment::new(vec![
            ("BACKEND_EMBEDDING_DEVICE".into(), "metal".into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("LC_ALL".into(), "C".into()),
        ])?;
        let limits = ProcessLimits::new(
            2 * 1024 * 1024,
            64 * 1024,
            Duration::from_secs(90),
            20 * 1024 * 1024,
        )?
        .with_input_bytes_limit(20 * 1024 * 1024)?
        .with_process_count_limit(8)?
        .with_workspace_limit(256 * 1024 * 1024)?;
        let runtime = EmbeddingExecutable::activate_with_spec(
            spec,
            helper,
            vec!["--persistent-bem2-v1".into()],
            root.clone(),
            environment,
            limits,
            executable,
            model,
            tokenizer,
        )?;
        let mut base = "a happy person ".repeat(100);
        base.truncate(65_528);
        base.extend(std::iter::repeat_n('x', 65_528 - base.len()));
        let texts = (0..256)
            .map(|index| {
                let mut text = base.clone();
                text.push_str(&format!("{index:08x}"));
                text
            })
            .collect::<Vec<_>>();
        assert!(texts.iter().all(|text| text.len() == 65_536));
        let borrowed = texts.iter().map(String::as_str).collect::<Vec<_>>();
        let sampling = Arc::new(AtomicBool::new(true));
        let peak_rss = Arc::new(Mutex::new((None::<u64>, None::<u64>)));
        let sampling_flag = Arc::clone(&sampling);
        let peak_rss_sample = Arc::clone(&peak_rss);
        let supervisor_pid = std::process::id();
        let sampler = std::thread::spawn(move || {
            while sampling_flag.load(Ordering::Acquire) {
                let (supervisor_rss, helper_group_rss) =
                    supervised_embedding_rss_kib(supervisor_pid);
                let mut peaks = peak_rss_sample
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                peaks.0 = peaks.0.max(supervisor_rss);
                peaks.1 = peaks.1.max(helper_group_rss);
                drop(peaks);
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let cold_started = Instant::now();
        let cold_result = runtime.infer_batch(EmbeddingPurpose::Document, &borrowed);
        sampling.store(false, Ordering::Release);
        sampler.join().map_err(|_| "RSS sampler panicked")?;
        let cold = cold_result?;
        let cold_elapsed = cold_started.elapsed();
        let (supervisor_peak_rss_kib, helper_group_peak_rss_kib) = *peak_rss
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(supervisor_peak_rss_kib.is_some());
        assert!(helper_group_peak_rss_kib.is_some());
        assert_eq!(cold.len(), 256);
        for coordinates in &cold {
            assert!(coordinates.values().iter().all(|value| value.is_finite()));
            let norm = coordinates
                .values()
                .iter()
                .map(|value| f64::from(*value).powi(2))
                .sum::<f64>();
            assert!((norm - 1.0).abs() < 0.001, "norm={norm}");
        }
        let warm_started = Instant::now();
        let warm = runtime.infer_batch(EmbeddingPurpose::Document, &borrowed)?;
        let warm_elapsed = warm_started.elapsed();
        assert_eq!(cold, warm);
        eprintln!(
            "BEM2 production supervisor persistent max-page: device=metal, items=256, text_bytes=65536, cold_ms={}, exact_cache_warm_ms={}, supervisor_peak_rss_kib={}, helper_process_group_peak_rss_kib={}, helper={}, model={}, tokenizer={}",
            cold_elapsed.as_millis(),
            warm_elapsed.as_millis(),
            supervisor_peak_rss_kib.expect("sample supervisor RSS"),
            helper_group_peak_rss_kib.expect("sample helper process-group RSS"),
            helper_identity,
            model_path.display(),
            tokenizer_path.display(),
        );
        drop(runtime);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn exact_input_cache_reuses_coordinates_and_keeps_probe_uncached()
    -> Result<(), Box<dyn std::error::Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let counter_root = std::env::temp_dir().join(format!(
            "backend-embedding-call-count-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&counter_root)?;
        fs::set_permissions(&counter_root, fs::Permissions::from_mode(0o700))?;
        let counter = counter_root.join("calls.txt");
        let (root, _, mut runtime) = runtime_with_call_counter(&counter)?;
        let document = EmbeddingInvocation {
            purpose: EmbeddingPurpose::Document,
            text: "alpha",
        };
        let query = EmbeddingInvocation {
            purpose: EmbeddingPurpose::Query,
            text: "alpha",
        };
        let document_identity = EmbeddingInputIdentity::new(runtime.execution_identity(), document);
        let query_identity = EmbeddingInputIdentity::new(runtime.execution_identity(), query);
        assert_ne!(document_identity, query_identity);
        assert_ne!(
            document_identity,
            EmbeddingInputIdentity::new(
                runtime.execution_identity(),
                EmbeddingInvocation {
                    purpose: EmbeddingPurpose::Document,
                    text: "beta",
                }
            )
        );
        let mut changed_configuration = runtime.execution_identity();
        changed_configuration.options_digest = [0xA4; 32];
        assert_ne!(
            document_identity,
            EmbeddingInputIdentity::new(changed_configuration, document)
        );
        let mut changed_launch = runtime.execution_identity();
        changed_launch.launch_configuration = [0x5C; 32];
        assert_ne!(
            document_identity,
            EmbeddingInputIdentity::new(changed_launch, document)
        );
        assert_eq!(fs::read_to_string(&counter)?.lines().count(), 1); // activation probe

        let cold_started = Instant::now();
        let cold = runtime.infer(document)?;
        let cold_elapsed = cold_started.elapsed();
        assert_eq!(fs::read_to_string(&counter)?.lines().count(), 2);

        let warm_started = Instant::now();
        let warm = runtime.infer(document)?;
        let warm_elapsed = warm_started.elapsed();
        assert_eq!(fs::read_to_string(&counter)?.lines().count(), 2);
        assert!(Arc::ptr_eq(&cold.values, &warm.values));
        assert_eq!(cold.values(), warm.values());

        runtime.infer(query)?;
        runtime.infer(EmbeddingInvocation {
            purpose: EmbeddingPurpose::Document,
            text: "beta",
        })?;
        assert_eq!(fs::read_to_string(&counter)?.lines().count(), 4);
        runtime.probe_ready()?;
        assert_eq!(fs::read_to_string(&counter)?.lines().count(), 5);
        eprintln!(
            "embedding fixture: cold inference={}ms, exact cache hit={}us; one process avoided",
            cold_elapsed.as_millis(),
            warm_elapsed.as_micros()
        );

        drop(runtime);
        fs::remove_dir_all(root)?;
        fs::remove_dir_all(counter_root)?;
        Ok(())
    }

    #[test]
    fn durable_exact_input_cache_reuses_vectors_after_runtime_restart()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, program) = fixture()?;
        let calls = root.join("durable-calls.txt");
        let environment = || {
            ProcessEnvironment::new(vec![
                (
                    "BACKEND_EMBEDDING_CALL_COUNTER".into(),
                    calls.to_string_lossy().into_owned(),
                ),
                ("PATH".into(), "/usr/bin:/bin".into()),
            ])
        };

        let cache_directory = root.join("cache").join("embedding");
        fs::create_dir_all(&cache_directory)?;
        let directory =
            backend_platform::DirectoryCapability::open_or_create_private(&cache_directory)?;
        let (root, _, runtime) =
            activate_fixture(root.clone(), program.clone(), Vec::new(), environment()?)?;
        let session = runtime
            .open_durable_cache_session(directory)
            .ok_or("cache actor admission failed")?;
        let invocation = EmbeddingInvocation {
            purpose: EmbeddingPurpose::Document,
            text: "unchanged document",
        };
        let cold = runtime.infer_batch_with_cache_session(
            invocation.purpose,
            &[invocation.text],
            &AtomicBool::new(false),
            &session,
        )?[0]
            .clone();
        assert_eq!(fs::read_to_string(&calls)?.lines().count(), 2);
        drop(session);
        drop(runtime);

        let directory =
            backend_platform::DirectoryCapability::open_or_create_private(&cache_directory)?;
        let (root, _, reopened) =
            activate_fixture(root.clone(), program, Vec::new(), environment()?)?;
        let session = reopened
            .open_durable_cache_session(directory)
            .ok_or("cache actor reopen failed")?;
        let warm = reopened.infer_batch_with_cache_session(
            invocation.purpose,
            &[invocation.text],
            &AtomicBool::new(false),
            &session,
        )?[0]
            .clone();
        assert_eq!(cold.values(), warm.values());
        assert_eq!(fs::read_to_string(&calls)?.lines().count(), 3);
        reopened.infer_batch_with_cache_session(
            EmbeddingPurpose::Document,
            &["changed document"],
            &AtomicBool::new(false),
            &session,
        )?;
        reopened.infer_batch_with_cache_session(
            EmbeddingPurpose::Query,
            &["unchanged document"],
            &AtomicBool::new(false),
            &session,
        )?;
        assert_eq!(fs::read_to_string(&calls)?.lines().count(), 5);
        drop(session);
        drop(reopened);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn cache_sessions_are_scoped_to_the_borrowed_workspace_on_a_shared_runtime()
    -> Result<(), Box<dyn std::error::Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let counter_root = std::env::temp_dir().join(format!(
            "backend-embedding-session-scope-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&counter_root)?;
        fs::set_permissions(&counter_root, fs::Permissions::from_mode(0o700))?;
        let counter = counter_root.join("calls.txt");
        let (root, _, runtime) = runtime_with_call_counter(&counter)?;
        let first_directory = root.join("workspace-a-cache");
        let second_directory = root.join("workspace-b-cache");
        let first_capability =
            backend_platform::DirectoryCapability::open_or_create_private(&first_directory)?;
        let second_capability =
            backend_platform::DirectoryCapability::open_or_create_private(&second_directory)?;
        let first = runtime
            .open_durable_cache_session(first_capability)
            .ok_or("first cache actor admission failed")?;
        let second = runtime
            .open_durable_cache_session(second_capability)
            .ok_or("second cache actor admission failed")?;
        let cancelled = AtomicBool::new(false);
        let text = "shared runtime, separate workspace cache";
        let identity = EmbeddingInputIdentity::new(
            runtime.execution_identity(),
            EmbeddingInvocation {
                purpose: EmbeddingPurpose::Document,
                text,
            },
        );
        let wait_for_entry = |directory: &Path| {
            let path = directory.join(format!("{}.vec", hex(&identity.as_bytes())));
            let deadline = Instant::now() + Duration::from_secs(2);
            while !path.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(path.exists(), "cache broker did not commit {path:?}");
        };
        let invoke = |session: &EmbeddingCacheSession| {
            runtime.infer_batch_with_cache_session(
                EmbeddingPurpose::Document,
                &[text],
                &cancelled,
                session,
            )
        };

        let first_cold = invoke(&first)?;
        assert_eq!(fs::read_to_string(&counter)?.lines().count(), 2);
        wait_for_entry(&first_directory);
        runtime.clear_inference_cache_for_test();
        let second_cold = invoke(&second)?;
        assert_eq!(fs::read_to_string(&counter)?.lines().count(), 3);
        assert_eq!(first_cold, second_cold);
        wait_for_entry(&second_directory);
        runtime.clear_inference_cache_for_test();
        assert_eq!(invoke(&first)?, first_cold);
        assert_eq!(fs::read_to_string(&counter)?.lines().count(), 3);
        runtime.clear_inference_cache_for_test();
        assert_eq!(invoke(&second)?, second_cold);
        assert_eq!(fs::read_to_string(&counter)?.lines().count(), 3);

        drop((first, second, runtime));
        fs::remove_dir_all(root)?;
        fs::remove_dir_all(counter_root)?;
        Ok(())
    }

    #[test]
    fn foreign_runtime_cache_sessions_are_ignored_without_mutating_their_files()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, _, runtime) = runtime()?;
        let identity = runtime.execution_identity();
        let mut different_dimension = identity;
        different_dimension.dimension += 1;
        let mut different_recipe = identity;
        different_recipe.recipe[0] ^= 1;
        let mut different_launch = identity;
        different_launch.launch_configuration[0] ^= 1;
        for (name, foreign_identity, text) in [
            ("foreign-dimension-cache", different_dimension, "alpha beta"),
            ("foreign-recipe-cache", different_recipe, "gamma alpha"),
            ("foreign-launch-cache", different_launch, "beta gamma"),
        ] {
            let cache_root = root.join(name);
            fs::create_dir(&cache_root)?;
            fs::set_permissions(&cache_root, fs::Permissions::from_mode(0o700))?;
            let foreign_session = EmbeddingCacheSession::open(
                backend_platform::DirectoryCapability::open(&cache_root)?,
                foreign_identity,
            )
            .ok_or("foreign cache actor admission failed")?;

            let result = runtime.infer_batch_with_cache_session(
                EmbeddingPurpose::Document,
                &[text],
                &AtomicBool::new(false),
                &foreign_session,
            )?;
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].values().len(), 2);
            drop(foreign_session);
            assert_eq!(
                fs::read_dir(&cache_root)?
                    .filter_map(Result::ok)
                    .filter(|entry| entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "vec"))
                    .count(),
                0,
                "foreign identity {name} must not read or mutate its cache files"
            );
        }

        drop(runtime);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn launch_configuration_binds_arguments_and_environment_but_not_private_artifact_paths()
    -> Result<(), Box<dyn std::error::Error>> {
        let first = ProcessEnvironment::new(vec![
            (MODEL_FILE_ENV.into(), "/private/first/model.bin".into()),
            (
                TOKENIZER_FILE_ENV.into(),
                "/private/first/tokenizer.bin".into(),
            ),
            ("EMBEDDING_MODE".into(), "float32".into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ])?;
        let relocated = ProcessEnvironment::new(vec![
            (MODEL_FILE_ENV.into(), "/private/second/model.bin".into()),
            (
                TOKENIZER_FILE_ENV.into(),
                "/private/second/tokenizer.bin".into(),
            ),
            ("EMBEDDING_MODE".into(), "float32".into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ])?;
        let changed_mode = ProcessEnvironment::new(vec![
            (MODEL_FILE_ENV.into(), "/private/second/model.bin".into()),
            (
                TOKENIZER_FILE_ENV.into(),
                "/private/second/tokenizer.bin".into(),
            ),
            ("EMBEDDING_MODE".into(), "bf16".into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ])?;
        let arguments = ["--device=cpu".to_owned(), "--precision=float32".to_owned()];
        let program = Path::new("/opt/backend/embedding-worker");
        let workspace = Path::new("/var/lib/backend/embedding-work");
        let limits = ProcessLimits::new(128, 64, Duration::from_secs(2), 256)?;
        let changed_limits = ProcessLimits::new(128, 64, Duration::from_secs(3), 256)?;
        assert_eq!(
            embedding_launch_configuration(program, workspace, &arguments, &first, limits),
            embedding_launch_configuration(program, workspace, &arguments, &relocated, limits)
        );
        assert_ne!(
            embedding_launch_configuration(program, workspace, &arguments, &first, limits),
            embedding_launch_configuration(program, workspace, &arguments, &changed_mode, limits)
        );
        assert_ne!(
            embedding_launch_configuration(program, workspace, &arguments, &first, limits),
            embedding_launch_configuration(
                program,
                workspace,
                &["--device=cpu".to_owned()],
                &first,
                limits
            )
        );
        assert_ne!(
            embedding_launch_configuration(program, workspace, &arguments, &first, limits),
            embedding_launch_configuration(
                Path::new("/opt/backend/other-worker"),
                workspace,
                &arguments,
                &first,
                limits
            )
        );
        assert_ne!(
            embedding_launch_configuration(program, workspace, &arguments, &first, limits),
            embedding_launch_configuration(
                program,
                Path::new("/var/lib/backend/other-work"),
                &arguments,
                &first,
                limits
            )
        );
        assert_ne!(
            embedding_launch_configuration(program, workspace, &arguments, &first, limits),
            embedding_launch_configuration(program, workspace, &arguments, &first, changed_limits)
        );
        Ok(())
    }

    #[test]
    fn batch_coordinate_retention_has_a_checked_hard_ceiling() {
        assert!(check_batch_result_limit(2_048, 8_192).is_ok());
        assert!(matches!(
            check_batch_result_limit(2_049, 8_192),
            Err(EmbeddingExecutableError::BatchResultLimit {
                observed: 67_141_632,
                maximum: MAX_EMBEDDING_BATCH_COORDINATE_BYTES,
            })
        ));
    }

    #[test]
    fn artifact_staging_checks_deadline_and_removes_partial_file()
    -> Result<(), Box<dyn std::error::Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let root = std::env::temp_dir().join(format!(
            "backend-embedding-artifact-stage-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root)?;
        #[cfg(unix)]
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let directory = backend_platform::DirectoryCapability::open(&root)?;
        let deadline = Instant::now() + Duration::from_millis(25);
        let bytes = vec![0x5A; 128 * 1024];
        let result = write_private_artifact_atomic(
            &directory,
            "model.bin",
            &bytes,
            deadline,
            |file, chunk| {
                use std::io::Write as _;
                file.write_all(chunk)?;
                std::thread::sleep(Duration::from_millis(40));
                Ok(())
            },
        );
        assert!(matches!(
            result,
            Err(EmbeddingExecutableError::Process(ProcessError::Deadline))
        ));
        assert!(directory.entries(8)?.is_empty());
        drop(directory);
        fs::remove_dir(root)?;
        Ok(())
    }

    #[test]
    fn activation_rejects_oversized_arguments_before_workspace_or_child_side_effects()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, program) = fixture()?;
        let call_counter = root.join("activation-calls.txt");
        let environment = ProcessEnvironment::new(vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            (
                "BACKEND_EMBEDDING_CALL_COUNTER".into(),
                call_counter.to_string_lossy().into_owned(),
            ),
        ])?;
        let limits = ProcessLimits::new(256, 64, Duration::from_secs(2), 256)?
            .with_input_bytes_limit(2_048)?;
        let executable = ToolchainArtifact::from_path(&program, Vec::new())?;
        let model = EmbeddingArtifact::new(Arc::from(MODEL_BYTES));
        let tokenizer = EmbeddingArtifact::new(Arc::from(TOKENIZER_BYTES));
        let spec = EmbeddingRuntimeSpecV1::new(
            model.identity().as_bytes(),
            [7; 32],
            tokenizer.identity().as_bytes(),
            executable.identity().to_bytes(),
            NonZeroU16::new(2).ok_or("dimensions")?,
            EmbeddingNormalization::L2,
            NonZeroU32::new(128).ok_or("text limit")?,
            [0xA5; 32],
        );
        for arguments in [vec!["x".to_owned(); 257], vec!["x".repeat(64 * 1024 + 1)]] {
            let result = EmbeddingExecutable::activate_with_spec(
                spec,
                program.clone(),
                arguments,
                root.clone(),
                environment.clone(),
                limits,
                executable.clone(),
                model.clone(),
                tokenizer.clone(),
            );
            assert!(matches!(
                result,
                Err(EmbeddingExecutableError::Process(
                    ProcessError::ConfigurationLimit
                ))
            ));
            assert_eq!(fs::read_dir(&root)?.count(), 1);
            assert!(!call_counter.exists());
        }
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn batch_input_bytes_are_bounded_before_identity_work_and_poll_cancellation() {
        let texts = ["same", "same", "other"];
        assert!(matches!(
            validate_batch_input_bytes(
                &texts,
                8,
                12,
                None,
                Instant::now() + Duration::from_secs(1),
            ),
            Err(EmbeddingExecutableError::BatchInputBytesLimit {
                observed: 13,
                maximum: 12,
            })
        ));

        let cancelled = AtomicBool::new(true);
        assert!(matches!(
            validate_batch_input_bytes(
                &texts,
                8,
                64,
                Some(&cancelled),
                Instant::now() + Duration::from_secs(1),
            ),
            Err(EmbeddingExecutableError::Process(ProcessError::Cancelled))
        ));
    }

    #[test]
    fn batch_activation_requires_one_maximum_text_to_fit_bem2_framing() {
        let maximum_text_bytes = 128;
        let bem1_extent = REQUEST_HEADER_BYTES + maximum_text_bytes;
        assert!(batch_single_request_fits(
            maximum_text_bytes,
            bem1_extent + BATCH_ITEM_HEADER_BYTES
        ));
        assert!(!batch_single_request_fits(maximum_text_bytes, bem1_extent));
    }

    #[test]
    fn batch_deduplicates_exact_documents_and_saves_a_supervised_process()
    -> Result<(), Box<dyn std::error::Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let counter_root = std::env::temp_dir().join(format!(
            "backend-embedding-batch-count-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&counter_root)?;
        fs::set_permissions(&counter_root, fs::Permissions::from_mode(0o700))?;
        let counter = counter_root.join("calls.txt");
        let (root, _, runtime) = runtime_with_call_counter(&counter)?;
        assert_eq!(runtime.batch_protocol(), EmbeddingBatchProtocol::BatchV2);
        assert_eq!(fs::read_to_string(&counter)?.lines().count(), 1); // activation probe

        let cold_started = Instant::now();
        let cold = runtime.infer_batch(EmbeddingPurpose::Document, &["alpha", "alpha", "beta"])?;
        let cold_elapsed = cold_started.elapsed();
        assert_eq!(cold.len(), 3);
        assert!(Arc::ptr_eq(&cold[0].values, &cold[1].values));
        assert_eq!(fs::read_to_string(&counter)?.lines().count(), 2);

        let warm_started = Instant::now();
        let warm = runtime.infer_batch(EmbeddingPurpose::Document, &["alpha", "alpha", "beta"])?;
        let warm_elapsed = warm_started.elapsed();
        assert!(Arc::ptr_eq(&cold[0].values, &warm[0].values));
        assert_eq!(fs::read_to_string(&counter)?.lines().count(), 2);
        eprintln!(
            "embedding batch fixture: 3 inputs / 2 unique -> 1 supervised call; cold={}us, exact cache hit={}us; avoids 2 single-item calls",
            cold_elapsed.as_micros(),
            warm_elapsed.as_micros()
        );

        drop(runtime);
        fs::remove_dir_all(root)?;
        fs::remove_dir_all(counter_root)?;
        Ok(())
    }

    #[test]
    fn concurrent_duplicate_batches_reuse_the_owner_transaction()
    -> Result<(), Box<dyn std::error::Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let counter_root = std::env::temp_dir().join(format!(
            "backend-embedding-concurrent-batch-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&counter_root)?;
        fs::set_permissions(&counter_root, fs::Permissions::from_mode(0o700))?;
        let active = counter_root.join("active.txt");
        let calls = counter_root.join("calls.txt");
        let (root, _, runtime) = runtime_with_counters(Some(&active), Some(&calls))?;
        assert_eq!(fs::read_to_string(&calls)?.lines().count(), 1); // activation probe

        let runtime = Arc::new(runtime);
        let start = Arc::new(std::sync::Barrier::new(9));
        let mut callers = Vec::new();
        for _ in 0..8 {
            let runtime = Arc::clone(&runtime);
            let start = Arc::clone(&start);
            callers.push(std::thread::spawn(move || {
                start.wait();
                runtime.infer_batch(EmbeddingPurpose::Document, &["alpha", "beta"])
            }));
        }
        start.wait();
        let mut shared: Option<[Arc<[f32]>; 2]> = None;
        for caller in callers {
            let coordinates = caller
                .join()
                .map_err(|_| io::Error::other("concurrent embedding caller panicked"))??;
            assert_eq!(coordinates.len(), 2);
            if let Some(expected) = shared.as_ref() {
                assert!(Arc::ptr_eq(&expected[0], &coordinates[0].values));
                assert!(Arc::ptr_eq(&expected[1], &coordinates[1].values));
            } else {
                shared = Some([
                    Arc::clone(&coordinates[0].values),
                    Arc::clone(&coordinates[1].values),
                ]);
            }
        }
        assert_eq!(fs::read_to_string(&calls)?.lines().count(), 2); // one cold batch
        let active_state = fs::read_to_string(&active)?;
        let observed = active_state
            .split_whitespace()
            .map(str::parse::<usize>)
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(observed.as_slice(), [0, 1]);

        drop(runtime);
        fs::remove_dir_all(root)?;
        fs::remove_dir_all(counter_root)?;
        Ok(())
    }

    #[test]
    fn partial_and_reordered_batch_responses_are_rejected_without_cache_fill()
    -> Result<(), Box<dyn std::error::Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let counter_root = std::env::temp_dir().join(format!(
            "backend-embedding-batch-fault-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&counter_root)?;
        fs::set_permissions(&counter_root, fs::Permissions::from_mode(0o700))?;
        let counter = counter_root.join("calls.txt");
        let fault_file = counter_root.join("fault.txt");
        fs::write(&fault_file, "ok")?;
        let (root, _, runtime) = runtime_with_fault_file(None, Some(&counter), Some(&fault_file))?;
        assert_eq!(runtime.batch_protocol(), EmbeddingBatchProtocol::BatchV2);

        fs::write(&fault_file, "partial")?;
        assert!(matches!(
            runtime.infer_batch(EmbeddingPurpose::Document, &["alpha", "beta"]),
            Err(EmbeddingExecutableError::BatchResponseCount {
                expected: 2,
                observed: 1
            })
        ));
        assert_eq!(fs::read_to_string(&counter)?.lines().count(), 2);

        fs::write(&fault_file, "identity")?;
        assert!(matches!(
            runtime.infer_batch(EmbeddingPurpose::Document, &["alpha", "beta"]),
            Err(EmbeddingExecutableError::BatchResponseIdentity { index: 0 })
        ));
        assert_eq!(fs::read_to_string(&counter)?.lines().count(), 3);

        fs::write(&fault_file, "ok")?;
        runtime.infer_batch(EmbeddingPurpose::Document, &["alpha", "beta"])?;
        assert_eq!(fs::read_to_string(&counter)?.lines().count(), 4);
        drop(runtime);
        fs::remove_dir_all(root)?;
        fs::remove_dir_all(counter_root)?;
        Ok(())
    }

    #[test]
    fn cancellation_terminates_and_reaps_a_supervised_batch()
    -> Result<(), Box<dyn std::error::Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let counter_root = std::env::temp_dir().join(format!(
            "backend-embedding-batch-cancel-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&counter_root)?;
        fs::set_permissions(&counter_root, fs::Permissions::from_mode(0o700))?;
        let counter = counter_root.join("calls.txt");
        let batch_gate = counter_root.join("batch-gate");
        let (root, _, runtime) = runtime_with_batch_gate(&counter, &batch_gate)?;
        assert_eq!(runtime.batch_protocol(), EmbeddingBatchProtocol::BatchV2);
        let runtime = Arc::new(runtime);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_runtime = Arc::clone(&runtime);
        let worker_cancelled = Arc::clone(&cancelled);
        let worker = std::thread::spawn(move || {
            worker_runtime.infer_batch_with_cancellation_flag(
                EmbeddingPurpose::Document,
                &["alpha", "beta"],
                &worker_cancelled,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while !batch_gate.with_extension("started").is_file() {
            if Instant::now() >= deadline {
                cancelled.store(true, Ordering::Release);
                let _ = worker.join();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "supervised child did not reach the cancellation gate",
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        cancelled.store(true, Ordering::Release);
        assert!(matches!(
            worker
                .join()
                .map_err(|_| io::Error::other("batch caller panicked"))?,
            Err(EmbeddingExecutableError::Process(ProcessError::Cancelled))
        ));
        fs::write(batch_gate.with_extension("release"), "release")?;
        let recovered = runtime.infer_batch(EmbeddingPurpose::Document, &["alpha", "beta"])?;
        assert_eq!(recovered.len(), 2);
        assert_eq!(fs::read_to_string(&counter)?.lines().count(), 3); // probe, cancelled, retry

        drop(runtime);
        fs::remove_dir_all(root)?;
        fs::remove_dir_all(counter_root)?;
        Ok(())
    }

    #[test]
    fn artifact_workspace_rejects_an_expired_activation_budget_before_writing()
    -> Result<(), Box<dyn std::error::Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let workspace = std::env::temp_dir().join(format!(
            "backend-embedding-expired-artifacts-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&workspace)?;
        fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700))?;
        let model = EmbeddingArtifact::new(Arc::from(MODEL_BYTES));
        let tokenizer = EmbeddingArtifact::new(Arc::from(TOKENIZER_BYTES));

        let result = EmbeddingArtifactWorkspace::create(
            &workspace,
            &model,
            &tokenizer,
            Instant::now() - Duration::from_millis(1),
        );

        assert!(matches!(
            result,
            Err(EmbeddingExecutableError::Process(ProcessError::Deadline))
        ));
        assert_eq!(fs::read_dir(&workspace)?.count(), 0);
        fs::remove_dir(&workspace)?;
        Ok(())
    }

    #[test]
    fn revocation_and_text_bounds_fail_before_execution() -> Result<(), Box<dyn std::error::Error>>
    {
        let (root, _, mut runtime) = runtime()?;
        assert!(matches!(
            runtime.infer(EmbeddingInvocation {
                purpose: EmbeddingPurpose::Query,
                text: &"x".repeat(129)
            }),
            Err(EmbeddingExecutableError::TextLimit { .. })
        ));
        let mut corrupted_tokenizer = TOKENIZER_BYTES.to_vec();
        corrupted_tokenizer[0] ^= 1;
        fs::write(
            runtime.artifact_workspace.tokenizer_path(),
            corrupted_tokenizer,
        )?;
        assert!(matches!(
            runtime.infer(EmbeddingInvocation {
                purpose: EmbeddingPurpose::Query,
                text: "alpha"
            }),
            Err(EmbeddingExecutableError::ArtifactDrift {
                artifact: "tokenizer"
            })
        ));
        fs::write(runtime.artifact_workspace.tokenizer_path(), TOKENIZER_BYTES)?;
        assert!(
            runtime
                .infer(EmbeddingInvocation {
                    purpose: EmbeddingPurpose::Query,
                    text: "alpha"
                })
                .is_ok()
        );
        runtime.revoke();
        assert!(matches!(
            runtime.infer(EmbeddingInvocation {
                purpose: EmbeddingPurpose::Query,
                text: "x"
            }),
            Err(EmbeddingExecutableError::Revoked)
        ));
        drop(runtime);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn shared_inference_gate_bounds_child_processes_and_releases_after_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let counter_root = std::env::temp_dir().join(format!(
            "backend-embedding-sentinel-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&counter_root)?;
        fs::set_permissions(&counter_root, fs::Permissions::from_mode(0o700))?;
        let counter = counter_root.join("active.txt");
        let (root, _, runtime) = runtime_with_counter(Some(&counter))?;
        assert_eq!(runtime.maximum_concurrent_inferences(), 1);

        let runtime = Arc::new(runtime);
        let start = Arc::new(std::sync::Barrier::new(3));
        let mut callers = Vec::new();
        for text in ["alpha", "beta"] {
            let runtime = Arc::clone(&runtime);
            let start = Arc::clone(&start);
            callers.push(std::thread::spawn(move || {
                start.wait();
                runtime.infer(EmbeddingInvocation {
                    purpose: EmbeddingPurpose::Document,
                    text,
                })
            }));
        }
        start.wait();
        for caller in callers {
            caller
                .join()
                .map_err(|_| io::Error::other("embedding caller panicked"))??;
        }
        let observed = fs::read_to_string(&counter)?;
        let values = observed
            .split_whitespace()
            .map(str::parse::<usize>)
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(values.as_slice(), [0, 1]);

        fs::write(runtime.artifact_workspace.tokenizer_path(), b"damaged")?;
        assert!(matches!(
            runtime.infer(EmbeddingInvocation {
                purpose: EmbeddingPurpose::Query,
                text: "alpha"
            }),
            Err(EmbeddingExecutableError::ArtifactDrift {
                artifact: "tokenizer"
            })
        ));
        fs::write(runtime.artifact_workspace.tokenizer_path(), TOKENIZER_BYTES)?;
        assert!(
            runtime
                .infer(EmbeddingInvocation {
                    purpose: EmbeddingPurpose::Query,
                    text: "alpha"
                })
                .is_ok()
        );

        drop(runtime);
        fs::remove_dir_all(root)?;
        fs::remove_file(counter)?;
        fs::remove_file(counter_root.join("active.txt.lock"))?;
        fs::remove_dir(counter_root)?;
        Ok(())
    }

    #[test]
    fn inference_gate_has_a_typed_finite_wait_and_releases_its_permit()
    -> Result<(), Box<dyn std::error::Error>> {
        let gate = InferenceAdmissionGate::new();
        let permit = gate.acquire(None, Instant::now() + Duration::from_secs(1))?;
        assert!(matches!(
            gate.acquire(None, Instant::now() + Duration::from_millis(10)),
            Err(EmbeddingExecutableError::Process(ProcessError::Deadline))
        ));
        drop(permit);
        assert!(
            gate.acquire(None, Instant::now() + Duration::from_secs(1))
                .is_ok()
        );
        Ok(())
    }

    #[test]
    fn cancelled_batch_waiter_exits_the_gate_without_waiting_for_its_deadline()
    -> Result<(), Box<dyn std::error::Error>> {
        let gate = Arc::new(InferenceAdmissionGate::new());
        let permit = gate.acquire(None, Instant::now() + Duration::from_secs(3))?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let (entered_sender, entered_receiver) = std::sync::mpsc::sync_channel(1);
        let (finished_sender, finished_receiver) = std::sync::mpsc::sync_channel(1);
        let waiter_gate = Arc::clone(&gate);
        let waiter_cancelled = Arc::clone(&cancelled);
        let waiter = std::thread::spawn(move || {
            entered_sender.send(()).expect("notify test thread entered");
            let started = Instant::now();
            let result = waiter_gate
                .acquire(
                    Some(&waiter_cancelled),
                    Instant::now() + Duration::from_secs(3),
                )
                .map(drop);
            finished_sender
                .send((result, started.elapsed()))
                .expect("send cancellation result");
        });
        entered_receiver.recv()?;
        std::thread::sleep(Duration::from_millis(20));
        cancelled.store(true, Ordering::Release);
        let (result, elapsed) = finished_receiver.recv_timeout(Duration::from_millis(500))?;
        assert!(matches!(
            result,
            Err(EmbeddingExecutableError::Process(ProcessError::Cancelled))
        ));
        assert!(elapsed < Duration::from_millis(500));
        waiter
            .join()
            .map_err(|_| io::Error::other("cancelled gate waiter panicked"))?;
        drop(permit);
        assert!(
            gate.acquire(None, Instant::now() + Duration::from_secs(1))
                .is_ok()
        );
        Ok(())
    }
}
