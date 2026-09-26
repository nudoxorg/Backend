//! Constructs, identifies, and runs one supervised command.

use super::*;

impl SupervisedCommand {
    /// Constructs a command with absolute program and working-directory paths.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::RelativePath`] for relative paths,
    /// [`ProcessError::InvalidProgram`] for a path without a file name, or
    /// [`ProcessError::Protocol`] when an argument contains a NUL byte.
    pub fn new(
        program: PathBuf,
        args: Vec<String>,
        cwd: PathBuf,
        environment: ProcessEnvironment,
        limits: ProcessLimits,
    ) -> Result<Self, ProcessError> {
        if !program.is_absolute() || !cwd.is_absolute() {
            return Err(ProcessError::RelativePath);
        }
        if program.file_name().is_none_or(std::ffi::OsStr::is_empty) {
            return Err(ProcessError::InvalidProgram);
        }
        let argument_bytes = args
            .iter()
            .try_fold(0_usize, |total, argument| total.checked_add(argument.len()));
        if args.len() > MAX_ARGUMENTS
            || argument_bytes.is_none_or(|bytes| bytes > MAX_ARGUMENT_BYTES)
        {
            return Err(ProcessError::ConfigurationLimit);
        }
        if args.iter().any(|arg| arg.as_bytes().contains(&0)) {
            return Err(ProcessError::Protocol);
        }
        let executable_identity = ExecutableIdentity::from_path(&program).ok();
        Ok(Self {
            program,
            args,
            cwd,
            environment,
            stdin: ProcessStdin::Null,
            toolchain: None,
            executable_identity,
            toolchain_artifact: None,
            session_key: None,
            protocol: ProtocolDescriptor::cold(),
            limits,
        })
    }

