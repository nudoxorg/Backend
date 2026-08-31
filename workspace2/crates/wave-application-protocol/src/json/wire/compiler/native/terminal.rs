use serde::{Serialize, Serializer, ser::SerializeStruct};
use wave_application_core::{NativePrimaryCause, NativeWorkCause, NativeWorkCleanupCause};

use super::super::terminal::DiagnosticRef;
use super::{
    io::{NativeIoFactOptionRef, NativeIoFactRef},
    work::{
        NativeArtifactActionWire, NativeArtifactCauseWire, NativeArtifactRoleWire,
        NativeDirectoryCauseWire,
    },
    worker::NativeWorkerPanicRef,
};

pub(crate) struct NativePrimaryCauseWire<'value>(pub(crate) &'value NativePrimaryCause);

impl Serialize for NativePrimaryCauseWire<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        serialize_native_primary_cause(self.0, serializer)
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn serialize_native_primary_cause<Output: Serializer>(
    cause: &NativePrimaryCause,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    let mut state = serializer.serialize_struct("NativePrimaryCause", 4)?;
    match cause {
        NativePrimaryCause::PrepareDirectory(cause) => {
            state.serialize_field("kind", "prepare_directory")?;
            state.serialize_field("cause", &NativeDirectoryCauseWire(cause))?;
        }
        NativePrimaryCause::PrepareArtifact {
            action,
            artifact,
            cause,
        } => {
            state.serialize_field("kind", "prepare_artifact")?;
            state.serialize_field("action", &NativeArtifactActionRef(action))?;
            state.serialize_field("artifact", &NativeArtifactRoleRef(artifact))?;
            state.serialize_field("cause", &NativeArtifactCauseWire(cause))?;
        }
        NativePrimaryCause::ToolStart(cause) => {
            state.serialize_field("kind", "tool_start")?;
            state.serialize_field("cause", &NativeIoFactRef(cause))?;
        }
        NativePrimaryCause::WorkerPanic(cause) => {
            state.serialize_field("kind", "worker_panic")?;
            state.serialize_field("cause", &NativeWorkerPanicRef(cause))?;
        }
        NativePrimaryCause::MissingInput { cleanup } => {
            state.serialize_field("kind", "missing_input")?;
            state.serialize_field("cleanup", &NativeIoFactOptionRef(cleanup))?;
        }
        NativePrimaryCause::MissingDiagnostic { cleanup } => {
            state.serialize_field("kind", "missing_diagnostic")?;
            state.serialize_field("cleanup", &NativeIoFactOptionRef(cleanup))?;
        }
        NativePrimaryCause::Input { cause, cleanup } => {
            state.serialize_field("kind", "input")?;
            state.serialize_field("cause", &NativeIoFactRef(cause))?;
            state.serialize_field("cleanup", &NativeIoFactOptionRef(cleanup))?;
        }
        NativePrimaryCause::Terminate(cause) => {
            state.serialize_field("kind", "terminate")?;
            state.serialize_field("cause", &NativeIoFactRef(cause))?;
        }
        NativePrimaryCause::Wait { cause, cleanup } => {
            state.serialize_field("kind", "wait")?;
            state.serialize_field("cause", &NativeIoFactRef(cause))?;
            state.serialize_field("cleanup", &NativeIoFactOptionRef(cleanup))?;
        }
        NativePrimaryCause::DiagnosticRead { cause, cleanup } => {
            state.serialize_field("kind", "diagnostic_read")?;
            state.serialize_field("cause", &NativeIoFactRef(cause))?;
            state.serialize_field("cleanup", &NativeIoFactOptionRef(cleanup))?;
        }
        NativePrimaryCause::Cancelled(diagnostic) => {
            state.serialize_field("kind", "cancelled")?;
            state.serialize_field("diagnostic", &DiagnosticRef(diagnostic))?;
        }
        NativePrimaryCause::DeadlineExceeded(diagnostic) => {
            state.serialize_field("kind", "deadline_exceeded")?;
            state.serialize_field("diagnostic", &DiagnosticRef(diagnostic))?;
        }
        NativePrimaryCause::DiagnosticLimit {
            limit,
            observed,
            diagnostic,
        } => {
            state.serialize_field("kind", "diagnostic_limit")?;
            state.serialize_field("limit", limit)?;
            state.serialize_field("observed", observed)?;
            state.serialize_field("diagnostic", &DiagnosticRef(diagnostic))?;
        }
        NativePrimaryCause::Rejected { code, diagnostic } => {
            state.serialize_field("kind", "rejected")?;
            state.serialize_field("code", code)?;
            state.serialize_field("diagnostic", &DiagnosticRef(diagnostic))?;
        }
    }
    state.end()
}

struct NativeArtifactActionRef<'value>(&'value wave_application_core::NativeArtifactAction);

impl Serialize for NativeArtifactActionRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        NativeArtifactActionWire::serialize(self.0, serializer)
    }
}

struct NativeArtifactRoleRef<'value>(&'value nudox_compile_vocab::NativeArtifactRole);

impl Serialize for NativeArtifactRoleRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        NativeArtifactRoleWire::serialize(self.0, serializer)
    }
}

const _: fn(&NativeWorkCause) = |_| {};
const _: fn(&NativeWorkCleanupCause) = |_| {};
