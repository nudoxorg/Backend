//! Lossless projections of closed upstream failures used by terminal benchmark errors.

use nudox_compile_driver::{
    CompileFailure, NativeArtifactRole, NativeTool, NativeWorkError, NativeWorkPhase,
    NativeWorkPrimary, ToolchainSelectionFact,
};
use nudox_compile_vocab::{Language, Stage as CompileStage};
use nudox_index_build::BuildError;
use nudox_index_core::{EntityDocumentId, ExactSegmentError, LexicalSegmentError};
use nudox_ir_format::{FragmentError, PrepareError, WriteError};
use nudox_ir_vocab::EntityId;

/// Exact structural facts retained when generated compiler input is unexpectedly rejected.
#[allow(
    dead_code,
    reason = "cold failure facts are retained for Debug terminal reporting, not read on the successful benchmark path"
)]
#[derive(Debug)]
pub(crate) enum CompileFailureFact {
    SourceLength {
        actual: usize,
    },
    UnsupportedStage {
        language: Language,
        stage: CompileStage,
    },
    ToolchainSelectionMismatch {
        language: Language,
        stage: CompileStage,
        selected: NativeTool,
        provided: ToolchainSelectionFact,
    },
    ToolchainMismatch {
        language: Language,
        stage: CompileStage,
        selected: NativeTool,
        resolved: NativeTool,
    },
    NativeWork {
        phase: NativeWorkPhase,
        cause: NativeWorkFault,
    },
    NativeWorkCleanup {
        primary: NativeWorkPrimaryFault,
        cleanup: NativeWorkFault,
    },
    ToolingUnavailable {
        language: Language,
        stage: CompileStage,
        tool: NativeTool,
    },
    ToolStart {
        kind: std::io::ErrorKind,
    },
    MissingToolInput,
    MissingToolInputCleanup {
        cleanup: std::io::ErrorKind,
    },
    MissingToolDiagnostic,
    MissingToolDiagnosticCleanup {
        cleanup: std::io::ErrorKind,
    },
    ToolInput {
        kind: std::io::ErrorKind,
    },
    ToolInputCleanup {
        kind: std::io::ErrorKind,
        cleanup: std::io::ErrorKind,
    },
    ToolTerminate {
        kind: std::io::ErrorKind,
    },
    ToolWait {
        kind: std::io::ErrorKind,
    },
    ToolWaitCleanup {
        kind: std::io::ErrorKind,
        cleanup: std::io::ErrorKind,
    },
    ToolDiagnosticRead {
        kind: std::io::ErrorKind,
    },
    ToolDiagnosticReadCleanup {
        kind: std::io::ErrorKind,
        cleanup: std::io::ErrorKind,
    },
    NativeWorkerPanic {
        cause: nudox_compile_vocab::NativeWorkerPanic,
    },
    Cancelled {
        diagnostic: DiagnosticFact,
    },
    DeadlineExceeded {
        diagnostic: DiagnosticFact,
    },
    DiagnosticLimit {
        limit: usize,
        observed: usize,
        diagnostic: DiagnosticFact,
    },
    NativeRejected {
        status: std::process::ExitStatus,
        diagnostic: DiagnosticFact,
    },
    LoweringUnsupported {
        cause: nudox_compile_driver::LoweringUnsupported,
    },
    Prepare {
        cause: PrepareError,
    },
    Write {
        cause: WriteFault,
    },
    Validate {
        cause: Box<FragmentError>,
    },
}

/// Bounded diagnostic structure retained without borrowing the caller's reusable buffer.
#[allow(
    dead_code,
    reason = "cold failure facts are retained for Debug terminal reporting, not read on the successful benchmark path"
)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct DiagnosticFact {
    captured_bytes: usize,
    truncated: bool,
}

