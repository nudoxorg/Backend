//! Exact caller-owned native-work and paired-cleanup terminal projection.

use nudox_compile_driver::{NativeWorkError, NativeWorkPrimary};
use wave_application_core::{
    NativeArtifactAction, NativeArtifactCause, NativeArtifactRole, NativeDirectoryCause,
    NativePrimaryCause, NativeWorkCause, NativeWorkCleanupCause, NativeWorkPhase,
};

use super::super::common::{compiler_diagnostic, io_fact};

pub(super) fn native_work_cause(phase: NativeWorkPhase, cause: NativeWorkError) -> NativeWorkCause {
    match cause {
        NativeWorkError::RelativeDirectory => {
            directory(phase, NativeDirectoryCause::RelativeDirectory)
        }
        NativeWorkError::NotEmpty => directory(phase, NativeDirectoryCause::NotEmpty),
        NativeWorkError::Inspect(cause) => {
            directory(phase, NativeDirectoryCause::InspectIo(io_fact(&cause)))
        }
        NativeWorkError::WriteArtifact { artifact, cause } => {
            native_artifact(phase, NativeArtifactAction::Write, artifact, &cause)
        }
        NativeWorkError::ArtifactText { artifact, fact } => NativeWorkCause::Artifact {
            phase,
            action: NativeArtifactAction::ResolveText,
            artifact,
            cause: NativeArtifactCause::InvalidText(fact),
        },
        NativeWorkError::CreateArtifactDirectory { artifact, cause } => native_artifact(
            phase,
            NativeArtifactAction::CreateDirectory,
            artifact,
            &cause,
        ),
        NativeWorkError::RemoveArtifact { artifact, cause } => {
            native_artifact(phase, NativeArtifactAction::Remove, artifact, &cause)
        }
    }
}

pub(super) fn native_work_cleanup(cause: NativeWorkError) -> NativeWorkCleanupCause {
    match cause {
        NativeWorkError::RelativeDirectory => NativeWorkCleanupCause::RelativeDirectory,
        NativeWorkError::NotEmpty => NativeWorkCleanupCause::NotEmpty,
        NativeWorkError::Inspect(cause) => NativeWorkCleanupCause::Io(io_fact(&cause)),
        NativeWorkError::WriteArtifact { artifact, cause } => NativeWorkCleanupCause::Artifact {
            action: NativeArtifactAction::Write,
            artifact,
            cause: NativeArtifactCause::Io(io_fact(&cause)),
        },
        NativeWorkError::ArtifactText { artifact, fact } => NativeWorkCleanupCause::Artifact {
            action: NativeArtifactAction::ResolveText,
            artifact,
            cause: NativeArtifactCause::InvalidText(fact),
        },
        NativeWorkError::CreateArtifactDirectory { artifact, cause } => {
            NativeWorkCleanupCause::Artifact {
                action: NativeArtifactAction::CreateDirectory,
                artifact,
                cause: NativeArtifactCause::Io(io_fact(&cause)),
            }
        }
        NativeWorkError::RemoveArtifact { artifact, cause } => NativeWorkCleanupCause::Artifact {
            action: NativeArtifactAction::Remove,
            artifact,
            cause: NativeArtifactCause::Io(io_fact(&cause)),
        },
    }
}

pub(super) fn native_primary(primary: NativeWorkPrimary<'_>) -> NativePrimaryCause {
    match primary {
        NativeWorkPrimary::Prepare { cause } => prepare_primary(cause),
        NativeWorkPrimary::ToolStart { cause } => NativePrimaryCause::ToolStart(io_fact(&cause)),
        NativeWorkPrimary::MissingToolInput => missing_input(None),
        NativeWorkPrimary::MissingToolInputCleanup { cleanup } => missing_input(Some(&cleanup)),
        NativeWorkPrimary::MissingToolDiagnostic => missing_diagnostic(None),
        NativeWorkPrimary::MissingToolDiagnosticCleanup { cleanup } => {
            missing_diagnostic(Some(&cleanup))
        }
        NativeWorkPrimary::ToolInput { cause } => input(&cause, None),
        NativeWorkPrimary::ToolInputCleanup { cause, cleanup } => input(&cause, Some(&cleanup)),
        NativeWorkPrimary::ToolTerminate { cause } => {
            NativePrimaryCause::Terminate(io_fact(&cause))
        }
        NativeWorkPrimary::ToolWait { cause } => wait(&cause, None),
        NativeWorkPrimary::ToolWaitCleanup { cause, cleanup } => wait(&cause, Some(&cleanup)),
        NativeWorkPrimary::ToolDiagnosticRead { cause } => diagnostic_read(&cause, None),
        NativeWorkPrimary::ToolDiagnosticReadCleanup { cause, cleanup } => {
            diagnostic_read(&cause, Some(&cleanup))
        }
        NativeWorkPrimary::WorkerPanic { cause } => NativePrimaryCause::WorkerPanic(cause),
        NativeWorkPrimary::Cancelled { diagnostic } => {
            NativePrimaryCause::Cancelled(compiler_diagnostic(diagnostic))
        }
        NativeWorkPrimary::DeadlineExceeded { diagnostic } => {
            NativePrimaryCause::DeadlineExceeded(compiler_diagnostic(diagnostic))
        }
        NativeWorkPrimary::DiagnosticLimit {
            limit,
            observed,
            diagnostic,
        } => NativePrimaryCause::DiagnosticLimit {
            limit,
            observed,
            diagnostic: compiler_diagnostic(diagnostic),
        },
        NativeWorkPrimary::NativeRejected { status, diagnostic } => NativePrimaryCause::Rejected {
            code: status.code(),
            diagnostic: compiler_diagnostic(diagnostic),
        },
    }
}

