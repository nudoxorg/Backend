//! Exercises the `interface-protocol` tests support compiler native work contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use interface_core::{
    NativeArtifactAction, NativeArtifactCause, NativeArtifactRole, NativeDirectoryCause,
    NativeWorkCause, NativeWorkCleanupCause, NativeWorkPhase,
};
use serde::Deserialize;

use super::io::GoldenNativeIoFact;
use super::worker::GoldenInvalidUtf8Fact;

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum GoldenNativeWorkCause {
    Directory {
        phase: GoldenNativeWorkPhase,
        cause: GoldenNativeDirectoryCause,
    },
    Artifact {
        phase: GoldenNativeWorkPhase,
        action: GoldenNativeArtifactAction,
        artifact: GoldenNativeArtifactRole,
        cause: GoldenNativeArtifactCause,
    },
    Primary {
        cause: super::terminal::GoldenNativePrimaryCause,
    },
    PrimaryAndCleanup {
        primary: super::terminal::GoldenNativePrimaryCause,
        cleanup: GoldenNativeWorkCleanupCause,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenNativeWorkPhase {
    Prepare,
    Cleanup,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum GoldenNativeDirectoryCause {
    RelativeDirectory,
    NotEmpty,
    InspectIo { cause: GoldenNativeIoFact },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenNativeArtifactAction {
    ResolveText,
    Write,
    CreateDirectory,
    Remove,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenNativeArtifactRole {
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

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum GoldenNativeArtifactCause {
    Io { cause: GoldenNativeIoFact },
    InvalidText { cause: GoldenInvalidUtf8Fact },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum GoldenNativeWorkCleanupCause {
    RelativeDirectory,
    NotEmpty,
    Io {
        cause: GoldenNativeIoFact,
    },
    Artifact {
        action: GoldenNativeArtifactAction,
        artifact: GoldenNativeArtifactRole,
        cause: GoldenNativeArtifactCause,
    },
}

impl From<NativeWorkCause> for GoldenNativeWorkCause {
    fn from(cause: NativeWorkCause) -> Self {
        match cause {
            NativeWorkCause::Directory { phase, cause } => Self::Directory {
                phase: phase.into(),
                cause: cause.into(),
            },
            NativeWorkCause::Artifact {
                phase,
                action,
                artifact,
                cause,
            } => Self::Artifact {
                phase: phase.into(),
                action: action.into(),
                artifact: artifact.into(),
                cause: cause.into(),
            },
            NativeWorkCause::Primary(cause) => Self::Primary {
                cause: cause.into(),
            },
            NativeWorkCause::PrimaryAndCleanup { primary, cleanup } => Self::PrimaryAndCleanup {
                primary: primary.into(),
                cleanup: cleanup.into(),
            },
        }
    }
}

impl From<NativeWorkPhase> for GoldenNativeWorkPhase {
    fn from(phase: NativeWorkPhase) -> Self {
        match phase {
            NativeWorkPhase::Prepare => Self::Prepare,
            NativeWorkPhase::Cleanup => Self::Cleanup,
        }
    }
}

impl From<NativeDirectoryCause> for GoldenNativeDirectoryCause {
    fn from(cause: NativeDirectoryCause) -> Self {
        match cause {
            NativeDirectoryCause::RelativeDirectory => Self::RelativeDirectory,
            NativeDirectoryCause::NotEmpty => Self::NotEmpty,
            NativeDirectoryCause::InspectIo(cause) => Self::InspectIo {
                cause: cause.into(),
            },
        }
    }
}

impl From<NativeArtifactAction> for GoldenNativeArtifactAction {
    fn from(action: NativeArtifactAction) -> Self {
        match action {
            NativeArtifactAction::ResolveText => Self::ResolveText,
            NativeArtifactAction::Write => Self::Write,
            NativeArtifactAction::CreateDirectory => Self::CreateDirectory,
            NativeArtifactAction::Remove => Self::Remove,
        }
    }
}

impl From<NativeArtifactRole> for GoldenNativeArtifactRole {
    fn from(artifact: NativeArtifactRole) -> Self {
        match artifact {
            NativeArtifactRole::RustMetadata => Self::RustMetadata,
            NativeArtifactRole::TypeScriptSource => Self::TypeScriptSource,
            NativeArtifactRole::TypeScriptWork => Self::TypeScriptWork,
            NativeArtifactRole::CSharpSource => Self::CSharpSource,
            NativeArtifactRole::CSharpProject => Self::CSharpProject,
            NativeArtifactRole::CSharpNuGetConfig => Self::CSharpNuGetConfig,
            NativeArtifactRole::CSharpIntermediateOutput => Self::CSharpIntermediateOutput,
            NativeArtifactRole::CSharpBuildOutput => Self::CSharpBuildOutput,
            NativeArtifactRole::CSharpWork => Self::CSharpWork,
            NativeArtifactRole::CSharpDotnetHome => Self::CSharpDotnetHome,
            NativeArtifactRole::CSharpNuGetPackages => Self::CSharpNuGetPackages,
            NativeArtifactRole::GoSource => Self::GoSource,
            NativeArtifactRole::GoObject => Self::GoObject,
            NativeArtifactRole::GoWork => Self::GoWork,
            NativeArtifactRole::JavaSource => Self::JavaSource,
            NativeArtifactRole::JavaArguments => Self::JavaArguments,
            NativeArtifactRole::JavaWork => Self::JavaWork,
        }
    }
}

impl From<NativeArtifactCause> for GoldenNativeArtifactCause {
    fn from(cause: NativeArtifactCause) -> Self {
        match cause {
            NativeArtifactCause::Io(cause) => Self::Io {
                cause: cause.into(),
            },
            NativeArtifactCause::InvalidText(fact) => Self::InvalidText { cause: fact.into() },
        }
    }
}

impl From<NativeWorkCleanupCause> for GoldenNativeWorkCleanupCause {
    fn from(cause: NativeWorkCleanupCause) -> Self {
        match cause {
            NativeWorkCleanupCause::RelativeDirectory => Self::RelativeDirectory,
            NativeWorkCleanupCause::NotEmpty => Self::NotEmpty,
            NativeWorkCleanupCause::Io(cause) => Self::Io {
                cause: cause.into(),
            },
            NativeWorkCleanupCause::Artifact {
                action,
                artifact,
                cause,
            } => Self::Artifact {
                action: action.into(),
                artifact: artifact.into(),
                cause: cause.into(),
            },
        }
    }
}
