//! Bounded cold-process protocol for an external embedding model runtime.
//!
//! This adapter owns immutable model and tokenizer bytes plus a content-checked executable. Every
//! inference is supervised under explicit input, output, deadline, process, memory, and workspace
//! limits. A runtime becomes active only after the executable answers a real self-test request.

use crate::{
    ProcessEnvironment, ProcessError, ProcessLimits, ProcessStdin, ProcessSupervisor,
    ProcessTerminal, ProtocolDescriptor, SupervisedCommand, ToolchainArtifact,
};
use std::{fmt, num::NonZeroU16, path::PathBuf, sync::Arc};

const REQUEST_MAGIC: &[u8; 4] = b"BEM1";
const RESPONSE_MAGIC: &[u8; 4] = b"BEC1";
const REQUEST_HEADER_BYTES: usize = 4 + 1 + 1 + 2 + 32 + 32 + 32 + 4;
const RESPONSE_HEADER_BYTES: usize = 4 + 2;

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
    executable: ToolchainArtifact,
    model: EmbeddingArtifact,
    tokenizer: EmbeddingArtifact,
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
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

impl EmbeddingExecutable {
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
        if maximum_text_bytes == 0 {
            return Err(EmbeddingExecutableError::ZeroTextLimit);
        }
        executable
            .verify_path(&program)
            .map_err(EmbeddingExecutableError::Process)?;
        if model.bytes().is_empty() || tokenizer.bytes().is_empty() {
            return Err(EmbeddingExecutableError::EmptyArtifact);
        }
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
            executable,
            model,
            tokenizer,
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
        let mut output = Vec::with_capacity(REQUEST_HEADER_BYTES + invocation.text.len());
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
        if output.len() > self.process_limits.input_bytes() {
            return Err(EmbeddingExecutableError::RequestExtent);
        }
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
    /// Input exceeded its independent bound.
    TextLimit {
        /// UTF-8 bytes in the rejected input.
        observed: usize,
        /// Configured independent text bound.
        maximum: usize,
    },
    /// Request header/text extent cannot be represented or exceeds the process input bound.
    RequestExtent,
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

    fn fixture() -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "backend-embedding-{}-{stamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root)?;
        let executable = root.join("fixture.sh");
        let shell = std::env::var_os("NUDOX_PROCESS_SHELL")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/bin/sh"));
        let cat = std::env::var_os("NUDOX_TEST_COREUTILS_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/bin"))
            .join("cat");
        let script = format!(
            "#!{}\n{} >/dev/null\nprintf '\\102\\105\\103\\061\\000\\002\\000\\000\\200\\077\\000\\000\\000\\000'\n",
            shell.display(),
            cat.display()
        );
        fs::write(&executable, script)?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
        Ok((root, executable))
    }

    fn runtime() -> Result<(PathBuf, EmbeddingExecutable), Box<dyn std::error::Error>> {
        let (root, program) = fixture()?;
        let artifact = ToolchainArtifact::from_path(&program, Vec::new())?;
        let environment = ProcessEnvironment::new(vec![("PATH".into(), "/usr/bin:/bin".into())])?;
        let limits =
            ProcessLimits::new(64, 64, Duration::from_secs(2), 128)?.with_input_bytes_limit(512)?;
        let runtime = EmbeddingExecutable::activate(
            program,
            Vec::new(),
            root.clone(),
            environment,
            limits,
            128,
            NonZeroU16::new(2).ok_or("dimensions")?,
            EmbeddingNormalization::L2,
            [9; 32],
            artifact,
            EmbeddingArtifact::new(Arc::from(&b"model"[..])),
            EmbeddingArtifact::new(Arc::from(&b"tokenizer"[..])),
        )?;
        Ok((root, runtime))
    }

    #[test]
    fn active_external_model_returns_typed_query_and_document_coordinates()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, runtime) = runtime()?;
        let query = runtime.infer(EmbeddingInvocation {
            purpose: EmbeddingPurpose::Query,
            text: "find parser",
        })?;
        let document = runtime.infer(EmbeddingInvocation {
            purpose: EmbeddingPurpose::Document,
            text: "fn parse() {}",
        })?;
        assert_eq!(query.values(), &[1.0, 0.0]);
        assert_eq!(query.purpose(), EmbeddingPurpose::Query);
        assert_eq!(document.purpose(), EmbeddingPurpose::Document);
        assert_eq!(query.recipe(), [9; 32]);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn revocation_and_text_bounds_fail_before_execution() -> Result<(), Box<dyn std::error::Error>>
    {
        let (root, mut runtime) = runtime()?;
        assert!(matches!(
            runtime.infer(EmbeddingInvocation {
                purpose: EmbeddingPurpose::Query,
                text: &"x".repeat(129)
            }),
            Err(EmbeddingExecutableError::TextLimit { .. })
        ));
        runtime.revoke();
        assert!(matches!(
            runtime.infer(EmbeddingInvocation {
                purpose: EmbeddingPurpose::Query,
                text: "x"
            }),
            Err(EmbeddingExecutableError::Revoked)
        ));
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
