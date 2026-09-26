//! Runs one checked native authority in cold or persistent mode.

use super::*;

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
