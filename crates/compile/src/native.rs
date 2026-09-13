//! Checked native-authority execution and session adapters.
//!
//! This module stops at the process/protocol boundary.  It returns bounded
//! bytes and explicit coverage; language frontends own parsing those bytes and
//! must bind any facts to their discovery manifest before publishing them.

#[path = "native_process.rs"]
mod native_process;

use crate::{
    Cancellation, Coverage, CoverageWitness, ErasedSession, InputManifestId, NativeEnvelope,
    NativeProtocolError, PROTOCOL_VERSION, ProcessError, ProcessReceipt, ProcessSupervisor,
    ProcessTerminal, ScopeRoot, SessionKey, SupervisedCommand, ToolchainId,
};
use native_process::PersistentProcess;
use std::fmt;

/// Description of the wire protocol an authority command implements.
///
/// A descriptor can retain a newer or otherwise unknown version so callers
/// can report [`NativeRunnerError::Unsupported`] explicitly.  The runner only
/// executes the protocol version understood by this crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolDescriptor {
    version: u16,
    persistent: bool,
}

impl ProtocolDescriptor {
    /// Creates a descriptor from an advertised version and persistence bit.
    #[must_use]
    pub const fn new(version: u16, persistent: bool) -> Self {
        Self {
            version,
            persistent,
        }
    }

    /// Returns a descriptor for one cold request using the current protocol.
    #[must_use]
    pub const fn cold() -> Self {
        Self::new(PROTOCOL_VERSION, false)
    }

    /// Returns a descriptor for the current persistent session protocol.
    #[must_use]
    pub const fn persistent() -> Self {
        Self::new(PROTOCOL_VERSION, true)
    }

    /// Returns the advertised protocol version.
    #[must_use]
    pub const fn version(self) -> u16 {
        self.version
    }

    /// Returns whether this descriptor advertises persistent sessions.
    #[must_use]
    pub const fn advertises_persistent(self) -> bool {
        self.persistent
    }

    /// Returns whether persistent sessions are supported by this crate.
    #[must_use]
    pub const fn supports_persistent(self) -> bool {
        self.persistent && self.version == PROTOCOL_VERSION
    }

    /// Returns whether this descriptor is understood for cold execution.
    #[must_use]
    pub const fn supports_cold(self) -> bool {
        self.version == PROTOCOL_VERSION
    }

    /// Returns whether this descriptor is understood for persistent execution.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        self.supports_cold() && (!self.persistent || self.supports_persistent())
    }
}

/// Terminal process state visible in a native observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeExit {
    /// No process was started because the operation was unavailable.
    NotStarted,
    /// A persistent authority is still alive after its response.
    Running,
    /// A cold authority exited successfully with the optional OS code.
    Success(Option<i32>),
    /// A cold authority exited unsuccessfully with the optional OS code.
    Failure(Option<i32>),
}

/// Bounded native output and its checked process/coverage metadata.
///
/// The output is intentionally only bytes.  Constructing facts remains a
/// frontend operation, so a parser cannot accidentally treat arbitrary native
/// output as an admitted semantic result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeObservation {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    payload: Option<NativeEnvelope>,
    exit: NativeExit,
    coverage: Coverage,
    scope: Option<ScopeRoot>,
}

impl NativeObservation {
    /// Returns bounded stdout bytes for frontend parsing.
    #[must_use]
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    /// Returns bounded stderr bytes for diagnostics.
    #[must_use]
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    /// Returns the response framing bytes.  For a cold observation this is
    /// the same bounded byte stream returned by [`Self::stdout`].
    #[must_use]
    pub fn frame(&self) -> &[u8] {
        &self.stdout
    }

    /// Returns the separately validated persistent response payload, if one
    /// was carried by the authority.
    #[must_use]
    pub fn payload(&self) -> Option<&NativeEnvelope> {
        self.payload.as_ref()
    }

