//! Closed startup observations shared by product surfaces.
//!
//! Recovery is chosen from the runtime variant and I/O kind, never from its
//! diagnostic text. The original diagnostic remains available for rendering.

use crate::RuntimeError;
use std::fmt;
use std::io::ErrorKind;

/// The action justified by a startup failure before any command was submitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupRecovery {
    /// Another bounded attempt can observe a pending owner or transient failure.
    RetryOriginalCommand,
    /// Install matched product binaries or rebuild the selected companion pair.
    RepairCompanion,
    /// Resolve the reported setup refusal before attempting the command again.
    RepairConfiguration,
}

/// The runtime cause retained when an error crosses into a client observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupFailureCause {
    /// A filesystem or process operation failed with this kind.
    Io(ErrorKind),
    /// Filesystem admission of the selected configuration failed with this kind.
    ConfigurationIo(ErrorKind),
    /// A selected path, workspace, credential, protocol, or platform was refused.
    Configuration,
    /// The selected companion executable is absent.
    MissingCompanion,
    /// Creating the selected owner process failed with this kind.
    Spawn(ErrorKind),
    /// Passive connection could not reach the selected endpoint.
    EndpointUnavailable(ErrorKind),
    /// The owner exited before readiness, with its actual exit code if available.
    OwnerExited(Option<i32>),
    /// An owner candidate is still starting; no command was submitted.
    #[cfg(unix)]
    OwnerPending,
    /// The selected companion does not implement the matched protocol.
    #[cfg(unix)]
    CompanionMismatch,
    /// The owner readiness deadline expired.
    StartTimeout,
}

impl StartupFailureCause {
    /// Classifies recovery without interpreting diagnostic prose.
    #[must_use]
    pub fn recovery(self) -> StartupRecovery {
        match self {
            Self::MissingCompanion => StartupRecovery::RepairCompanion,
            #[cfg(unix)]
            Self::CompanionMismatch => StartupRecovery::RepairCompanion,
            Self::Io(kind) | Self::EndpointUnavailable(kind)
                if is_transient_io(kind) || matches!(kind, ErrorKind::NotFound) =>
            {
                StartupRecovery::RetryOriginalCommand
            }
            Self::Spawn(kind) if is_transient_io(kind) => StartupRecovery::RetryOriginalCommand,
            Self::OwnerExited(Some(code))
                if code == i32::from(crate::OWNER_CONTENDED_EXIT_CODE) =>
            {
                StartupRecovery::RetryOriginalCommand
            }
            #[cfg(unix)]
            Self::OwnerPending => StartupRecovery::RetryOriginalCommand,
            Self::StartTimeout => StartupRecovery::RetryOriginalCommand,
            Self::Io(_)
            | Self::Spawn(_)
            | Self::EndpointUnavailable(_)
            | Self::ConfigurationIo(_)
            | Self::Configuration
            | Self::OwnerExited(_) => StartupRecovery::RepairConfiguration,
        }
    }
}

/// A typed observation of the actual runtime failure, with its original detail.
///
/// This value does not clone an I/O error or parse the owner's diagnostic. Its
/// closed cause preserves recovery semantics through cloneable client errors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartupFailure {
    cause: StartupFailureCause,
    detail: String,
}

impl StartupFailure {
    /// Returns the actual closed runtime cause.
    #[must_use]
    pub const fn cause(&self) -> StartupFailureCause {
        self.cause
    }

    /// Returns the action justified by that cause.
    #[must_use]
    pub fn recovery(&self) -> StartupRecovery {
        self.cause.recovery()
    }

    /// Returns the original runtime diagnostic, including path and owner detail.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl From<RuntimeError> for StartupFailure {
    fn from(error: RuntimeError) -> Self {
        Self {
            cause: error.startup_cause(),
            detail: error.to_string(),
        }
    }
}

impl fmt::Display for StartupFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl RuntimeError {
    /// Returns the closed startup cause without erasing its I/O kind.
    #[must_use]
    pub fn startup_cause(&self) -> StartupFailureCause {
        match self {
            Self::Io(source) => StartupFailureCause::Io(source.kind()),
            Self::DefaultStateInitialization { source, .. } => {
                StartupFailureCause::ConfigurationIo(source.kind())
            }
            Self::InvalidPath(_)
            | Self::WorkspaceProjectMismatch { .. }
            | Self::InvalidCredential(_)
            | Self::EndpointTooLong { .. }
            | Self::Unsupported => StartupFailureCause::Configuration,
            Self::MissingExecutable(_) => StartupFailureCause::MissingCompanion,
            Self::Spawn { source, .. } => StartupFailureCause::Spawn(source.kind()),
            Self::EndpointUnavailable { source, .. } => {
                StartupFailureCause::EndpointUnavailable(source.kind())
            }
            Self::DaemonExited { code, .. } => StartupFailureCause::OwnerExited(*code),
            #[cfg(unix)]
            Self::StartupPending(_) => StartupFailureCause::OwnerPending,
            #[cfg(unix)]
            Self::StartupChannel(_) => StartupFailureCause::Configuration,
            #[cfg(unix)]
            Self::CompanionProtocolMismatch => StartupFailureCause::CompanionMismatch,
            Self::StartTimeout(_) => StartupFailureCause::StartTimeout,
        }
    }

