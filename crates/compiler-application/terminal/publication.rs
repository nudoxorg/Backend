//! Defines terminal publication behavior for `compiler-application`, whose purpose is to bind application requests to native compilation and durable publication.
//! This module owns the terminal publication invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Durable-publication terminal projection.

use compiler_publication::{
    OpenPublishedError, OpenedSemanticArtifactError, PublishCompiledError, PublishSemanticError,
    UncommittedPublication,
};
use backend_library::interface::{CompilerTerminal, PublicationCause, PublicationPhase};

use super::common::attempt;

fn publication_terminal(
    source: backend_library::interface::SourceAuthority,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    error: PublishCompiledError,
) -> CompilerTerminal {
    let cause = match error {
        PublishCompiledError::CancelledBeforeStorage => PublicationCause::CancelledBeforeStorage,
        PublishCompiledError::Canonical(_) => {
            PublicationCause::Rejected(PublicationPhase::Canonical)
        }
        PublishCompiledError::ManifestWrite(_) | PublishCompiledError::ManifestStorage(_) => {
            PublicationCause::Rejected(PublicationPhase::Manifest)
        }
        PublishCompiledError::FragmentStorage { .. }
        | PublishCompiledError::FragmentStorageOwner(_)
        | PublishCompiledError::FragmentManifest { .. } => {
            PublicationCause::Rejected(PublicationPhase::Fragment)
        }
        PublishCompiledError::Generation(_) => {
            PublicationCause::Rejected(PublicationPhase::Generation)
        }
        PublishCompiledError::BindingWrite(_)
        | PublishCompiledError::BindingStorage(_)
        | PublishCompiledError::BindingOutputLength { .. } => {
            PublicationCause::Rejected(PublicationPhase::Binding)
        }
        PublishCompiledError::Uncommitted(uncommitted) => uncommitted_cause(&uncommitted),
        PublishCompiledError::StableGenerationMismatch { .. } => {
            PublicationCause::StableGenerationMismatch
        }
    };
    CompilerTerminal::Publication {
        attempted: attempt(source, recipe),
        cause,
    }
}

pub(crate) fn semantic_publication_terminal(
    source: backend_library::interface::SourceAuthority,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    error: PublishSemanticError,
) -> CompilerTerminal {
    match error {
        PublishSemanticError::Publication(error) => publication_terminal(source, recipe, error),
        PublishSemanticError::CancelledBeforeStorage => CompilerTerminal::Publication {
            attempted: attempt(source, recipe),
            cause: PublicationCause::CancelledBeforeStorage,
        },
        PublishSemanticError::Canonical(_) => rejected(source, recipe, PublicationPhase::Canonical),
        PublishSemanticError::ManifestWrite(_) => {
            rejected(source, recipe, PublicationPhase::Manifest)
        }
        PublishSemanticError::FragmentStorageOwner(_)
        | PublishSemanticError::FragmentStorage { .. }
        | PublishSemanticError::FragmentManifest { .. } => {
            rejected(source, recipe, PublicationPhase::Fragment)
        }
        PublishSemanticError::ImagePlanTooSmall { .. }
        | PublishSemanticError::ImageMeasure { .. }
        | PublishSemanticError::ImageLengthAddressSpace { .. }
        | PublishSemanticError::ImagePlanLengthAddressSpace { .. }
        | PublishSemanticError::ImageExtentOverflow { .. }
        | PublishSemanticError::ImageOutputTooSmall { .. }
        | PublishSemanticError::ImageEncode { .. }
        | PublishSemanticError::ImageWriteLengthMismatch { .. }
        | PublishSemanticError::ImageReopen { .. }
        | PublishSemanticError::ImageProvenanceUnavailable { .. }
        | PublishSemanticError::ImageSource { .. }
        | PublishSemanticError::ImageRecipe { .. }
        | PublishSemanticError::SemanticStorageOwner(_)
        | PublishSemanticError::SemanticStorage { .. }
        | PublishSemanticError::SemanticStorageFacts { .. } => {
            rejected(source, recipe, PublicationPhase::SemanticImage)
        }
        _ => rejected(source, recipe, PublicationPhase::SemanticImage),
    }
}

pub(crate) fn semantic_reopen_terminal(
    source: backend_library::interface::SourceAuthority,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    _error: OpenPublishedError,
) -> CompilerTerminal {
    rejected(source, recipe, PublicationPhase::Reopen)
}

pub(crate) fn semantic_artifact_terminal(
    source: backend_library::interface::SourceAuthority,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    _error: OpenedSemanticArtifactError,
) -> CompilerTerminal {
    rejected(source, recipe, PublicationPhase::Reopen)
}

pub(crate) const fn semantic_reopen_absent(
    source: backend_library::interface::SourceAuthority,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
) -> CompilerTerminal {
    rejected(source, recipe, PublicationPhase::Reopen)
}

pub(crate) const fn semantic_reopen_cardinality(
    source: backend_library::interface::SourceAuthority,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
) -> CompilerTerminal {
    rejected(source, recipe, PublicationPhase::Reopen)
}

const fn rejected(
    source: backend_library::interface::SourceAuthority,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    phase: PublicationPhase,
) -> CompilerTerminal {
    CompilerTerminal::Publication {
        attempted: attempt(source, recipe),
        cause: PublicationCause::Rejected(phase),
    }
}

const fn uncommitted_cause(error: &UncommittedPublication) -> PublicationCause {
    match error {
        UncommittedPublication::Full { .. } => PublicationCause::AdmissionFull,
        UncommittedPublication::Closed { .. } | UncommittedPublication::OwnerLost { .. } => {
            PublicationCause::AdmissionClosed
        }
        UncommittedPublication::Cancelled { .. } => PublicationCause::CancelledAfterAdmission,
        UncommittedPublication::CancelledBeforeAdmission { .. } => {
            PublicationCause::CancelledBeforeAdmission
        }
        UncommittedPublication::Failed { .. } => {
            PublicationCause::Rejected(PublicationPhase::Durable)
        }
    }
}
