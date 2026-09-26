//! Checked native command templates for language authority helpers.

use super::{HelperFailure, NativeTemplate};
use crate::{
    AuthorityError, AuthorityIdentity, ExecutableIdentity, ProcessEnvironment, ProcessLimits,
    ProcessStdin, ProtocolDescriptor, SessionKey, SupervisedCommand, ToolchainId,
};
use std::path::{Path, PathBuf};

impl NativeTemplate {
    /// Builds a controlled command template for an absolute helper.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorityError`] when a helper or toolchain path is not
    /// absolute, the workspace cannot be determined, or the command contract
    /// is invalid.
    pub fn new(
        language: &str,
        helper: impl Into<PathBuf>,
        toolchain_path: impl Into<PathBuf>,
        toolchain: ToolchainId,
        protocol: ProtocolDescriptor,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        let helper = helper.into();
        let toolchain_path = toolchain_path.into();
        if !toolchain_path.is_absolute() {
            return Err(AuthorityError::Discovery(
                "native authority toolchain must be absolute".into(),
            ));
        }
        if !helper.is_absolute() {
            return Err(AuthorityError::Discovery(
                "native authority helper must be absolute".into(),
            ));
        }
        let cwd = std::env::current_dir().map_err(|error| {
            AuthorityError::Discovery(format!("native authority workspace unavailable: {error}"))
        })?;
        let environment = ProcessEnvironment::new(vec![
            ("BACKEND_NATIVE_LANGUAGE".into(), language.to_owned()),
            (
                "BACKEND_NATIVE_PROTOCOL".into(),
                protocol.version().to_string(),
            ),
            (
                "BACKEND_NATIVE_TOOLCHAIN".into(),
                toolchain_path.to_string_lossy().into_owned(),
            ),
            ("LANG".into(), "C".into()),
            ("LC_ALL".into(), "C".into()),
        ])
        .map_err(|error| AuthorityError::Discovery(error.to_string()))?;
        let command = SupervisedCommand::for_authority(
            helper.clone(),
            vec!["--backend-native-authority".into(), language.to_owned()],
            environment,
            cwd,
            ProcessStdin::null(),
            toolchain,
            None,
            protocol,
            limits,
        )
        .map_err(|error| AuthorityError::Discovery(error.to_string()))?;
        Ok(Self {
            command,
            helper,
            toolchain: toolchain_path,
            protocol,
            failure: HelperFailure::Fault,
        })
    }

