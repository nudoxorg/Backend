//! Defines json wire compiler terminal behavior for `interface-protocol`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the json wire compiler terminal invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::time::Duration;

use compiler_vocabulary::{AuthorityPhase, Language, LoweringUnsupported, NativeTool, Stage};
use interface_core::{
    CompilerAttempt, CompilerCause, CompilerDiagnostic, FragmentCause, PublicationCause,
    SourceAuthority,
};
use serde::{Serialize, Serializer, ser::SerializeStruct};

use super::super::scalar::{AuthorityPhaseWire, LanguageWire, NativeToolWire, StageWire};
use super::authority::{CompilerAttemptWire, SourceAuthorityWire};
use super::native::{NativeIoFactRef, NativeIoPhaseRef, NativeWorkCauseWire};
use super::publication::serialize_publication_cause;

/// Remote serde definition for the closed compiler terminal.
///
/// All terminal variants are structs in the core model, so serde can perform the exhaustive
/// projection directly.  The cause fields below retain their established named payload shape
/// through the small custom serializers for tuple variants.
#[derive(Serialize)]
#[serde(
    remote = "interface_core::CompilerTerminal",
    tag = "kind",
    rename_all = "snake_case"
)]
pub(crate) enum CompilerTerminalWire {
    SourceLength {
        actual: usize,
    },
    Unavailable {
        #[serde(with = "LanguageWire")]
        language: Language,
        #[serde(with = "StageWire")]
        stage: Stage,
    },
    UnsupportedStage {
        #[serde(with = "SourceAuthorityWire")]
        source: SourceAuthority,
        #[serde(with = "LanguageWire")]
        language: Language,
        #[serde(with = "StageWire")]
        stage: Stage,
    },
    DeadlineConstruction {
        #[serde(with = "SourceAuthorityWire")]
        source: SourceAuthority,
        #[serde(with = "LanguageWire")]
        language: Language,
        #[serde(with = "StageWire")]
        stage: Stage,
        timeout: Duration,
    },
    Toolchain {
        #[serde(with = "SourceAuthorityWire")]
        source: SourceAuthority,
        #[serde(with = "LanguageWire")]
        language: Language,
        #[serde(with = "StageWire")]
        stage: Stage,
        #[serde(with = "NativeToolWire")]
        selected: NativeTool,
        #[serde(serialize_with = "serialize_optional_native_tool")]
        configured: Option<NativeTool>,
    },
    ToolingUnavailable {
        #[serde(with = "SourceAuthorityWire")]
        source: SourceAuthority,
        #[serde(with = "LanguageWire")]
        language: Language,
        #[serde(with = "StageWire")]
        stage: Stage,
        #[serde(with = "NativeToolWire")]
        tool: NativeTool,
    },
    Cancelled {
        #[serde(with = "CompilerAttemptWire")]
        attempted: CompilerAttempt,
        #[serde(serialize_with = "serialize_diagnostic_option")]
        diagnostic: Option<CompilerDiagnostic>,
    },
    Compile {
        #[serde(with = "CompilerAttemptWire")]
        attempted: CompilerAttempt,
        #[serde(serialize_with = "serialize_compiler_cause")]
        cause: CompilerCause,
    },
    Publication {
        #[serde(with = "CompilerAttemptWire")]
        attempted: CompilerAttempt,
        #[serde(serialize_with = "serialize_publication_cause")]
        cause: PublicationCause,
    },
}

/// Closed lowering vocabulary projected as the value of a named `cause` field.
#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::LoweringUnsupported",
    rename_all = "snake_case"
)]
pub(crate) enum LoweringUnsupportedWire {
    NoSupportedDeclaration,
    RustFunction,
    RustConstantType,
    PythonAssignmentName,
    PythonAssignmentValue,
    ClangDeclarationForm,
    TypeScriptDeclarationForm,
    TypeScriptDeclarationType,
    CSharpDeclarationForm,
    CSharpDeclarationType,
    GoDeclarationForm,
    GoDeclarationType,
    JavaDeclarationForm,
}

/// Closed compact-IR failure vocabulary projected as the value of a named `cause` field.
#[derive(Serialize)]
#[serde(remote = "interface_core::FragmentCause", rename_all = "snake_case")]
pub(crate) enum FragmentCauseWire {
    Prepare,
    Write,
    Validate,
}