    /// Consumes the observation and returns its validated persistent payload.
    ///
    /// This transfer avoids cloning records when the caller is going to admit
    /// the envelope immediately.
    #[must_use]
    pub fn into_payload(self) -> Option<NativeEnvelope> {
        self.payload
    }

    /// Returns the explicit cold exit or persistent-running state.
    #[must_use]
    pub const fn exit(&self) -> NativeExit {
        self.exit
    }

    /// Returns the OS exit code when a cold process has exited.
    #[must_use]
    pub const fn status(&self) -> Option<i32> {
        match self.exit {
            NativeExit::Success(status) | NativeExit::Failure(status) => status,
            NativeExit::NotStarted | NativeExit::Running => None,
        }
    }

    /// Returns the conservative coverage state attached to these bytes.
    #[must_use]
    pub const fn coverage(&self) -> Coverage {
        self.coverage
    }

    /// Returns the exact scope against which a later coverage witness is
    /// checked, when the command carried a session key.
    #[must_use]
    pub const fn coverage_scope(&self) -> Option<ScopeRoot> {
        self.scope
    }

    /// Attaches a producer-checked coverage witness to this raw observation.
    ///
    /// Complete coverage cannot be attached to raw process bytes. The native
    /// adapter admits it only after its private authority registry validates
    /// the immutable command, manifest boundary, typed records, and authority
    /// epoch. Incomplete witnesses remain explicit even when they cover a
    /// narrower scope.
    ///
    /// # Errors
    ///
    /// Returns [`NativeRunnerError::Unavailable`] when a complete witness is
    /// attached to a failed observation, or [`NativeRunnerError::Protocol`]
    /// when its scope cannot be checked against the command binding.
    pub fn with_coverage(mut self, witness: CoverageWitness) -> Result<Self, NativeRunnerError> {
        if witness.state().is_complete() {
            // A raw backend-version complete label is not an authority
            // capability. Complete admission belongs to AuthorityRegistry,
            // which validates the immutable command, manifest boundary,
            // typed records, and epoch together.
            return Err(NativeRunnerError::Protocol);
        }
        self.coverage = witness.state();
        Ok(self)
    }

    /// Consumes the observation and returns owned stdout and stderr bytes.
    #[must_use]
    pub fn into_output(self) -> (Vec<u8>, Vec<u8>) {
        (self.stdout, self.stderr)
    }

    /// Returns the decoded native payload envelope with untrusted claims.
    ///
    /// Cold observations decode their bounded stdout here. Persistent
    /// observations return the payload that was validated separately from
    /// their framing bytes. Callers must admit the returned
    /// `NativeEnvelope<crate::Unbound>` before using its identities;
    /// language-specific fact construction remains the caller's responsibility.
    ///
    /// # Errors
    ///
    /// Returns [`NativeProtocolError`] when a cold stdout stream is not a
    /// valid native envelope.
    pub fn decode_payload(&self) -> Result<NativeEnvelope, NativeProtocolError> {
        self.payload
            .clone()
            .map_or_else(|| NativeEnvelope::decode(&self.stdout), Ok)
    }

    /// Returns an observation for an unsupported authority operation.
    #[must_use]
    pub fn unsupported() -> Self {
        Self {
            stdout: Vec::new(),
            stderr: Vec::new(),
            payload: None,
            exit: NativeExit::NotStarted,
            coverage: Coverage::Unsupported,
            scope: None,
        }
    }

