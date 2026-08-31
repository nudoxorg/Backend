use nudox_compile_vocab::{InvalidUtf8Fact, NativeArtifactRole, NativeWorkPhase};
use serde::{Serialize, Serializer, ser::SerializeStruct};
use wave_application_core::{
    NativeArtifactAction, NativeArtifactCause, NativeDirectoryCause, NativeWorkCause,
    NativeWorkCleanupCause,
};

use super::{io::NativeIoFactRef, terminal::NativePrimaryCauseWire, worker::InvalidUtf8FactRef};

/// Remote serde definitions for shared closed native-work vocabularies.
#[derive(Serialize)]
#[serde(
    remote = "nudox_compile_vocab::NativeWorkPhase",
    rename_all = "snake_case"
)]
pub(crate) enum NativeWorkPhaseWire {
    Prepare,
    Cleanup,
}

#[derive(Serialize)]
#[serde(
    remote = "wave_application_core::NativeArtifactAction",
    rename_all = "snake_case"
)]
pub(crate) enum NativeArtifactActionWire {
    ResolveText,
    Write,
    CreateDirectory,
    Remove,
}

#[derive(Serialize)]
#[serde(
    remote = "nudox_compile_vocab::NativeArtifactRole",
    rename_all = "snake_case"
)]
pub(crate) enum NativeArtifactRoleWire {
    RustMetadata,
    TypeScriptSource,
    TypeScriptWork,
    CSharpSource,
    CSharpProject,
    CSharpNuGetConfig,
    CSharpIntermediateOutput,
    CSharpBuildOutput,
    CSharpWork,
    CSharpDotnetHome,
    CSharpNuGetPackages,
    GoSource,
    GoObject,
    GoWork,
    JavaSource,
    JavaArguments,
    JavaWork,
}

struct NativeWorkPhaseRef<'value>(&'value NativeWorkPhase);

impl Serialize for NativeWorkPhaseRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        NativeWorkPhaseWire::serialize(self.0, serializer)
    }
}

struct NativeArtifactActionRef<'value>(&'value NativeArtifactAction);

impl Serialize for NativeArtifactActionRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        NativeArtifactActionWire::serialize(self.0, serializer)
    }
}

struct NativeArtifactRoleRef<'value>(&'value NativeArtifactRole);

impl Serialize for NativeArtifactRoleRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        NativeArtifactRoleWire::serialize(self.0, serializer)
    }
}

pub(crate) struct NativeDirectoryCauseWire<'value>(pub(crate) &'value NativeDirectoryCause);

impl Serialize for NativeDirectoryCauseWire<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        serialize_native_directory_cause(self.0, serializer)
    }
}

pub(crate) struct NativeArtifactCauseWire<'value>(pub(crate) &'value NativeArtifactCause);

impl Serialize for NativeArtifactCauseWire<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        serialize_native_artifact_cause(self.0, serializer)
    }
}

pub(crate) struct NativeWorkCauseWire<'value>(pub(crate) &'value NativeWorkCause);

impl Serialize for NativeWorkCauseWire<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        serialize_native_work_cause(self.0, serializer)
    }
}

pub(crate) struct NativeWorkCleanupCauseWire<'value>(pub(crate) &'value NativeWorkCleanupCause);

impl Serialize for NativeWorkCleanupCauseWire<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        serialize_native_work_cleanup_cause(self.0, serializer)
    }
}

pub(crate) fn serialize_native_directory_cause<Output: Serializer>(
    cause: &NativeDirectoryCause,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    let mut state = serializer.serialize_struct("NativeDirectoryCause", 1)?;
    match cause {
        NativeDirectoryCause::RelativeDirectory => {
            state.serialize_field("kind", "relative_directory")?;
        }
        NativeDirectoryCause::NotEmpty => state.serialize_field("kind", "not_empty")?,
        NativeDirectoryCause::InspectIo(cause) => {
            state.serialize_field("kind", "inspect_io")?;
            state.serialize_field("cause", &NativeIoFactRef(cause))?;
        }
    }
    state.end()
}