    /// Builds the source-distributed Go helper under its verified toolchain.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorityError`] when either path is relative, the workspace
    /// is unavailable, or the resulting command violates process policy.
    pub fn go_source(
        language: &str,
        helper: impl Into<PathBuf>,
        interpreter: impl Into<PathBuf>,
        toolchain: ToolchainId,
        protocol: ProtocolDescriptor,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        let helper = helper.into();
        let interpreter = interpreter.into();
        if !helper.is_absolute() || !interpreter.is_absolute() {
            return Err(AuthorityError::Discovery(
                "native authority helper and interpreter must be absolute".into(),
            ));
        }
        let cache = std::env::temp_dir().join("backend-go-helper-cache");
        let canonical_interpreter = std::fs::canonicalize(&interpreter)
            .map_err(|error| AuthorityError::Discovery(format!("resolve Go toolchain: {error}")))?;
        let go_root = canonical_interpreter
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| AuthorityError::Discovery("Go toolchain has no GOROOT".into()))?;
        let workspace = if helper.is_dir() {
            helper.clone()
        } else {
            helper
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| AuthorityError::Discovery("Go helper has no parent".into()))?
        };
        let environment = ProcessEnvironment::new(vec![
            ("BACKEND_NATIVE_LANGUAGE".into(), language.to_owned()),
            (
                "BACKEND_NATIVE_PROTOCOL".into(),
                protocol.version().to_string(),
            ),
            (
                "BACKEND_NATIVE_TOOLCHAIN".into(),
                interpreter.to_string_lossy().into_owned(),
            ),
            ("LANG".into(), "C".into()),
            ("LC_ALL".into(), "C".into()),
            ("HOME".into(), cache.to_string_lossy().into_owned()),
            (
                "GOCACHE".into(),
                cache.join("build").to_string_lossy().into_owned(),
            ),
            (
                "GOPATH".into(),
                cache.join("path").to_string_lossy().into_owned(),
            ),
            ("GOROOT".into(), go_root.to_string_lossy().into_owned()),
            ("GOPROXY".into(), "off".into()),
            ("GOSUMDB".into(), "off".into()),
            ("GOTOOLCHAIN".into(), "local".into()),
            ("GOFLAGS".into(), "-mod=vendor -p=2".into()),
        ])
        .map_err(|error| AuthorityError::Discovery(error.to_string()))?;
        let command = SupervisedCommand::for_authority(
            interpreter.clone(),
            vec![
                "run".into(),
                ".".into(),
                "--backend-native-authority".into(),
                language.to_owned(),
            ],
            environment,
            workspace,
            ProcessStdin::null(),
            toolchain,
            None,
            protocol,
            limits,
        )
        .map_err(|error| AuthorityError::Discovery(error.to_string()))?;
        Ok(Self {
            command,
            helper,
            toolchain: interpreter,
            protocol,
            failure: HelperFailure::Unavailable,
        })
    }

    /// Builds a source-distributed Java helper under a verified Java runtime.
    ///
    /// # Errors
    /// Returns [`AuthorityError`] when either path is relative or the command
    /// violates the bounded process policy.
    pub fn java_source(
        language: &str,
        helper: impl Into<PathBuf>,
        runtime: impl Into<PathBuf>,
        toolchain: ToolchainId,
        protocol: ProtocolDescriptor,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        let helper = helper.into();
        let runtime = runtime.into();
        if !helper.is_absolute() || !runtime.is_absolute() {
            return Err(AuthorityError::Discovery(
                "native authority helper and runtime must be absolute".into(),
            ));
        }
        let workspace = helper
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| AuthorityError::Discovery("Java helper has no parent".into()))?;
        let environment = ProcessEnvironment::new(vec![
            ("BACKEND_NATIVE_LANGUAGE".into(), language.to_owned()),
            (
                "BACKEND_NATIVE_PROTOCOL".into(),
                protocol.version().to_string(),
            ),
            (
                "BACKEND_NATIVE_TOOLCHAIN".into(),
                runtime.to_string_lossy().into_owned(),
            ),
            ("LANG".into(), "C".into()),
            ("LC_ALL".into(), "C".into()),
        ])
        .map_err(|error| AuthorityError::Discovery(error.to_string()))?;
        let command = SupervisedCommand::for_authority(
            runtime.clone(),
            vec![
                helper.to_string_lossy().into_owned(),
                "--backend-native-authority".into(),
                language.to_owned(),
            ],
            environment,
            workspace,
            ProcessStdin::null(),
            toolchain,
            None,
            protocol,
            limits,
        )
        .map_err(|error| AuthorityError::Discovery(error.to_string()))?;
        Ok(Self {
            command,
            helper,
            toolchain: runtime,
            protocol,
            failure: HelperFailure::Unavailable,
        })
    }

    /// Builds a source-distributed script helper under a verified interpreter.
    ///
    /// # Errors
    /// Returns [`AuthorityError`] when either path is relative or the command
    /// violates the bounded process policy.
    pub fn script_source(
        language: &str,
        helper: impl Into<PathBuf>,
        interpreter: impl Into<PathBuf>,
        toolchain: ToolchainId,
        protocol: ProtocolDescriptor,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        let helper = helper.into();
        let interpreter = interpreter.into();
        if !helper.is_absolute() || !interpreter.is_absolute() {
            return Err(AuthorityError::Discovery(
                "native authority helper and interpreter must be absolute".into(),
            ));
        }
        let workspace = helper
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| AuthorityError::Discovery("script helper has no parent".into()))?;
        let environment = ProcessEnvironment::new(vec![
            ("BACKEND_NATIVE_LANGUAGE".into(), language.to_owned()),
            (
                "BACKEND_NATIVE_PROTOCOL".into(),
                protocol.version().to_string(),
            ),
            (
                "BACKEND_NATIVE_TOOLCHAIN".into(),
                interpreter.to_string_lossy().into_owned(),
            ),
            ("LANG".into(), "C".into()),
            ("LC_ALL".into(), "C".into()),
            ("PYTHONHASHSEED".into(), "0".into()),
            ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
        ])
        .map_err(|error| AuthorityError::Discovery(error.to_string()))?;
        let command = SupervisedCommand::for_authority(
            interpreter.clone(),
            vec![
                "-I".into(),
                helper.to_string_lossy().into_owned(),
                "--backend-native-authority".into(),
                language.to_owned(),
            ],
            environment,
            workspace,
            ProcessStdin::null(),
            toolchain,
            None,
            protocol,
            limits,
        )
        .map_err(|error| AuthorityError::Discovery(error.to_string()))?;
        Ok(Self {
            command,
            helper,
            toolchain: interpreter,
            protocol,
            failure: HelperFailure::Unavailable,
        })
    }

    /// Returns the checked command specification.
    #[must_use]
    pub const fn command(&self) -> &SupervisedCommand {
        &self.command
    }

    /// Returns the exact configured helper path.
    #[must_use]
    pub fn helper(&self) -> &Path {
        &self.helper
    }

    /// Returns the exact absolute toolchain path bound to this helper.
    #[must_use]
    pub fn toolchain(&self) -> &Path {
        &self.toolchain
    }

    /// Returns the advertised helper protocol.
    #[must_use]
    pub const fn protocol(&self) -> ProtocolDescriptor {
        self.protocol
    }

    /// Returns whether this helper advertises persistent sessions.
    #[must_use]
    pub const fn persistent(&self) -> bool {
        self.protocol.supports_persistent()
    }

    pub(super) const fn failure_is_unavailable(&self) -> bool {
        matches!(self.failure, HelperFailure::Unavailable)
    }

    pub(super) fn verify_toolchain(&self) -> Result<(), AuthorityError> {
        let current =
            ExecutableIdentity::from_path(&self.toolchain).map_err(AuthorityError::Process)?;
        if self.command.toolchain() != Some(current.digest()) {
            return Err(AuthorityError::Process(
                crate::ProcessError::ExecutableDrift,
            ));
        }
        Ok(())
    }

    pub(super) fn command_for(
        &self,
        identity: &AuthorityIdentity,
        key: SessionKey,
        request: Option<ProcessStdin>,
    ) -> Result<SupervisedCommand, AuthorityError> {
        let stdin = match request {
            Some(stdin) => stdin,
            None => ProcessStdin::null(),
        };
        self.command
            .clone()
            .bind_authority(stdin, identity.toolchain, Some(key), self.protocol)
            .map_err(|error| AuthorityError::Discovery(error.to_string()))
    }
}