/// Closed filesystem failure fact from native work preparation or cleanup.
#[allow(
    dead_code,
    reason = "cold failure facts are retained for Debug terminal reporting, not read on the successful benchmark path"
)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum NativeWorkFault {
    RelativeDirectory,
    Inspect {
        kind: std::io::ErrorKind,
    },
    NotEmpty,
    WriteArtifact {
        artifact: NativeArtifactRole,
        kind: std::io::ErrorKind,
    },
    CreateArtifactDirectory {
        artifact: NativeArtifactRole,
        kind: std::io::ErrorKind,
    },
    ArtifactText {
        artifact: NativeArtifactRole,
        fact: nudox_compile_vocab::InvalidUtf8Fact,
    },
    RemoveArtifact {
        artifact: NativeArtifactRole,
        kind: std::io::ErrorKind,
    },
}

/// Closed native terminal fact retained when a second cleanup cause is also present.
#[allow(
    dead_code,
    reason = "cold failure facts are retained for Debug terminal reporting, not read on the successful benchmark path"
)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum NativeWorkPrimaryFault {
    Prepare {
        cause: NativeWorkFault,
    },
    ToolStart {
        kind: std::io::ErrorKind,
    },
    MissingToolInput,
    MissingToolInputCleanup {
        cleanup: std::io::ErrorKind,
    },
    MissingToolDiagnostic,
    MissingToolDiagnosticCleanup {
        cleanup: std::io::ErrorKind,
    },
    ToolInput {
        kind: std::io::ErrorKind,
    },
    ToolInputCleanup {
        kind: std::io::ErrorKind,
        cleanup: std::io::ErrorKind,
    },
    ToolTerminate {
        kind: std::io::ErrorKind,
    },
    ToolWait {
        kind: std::io::ErrorKind,
    },
    ToolWaitCleanup {
        kind: std::io::ErrorKind,
        cleanup: std::io::ErrorKind,
    },
    ToolDiagnosticRead {
        kind: std::io::ErrorKind,
    },
    ToolDiagnosticReadCleanup {
        kind: std::io::ErrorKind,
        cleanup: std::io::ErrorKind,
    },
    WorkerPanic {
        cause: nudox_compile_vocab::NativeWorkerPanic,
    },
    Cancelled {
        diagnostic: DiagnosticFact,
    },
    DeadlineExceeded {
        diagnostic: DiagnosticFact,
    },
    DiagnosticLimit {
        limit: usize,
        observed: usize,
        diagnostic: DiagnosticFact,
    },
    NativeRejected {
        status: std::process::ExitStatus,
        diagnostic: DiagnosticFact,
    },
}

/// Closed writer cause retained without erasing capacity or atom facts.
#[allow(
    dead_code,
    reason = "cold failure facts are retained for Debug terminal reporting, not read on the successful benchmark path"
)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum WriteFault {
    OutputTooSmall {
        required: usize,
        available: usize,
    },
    AtomLength {
        ordinal: nudox_ir_vocab::AtomId,
        actual: usize,
    },
    AtomExtent {
        ordinal: nudox_ir_vocab::AtomId,
    },
}

/// Structural exact-segment failure facts detached from borrowed row slices.
#[allow(
    dead_code,
    reason = "cold failure facts are retained for Debug terminal reporting, not read on the successful benchmark path"
)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum ExactSegmentFault {
    TooManyRows {
        maximum: usize,
        observed: usize,
    },
    PayloadBytesLimit {
        maximum: usize,
        observed: usize,
    },
    PayloadBytesOverflow {
        index: usize,
    },
    OutOfOrder {
        index: usize,
        previous_bytes: usize,
        observed_bytes: usize,
    },
    DuplicateKey {
        index: usize,
        key_bytes: usize,
    },
}

/// Structural lexical-segment failure facts detached from borrowed row slices.
#[allow(
    dead_code,
    reason = "cold failure facts are retained for Debug terminal reporting, not read on the successful benchmark path"
)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum LexicalSegmentFault {
    TooManyRows {
        maximum: usize,
        observed: usize,
    },
    PayloadBytesLimit {
        maximum: usize,
        observed: usize,
    },
    PayloadBytesOverflow {
        index: usize,
    },
    OutOfOrder {
        index: usize,
        previous_term_bytes: usize,
        previous_document: EntityDocumentId,
        observed_term_bytes: usize,
        observed_document: EntityDocumentId,
    },
    DuplicateRow {
        index: usize,
        term_bytes: usize,
        document: EntityDocumentId,
    },
}

