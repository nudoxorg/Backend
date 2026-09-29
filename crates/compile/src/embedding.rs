//! Bounded cold-process protocol for an external embedding model runtime.
//!
//! This adapter owns immutable model and tokenizer bytes plus a content-checked executable. Every
//! inference is supervised under explicit input, output, deadline, process, memory, and workspace
//! limits. A runtime becomes active only after the executable answers a real self-test request.

use crate::{
    Cancellation, ProcessEnvironment, ProcessError, ProcessLimits, ProcessStdin, ProcessSupervisor,
    ProcessTerminal, ProtocolDescriptor, SupervisedCommand, ToolchainArtifact,
};
#[cfg(unix)]
use std::fs;
use std::{
    collections::{HashMap, VecDeque},
    fmt,
    io::{self, Read},
    num::{NonZeroU16, NonZeroU32},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
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
    wait_budget: Duration,
}

impl InferenceAdmissionGate {
    fn new(wait_budget: Duration) -> Self {
        Self {
            occupied: Mutex::new(false),
            available: Condvar::new(),
            wait_budget,
        }
    }

    fn acquire(
        &self,
        cancelled: Option<&AtomicBool>,
    ) -> Result<InferenceAdmissionPermit<'_>, EmbeddingExecutableError> {
        check_cancelled(cancelled)?;
        let deadline = Instant::now()
            .checked_add(self.wait_budget)
            .ok_or(EmbeddingExecutableError::InferenceAdmissionTimeout)?;
        let mut occupied = self
            .occupied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *occupied {
            check_cancelled(cancelled)?;
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(EmbeddingExecutableError::InferenceAdmissionTimeout);
            };
            if remaining.is_zero() {
                return Err(EmbeddingExecutableError::InferenceAdmissionTimeout);
            }
            let (next, timeout) = self
                .available
                .wait_timeout(occupied, remaining.min(Duration::from_millis(10)))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            occupied = next;
            check_cancelled(cancelled)?;
            if timeout.timed_out() && Instant::now() >= deadline {
                return Err(EmbeddingExecutableError::InferenceAdmissionTimeout);
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
    #[cfg(windows)]
    name: String,
    directory: PathBuf,
    model_path: PathBuf,
    tokenizer_path: PathBuf,
    model_length: usize,
    tokenizer_length: usize,
    #[cfg(windows)]
    parent: backend_platform::win32::workspace_fs::WorkspaceRoot,
    #[cfg(windows)]
    directory_handle: backend_platform::win32::workspace_fs::WorkspaceRoot,
}

impl EmbeddingArtifactWorkspace {
    fn create(
        workspace: &Path,
        model: &EmbeddingArtifact,
        tokenizer: &EmbeddingArtifact,
    ) -> Result<Self, EmbeddingExecutableError> {
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

        #[cfg(unix)]
        create_private_artifact_directory(&directory)?;
        #[cfg(windows)]
        let (parent, directory_handle) = {
            use backend_platform::win32::workspace_fs::WorkspaceRoot;
            let parent = WorkspaceRoot::open(workspace)
                .map_err(EmbeddingExecutableError::ArtifactWorkspace)?;
            let child = parent
                .create_child_dir_exclusive(&name)
                .map_err(EmbeddingExecutableError::ArtifactWorkspace)?;
            (parent, child)
        };
        #[cfg(not(any(unix, windows)))]
        return Err(EmbeddingExecutableError::ArtifactWorkspace(io::Error::new(
            io::ErrorKind::Unsupported,
            "private embedding artifact workspaces are unsupported on this platform",
        )));

        let artifacts = Self {
            #[cfg(windows)]
            name,
            model_path: directory.join(MODEL_FILE_NAME),
            tokenizer_path: directory.join(TOKENIZER_FILE_NAME),
            model_length: model.bytes().len(),
            tokenizer_length: tokenizer.bytes().len(),
            directory,
            #[cfg(windows)]
            parent,
            #[cfg(windows)]
            directory_handle,
        };
        artifacts.write_artifact(MODEL_FILE_NAME, model.bytes())?;
        artifacts.write_artifact(TOKENIZER_FILE_NAME, tokenizer.bytes())?;
        artifacts.verify_artifact(MODEL_FILE_NAME, model.identity, model.bytes().len())?;
        artifacts.verify_artifact(
            TOKENIZER_FILE_NAME,
            tokenizer.identity,
            tokenizer.bytes().len(),
        )?;
        Ok(artifacts)
    }