fn native_artifact(
    phase: NativeWorkPhase,
    action: NativeArtifactAction,
    artifact: NativeArtifactRole,
    cause: &std::io::Error,
) -> NativeWorkCause {
    NativeWorkCause::Artifact {
        phase,
        action,
        artifact,
        cause: NativeArtifactCause::Io(io_fact(cause)),
    }
}

const fn directory(phase: NativeWorkPhase, cause: NativeDirectoryCause) -> NativeWorkCause {
    NativeWorkCause::Directory { phase, cause }
}

fn missing_input(cleanup: Option<&std::io::Error>) -> NativePrimaryCause {
    NativePrimaryCause::MissingInput {
        cleanup: cleanup.map(io_fact),
    }
}

fn missing_diagnostic(cleanup: Option<&std::io::Error>) -> NativePrimaryCause {
    NativePrimaryCause::MissingDiagnostic {
        cleanup: cleanup.map(io_fact),
    }
}

fn input(cause: &std::io::Error, cleanup: Option<&std::io::Error>) -> NativePrimaryCause {
    NativePrimaryCause::Input {
        cause: io_fact(cause),
        cleanup: cleanup.map(io_fact),
    }
}

fn wait(cause: &std::io::Error, cleanup: Option<&std::io::Error>) -> NativePrimaryCause {
    NativePrimaryCause::Wait {
        cause: io_fact(cause),
        cleanup: cleanup.map(io_fact),
    }
}

fn diagnostic_read(cause: &std::io::Error, cleanup: Option<&std::io::Error>) -> NativePrimaryCause {
    NativePrimaryCause::DiagnosticRead {
        cause: io_fact(cause),
        cleanup: cleanup.map(io_fact),
    }
}

fn prepare_primary(cause: NativeWorkError) -> NativePrimaryCause {
    match cause {
        NativeWorkError::RelativeDirectory => {
            NativePrimaryCause::PrepareDirectory(NativeDirectoryCause::RelativeDirectory)
        }
        NativeWorkError::NotEmpty => {
            NativePrimaryCause::PrepareDirectory(NativeDirectoryCause::NotEmpty)
        }
        NativeWorkError::Inspect(cause) => {
            NativePrimaryCause::PrepareDirectory(NativeDirectoryCause::InspectIo(io_fact(&cause)))
        }
        NativeWorkError::WriteArtifact { artifact, cause } => NativePrimaryCause::PrepareArtifact {
            action: NativeArtifactAction::Write,
            artifact,
            cause: NativeArtifactCause::Io(io_fact(&cause)),
        },
        NativeWorkError::ArtifactText { artifact, fact } => NativePrimaryCause::PrepareArtifact {
            action: NativeArtifactAction::ResolveText,
            artifact,
            cause: NativeArtifactCause::InvalidText(fact),
        },
        NativeWorkError::CreateArtifactDirectory { artifact, cause } => {
            NativePrimaryCause::PrepareArtifact {
                action: NativeArtifactAction::CreateDirectory,
                artifact,
                cause: NativeArtifactCause::Io(io_fact(&cause)),
            }
        }
        NativeWorkError::RemoveArtifact { artifact, cause } => {
            NativePrimaryCause::PrepareArtifact {
                action: NativeArtifactAction::Remove,
                artifact,
                cause: NativeArtifactCause::Io(io_fact(&cause)),
            }
        }
    }
}