/// Exact deterministic-builder rejection facts detached from caller-owned scratch borrows.
#[allow(
    dead_code,
    reason = "cold failure facts are retained for Debug terminal reporting, not read on the successful benchmark path"
)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum BuildFailureFact {
    Admission {
        cause: nudox_index_build::BuildAdmissionError,
    },
    EntityOrdinalAddressSpace {
        ordinal: usize,
    },
    ScratchInitialization {
        region: nudox_index_build::BuildRegion,
        required: usize,
        available: usize,
    },
    AtomAddressSpace {
        entity: EntityId,
        name: nudox_ir_vocab::AtomId,
    },
    TypeAddressSpace {
        entity: EntityId,
        semantic_type: nudox_ir_vocab::TypeId,
    },
    MissingAtom {
        entity: EntityId,
        name: nudox_ir_vocab::AtomId,
    },
    MissingTypeNode {
        entity: EntityId,
        semantic_type: nudox_ir_vocab::TypeId,
    },
    Exact {
        cause: ExactSegmentFault,
    },
    Lexical {
        cause: LexicalSegmentFault,
    },
}

#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive closed public compiler-error mapping intentionally preserves every terminal cause"
)]
pub(crate) fn compile_failure_fact(error: CompileFailure<'_>) -> CompileFailureFact {
    match error {
        CompileFailure::SourceLength { actual, .. } => CompileFailureFact::SourceLength { actual },
        CompileFailure::UnsupportedStage {
            language, stage, ..
        } => CompileFailureFact::UnsupportedStage { language, stage },
        CompileFailure::ToolchainSelectionMismatch {
            language,
            stage,
            selected,
            provided,
            ..
        } => CompileFailureFact::ToolchainSelectionMismatch {
            language,
            stage,
            selected,
            provided,
        },
        CompileFailure::ToolchainMismatch {
            language,
            stage,
            selected,
            resolved,
            ..
        } => CompileFailureFact::ToolchainMismatch {
            language,
            stage,
            selected,
            resolved,
        },
        CompileFailure::NativeWork { phase, cause, .. } => CompileFailureFact::NativeWork {
            phase,
            cause: native_work_fault(cause),
        },
        CompileFailure::NativeWorkCleanup {
            primary, cleanup, ..
        } => CompileFailureFact::NativeWorkCleanup {
            primary: native_work_primary_fault(primary),
            cleanup: native_work_fault(cleanup),
        },
        CompileFailure::ToolingUnavailable {
            language,
            stage,
            tool,
            ..
        } => CompileFailureFact::ToolingUnavailable {
            language,
            stage,
            tool,
        },
        CompileFailure::ToolStart { cause, .. } => {
            CompileFailureFact::ToolStart { kind: cause.kind() }
        }
        CompileFailure::MissingToolInput { .. } => CompileFailureFact::MissingToolInput,
        CompileFailure::MissingToolInputCleanup { cleanup, .. } => {
            CompileFailureFact::MissingToolInputCleanup {
                cleanup: cleanup.kind(),
            }
        }
        CompileFailure::MissingToolDiagnostic { .. } => CompileFailureFact::MissingToolDiagnostic,
        CompileFailure::MissingToolDiagnosticCleanup { cleanup, .. } => {
            CompileFailureFact::MissingToolDiagnosticCleanup {
                cleanup: cleanup.kind(),
            }
        }
        CompileFailure::ToolInput { cause, .. } => {
            CompileFailureFact::ToolInput { kind: cause.kind() }
        }
        CompileFailure::ToolInputCleanup { cause, cleanup, .. } => {
            CompileFailureFact::ToolInputCleanup {
                kind: cause.kind(),
                cleanup: cleanup.kind(),
            }
        }
        CompileFailure::ToolTerminate { cause, .. } => {
            CompileFailureFact::ToolTerminate { kind: cause.kind() }
        }
        CompileFailure::ToolWait { cause, .. } => {
            CompileFailureFact::ToolWait { kind: cause.kind() }
        }
        CompileFailure::ToolWaitCleanup { cause, cleanup, .. } => {
            CompileFailureFact::ToolWaitCleanup {
                kind: cause.kind(),
                cleanup: cleanup.kind(),
            }
        }
        CompileFailure::ToolDiagnosticRead { cause, .. } => {
            CompileFailureFact::ToolDiagnosticRead { kind: cause.kind() }
        }
        CompileFailure::ToolDiagnosticReadCleanup { cause, cleanup, .. } => {
            CompileFailureFact::ToolDiagnosticReadCleanup {
                kind: cause.kind(),
                cleanup: cleanup.kind(),
            }
        }
        CompileFailure::NativeWorkerPanic { cause, .. } => {
            CompileFailureFact::NativeWorkerPanic { cause }
        }
        CompileFailure::Cancelled { diagnostic, .. } => CompileFailureFact::Cancelled {
            diagnostic: diagnostic_fact(diagnostic),
        },
        CompileFailure::DeadlineExceeded { diagnostic, .. } => {
            CompileFailureFact::DeadlineExceeded {
                diagnostic: diagnostic_fact(diagnostic),
            }
        }
        CompileFailure::DiagnosticLimit {
            limit,
            observed,
            diagnostic,
            ..
        } => CompileFailureFact::DiagnosticLimit {
            limit,
            observed,
            diagnostic: diagnostic_fact(diagnostic),
        },
        CompileFailure::NativeRejected {
            status, diagnostic, ..
        } => CompileFailureFact::NativeRejected {
            status,
            diagnostic: diagnostic_fact(diagnostic),
        },
        CompileFailure::LoweringUnsupported { cause, .. } => {
            CompileFailureFact::LoweringUnsupported { cause }
        }
        CompileFailure::Prepare { cause, .. } => CompileFailureFact::Prepare { cause },
        CompileFailure::Write { cause, .. } => CompileFailureFact::Write {
            cause: write_fault(&cause),
        },
        CompileFailure::Validate { cause, .. } => CompileFailureFact::Validate {
            cause: Box::new(cause),
        },
    }
}

