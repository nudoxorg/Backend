//! Bounded cold-process protocol for an external embedding model runtime.
//!
//! This adapter owns immutable model and tokenizer bytes plus a content-checked executable. Every
//! inference is supervised under explicit input, output, deadline, process, memory, and workspace
//! limits. A runtime becomes active only after the executable answers a real self-test request.

use crate::{
    ProcessEnvironment, ProcessError, ProcessLimits, ProcessStdin, ProcessSupervisor,
    ProcessTerminal, ProtocolDescriptor, SupervisedCommand, ToolchainArtifact,
};
#[cfg(unix)]
use std::fs;
use std::{
    fmt,
    io::{self, Read},
    num::{NonZeroU16, NonZeroU32},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

const REQUEST_MAGIC: &[u8; 4] = b"BEM1";
const RESPONSE_MAGIC: &[u8; 4] = b"BEC1";
const REQUEST_HEADER_BYTES: usize = 4 + 1 + 1 + 2 + 32 + 32 + 32 + 4;
const RESPONSE_HEADER_BYTES: usize = 4 + 2;
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

    fn acquire(&self) -> Result<InferenceAdmissionPermit<'_>, EmbeddingExecutableError> {
        let deadline = Instant::now()
            .checked_add(self.wait_budget)
            .ok_or(EmbeddingExecutableError::InferenceAdmissionTimeout)?;
        let mut occupied = self
            .occupied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *occupied {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(EmbeddingExecutableError::InferenceAdmissionTimeout);
            };
            if remaining.is_zero() {
                return Err(EmbeddingExecutableError::InferenceAdmissionTimeout);
            }
            let (next, timeout) = self
                .available
                .wait_timeout(occupied, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            occupied = next;
            if timeout.timed_out() || Instant::now() >= deadline {
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
/// temporary artifact paths, and process environment values are deliberately excluded.
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

/// Typed coordinates tied to the exact recipe, model, tokenizer, and task.
#[derive(Clone, Debug, PartialEq)]
pub struct EmbeddingCoordinates {
    recipe: [u8; 32],
    model: EmbeddingArtifactId,
    tokenizer: EmbeddingArtifactId,
    purpose: EmbeddingPurpose,
    normalization: EmbeddingNormalization,
    values: Box<[f32]>,
}

/// Portable identity of one activated embedding invocation recipe.
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
    inference_gate: InferenceAdmissionGate,
    active: bool,
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
    /// Returns the portable identity of this activated model runtime.
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

    /// Upper bound for the Rust-owned transient request, captured process output, and decoded
    /// coordinates retained while one inference is in flight.
    #[must_use]
    pub const fn maximum_inference_scratch_bytes(&self) -> usize {
        self.process_limits
            .input_bytes()
            .saturating_add(self.process_limits.output_bytes())
            .saturating_add(self.dimensions.get() as usize * size_of::<f32>())
    }

    /// Maximum simultaneous child processes admitted through this activated model runtime.
    #[must_use]
    pub const fn maximum_concurrent_inferences(&self) -> usize {
        1
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
            inference_gate: InferenceAdmissionGate::new(process_limits.wall_time()),
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
        self.infer(EmbeddingInvocation {
            purpose: EmbeddingPurpose::Query,
            text: "backend embedding readiness",
        })
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
        if !self.active {
            return Err(EmbeddingExecutableError::Revoked);
        }
        let _permit = self.inference_gate.acquire()?;
        self.artifact_workspace
            .verify(self.model.identity, self.tokenizer.identity)?;
        if invocation.text.len() > self.maximum_text_bytes {
            return Err(EmbeddingExecutableError::TextLimit {
                observed: invocation.text.len(),
                maximum: self.maximum_text_bytes,
            });
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
        let receipt = ProcessSupervisor::new(command)
            .run()
            .map_err(EmbeddingExecutableError::Process)?;
        if receipt.terminal() != ProcessTerminal::Success || !receipt.reaped() {
            return Err(EmbeddingExecutableError::Terminal(receipt.terminal()));
        }
        self.decode_response(invocation.purpose, receipt.stdout())
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
            values: values.into_boxed_slice(),
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
    /// Request header/text extent cannot be represented or exceeds the process input bound.
    RequestExtent,
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
import json, math, os, struct, sys
EXPECTED_MODEL = bytes.fromhex("{}")
EXPECTED_TOKENIZER = bytes.fromhex("{}")
EXPECTED_MODEL_ID = "{}"
EXPECTED_TOKENIZER_ID = "{}"
counter = os.environ.get("BACKEND_EMBEDDING_ACTIVE_COUNTER")
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
if len(frame) < 108 or frame[:4] != b"BEM1":
    sys.exit(72)
if frame[40:72].hex() != EXPECTED_MODEL_ID or frame[72:104].hex() != EXPECTED_TOKENIZER_ID:
    sys.exit(73)
text_length = struct.unpack(">I", frame[104:108])[0]
text = frame[108:]
if len(text) != text_length or frame[6:8] == b"\x00\x00":
    sys.exit(74)
dimension = struct.unpack(">H", frame[6:8])[0]
normalization = frame[5]
model = json.loads(model_bytes)
tokenizer = json.loads(tokenizer_bytes)
text = text.decode("utf-8")
if tokenizer.get("lowercase"):
    text = text.lower()
tokens = text.split()
if not tokens:
    sys.exit(75)
vectors = [model.get(token, model["<unk>"]) for token in tokens]
if any(len(vector) != dimension for vector in vectors):
    sys.exit(76)
values = [sum(vector[index] for vector in vectors) / len(vectors) for index in range(dimension)]
if normalization == 1:
    norm = math.sqrt(sum(value * value for value in values))
    if norm == 0:
        sys.exit(77)
    values = [value / norm for value in values]
if counter:
    update_counter(-1)
sys.stdout.buffer.write(b"BEC1" + struct.pack(">H", dimension) + struct.pack("<" + "f" * dimension, *values))
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
        let (root, program) = fixture()?;
        let artifact = ToolchainArtifact::from_path(&program, Vec::new())?;
        let mut environment = vec![("PATH".into(), "/usr/bin:/bin".into())];
        if let Some(counter) = counter {
            environment.push((
                "BACKEND_EMBEDDING_ACTIVE_COUNTER".into(),
                counter.to_string_lossy().into_owned(),
            ));
        }
        let environment = ProcessEnvironment::new(environment)?;
        let limits =
            ProcessLimits::new(64, 64, Duration::from_secs(2), 128)?.with_input_bytes_limit(512)?;
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
            Vec::new(),
            root.clone(),
            environment,
            limits,
            artifact,
            model,
            tokenizer,
        )?;
        Ok((root, spec, runtime))
    }

    fn runtime(
    ) -> Result<(PathBuf, EmbeddingRuntimeSpecV1, EmbeddingExecutable), Box<dyn std::error::Error>>
    {
        runtime_with_counter(None)
    }

    #[test]
    fn active_external_model_returns_typed_query_and_document_coordinates(
    ) -> Result<(), Box<dyn std::error::Error>> {
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
        assert!(runtime
            .infer(EmbeddingInvocation {
                purpose: EmbeddingPurpose::Query,
                text: "alpha"
            })
            .is_ok());
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
    fn shared_inference_gate_bounds_child_processes_and_releases_after_failure(
    ) -> Result<(), Box<dyn std::error::Error>> {
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
        assert!(runtime
            .infer(EmbeddingInvocation {
                purpose: EmbeddingPurpose::Query,
                text: "alpha"
            })
            .is_ok());

        drop(runtime);
        fs::remove_dir_all(root)?;
        fs::remove_file(counter)?;
        fs::remove_file(counter_root.join("active.txt.lock"))?;
        fs::remove_dir(counter_root)?;
        Ok(())
    }

    #[test]
    fn inference_gate_has_a_typed_finite_wait_and_releases_its_permit(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let gate = InferenceAdmissionGate::new(Duration::from_millis(10));
        let permit = gate.acquire()?;
        assert!(matches!(
            gate.acquire(),
            Err(EmbeddingExecutableError::InferenceAdmissionTimeout)
        ));
        drop(permit);
        assert!(gate.acquire().is_ok());
        Ok(())
    }
}