    /// Returns the shared recovery policy for CLI and client presentation.
    #[must_use]
    pub fn startup_recovery(&self) -> StartupRecovery {
        self.startup_cause().recovery()
    }

    /// Preserves the established MCP restart-window reconnect boundary.
    ///
    /// Only direct runtime I/O in these classes triggers automatic reconnect;
    /// a missing executable, rejected path, or spawn refusal must be reported.
    #[must_use]
    pub fn reconnect_kind(&self) -> Option<ErrorKind> {
        match self {
            Self::Io(source)
                if is_disconnect_io(source.kind()) || source.kind() == ErrorKind::NotFound =>
            {
                Some(source.kind())
            }
            _ => None,
        }
    }
}

const fn is_transient_io(kind: ErrorKind) -> bool {
    is_disconnect_io(kind) || matches!(kind, ErrorKind::Interrupted)
}

const fn is_disconnect_io(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionRefused
            | ErrorKind::BrokenPipe
            | ErrorKind::NotConnected
            | ErrorKind::UnexpectedEof
            | ErrorKind::TimedOut
            | ErrorKind::WouldBlock
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StartupDiagnostic;
    use std::io;
    use std::path::PathBuf;

    #[test]
    fn actual_runtime_errors_keep_original_detail_and_closed_recovery() {
        use StartupFailureCause as Cause;
        use StartupRecovery as Recovery;
        let cases = [
            (
                RuntimeError::MissingExecutable(PathBuf::from("/debug/deps/backend-locald")),
                Cause::MissingCompanion,
                Recovery::RepairCompanion,
            ),
            (
                RuntimeError::InvalidPath("invalid endpoint; ConnectionRefused; retry"),
                Cause::Configuration,
                Recovery::RepairConfiguration,
            ),
            (
                RuntimeError::WorkspaceProjectMismatch {
                    project: PathBuf::from("/project"),
                    workspace: PathBuf::from("/state"),
                    workspace_project: PathBuf::from("/other"),
                },
                Cause::Configuration,
                Recovery::RepairConfiguration,
            ),
            (
                RuntimeError::InvalidCredential(PathBuf::from("/state/authority.secret")),
                Cause::Configuration,
                Recovery::RepairConfiguration,
            ),
            (
                RuntimeError::EndpointTooLong {
                    endpoint: PathBuf::from("/long/socket"),
                    bytes: 200,
                    limit: 103,
                },
                Cause::Configuration,
                Recovery::RepairConfiguration,
            ),
            (
                RuntimeError::DefaultStateInitialization {
                    path: PathBuf::from("/profile/state"),
                    source: io::Error::from(ErrorKind::PermissionDenied),
                },
                Cause::ConfigurationIo(ErrorKind::PermissionDenied),
                Recovery::RepairConfiguration,
            ),
            (
                RuntimeError::Io(io::Error::new(
                    ErrorKind::PermissionDenied,
                    "ConnectionRefused; retry",
                )),
                Cause::Io(ErrorKind::PermissionDenied),
                Recovery::RepairConfiguration,
            ),
            (
                RuntimeError::Io(io::Error::new(
                    ErrorKind::ConnectionRefused,
                    "install matched binaries",
                )),
                Cause::Io(ErrorKind::ConnectionRefused),
                Recovery::RetryOriginalCommand,
            ),
            (
                RuntimeError::Spawn {
                    executable: PathBuf::from("/bin/backend-locald"),
                    source: io::Error::from(ErrorKind::PermissionDenied),
                },
                Cause::Spawn(ErrorKind::PermissionDenied),
                Recovery::RepairConfiguration,
            ),
            (
                RuntimeError::Spawn {
                    executable: PathBuf::from("/bin/backend-locald"),
                    source: io::Error::from(ErrorKind::NotFound),
                },
                Cause::Spawn(ErrorKind::NotFound),
                Recovery::RepairConfiguration,
            ),
            (
                RuntimeError::Spawn {
                    executable: PathBuf::from("/bin/backend-locald"),
                    source: io::Error::from(ErrorKind::Interrupted),
                },
                Cause::Spawn(ErrorKind::Interrupted),
                Recovery::RetryOriginalCommand,
            ),
            (
                RuntimeError::EndpointUnavailable {
                    endpoint: PathBuf::from("/socket"),
                    source: io::Error::from(ErrorKind::ConnectionRefused),
                },
                Cause::EndpointUnavailable(ErrorKind::ConnectionRefused),
                Recovery::RetryOriginalCommand,
            ),
            (
                RuntimeError::EndpointUnavailable {
                    endpoint: PathBuf::from("/socket"),
                    source: io::Error::from(ErrorKind::PermissionDenied),
                },
                Cause::EndpointUnavailable(ErrorKind::PermissionDenied),
                Recovery::RepairConfiguration,
            ),
            (
                RuntimeError::DaemonExited {
                    code: Some(70),
                    diagnostic: Some(StartupDiagnostic(
                        "private state parent /state was refused".to_owned(),
                    )),
                },
                Cause::OwnerExited(Some(70)),
                Recovery::RepairConfiguration,
            ),
            (
                RuntimeError::DaemonExited {
                    code: None,
                    diagnostic: None,
                },
                Cause::OwnerExited(None),
                Recovery::RepairConfiguration,
            ),
            (
                RuntimeError::DaemonExited {
                    code: Some(i32::from(crate::OWNER_CONTENDED_EXIT_CODE)),
                    diagnostic: None,
                },
                Cause::OwnerExited(Some(i32::from(crate::OWNER_CONTENDED_EXIT_CODE))),
                Recovery::RetryOriginalCommand,
            ),
            (
                RuntimeError::StartTimeout(PathBuf::from("/socket")),
                Cause::StartTimeout,
                Recovery::RetryOriginalCommand,
            ),
            (
                RuntimeError::Unsupported,
                Cause::Configuration,
                Recovery::RepairConfiguration,
            ),
        ];
        for (error, cause, recovery) in cases {
            let detail = error.to_string();
            assert_eq!(error.startup_cause(), cause, "{detail}");
            assert_eq!(error.startup_recovery(), recovery, "{detail}");
            let observation = StartupFailure::from(error);
            assert_eq!(observation.cause(), cause);
            assert_eq!(observation.recovery(), recovery);
            assert_eq!(observation.detail(), detail);
            assert_eq!(observation.to_string(), detail);
        }
    }

    #[cfg(unix)]
    #[test]
    fn startup_protocol_failures_require_repair_without_diagnostic_guessing() {
        let mismatch = RuntimeError::CompanionProtocolMismatch;
        assert_eq!(
            mismatch.startup_recovery(),
            StartupRecovery::RepairCompanion
        );
        let observation = StartupFailure::from(mismatch);
        assert_eq!(observation.cause(), StartupFailureCause::CompanionMismatch);
        assert!(
            observation
                .detail()
                .contains("install matched CLI and locald or rebuild")
        );
        let channel = RuntimeError::StartupChannel("invalid startup record; retry");
        assert_eq!(
            channel.startup_recovery(),
            StartupRecovery::RepairConfiguration
        );
        assert_eq!(
            StartupFailure::from(channel).cause(),
            StartupFailureCause::Configuration
        );
    }

    #[test]
    fn reconnect_is_limited_to_the_existing_direct_io_restart_classes() {
        for kind in [
            ErrorKind::ConnectionReset,
            ErrorKind::ConnectionAborted,
            ErrorKind::ConnectionRefused,
            ErrorKind::BrokenPipe,
            ErrorKind::NotConnected,
            ErrorKind::UnexpectedEof,
            ErrorKind::TimedOut,
            ErrorKind::WouldBlock,
            ErrorKind::NotFound,
        ] {
            let error = RuntimeError::Io(io::Error::from(kind));
            assert_eq!(error.reconnect_kind(), Some(kind));
            assert_eq!(
                error.startup_recovery(),
                StartupRecovery::RetryOriginalCommand
            );
            let spawn = RuntimeError::Spawn {
                executable: PathBuf::from("/bin/locald"),
                source: io::Error::from(kind),
            };
            assert_eq!(spawn.reconnect_kind(), None);
        }
        for kind in [
            ErrorKind::InvalidData,
            ErrorKind::InvalidInput,
            ErrorKind::PermissionDenied,
            ErrorKind::AlreadyExists,
            ErrorKind::Other,
        ] {
            let error = RuntimeError::Io(io::Error::from(kind));
            assert_eq!(error.reconnect_kind(), None);
            assert_eq!(
                error.startup_recovery(),
                StartupRecovery::RepairConfiguration
            );
        }
    }
}