const fn diagnostic_fact(diagnostic: nudox_compile_driver::NativeDiagnostic<'_>) -> DiagnosticFact {
    DiagnosticFact {
        captured_bytes: diagnostic.bytes.len(),
        truncated: diagnostic.truncated,
    }
}

fn native_work_fault(error: NativeWorkError) -> NativeWorkFault {
    match error {
        NativeWorkError::RelativeDirectory => NativeWorkFault::RelativeDirectory,
        NativeWorkError::Inspect(cause) => NativeWorkFault::Inspect { kind: cause.kind() },
        NativeWorkError::NotEmpty => NativeWorkFault::NotEmpty,
        NativeWorkError::WriteArtifact { artifact, cause } => NativeWorkFault::WriteArtifact {
            artifact,
            kind: cause.kind(),
        },
        NativeWorkError::CreateArtifactDirectory { artifact, cause } => {
            NativeWorkFault::CreateArtifactDirectory {
                artifact,
                kind: cause.kind(),
            }
        }
        NativeWorkError::ArtifactText { artifact, fact } => {
            NativeWorkFault::ArtifactText { artifact, fact }
        }
        NativeWorkError::RemoveArtifact { artifact, cause } => NativeWorkFault::RemoveArtifact {
            artifact,
            kind: cause.kind(),
        },
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive cleanup-aware public compiler-error mapping retains each closed primary cause"
)]
fn native_work_primary_fault(error: NativeWorkPrimary<'_>) -> NativeWorkPrimaryFault {
    match error {
        NativeWorkPrimary::Prepare { cause } => NativeWorkPrimaryFault::Prepare {
            cause: native_work_fault(cause),
        },
        NativeWorkPrimary::ToolStart { cause } => {
            NativeWorkPrimaryFault::ToolStart { kind: cause.kind() }
        }
        NativeWorkPrimary::MissingToolInput => NativeWorkPrimaryFault::MissingToolInput,
        NativeWorkPrimary::MissingToolInputCleanup { cleanup } => {
            NativeWorkPrimaryFault::MissingToolInputCleanup {
                cleanup: cleanup.kind(),
            }
        }
        NativeWorkPrimary::MissingToolDiagnostic => NativeWorkPrimaryFault::MissingToolDiagnostic,
        NativeWorkPrimary::MissingToolDiagnosticCleanup { cleanup } => {
            NativeWorkPrimaryFault::MissingToolDiagnosticCleanup {
                cleanup: cleanup.kind(),
            }
        }
        NativeWorkPrimary::ToolInput { cause } => {
            NativeWorkPrimaryFault::ToolInput { kind: cause.kind() }
        }
        NativeWorkPrimary::ToolInputCleanup { cause, cleanup } => {
            NativeWorkPrimaryFault::ToolInputCleanup {
                kind: cause.kind(),
                cleanup: cleanup.kind(),
            }
        }
        NativeWorkPrimary::ToolTerminate { cause } => {
            NativeWorkPrimaryFault::ToolTerminate { kind: cause.kind() }
        }
        NativeWorkPrimary::ToolWait { cause } => {
            NativeWorkPrimaryFault::ToolWait { kind: cause.kind() }
        }
        NativeWorkPrimary::ToolWaitCleanup { cause, cleanup } => {
            NativeWorkPrimaryFault::ToolWaitCleanup {
                kind: cause.kind(),
                cleanup: cleanup.kind(),
            }
        }
        NativeWorkPrimary::ToolDiagnosticRead { cause } => {
            NativeWorkPrimaryFault::ToolDiagnosticRead { kind: cause.kind() }
        }
        NativeWorkPrimary::ToolDiagnosticReadCleanup { cause, cleanup } => {
            NativeWorkPrimaryFault::ToolDiagnosticReadCleanup {
                kind: cause.kind(),
                cleanup: cleanup.kind(),
            }
        }
        NativeWorkPrimary::WorkerPanic { cause } => NativeWorkPrimaryFault::WorkerPanic { cause },
        NativeWorkPrimary::Cancelled { diagnostic } => NativeWorkPrimaryFault::Cancelled {
            diagnostic: diagnostic_fact(diagnostic),
        },
        NativeWorkPrimary::DeadlineExceeded { diagnostic } => {
            NativeWorkPrimaryFault::DeadlineExceeded {
                diagnostic: diagnostic_fact(diagnostic),
            }
        }
        NativeWorkPrimary::DiagnosticLimit {
            limit,
            observed,
            diagnostic,
        } => NativeWorkPrimaryFault::DiagnosticLimit {
            limit,
            observed,
            diagnostic: diagnostic_fact(diagnostic),
        },
        NativeWorkPrimary::NativeRejected { status, diagnostic } => {
            NativeWorkPrimaryFault::NativeRejected {
                status,
                diagnostic: diagnostic_fact(diagnostic),
            }
        }
    }
}