    /// Constructs a command with explicit native-authority provenance and
    /// protocol policy.
    ///
    /// The executable, workspace, environment, input, identity, protocol,
    /// and input/output/time bounds are retained together so a frontend cannot run
    /// an authority with ambient process state.  A session key is optional for
    /// a cold discovery operation; persistent execution requires one.
    ///
    /// # Errors
    ///
    /// Returns a [`ProcessError`] when paths, standard input, or the command
    /// contract fail validation.
    #[allow(
        clippy::too_many_arguments,
        reason = "the native command boundary keeps every authority input explicit"
    )]
    pub(crate) fn for_authority(
        program: PathBuf,
        args: Vec<String>,
        environment: ProcessEnvironment,
        workspace: PathBuf,
        stdin: ProcessStdin,
        toolchain: ToolchainId,
        session_key: Option<SessionKey>,
        protocol: ProtocolDescriptor,
        limits: ProcessLimits,
    ) -> Result<Self, ProcessError> {
        let mut command = Self::new(program, args, workspace, environment, limits)?;
        command.stdin = stdin;
        command.toolchain = Some(toolchain);
        command.session_key = session_key;
        command.protocol = protocol;
        command.validate_request_bytes()?;
        Ok(command)
    }

    /// Constructs a command bound to a content-checked toolchain artifact.
    ///
    /// # Errors
    ///
    /// Returns a [`ProcessError`] when the command is invalid or the executable
    /// does not match the supplied artifact.
    #[allow(
        clippy::too_many_arguments,
        reason = "the native command boundary keeps every authority input explicit"
    )]
    pub fn for_authority_with_artifact(
        program: PathBuf,
        args: Vec<String>,
        environment: ProcessEnvironment,
        workspace: PathBuf,
        stdin: ProcessStdin,
        artifact: ToolchainArtifact,
        session_key: Option<SessionKey>,
        protocol: ProtocolDescriptor,
        limits: ProcessLimits,
    ) -> Result<Self, ProcessError> {
        let mut command = Self::new(program, args, workspace, environment, limits)?;
        artifact.verify_path(command.program())?;
        command.stdin = stdin;
        command.toolchain = Some(artifact.identity());
        command.executable_identity = Some(artifact.executable().clone());
        command.toolchain_artifact = Some(artifact);
        command.session_key = session_key;
        command.protocol = protocol;
        command.validate_request_bytes()?;
        Ok(command)
    }

    /// Replaces standard input while retaining all other checked command
    /// fields.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::InputLimit`] when the new input exceeds the
    /// request-input bound.
    pub fn with_stdin(mut self, stdin: ProcessStdin) -> Result<Self, ProcessError> {
        self.stdin = stdin;
        self.validate_request_bytes()?;
        Ok(self)
    }

    /// Adds native-authority provenance to an existing cold command.
    ///
    /// This adapter keeps [`SupervisedCommand::new`] source compatible for
    /// generic process users while giving authority frontends one checked
    /// path for the richer command contract.
    ///
    /// # Errors
    ///
    /// Returns a [`ProcessError`] when the authority binding or standard input
    /// fails validation.
    pub(crate) fn bind_authority(
        mut self,
        stdin: ProcessStdin,
        toolchain: ToolchainId,
        session_key: Option<SessionKey>,
        protocol: ProtocolDescriptor,
    ) -> Result<Self, ProcessError> {
        self.stdin = stdin;
        self.toolchain = Some(toolchain);
        self.session_key = session_key;
        self.protocol = protocol;
        self.validate_request_bytes()?;
        Ok(self)
    }

    /// Binds a checked toolchain artifact to an existing command.
    ///
    /// # Errors
    ///
    /// Returns a [`ProcessError`] when the artifact does not match the
    /// executable or the command binding fails validation.
    pub fn bind_authority_with_artifact(
        mut self,
        stdin: ProcessStdin,
        artifact: ToolchainArtifact,
        session_key: Option<SessionKey>,
        protocol: ProtocolDescriptor,
    ) -> Result<Self, ProcessError> {
        artifact.verify_path(&self.program)?;
        self.stdin = stdin;
        self.toolchain = Some(artifact.identity());
        self.executable_identity = Some(artifact.executable().clone());
        self.toolchain_artifact = Some(artifact);
        self.session_key = session_key;
        self.protocol = protocol;
        self.validate_request_bytes()?;
        Ok(self)
    }

    fn validate_request_bytes(&self) -> Result<(), ProcessError> {
        if self.stdin.len() > self.limits.input_bytes() {
            return Err(ProcessError::InputLimit);
        }
        Ok(())
    }

    /// Returns the absolute program path.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Returns the executable path under its native-authority name.
    #[must_use]
    pub fn executable(&self) -> &Path {
        self.program()
    }

    /// Returns arguments in caller order.
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// Returns the absolute working directory.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Returns the explicit environment policy.
    #[must_use]
    pub const fn environment(&self) -> &ProcessEnvironment {
        &self.environment
    }

    /// Returns the explicit standard-input policy.
    #[must_use]
    pub const fn stdin(&self) -> &ProcessStdin {
        &self.stdin
    }

    /// Returns the authority toolchain identity, if one was bound.
    #[must_use]
    pub const fn toolchain(&self) -> Option<ToolchainId> {
        self.toolchain
    }

    /// Returns the content-derived executable identity, when admitted.
    #[must_use]
    pub const fn executable_identity(&self) -> Option<&ExecutableIdentity> {
        self.executable_identity.as_ref()
    }

    /// Returns the checked toolchain artifact, when one was bound.
    #[must_use]
    pub const fn toolchain_artifact(&self) -> Option<&ToolchainArtifact> {
        self.toolchain_artifact.as_ref()
    }

    /// Verifies that the authority executable still has its admitted bytes
    /// and mints a capability for this exact command path.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::ExecutableDrift`] when the executable changed,
    /// or [`ProcessError::ExecutableUnavailable`] when a bound executable
    /// cannot be read.
    pub fn verify_executable(&self) -> Result<super::super::VerifiedExecutable, ProcessError> {
        let identity = if let Some(artifact) = &self.toolchain_artifact {
            artifact.verify_path(&self.program)?;
            Some(artifact.executable().clone())
        } else if let Some(identity) = &self.executable_identity {
            identity.verify_path(&self.program)?;
            Some(identity.clone())
        } else if self.toolchain.is_some() {
            return Err(ProcessError::ExecutableUnavailable);
        } else {
            None
        };
        Ok(super::super::VerifiedExecutable::new(
            &self.program,
            identity,
        ))
    }

    /// Returns the optional input-bound persistent session key.
    #[must_use]
    pub const fn session_key(&self) -> Option<SessionKey> {
        self.session_key
    }

    /// Returns the protocol descriptor carried by this command.
    #[must_use]
    pub const fn protocol(&self) -> ProtocolDescriptor {
        self.protocol
    }

    /// Returns the command workspace.  This is the same path as [`Self::cwd`]
    /// and is named explicitly for native-authority callers.
    #[must_use]
    pub fn workspace(&self) -> &Path {
        self.cwd()
    }

    /// Returns bounded execution limits.
    #[must_use]
    pub const fn limits(&self) -> ProcessLimits {
        self.limits
    }

    /// Returns the canonical identity of this complete command specification.
    ///
    /// The identity binds the executable location, exact argument vector,
    /// workspace, explicit environment, standard input, toolchain binding,
    /// optional session key, protocol descriptor, and every configured limit.
    /// It is useful as a cache or authority fence; spawning still rechecks the
    /// content-derived executable identity immediately before and after spawn.
    #[must_use]
    pub fn identity(&self) -> CommandId {
        let mut encoded = Vec::new();
        put_identity_path(&mut encoded, self.program());
        put_identity_path(&mut encoded, self.cwd());
        put_identity_strings(&mut encoded, &self.args);
        put_identity_environment(&mut encoded, &self.environment);
        match &self.stdin {
            ProcessStdin::Null => encoded.push(0),
            ProcessStdin::Bytes(bytes) => {
                encoded.push(1);
                put_identity_bytes(&mut encoded, bytes);
            }
            ProcessStdin::Shared(bytes) => {
                encoded.push(1);
                put_identity_bytes(&mut encoded, bytes);
            }
        }
        put_identity_optional_id(&mut encoded, self.toolchain);
        put_identity_optional_id(
            &mut encoded,
            self.executable_identity
                .as_ref()
                .map(ExecutableIdentity::digest),
        );
        put_identity_optional_id(
            &mut encoded,
            self.toolchain_artifact
                .as_ref()
                .map(ToolchainArtifact::identity),
        );
        match self.session_key {
            Some(key) => {
                encoded.push(1);
                encoded.extend_from_slice(&key.digest().to_bytes());
                encoded.extend_from_slice(&key.authority().to_bytes());
                encoded.extend_from_slice(&key.manifest().to_bytes());
            }
            None => encoded.push(0),
        }
        encoded.extend_from_slice(&self.protocol.version().to_be_bytes());
        encoded.push(u8::from(self.protocol.advertises_persistent()));
        let limits = self.limits.identity_parts();
        encoded.extend_from_slice(&identity_usize(limits.stdout).to_be_bytes());
        encoded.extend_from_slice(&identity_usize(limits.stderr).to_be_bytes());
        encoded.extend_from_slice(&identity_duration(limits.wall_time));
        encoded.extend_from_slice(&identity_usize(limits.output_bytes).to_be_bytes());
        encoded.extend_from_slice(&identity_usize(limits.input_bytes).to_be_bytes());
        put_identity_optional_usize(&mut encoded, limits.workspace_limit);
        put_identity_optional_usize(&mut encoded, limits.process_count_limit);
        put_identity_optional_usize(&mut encoded, limits.memory_bytes_limit);
        match limits.cpu_time_limit {
            Some(duration) => {
                encoded.push(1);
                encoded.extend_from_slice(&identity_duration(duration));
            }
            None => encoded.push(0),
        }
        typed_of::<CommandSchema>(&encoded)
    }

    /// Runs one cold, bounded subprocess.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when the command is unsupported, cancelled,
    /// exceeds a configured limit, or cannot be spawned or reaped.
    pub fn run(&self) -> Result<ProcessReceipt, ProcessError> {
        crate::supervisor::ProcessSupervisor::new(self.clone()).run()
    }

    /// Runs one subprocess while observing a caller-owned cancellation token.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when cancellation, a configured limit, process
    /// startup, or process completion fails.
    pub fn run_with_cancellation(
        &self,
        cancellation: &Cancellation,
    ) -> Result<ProcessReceipt, ProcessError> {
        crate::supervisor::ProcessSupervisor::new(self.clone()).run_with_cancellation(cancellation)
    }
}