pub(crate) fn serialize_native_artifact_cause<Output: Serializer>(
    cause: &NativeArtifactCause,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    let mut state = serializer.serialize_struct("NativeArtifactCause", 2)?;
    match cause {
        NativeArtifactCause::Io(cause) => {
            state.serialize_field("kind", "io")?;
            state.serialize_field("cause", &NativeIoFactRef(cause))?;
        }
        NativeArtifactCause::InvalidText(fact) => {
            state.serialize_field("kind", "invalid_text")?;
            state.serialize_field("cause", &InvalidUtf8FactRef(fact))?;
        }
    }
    state.end()
}

pub(crate) fn serialize_native_work_cause<Output: Serializer>(
    cause: &NativeWorkCause,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    let mut state = serializer.serialize_struct("NativeWorkCause", 4)?;
    match cause {
        NativeWorkCause::Directory { phase, cause } => {
            state.serialize_field("kind", "directory")?;
            state.serialize_field("phase", &NativeWorkPhaseRef(phase))?;
            state.serialize_field("cause", &NativeDirectoryCauseWire(cause))?;
        }
        NativeWorkCause::Artifact {
            phase,
            action,
            artifact,
            cause,
        } => {
            state.serialize_field("kind", "artifact")?;
            state.serialize_field("phase", &NativeWorkPhaseRef(phase))?;
            state.serialize_field("action", &NativeArtifactActionRef(action))?;
            state.serialize_field("artifact", &NativeArtifactRoleRef(artifact))?;
            state.serialize_field("cause", &NativeArtifactCauseWire(cause))?;
        }
        NativeWorkCause::Primary(cause) => {
            state.serialize_field("kind", "primary")?;
            state.serialize_field("cause", &NativePrimaryCauseWire(cause))?;
        }
        NativeWorkCause::PrimaryAndCleanup { primary, cleanup } => {
            state.serialize_field("kind", "primary_and_cleanup")?;
            state.serialize_field("primary", &NativePrimaryCauseWire(primary))?;
            state.serialize_field("cleanup", &NativeWorkCleanupCauseWire(cleanup))?;
        }
    }
    state.end()
}

pub(crate) fn serialize_native_work_cleanup_cause<Output: Serializer>(
    cause: &NativeWorkCleanupCause,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    let mut state = serializer.serialize_struct("NativeWorkCleanupCause", 4)?;
    match cause {
        NativeWorkCleanupCause::RelativeDirectory => {
            state.serialize_field("kind", "relative_directory")?;
        }
        NativeWorkCleanupCause::NotEmpty => {
            state.serialize_field("kind", "not_empty")?;
        }
        NativeWorkCleanupCause::Io(cause) => {
            state.serialize_field("kind", "io")?;
            state.serialize_field("cause", &NativeIoFactRef(cause))?;
        }
        NativeWorkCleanupCause::Artifact {
            action,
            artifact,
            cause,
        } => {
            state.serialize_field("kind", "artifact")?;
            state.serialize_field("action", &NativeArtifactActionRef(action))?;
            state.serialize_field("artifact", &NativeArtifactRoleRef(artifact))?;
            state.serialize_field("cause", &NativeArtifactCauseWire(cause))?;
        }
    }
    state.end()
}

const _: fn(&InvalidUtf8Fact) = |_| {};
const _: fn(&NativeArtifactRole) = |_| {};
const _: fn(&NativeWorkPhase) = |_| {};
const _: fn(&NativeArtifactAction) = |_| {};
const _: fn(&NativeDirectoryCause) = |_| {};
const _: fn(&NativeArtifactCause) = |_| {};
const _: fn(&NativeWorkCause) = |_| {};
const _: fn(&NativeWorkCleanupCause) = |_| {};