    /// Returns an observation for an authority that was unavailable.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            stdout: Vec::new(),
            stderr: Vec::new(),
            payload: None,
            exit: NativeExit::NotStarted,
            coverage: Coverage::Unavailable,
            scope: None,
        }
    }

    fn cold(receipt: ProcessReceipt, scope: Option<ScopeRoot>) -> Self {
        let exit = match receipt.terminal() {
            ProcessTerminal::Success => NativeExit::Success(receipt.status()),
            ProcessTerminal::Exit
            | ProcessTerminal::Cancelled
            | ProcessTerminal::Deadline
            | ProcessTerminal::OutputLimit
            | ProcessTerminal::SpawnFailure
            | ProcessTerminal::ProtocolFailure => NativeExit::Failure(receipt.status()),
        };
        let (stdout, stderr) = receipt.into_output();
        Self {
            stdout,
            stderr,
            payload: None,
            exit,
            coverage: Coverage::Unavailable,
            scope,
        }
    }

    fn running(frame: Vec<u8>, stderr: Vec<u8>, payload: NativeEnvelope, scope: ScopeRoot) -> Self {
        Self {
            stdout: frame,
            stderr,
            payload: Some(payload),
            exit: NativeExit::Running,
            coverage: Coverage::Unavailable,
            scope: Some(scope),
        }
    }
}

/// Failure while selecting or running a native authority mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeRunnerError {
    /// The descriptor does not support the requested mode or version.
    Unsupported,
    /// The authority cannot be used for this request, such as after a lost
    /// persistent process or unavailable toolchain.
    Unavailable,
    /// The command did not carry a checked toolchain identity.
    MissingToolchain,
    /// Persistent execution was requested without a session key.
    MissingSessionKey,
    /// The authority violated the session frame contract.
    Protocol,
    /// The bounded process supervisor rejected or failed the command.
    Process(ProcessError),
}

impl fmt::Display for NativeRunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("native authority mode is unsupported"),
            Self::Unavailable => f.write_str("native authority is unavailable"),
            Self::MissingToolchain => f.write_str("native command has no toolchain identity"),
            Self::MissingSessionKey => f.write_str("persistent native command has no session key"),
            Self::Protocol => f.write_str("native authority protocol failed"),
            Self::Process(error) => write!(f, "native authority process failed: {error}"),
        }
    }
}

impl std::error::Error for NativeRunnerError {}

impl From<ProcessError> for NativeRunnerError {
    fn from(error: ProcessError) -> Self {
        Self::Process(error)
    }
}

/// Compatibility alias for code that names this boundary a native error.
pub type NativeError = NativeRunnerError;

/// Checked cold or persistent execution entry point for one authority.
#[derive(Clone, Debug)]
pub struct NativeAuthorityRunner {
    command: SupervisedCommand,
}

/// Short alias for the native authority runner.
pub type NativeRunner = NativeAuthorityRunner;

impl NativeAuthorityRunner {
    /// Binds the runner to one validated command specification.
    #[must_use]
    pub const fn new(command: SupervisedCommand) -> Self {
        Self { command }
    }

    /// Returns the command specification owned by this runner.
    #[must_use]
    pub const fn command(&self) -> &SupervisedCommand {
        &self.command
    }

    /// Returns the bound toolchain identity, if present.
    #[must_use]
    pub const fn toolchain(&self) -> Option<ToolchainId> {
        self.command.toolchain()
    }

    /// Returns the protocol descriptor selected by the command.
    #[must_use]
    pub const fn protocol(&self) -> ProtocolDescriptor {
        self.command.protocol()
    }

    /// Runs one bounded cold authority request.
    ///
    /// # Errors
    ///
    /// Returns [`NativeRunnerError`] when the protocol is unsupported, the
    /// executable cannot be verified, or process execution fails.
    pub fn run_cold(&self) -> Result<NativeObservation, NativeRunnerError> {
        let (cancellation, _handle) = Cancellation::new();
        self.run_cold_with_cancellation(&cancellation)
    }

    /// Runs the default cold authority mode.
    ///
    /// # Errors
    ///
    /// Returns [`NativeRunnerError`] when cold execution or output validation
    /// fails.
    pub fn run(&self) -> Result<NativeObservation, NativeRunnerError> {
        self.run_cold()
    }