fn identity_usize(value: usize) -> u64 {
    u64::try_from(value).map_or(u64::MAX, |value| value)
}

fn identity_duration(value: Duration) -> [u8; 16] {
    let mut encoded = [0; 16];
    encoded[..8].copy_from_slice(&value.as_secs().to_be_bytes());
    encoded[8..].copy_from_slice(&u64::from(value.subsec_nanos()).to_be_bytes());
    encoded
}

fn put_identity_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&identity_usize(bytes.len()).to_be_bytes());
    output.extend_from_slice(bytes);
}

fn put_identity_path(output: &mut Vec<u8>, path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        put_identity_bytes(output, path.as_os_str().as_bytes());
    }
    #[cfg(not(unix))]
    {
        put_identity_bytes(output, path.to_string_lossy().as_bytes());
    }
}

fn put_identity_strings(output: &mut Vec<u8>, values: &[String]) {
    output.extend_from_slice(&identity_usize(values.len()).to_be_bytes());
    for value in values {
        put_identity_bytes(output, value.as_bytes());
    }
}

fn put_identity_environment(output: &mut Vec<u8>, environment: &ProcessEnvironment) {
    output.extend_from_slice(&identity_usize(environment.variables().len()).to_be_bytes());
    for (key, value) in environment.variables() {
        put_identity_bytes(output, key.as_bytes());
        put_identity_bytes(output, value.as_bytes());
    }
}

fn put_identity_optional_id<T: backend_version::Schema>(
    output: &mut Vec<u8>,
    value: Option<backend_version::ObjectVersion<T>>,
) {
    match value {
        Some(value) => {
            output.push(1);
            output.extend_from_slice(&value.to_bytes());
        }
        None => output.push(0),
    }
}

fn put_identity_optional_usize(output: &mut Vec<u8>, value: Option<usize>) {
    match value {
        Some(value) => {
            output.push(1);
            output.extend_from_slice(&identity_usize(value).to_be_bytes());
        }
        None => output.push(0),
    }
}