const fn write_fault(error: &WriteError) -> WriteFault {
    match error {
        WriteError::OutputTooSmall {
            required,
            available,
        } => WriteFault::OutputTooSmall {
            required: *required,
            available: *available,
        },
        WriteError::AtomLength {
            ordinal, actual, ..
        } => WriteFault::AtomLength {
            ordinal: *ordinal,
            actual: *actual,
        },
        WriteError::AtomExtent { ordinal } => WriteFault::AtomExtent { ordinal: *ordinal },
    }
}

pub(crate) const fn exact_segment_fault(error: ExactSegmentError<'_>) -> ExactSegmentFault {
    match error {
        ExactSegmentError::TooManyRows { max, observed } => ExactSegmentFault::TooManyRows {
            maximum: max,
            observed,
        },
        ExactSegmentError::PayloadBytesLimit { max, observed } => {
            ExactSegmentFault::PayloadBytesLimit {
                maximum: max,
                observed,
            }
        }
        ExactSegmentError::PayloadBytesOverflow { index } => {
            ExactSegmentFault::PayloadBytesOverflow { index }
        }
        ExactSegmentError::OutOfOrder {
            index,
            previous,
            observed,
        } => ExactSegmentFault::OutOfOrder {
            index,
            previous_bytes: previous.len(),
            observed_bytes: observed.len(),
        },
        ExactSegmentError::DuplicateKey { index, key } => ExactSegmentFault::DuplicateKey {
            index,
            key_bytes: key.len(),
        },
    }
}