    /// Runs one bounded cold request while observing cancellation.
    ///
    /// # Errors
    ///
    /// Returns [`NativeRunnerError`] when the command is unsupported, is
    /// cancelled, or cannot complete within its process limits.
    pub fn run_cold_with_cancellation(
        &self,
        cancellation: &Cancellation,
    ) -> Result<NativeObservation, NativeRunnerError> {
        self.validate_cold()?;
        let receipt = ProcessSupervisor::new(self.command.clone())
            .run_with_cancellation(cancellation)
            .map_err(NativeRunnerError::Process)?;
        let scope = self
            .command
            .session_key()
            .map(|key| ScopeRoot::from_bytes(key.manifest().to_bytes()));
        Ok(NativeObservation::cold(receipt, scope))
    }

    /// Runs the default cold mode while observing cancellation.
    ///
    /// # Errors
    ///
    /// Returns [`NativeRunnerError`] when cold execution is rejected or
    /// cancelled.
    pub fn run_with_cancellation(
        &self,
        cancellation: &Cancellation,
    ) -> Result<NativeObservation, NativeRunnerError> {
        self.run_cold_with_cancellation(cancellation)
    }

    /// Runs one persistent request and closes the session afterward.
    ///
    /// # Errors
    ///
    /// Returns [`NativeRunnerError`] when persistent mode is unavailable or
    /// the checked request fails.
    pub fn run_persistent(
        &self,
        manifest: InputManifestId,
        revision: u64,
    ) -> Result<NativeObservation, NativeRunnerError> {
        let (cancellation, _handle) = Cancellation::new();
        self.run_persistent_with_request_with_cancellation(manifest, revision, &[], &cancellation)
    }

    /// Runs one persistent request carrying an exact canonical request body.
    ///
    /// The request body is sent after the checked session request frame and
    /// before the helper can answer it.  This keeps source/configuration bytes
    /// inside the input-bound request instead of hiding them in process
    /// environment or startup state.
    ///
    /// # Errors
    ///
    /// Returns [`NativeRunnerError`] when the request exceeds its bound, the
    /// session cannot be established, or the helper returns an invalid reply.
    pub fn run_persistent_with_request(
        &self,
        manifest: InputManifestId,
        revision: u64,
        request: &[u8],
    ) -> Result<NativeObservation, NativeRunnerError> {
        let (cancellation, _handle) = Cancellation::new();
        self.run_persistent_with_request_with_cancellation(
            manifest,
            revision,
            request,
            &cancellation,
        )
    }

    /// Runs one persistent request while observing cancellation.
    ///
    /// # Errors
    ///
    /// Returns [`NativeRunnerError`] when the session is unsupported, the
    /// request fails, or cancellation is observed.
    pub fn run_persistent_with_cancellation(
        &self,
        manifest: InputManifestId,
        revision: u64,
        cancellation: &Cancellation,
    ) -> Result<NativeObservation, NativeRunnerError> {
        self.run_persistent_with_request_with_cancellation(manifest, revision, &[], cancellation)
    }

    /// Runs one persistent request with cancellation and an exact request
    /// body supplied in the request stream.
    ///
    /// # Errors
    ///
    /// Returns [`NativeRunnerError`] when the request exceeds its bound, the
    /// session cannot be established, or execution is cancelled or malformed.
    pub fn run_persistent_with_request_with_cancellation(
        &self,
        manifest: InputManifestId,
        revision: u64,
        request: &[u8],
        cancellation: &Cancellation,
    ) -> Result<NativeObservation, NativeRunnerError> {
        if request.len() > self.command.limits().input_bytes() {
            return Err(NativeRunnerError::Process(ProcessError::InputLimit));
        }
        let mut session = self.start_persistent_with_cancellation(cancellation)?;
        session.request_with_payload_with_cancellation(manifest, revision, request, cancellation)
    }