    fn write_artifact(&self, name: &str, bytes: &[u8]) -> Result<(), EmbeddingExecutableError> {
        #[cfg(unix)]
        backend_platform::durable::write_private_atomic(&self.directory.join(name), bytes)
            .map_err(EmbeddingExecutableError::ArtifactWorkspace)?;
        #[cfg(windows)]
        {
            use std::io::Write as _;

            let mut file = self
                .directory_handle
                .create_file_exclusive(&[name])
                .map_err(EmbeddingExecutableError::ArtifactWorkspace)?;
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(EmbeddingExecutableError::ArtifactWorkspace)?;
            self.directory_handle
                .flush_dir()
                .map_err(EmbeddingExecutableError::ArtifactWorkspace)?;
        }
        Ok(())
    }

    fn verify(
        &self,
        model: EmbeddingArtifactId,
        tokenizer: EmbeddingArtifactId,
    ) -> Result<(), EmbeddingExecutableError> {
        self.verify_artifact(MODEL_FILE_NAME, model, self.model_length)?;
        self.verify_artifact(TOKENIZER_FILE_NAME, tokenizer, self.tokenizer_length)
    }

    fn verify_artifact(
        &self,
        name: &str,
        expected: EmbeddingArtifactId,
        expected_length: usize,
    ) -> Result<(), EmbeddingExecutableError> {
        let path = self.directory.join(name);
        let mut file = backend_platform::durable::open_private_read(&path)
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
        Ok(())
    }

    fn model_path(&self) -> &Path {
        &self.model_path
    }

    fn tokenizer_path(&self) -> &Path {
        &self.tokenizer_path
    }
}

#[cfg(unix)]
fn create_private_artifact_directory(path: &Path) -> Result<(), EmbeddingExecutableError> {
    use std::os::unix::fs::DirBuilderExt as _;

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(path)
        .map_err(EmbeddingExecutableError::ArtifactWorkspace)?;
    if let Err(error) = backend_platform::durable::ensure_private_directory(path) {
        let _ = fs::remove_dir(path);
        return Err(EmbeddingExecutableError::ArtifactWorkspace(error));
    }
    Ok(())
}

impl Drop for EmbeddingArtifactWorkspace {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = backend_platform::durable::remove_private(&self.model_path);
            let _ = backend_platform::durable::remove_private(&self.tokenizer_path);
            if fs::remove_dir(&self.directory).is_ok() {
                let _ = backend_platform::durable::sync_parent(&self.directory);
            }
        }
        #[cfg(windows)]
        {
            let _ = self.parent.remove_dir_tree(&[&self.name]);
        }
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

