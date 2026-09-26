//! Native work, artifact, and child-lifecycle causes.
//!
//! Compiler attempts, publication phases, and diagnostic storage stay with
//! the compiler boundary. These causes name the native failure without
//! formatting an operating-system error.

use super::{
    CompilerDiagnostic, InvalidUtf8Fact, NativeArtifactRole, NativeWorkPhase, NativeWorkerPanic,
};
use std::io::ErrorKind;

/// Exact caller-owned native-work phase projected without an allocation.
#[derive(Debug, Eq, PartialEq)]
pub enum NativeWorkCause {
    /// One exact caller-owned work-directory phase failed before or after native execution.
    Directory {
        /// Exact work-directory phase retaining this terminal.
        phase: NativeWorkPhase,
        /// Closed typed directory failure cause.
        cause: NativeDirectoryCause,
    },
    /// A named native adapter artifact could not be materialized or removed.
    Artifact {
        /// Native-work lifecycle phase owning the operation.
        phase: NativeWorkPhase,
        /// Exact filesystem action the adapter attempted.
        action: NativeArtifactAction,
        /// Closed owned artifact role.
        artifact: NativeArtifactRole,
        /// Exact materialization or text-conversion rejection.
        cause: NativeArtifactCause,
    },
    /// A native child reached one exact terminal without a second work-directory terminal.
    Primary(NativePrimaryCause),
    /// A native primary terminal and cleanup terminal both occurred; neither is fabricated.
    PrimaryAndCleanup {
        /// Closed primary class.
        primary: NativePrimaryCause,
        /// Exact cleanup class.
        cleanup: NativeWorkCleanupCause,
    },
}

/// Exact named-artifact rejection without formatting a filesystem or text error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeArtifactCause {
    /// The filesystem operation retained portable and platform I/O facts.
    Io(NativeIoFact),
    /// A source-derived artifact name was not valid UTF-8.
    InvalidText(InvalidUtf8Fact),
}

/// Closed native work-directory failure cause.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeDirectoryCause {
    /// The configured work directory was relative.
    RelativeDirectory,
    /// The configured work directory was not empty.
    NotEmpty,
    /// Inspecting the configured work directory had this I/O category.
    InspectIo(NativeIoFact),
}

/// Exact filesystem action applied to one named native adapter artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeArtifactAction {
    /// Converting a source-derived artifact name to its required text representation.
    ResolveText,
    /// Materializing the artifact before or during native execution.
    Write,
    /// Creating the exact adapter-owned directory that contains an artifact family.
    CreateDirectory,
    /// Removing the artifact after native execution.
    Remove,
}

/// Allocation-free native I/O facts that retain both the portable class and platform code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeIoFact {
    /// Portable standard-library I/O category.
    pub kind: ErrorKind,
    /// Exact platform error number when the operating system supplied one.
    pub raw_os_code: Option<i32>,
}

/// Named primary native terminal retained when cleanup also failed.
#[derive(Debug, Eq, PartialEq)]
pub enum NativePrimaryCause {
    /// Adapter preparation failed and subsequent cleanup also failed.
    PrepareDirectory(NativeDirectoryCause),
    /// A named preparation artifact operation and subsequent cleanup both failed.
    PrepareArtifact {
        /// Exact preparation action.
        action: NativeArtifactAction,
        /// Exact adapter-owned artifact.
        artifact: NativeArtifactRole,
        /// Exact materialization or text-conversion rejection.
        cause: NativeArtifactCause,
    },
    /// Native child startup failed.
    ToolStart(NativeIoFact),
    /// A scoped native I/O worker panicked after child ownership was established.
    WorkerPanic(NativeWorkerPanic),
    /// Native standard input was unavailable.
    MissingInput {
        /// Optional child-cleanup I/O category retained from the paired terminal.
        cleanup: Option<NativeIoFact>,
    },
    /// Native standard error was unavailable.
    MissingDiagnostic {
        /// Optional child-cleanup I/O category retained from the paired terminal.
        cleanup: Option<NativeIoFact>,
    },
    /// Writing source to native standard input failed.
    Input {
        /// Exact I/O category from source write.
        cause: NativeIoFact,
        /// Optional child-cleanup I/O category retained from the paired terminal.
        cleanup: Option<NativeIoFact>,
    },
    /// Interrupting native work failed.
    Terminate(NativeIoFact),
    /// Waiting for native work failed.
    Wait {
        /// Exact I/O category from child wait.
        cause: NativeIoFact,
        /// Optional child-cleanup I/O category retained from the paired terminal.
        cleanup: Option<NativeIoFact>,
    },
    /// Reading native diagnostics failed, with any paired child cleanup retained.
    DiagnosticRead {
        /// Exact diagnostic-reader I/O category.
        cause: NativeIoFact,
        /// Optional child-cleanup I/O category retained from the paired terminal.
        cleanup: Option<NativeIoFact>,
    },
    /// Cancellation won and retained native diagnostics only when native work emitted them.
    Cancelled(Option<CompilerDiagnostic>),
    /// Deadline expiry won and retained native diagnostics only when native work emitted them.
    DeadlineExceeded(Option<CompilerDiagnostic>),
    /// Diagnostic capacity terminated native work with exact observed width.
    DiagnosticLimit {
        /// Retained diagnostic capacity.
        limit: usize,
        /// Total diagnostic bytes observed before termination.
        observed: usize,
        /// Native diagnostic copied only when native work emitted one.
        diagnostic: Option<CompilerDiagnostic>,
    },
    /// Native syntax rejection retained its exit code and bounded diagnostics.
    Rejected {
        /// Exit status code when the child exposed one.
        code: Option<i32>,
        /// Native diagnostic copied only when native work emitted one.
        diagnostic: Option<CompilerDiagnostic>,
    },
}

/// Cleanup class that accompanies a primary native terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeWorkCleanupCause {
    /// The directory was relative during cleanup.
    RelativeDirectory,
    /// The directory retained unknown files after cleanup.
    NotEmpty,
    /// Cleanup inspection or removal had this I/O category.
    Io(NativeIoFact),
    /// Cleanup of one named native adapter artifact failed.
    Artifact {
        /// Exact cleanup operation.
        action: NativeArtifactAction,
        /// Exact adapter-owned artifact.
        artifact: NativeArtifactRole,
        /// Exact materialization or text-conversion rejection.
        cause: NativeArtifactCause,
    },
}

/// Named native child lifecycle phase for compact I/O diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeIoPhase {
    /// Starting the configured executable.
    Start,
    /// Writing exact source bytes to child standard input.
    Input,
    /// Terminating an interrupted child.
    Terminate,
    /// Waiting for child termination.
    Wait,
    /// Reading bounded child diagnostics.
    DiagnosticRead,
}