    /// Starts a persistent authority session when the protocol descriptor
    /// advertises that capability.
    ///
    /// # Errors
    ///
    /// Returns [`NativeRunnerError`] when persistent mode is unavailable, the
    /// executable fails verification, or the handshake fails.
    pub fn start_persistent(&self) -> Result<PersistentNativeSession, NativeRunnerError> {
        let (cancellation, _handle) = Cancellation::new();
        self.start_persistent_with_cancellation(&cancellation)
    }

    /// Starts a persistent authority session while observing cancellation.
    ///
    /// # Errors
    ///
    /// Returns [`NativeRunnerError`] when the session is unsupported, missing
    /// its key, cancelled, or rejected by the helper.
    pub fn start_persistent_with_cancellation(
        &self,
        cancellation: &Cancellation,
    ) -> Result<PersistentNativeSession, NativeRunnerError> {
        self.validate_persistent()?;
        let key = self
            .command
            .session_key()
            .ok_or(NativeRunnerError::MissingSessionKey)?;
        if self.command.stdin().as_bytes().is_some() {
            return Err(NativeRunnerError::Process(ProcessError::Protocol));
        }
        cancellation
            .checkpoint()
            .map_err(|_| NativeRunnerError::Process(ProcessError::Cancelled))?;
        let mut process = PersistentProcess::spawn(&self.command)?;
        let mut session = ErasedSession::new(key.digest());
        let hello = session.hello().map_err(NativeRunnerError::Process)?;
        process.send(&hello)?;
        let (response, _payload) = process.receive(cancellation, false)?;
        session.accept_wire(&response).map_err(|_| {
            let _ = process.terminate();
            NativeRunnerError::Protocol
        })?;
        if session.state() != crate::SessionState::Ready {
            let _ = process.terminate();
            return Err(NativeRunnerError::Protocol);
        }
        Ok(PersistentNativeSession {
            command: self.command.clone(),
            key,
            session,
            process,
        })
    }

    fn validate_cold(&self) -> Result<(), NativeRunnerError> {
        if !self.protocol().supports_cold() {
            return Err(NativeRunnerError::Unsupported);
        }
        if self.toolchain().is_none() {
            return Err(NativeRunnerError::MissingToolchain);
        }
        Ok(())
    }

    fn validate_persistent(&self) -> Result<(), NativeRunnerError> {
        if !self.protocol().supports_cold() || !self.protocol().supports_persistent() {
            return Err(NativeRunnerError::Unsupported);
        }
        if self.toolchain().is_none() {
            return Err(NativeRunnerError::MissingToolchain);
        }
        Ok(())
    }
}

/// A live native authority process with one checked persistent session.
pub struct PersistentNativeSession {
    command: SupervisedCommand,
    key: SessionKey,
    session: ErasedSession,
    process: PersistentProcess,
}

impl fmt::Debug for PersistentNativeSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PersistentNativeSession")
            .field("program", &self.command.program())
            .field("key", &self.key.digest())
            .field("state", &self.session.state())
            .finish_non_exhaustive()
    }
}

impl PersistentNativeSession {
    /// Returns the input-bound session key.
    #[must_use]
    pub const fn key(&self) -> SessionKey {
        self.key
    }

    /// Returns the current checked session state.
    #[must_use]
    pub const fn state(&self) -> crate::SessionState {
        self.session.state()
    }

    /// Sends one manifest-bound request with a fresh cancellation observation.
    ///
    /// # Errors
    ///
    /// Returns [`NativeRunnerError`] when the persistent process rejects the
    /// request or cannot produce a checked response.
    pub fn request(
        &mut self,
        manifest: InputManifestId,
        revision: u64,
    ) -> Result<NativeObservation, NativeRunnerError> {
        let (cancellation, _handle) = Cancellation::new();
        self.request_with_cancellation(manifest, revision, &cancellation)
    }