/// Bounded compiler diagnostic facts.  Only the meaningful retained prefix crosses the wire.
#[allow(clippy::ref_option, clippy::trivially_copy_pass_by_ref)]
pub(crate) fn serialize_diagnostic_option<Output: Serializer>(
    diagnostic: &Option<CompilerDiagnostic>,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    match diagnostic.as_ref() {
        Some(diagnostic) => serialize_compiler_diagnostic(diagnostic, serializer),
        None => serializer.serialize_none(),
    }
}

fn serialize_compiler_diagnostic<Output: Serializer>(
    diagnostic: &CompilerDiagnostic,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    let mut state = serializer.serialize_struct("CompilerDiagnostic", 4)?;
    state.serialize_field("byte_len", &diagnostic.byte_len)?;
    state.serialize_field("observed", &diagnostic.observed)?;
    state.serialize_field("truncated", &diagnostic.truncated)?;
    state.serialize_field("bytes", &diagnostic.bytes[..diagnostic.byte_len])?;
    state.end()
}

pub(crate) struct LoweringUnsupportedRef<'value>(pub(crate) &'value LoweringUnsupported);

impl Serialize for LoweringUnsupportedRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        LoweringUnsupportedWire::serialize(self.0, serializer)
    }
}

pub(crate) struct FragmentCauseRef<'value>(pub(crate) &'value FragmentCause);

impl Serialize for FragmentCauseRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        FragmentCauseWire::serialize(self.0, serializer)
    }
}

pub(crate) struct AuthorityPhaseRef<'value>(pub(crate) &'value AuthorityPhase);

impl Serialize for AuthorityPhaseRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        AuthorityPhaseWire::serialize(self.0, serializer)
    }
}

/// Projects the named wire shape for compiler causes whose core enum contains tuple variants.
pub(crate) fn serialize_compiler_cause<Output: Serializer>(
    cause: &CompilerCause,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    let mut state = serializer.serialize_struct("CompilerCause", 3)?;
    match cause {
        CompilerCause::Authority { phase, diagnostic } => {
            state.serialize_field("kind", "authority")?;
            state.serialize_field("phase", &AuthorityPhaseRef(phase))?;
            state.serialize_field("diagnostic", &DiagnosticRef(diagnostic))?;
        }
        CompilerCause::NativeWork(cause) => {
            state.serialize_field("kind", "native_work")?;
            state.serialize_field("cause", &NativeWorkCauseWire(cause))?;
        }
        CompilerCause::NativeIo { phase, cause } => {
            state.serialize_field("kind", "native_io")?;
            state.serialize_field("phase", &NativeIoPhaseRef(phase))?;
            state.serialize_field("cause", &NativeIoFactRef(cause))?;
        }
        CompilerCause::NativeRejected { code, diagnostic } => {
            state.serialize_field("kind", "native_rejected")?;
            state.serialize_field("code", code)?;
            state.serialize_field("diagnostic", &DiagnosticRef(diagnostic))?;
        }
        CompilerCause::DeadlineExceeded { diagnostic } => {
            state.serialize_field("kind", "deadline_exceeded")?;
            state.serialize_field("diagnostic", &DiagnosticRef(diagnostic))?;
        }
        CompilerCause::DiagnosticLimit {
            limit,
            observed,
            diagnostic,
        } => {
            state.serialize_field("kind", "diagnostic_limit")?;
            state.serialize_field("limit", limit)?;
            state.serialize_field("observed", observed)?;
            state.serialize_field("diagnostic", &DiagnosticRef(diagnostic))?;
        }
        CompilerCause::Lowering(cause) => {
            state.serialize_field("kind", "lowering")?;
            state.serialize_field("cause", &LoweringUnsupportedRef(cause))?;
        }
        CompilerCause::Fragment(cause) => {
            state.serialize_field("kind", "fragment")?;
            state.serialize_field("cause", &FragmentCauseRef(cause))?;
        }
    }
    state.end()
}

pub(crate) struct DiagnosticRef<'value>(pub(crate) &'value Option<CompilerDiagnostic>);

impl Serialize for DiagnosticRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        serialize_diagnostic_option(self.0, serializer)
    }
}

#[allow(clippy::ref_option, clippy::trivially_copy_pass_by_ref)]
pub(crate) fn serialize_optional_native_tool<Output: Serializer>(
    tool: &Option<NativeTool>,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    match tool {
        Some(tool) => NativeToolWire::serialize(tool, serializer),
        None => serializer.serialize_none(),
    }
}
