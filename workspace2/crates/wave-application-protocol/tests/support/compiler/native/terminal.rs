use serde::Deserialize;
use wave_application_core::NativePrimaryCause;

use super::super::terminal::GoldenCompilerDiagnostic;
use super::io::GoldenNativeIoFact;
use super::work::{
    GoldenNativeArtifactAction, GoldenNativeArtifactCause, GoldenNativeArtifactRole,
    GoldenNativeDirectoryCause,
};
use super::worker::GoldenNativeWorkerPanic;

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GoldenNativePrimaryCause {
    PrepareDirectory {
        cause: GoldenNativeDirectoryCause,
    },
    PrepareArtifact {
        action: GoldenNativeArtifactAction,
        artifact: GoldenNativeArtifactRole,
        cause: GoldenNativeArtifactCause,
    },
    ToolStart {
        cause: GoldenNativeIoFact,
    },
    WorkerPanic {
        cause: GoldenNativeWorkerPanic,
    },
    MissingInput {
        cleanup: Option<GoldenNativeIoFact>,
    },
    MissingDiagnostic {
        cleanup: Option<GoldenNativeIoFact>,
    },
    Input {
        cause: GoldenNativeIoFact,
        cleanup: Option<GoldenNativeIoFact>,
    },
    Terminate {
        cause: GoldenNativeIoFact,
    },
    Wait {
        cause: GoldenNativeIoFact,
        cleanup: Option<GoldenNativeIoFact>,
    },
    DiagnosticRead {
        cause: GoldenNativeIoFact,
        cleanup: Option<GoldenNativeIoFact>,
    },
    Cancelled {
        diagnostic: Option<GoldenCompilerDiagnostic>,
    },
    DeadlineExceeded {
        diagnostic: Option<GoldenCompilerDiagnostic>,
    },
    DiagnosticLimit {
        limit: usize,
        observed: usize,
        diagnostic: Option<GoldenCompilerDiagnostic>,
    },
    Rejected {
        code: Option<i32>,
        diagnostic: Option<GoldenCompilerDiagnostic>,
    },
}

impl From<NativePrimaryCause> for GoldenNativePrimaryCause {
    fn from(cause: NativePrimaryCause) -> Self {
        match cause {
            NativePrimaryCause::PrepareDirectory(cause) => Self::PrepareDirectory {
                cause: cause.into(),
            },
            NativePrimaryCause::PrepareArtifact {
                action,
                artifact,
                cause,
            } => Self::PrepareArtifact {
                action: action.into(),
                artifact: artifact.into(),
                cause: cause.into(),
            },
            NativePrimaryCause::ToolStart(cause) => Self::ToolStart {
                cause: cause.into(),
            },
            NativePrimaryCause::WorkerPanic(cause) => Self::WorkerPanic {
                cause: cause.into(),
            },
            NativePrimaryCause::MissingInput { cleanup } => Self::MissingInput {
                cleanup: cleanup.map(Into::into),
            },
            NativePrimaryCause::MissingDiagnostic { cleanup } => Self::MissingDiagnostic {
                cleanup: cleanup.map(Into::into),
            },
            NativePrimaryCause::Input { cause, cleanup } => Self::Input {
                cause: cause.into(),
                cleanup: cleanup.map(Into::into),
            },
            NativePrimaryCause::Terminate(cause) => Self::Terminate {
                cause: cause.into(),
            },
            NativePrimaryCause::Wait { cause, cleanup } => Self::Wait {
                cause: cause.into(),
                cleanup: cleanup.map(Into::into),
            },
            NativePrimaryCause::DiagnosticRead { cause, cleanup } => Self::DiagnosticRead {
                cause: cause.into(),
                cleanup: cleanup.map(Into::into),
            },
            NativePrimaryCause::Cancelled(diagnostic) => Self::Cancelled {
                diagnostic: diagnostic.map(Into::into),
            },
            NativePrimaryCause::DeadlineExceeded(diagnostic) => Self::DeadlineExceeded {
                diagnostic: diagnostic.map(Into::into),
            },
            NativePrimaryCause::DiagnosticLimit {
                limit,
                observed,
                diagnostic,
            } => Self::DiagnosticLimit {
                limit,
                observed,
                diagnostic: diagnostic.map(Into::into),
            },
            NativePrimaryCause::Rejected { code, diagnostic } => Self::Rejected {
                code,
                diagnostic: diagnostic.map(Into::into),
            },
        }
    }
}