    /// Sends one manifest-bound request and returns its bounded native bytes.
    ///
    /// The response frame is retained as stdout so the caller can audit the
    /// exact protocol envelope.  Language-specific payloads remain the
    /// frontend's responsibility; this method never creates facts.
    ///
    /// # Errors
    ///
    /// Returns [`NativeRunnerError`] when the request or persistent process is
    /// invalid, unavailable, cancelled, or exceeds a configured limit.
    pub fn request_with_cancellation(
        &mut self,
        manifest: InputManifestId,
        revision: u64,
        cancellation: &Cancellation,
    ) -> Result<NativeObservation, NativeRunnerError> {
        self.request_with_payload_with_cancellation(manifest, revision, &[], cancellation)
    }

    /// Sends one manifest-bound request with an exact canonical request body.
    ///
    /// # Errors
    ///
    /// Returns [`NativeRunnerError`] when the request body or persistent
    /// process violates the checked session contract.
    pub fn request_with_payload(
        &mut self,
        manifest: InputManifestId,
        revision: u64,
        payload: &[u8],
    ) -> Result<NativeObservation, NativeRunnerError> {
        let (cancellation, _handle) = Cancellation::new();
        self.request_with_payload_with_cancellation(manifest, revision, payload, &cancellation)
    }

    /// Sends one manifest-bound request with cancellation and an exact body.
    ///
    /// # Errors
    ///
    /// Returns [`NativeRunnerError`] when the body exceeds its bound, the
    /// session is no longer ready, the process is cancelled, or the reply is
    /// malformed.
    pub fn request_with_payload_with_cancellation(
        &mut self,
        manifest: InputManifestId,
        revision: u64,
        payload: &[u8],
        cancellation: &Cancellation,
    ) -> Result<NativeObservation, NativeRunnerError> {
        if payload.len() > self.command.limits().input_bytes() {
            self.session.fallback_to_cold();
            let _ = self.process.terminate();
            return Err(NativeRunnerError::Process(ProcessError::InputLimit));
        }
        if self.session.state() != crate::SessionState::Ready || self.key.manifest() != manifest {
            self.session.fallback_to_cold();
            let _ = self.process.terminate();
            return Err(NativeRunnerError::Unavailable);
        }
        let request = self
            .session
            .request(manifest, revision)
            .map_err(NativeRunnerError::Process)?;
        if let Err(error) = self.process.send(&request) {
            self.session.fallback_to_cold();
            let _ = self.process.terminate();
            return Err(error);
        }
        if !payload.is_empty()
            && let Err(error) = self.process.send_bytes(payload)
        {
            self.session.fallback_to_cold();
            let _ = self.process.terminate();
            return Err(error);
        }
        let (response, payload) = match self.process.receive(cancellation, true) {
            Ok(response) => response,
            Err(error) => {
                self.session.fallback_to_cold();
                let _ = self.process.terminate();
                return Err(error);
            }
        };
        if self.session.accept_wire(&response).is_err() {
            self.session.fallback_to_cold();
            let _ = self.process.terminate();
            return Err(NativeRunnerError::Protocol);
        }
        let Some(payload) = payload else {
            self.session.fallback_to_cold();
            let _ = self.process.terminate();
            return Err(NativeRunnerError::Protocol);
        };
        Ok(NativeObservation::running(
            response,
            self.process.take_stderr(),
            payload,
            ScopeRoot::from_bytes(self.key.manifest().to_bytes()),
        ))
    }

    /// Requests process termination and returns to a cold fallback state.
    ///
    /// # Errors
    ///
    /// Returns [`NativeRunnerError::Process`] when process termination fails.
    pub fn fallback_to_cold(mut self) -> Result<(), NativeRunnerError> {
        self.session.fallback_to_cold();
        self.process.terminate().map_err(NativeRunnerError::Process)
    }
}

impl Drop for PersistentNativeSession {
    fn drop(&mut self) {
        let _ = self.process.terminate();
    }
}