pub(crate) const fn lexical_segment_fault(error: LexicalSegmentError<'_>) -> LexicalSegmentFault {
    match error {
        LexicalSegmentError::TooManyRows { max, observed } => LexicalSegmentFault::TooManyRows {
            maximum: max,
            observed,
        },
        LexicalSegmentError::PayloadBytesLimit { max, observed } => {
            LexicalSegmentFault::PayloadBytesLimit {
                maximum: max,
                observed,
            }
        }
        LexicalSegmentError::PayloadBytesOverflow { index } => {
            LexicalSegmentFault::PayloadBytesOverflow { index }
        }
        LexicalSegmentError::OutOfOrder {
            index,
            previous,
            observed,
        } => LexicalSegmentFault::OutOfOrder {
            index,
            previous_term_bytes: previous.term.len(),
            previous_document: previous.document,
            observed_term_bytes: observed.term.len(),
            observed_document: observed.document,
        },
        LexicalSegmentError::DuplicateRow {
            index,
            term,
            document,
        } => LexicalSegmentFault::DuplicateRow {
            index,
            term_bytes: term.len(),
            document,
        },
    }
}

pub(crate) const fn build_failure_fact(error: BuildError<'_>) -> BuildFailureFact {
    match error {
        BuildError::Admission(cause) => BuildFailureFact::Admission { cause },
        BuildError::Derivation(cause) => build_derivation_failure_fact(&cause),
        BuildError::Exact { cause } => BuildFailureFact::Exact {
            cause: exact_segment_fault(cause),
        },
        BuildError::Lexical { cause } => BuildFailureFact::Lexical {
            cause: lexical_segment_fault(cause),
        },
    }
}

const fn build_derivation_failure_fact(
    error: &nudox_index_build::BuildDerivationError,
) -> BuildFailureFact {
    match error {
        nudox_index_build::BuildDerivationError::EntityOrdinalAddressSpace { ordinal, .. } => {
            BuildFailureFact::EntityOrdinalAddressSpace { ordinal: *ordinal }
        }
        nudox_index_build::BuildDerivationError::ScratchInitialization {
            region,
            required,
            available,
        } => BuildFailureFact::ScratchInitialization {
            region: *region,
            required: *required,
            available: *available,
        },
        nudox_index_build::BuildDerivationError::AtomAddressSpace { entity, name, .. } => {
            BuildFailureFact::AtomAddressSpace {
                entity: *entity,
                name: *name,
            }
        }
        nudox_index_build::BuildDerivationError::TypeAddressSpace {
            entity,
            semantic_type,
            ..
        } => BuildFailureFact::TypeAddressSpace {
            entity: *entity,
            semantic_type: *semantic_type,
        },
        nudox_index_build::BuildDerivationError::MissingAtom { entity, name } => {
            BuildFailureFact::MissingAtom {
                entity: *entity,
                name: *name,
            }
        }
        nudox_index_build::BuildDerivationError::MissingTypeNode {
            entity,
            semantic_type,
        } => BuildFailureFact::MissingTypeNode {
            entity: *entity,
            semantic_type: *semantic_type,
        },
    }
}