    /// Upper bound for cache storage, batch request/output scratch, decoded coordinates, and
    /// per-input batch metadata. Compiler staging reserves encoded BVE1 payloads separately.
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
    /// active runtime.
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
        if maximum_text_bytes == 0 {
            return Err(EmbeddingExecutableError::ZeroTextLimit);
        }
        let _maximum_text_bytes = u32::try_from(maximum_text_bytes)
            .map_err(|_| EmbeddingExecutableError::RequestExtent)?;
        executable
            .verify_path(&program)
            .map_err(EmbeddingExecutableError::Process)?;
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
        let artifact_workspace =
            EmbeddingArtifactWorkspace::create(&workspace, &model, &tokenizer)?;
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
            inference_gate: InferenceAdmissionGate::new(process_limits.wall_time()),
            inference_cache: Mutex::new(EmbeddingInferenceCache::default()),
            batch_protocol: EmbeddingBatchProtocol::SingleV1,
            active: true,
        };
        if let Err(error) = runtime.probe_ready() {
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
        if !self.active {
            return Err(EmbeddingExecutableError::Revoked);
        }
        let probe = EmbeddingInvocation {
            purpose: EmbeddingPurpose::Query,
            text: "backend embedding readiness",
        };
        let second_probe = EmbeddingInvocation {
            purpose: EmbeddingPurpose::Query,
            text: "backend embedding batch check",
        };
        let probes = [probe, second_probe];
        let batch = probes.map(|invocation| {
            (
                EmbeddingInputIdentity::new(self.execution_identity(), invocation),
                invocation.text,
            )
        });
        if batch_single_request_fits(self.maximum_text_bytes, self.process_limits.input_bytes())
            && let Ok(coordinates) = self.run_batch_v2(&batch, probe.purpose, None)
            && coordinates.len() == probes.len()
        {
            self.batch_protocol = EmbeddingBatchProtocol::BatchV2;
            return Ok(());
        }
        self.batch_protocol = EmbeddingBatchProtocol::SingleV1;
        self.infer_inner(probe, false).map(|_| ())
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
        if self.batch_protocol == EmbeddingBatchProtocol::BatchV2 {
            return self
                .infer_batch(invocation.purpose, &[invocation.text])?
                .into_iter()
                .next()
                .ok_or(EmbeddingExecutableError::Protocol);
        }
        self.infer_inner(invocation, true)
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
        self.infer_batch_inner(purpose, texts, None)
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
        self.infer_batch_inner(purpose, texts, Some(cancelled))
    }

    fn infer_batch_inner(
        &self,
        purpose: EmbeddingPurpose,
        texts: &[&str],
        cancelled: Option<&AtomicBool>,
    ) -> Result<Vec<EmbeddingCoordinates>, EmbeddingExecutableError> {
        if !self.active {
            return Err(EmbeddingExecutableError::Revoked);
        }
        if texts.len() > MAX_EMBEDDING_BATCH_INPUTS {
            return Err(EmbeddingExecutableError::BatchInputLimit {
                observed: texts.len(),
                maximum: MAX_EMBEDDING_BATCH_INPUTS,
            });
        }
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        check_cancelled(cancelled)?;
        for text in texts {
            if text.len() > self.maximum_text_bytes {
                return Err(EmbeddingExecutableError::TextLimit {
                    observed: text.len(),
                    maximum: self.maximum_text_bytes,
                });
            }
        }

        // Cache hits are accepted only after every mutable artifact path has been checked against
        // the activated manifest. Cache residency never becomes executable/model authority.
        self.artifact_workspace
            .verify(self.model.identity, self.tokenizer.identity)?;
        self.executable
            .verify_path(&self.program)
            .map_err(EmbeddingExecutableError::Process)?;

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
            let invocation = EmbeddingInvocation { purpose, text };
            let identity = EmbeddingInputIdentity::new(self.execution_identity(), invocation);
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

        let mut unique_results = vec![None; unique_inputs.len()];
        let mut misses = Vec::new();
        misses
            .try_reserve_exact(unique_inputs.len())
            .map_err(|_| EmbeddingExecutableError::BatchRequestExtent)?;
        {
            let cache = self
                .inference_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (index, (identity, _)) in unique_inputs.iter().enumerate() {
                if let Some(coordinates) = cache.get(*identity) {
                    unique_results[index] = Some(coordinates);
                } else {
                    misses.push(index);
                }
            }
        }

        // Admit a whole API batch as one owner. Besides bounding external model processes, this
        // lets concurrent callers recheck exact identities after the prior owner's transactional
        // cache commit, so simultaneous duplicate cold requests share one inference batch.
        let _permit = if misses.is_empty() {
            None
        } else {
            Some(self.inference_gate.acquire(cancelled)?)
        };
        if _permit.is_some() {
            check_cancelled(cancelled)?;
            self.artifact_workspace
                .verify(self.model.identity, self.tokenizer.identity)?;
            self.executable
                .verify_path(&self.program)
                .map_err(EmbeddingExecutableError::Process)?;

            let mut still_missing = Vec::new();
            still_missing
                .try_reserve_exact(misses.len())
                .map_err(|_| EmbeddingExecutableError::BatchRequestExtent)?;
            let cache = self
                .inference_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for index in misses.drain(..) {
                let (identity, _) = unique_inputs[index];
                if let Some(coordinates) = cache.get(identity) {
                    unique_results[index] = Some(coordinates);
                } else {
                    still_missing.push(index);
                }
            }
            misses = still_missing;
        }
        check_batch_result_limit(misses.len(), self.dimensions.get())?;

        if self.batch_protocol == EmbeddingBatchProtocol::SingleV1 {
            for index in misses {
                check_cancelled(cancelled)?;
                let (_, text) = unique_inputs[index];
                unique_results[index] = Some(self.infer_inner_admitted(
                    EmbeddingInvocation { purpose, text },
                    false,
                    cancelled,
                )?);
            }
        } else {
            let mut cursor = 0;
            while cursor < misses.len() {
                check_cancelled(cancelled)?;
                let first_index = misses[cursor];
                let first = unique_inputs[first_index];
                if !self.batch_request_fits(&[first]) {
                    return Err(EmbeddingExecutableError::BatchRequestExtent);
                }
                let mut end = cursor + 1;
                while end < misses.len() && end - cursor < MAX_EMBEDDING_BATCH_ITEMS {
                    let candidate_end = end + 1;
                    let candidate = misses[cursor..candidate_end]
                        .iter()
                        .map(|index| unique_inputs[*index])
                        .collect::<Vec<_>>();
                    if !self.batch_request_fits(&candidate) {
                        break;
                    }
                    end = candidate_end;
                }
                let batch = misses[cursor..end]
                    .iter()
                    .map(|index| unique_inputs[*index])
                    .collect::<Vec<_>>();
                let coordinates = self.run_batch_v2_admitted(&batch, purpose, cancelled)?;
                check_cancelled(cancelled)?;
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
        check_cancelled(cancelled)?;
        // Treat the whole API call transactionally. If a later microbatch fails, no earlier
        // result from this call becomes a warm-cache hit on retry.
        let mut cache = self
            .inference_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for ((identity, _), coordinates) in unique_inputs.iter().zip(&unique_results) {
            if let Some(coordinates) = coordinates {
                cache.insert(*identity, coordinates);
            }
        }
        Ok(output)
    }

    fn infer_inner(
        &self,
        invocation: EmbeddingInvocation<'_>,
        use_cache: bool,
    ) -> Result<EmbeddingCoordinates, EmbeddingExecutableError> {
        self.infer_inner_with_cancellation_flag(invocation, use_cache, None)
    }

    fn infer_inner_with_cancellation_flag(
        &self,
        invocation: EmbeddingInvocation<'_>,
        use_cache: bool,
        cancelled: Option<&AtomicBool>,
    ) -> Result<EmbeddingCoordinates, EmbeddingExecutableError> {
        if !self.active {
            return Err(EmbeddingExecutableError::Revoked);
        }
        check_cancelled(cancelled)?;
        let _permit = self.inference_gate.acquire(cancelled)?;
        self.infer_inner_admitted(invocation, use_cache, cancelled)
    }

    fn infer_inner_admitted(
        &self,
        invocation: EmbeddingInvocation<'_>,
        use_cache: bool,
        cancelled: Option<&AtomicBool>,
    ) -> Result<EmbeddingCoordinates, EmbeddingExecutableError> {
        if !self.active {
            return Err(EmbeddingExecutableError::Revoked);
        }
        check_cancelled(cancelled)?;
        self.artifact_workspace
            .verify(self.model.identity, self.tokenizer.identity)?;
        self.executable
            .verify_path(&self.program)
            .map_err(EmbeddingExecutableError::Process)?;
        if invocation.text.len() > self.maximum_text_bytes {
            return Err(EmbeddingExecutableError::TextLimit {
                observed: invocation.text.len(),
                maximum: self.maximum_text_bytes,
            });
        }
        let input_identity = EmbeddingInputIdentity::new(self.execution_identity(), invocation);
        if use_cache
            && let Some(coordinates) = self
                .inference_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(input_identity)
        {
            return Ok(coordinates);
        }
        let request = self.encode_request(invocation)?;
        let command = SupervisedCommand::for_authority_with_artifact(
            self.program.clone(),
            self.arguments.clone(),
            self.environment.clone(),
            self.workspace.clone(),
            ProcessStdin::bytes(request),
            self.executable.clone(),
            None,
            ProtocolDescriptor::cold(),
            self.process_limits,
        )
        .map_err(EmbeddingExecutableError::Process)?;
        let receipt = run_supervised_command(command, cancelled)?;
        check_cancelled(cancelled)?;
        if receipt.terminal() != ProcessTerminal::Success || !receipt.reaped() {
            return Err(EmbeddingExecutableError::Terminal(receipt.terminal()));
        }
        let coordinates = self.decode_response(invocation.purpose, receipt.stdout())?;
        if use_cache {
            self.inference_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(input_identity, &coordinates);
        }
        Ok(coordinates)
    }

    /// Revokes this active runtime. Further requests fail before process creation.
    pub fn revoke(&mut self) {
        self.active = false;
    }

    fn encode_request(
        &self,
        invocation: EmbeddingInvocation<'_>,
    ) -> Result<Vec<u8>, EmbeddingExecutableError> {
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
        output.extend_from_slice(invocation.text.as_bytes());
        Ok(output)
    }

    fn batch_request_fits(&self, batch: &[(EmbeddingInputIdentity, &str)]) -> bool {
        if batch.is_empty() || batch.len() > MAX_EMBEDDING_BATCH_ITEMS {
            return false;
        }
        let Some(request_bytes) =
            batch
                .iter()
                .try_fold(REQUEST_HEADER_BYTES, |total, (_, text)| {
                    if text.len() > self.maximum_text_bytes || u32::try_from(text.len()).is_err() {
                        None
                    } else {
                        total
                            .checked_add(BATCH_ITEM_HEADER_BYTES)?
                            .checked_add(text.len())
                    }
                })
        else {
            return false;
        };
        let Some(vector_bytes) = usize::from(self.dimensions.get())
            .checked_mul(size_of::<f32>())
            .and_then(|bytes| bytes.checked_add(BATCH_RESPONSE_ITEM_HEADER_BYTES))
        else {
            return false;
        };
        let Some(response_bytes) = batch
            .len()
            .checked_mul(vector_bytes)
            .and_then(|bytes| bytes.checked_add(BATCH_RESPONSE_HEADER_BYTES))
        else {
            return false;
        };
        request_bytes <= self.process_limits.input_bytes()
            && response_bytes <= self.process_limits.stdout()
            && response_bytes <= self.process_limits.output_bytes()
    }

    fn encode_batch_request(
        &self,
        batch: &[(EmbeddingInputIdentity, &str)],
        purpose: EmbeddingPurpose,
    ) -> Result<Vec<u8>, EmbeddingExecutableError> {
        if !self.batch_request_fits(batch) {
            return Err(EmbeddingExecutableError::BatchRequestExtent);
        }
        let request_bytes = batch
            .iter()
            .try_fold(REQUEST_HEADER_BYTES, |total, (_, text)| {
                total
                    .checked_add(BATCH_ITEM_HEADER_BYTES)
                    .and_then(|bytes| bytes.checked_add(text.len()))
            })
            .ok_or(EmbeddingExecutableError::BatchRequestExtent)?;
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
            let text_len = u32::try_from(text.len())
                .map_err(|_| EmbeddingExecutableError::BatchRequestExtent)?;
            output.extend_from_slice(&identity.as_bytes());
            output.extend_from_slice(&text_len.to_be_bytes());
            output.extend_from_slice(text.as_bytes());
        }
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
        check_cancelled(cancelled)?;
        let _permit = self.inference_gate.acquire(cancelled)?;
        self.run_batch_v2_admitted(batch, purpose, cancelled)
    }

    fn run_batch_v2_admitted(
        &self,
        batch: &[(EmbeddingInputIdentity, &str)],
        purpose: EmbeddingPurpose,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Vec<EmbeddingCoordinates>, EmbeddingExecutableError> {
        if !self.active {
            return Err(EmbeddingExecutableError::Revoked);
        }
        check_cancelled(cancelled)?;
        self.artifact_workspace
            .verify(self.model.identity, self.tokenizer.identity)?;
        self.executable
            .verify_path(&self.program)
            .map_err(EmbeddingExecutableError::Process)?;
        let request = self.encode_batch_request(batch, purpose)?;
        let command = SupervisedCommand::for_authority_with_artifact(
            self.program.clone(),
            self.arguments.clone(),
            self.environment.clone(),
            self.workspace.clone(),
            ProcessStdin::bytes(request),
            self.executable.clone(),
            None,
            ProtocolDescriptor::cold(),
            self.process_limits,
        )
        .map_err(EmbeddingExecutableError::Process)?;
        let receipt = run_supervised_command(command, cancelled)?;
        check_cancelled(cancelled)?;
        if receipt.terminal() != ProcessTerminal::Success || !receipt.reaped() {
            return Err(EmbeddingExecutableError::Terminal(receipt.terminal()));
        }
        self.decode_batch_response(batch, purpose, receipt.stdout())
    }

    fn decode_batch_response(
        &self,
        batch: &[(EmbeddingInputIdentity, &str)],
        purpose: EmbeddingPurpose,
        bytes: &[u8],
    ) -> Result<Vec<EmbeddingCoordinates>, EmbeddingExecutableError> {
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
            for encoded in bytes[offset..values_end].chunks_exact(size_of::<f32>()) {
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
        Ok(coordinates)
    }

    fn decode_response(
        &self,
        purpose: EmbeddingPurpose,
        bytes: &[u8],
    ) -> Result<EmbeddingCoordinates, EmbeddingExecutableError> {
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
        for encoded in bytes[RESPONSE_HEADER_BYTES..].chunks_exact(size_of::<f32>()) {
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

fn check_batch_result_limit(
    unique_misses: usize,
    dimensions: u16,
) -> Result<(), EmbeddingExecutableError> {
    let coordinate_bytes = unique_misses
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
) -> Result<crate::ProcessReceipt, EmbeddingExecutableError> {
    let Some(cancelled) = cancelled else {
        return ProcessSupervisor::new(command)
            .run()
            .map_err(EmbeddingExecutableError::Process);
    };
    check_cancelled(Some(cancelled))?;
    let (cancellation, handle) = Cancellation::new();
    let finished = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let monitor = scope.spawn(|| {
            while !finished.load(Ordering::Acquire) {
                if cancelled.load(Ordering::Acquire) {
                    handle.cancel();
                    return;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        let result = ProcessSupervisor::new(command).run_with_cancellation(&cancellation);
        finished.store(true, Ordering::Release);
        if monitor.join().is_err() && result.is_ok() {
            return Err(ProcessError::Io);
        }
        result
    })
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

    fn activate_fixture(
        root: PathBuf,
        program: PathBuf,
        arguments: Vec<String>,
        environment: ProcessEnvironment,
    ) -> Result<(PathBuf, EmbeddingRuntimeSpecV1, EmbeddingExecutable), Box<dyn std::error::Error>>
    {
        let artifact = ToolchainArtifact::from_path(&program, Vec::new())?;
        let limits = ProcessLimits::new(128, 64, Duration::from_secs(2), 256)?
            .with_input_bytes_limit(512)?;
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
        let mut shared = None;
        for caller in callers {
            let coordinates = caller
                .join()
                .map_err(|_| io::Error::other("concurrent embedding caller panicked"))??;
            assert_eq!(coordinates.len(), 2);
            if let Some(expected) = shared.as_ref() {
                assert!(Arc::ptr_eq(expected, &coordinates[0].values));
                assert!(Arc::ptr_eq(expected, &coordinates[1].values));
            } else {
                shared = Some(Arc::clone(&coordinates[0].values));
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
        fs::remove_file(counter_root.join("active.txt.lock"))?;
        fs::remove_dir(counter_root)?;
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
        let (root, _, runtime) =
            runtime_with_counters(Some(&counter_root.join("active.txt")), Some(&counter))?;
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
        let deadline = Instant::now() + Duration::from_secs(1);
        while fs::read_to_string(&counter)?.lines().count() < 2 {
            if Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "batch did not start").into());
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
        let recovered = runtime.infer_batch(EmbeddingPurpose::Document, &["alpha", "beta"])?;
        assert_eq!(recovered.len(), 2);
        assert_eq!(fs::read_to_string(&counter)?.lines().count(), 3); // probe, cancelled, retry

        drop(runtime);
        fs::remove_dir_all(root)?;
        fs::remove_dir_all(counter_root)?;
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
        let gate = InferenceAdmissionGate::new(Duration::from_millis(10));
        let permit = gate.acquire(None)?;
        assert!(matches!(
            gate.acquire(None),
            Err(EmbeddingExecutableError::InferenceAdmissionTimeout)
        ));
        drop(permit);
        assert!(gate.acquire(None).is_ok());
        Ok(())
    }

    #[test]
    fn cancelled_batch_waiter_exits_the_gate_without_waiting_for_its_deadline()
    -> Result<(), Box<dyn std::error::Error>> {
        let gate = Arc::new(InferenceAdmissionGate::new(Duration::from_secs(3)));
        let permit = gate.acquire(None)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let (entered_sender, entered_receiver) = std::sync::mpsc::sync_channel(1);
        let (finished_sender, finished_receiver) = std::sync::mpsc::sync_channel(1);
        let waiter_gate = Arc::clone(&gate);
        let waiter_cancelled = Arc::clone(&cancelled);
        let waiter = std::thread::spawn(move || {
            entered_sender.send(()).expect("notify test thread entered");
            let started = Instant::now();
            let result = waiter_gate.acquire(Some(&waiter_cancelled)).map(drop);
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
        assert!(gate.acquire(None).is_ok());
        Ok(())
    }
}
