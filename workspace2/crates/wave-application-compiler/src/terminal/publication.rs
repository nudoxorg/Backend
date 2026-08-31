//! Durable-publication terminal projection.

use nudox_compile_publication::{PublishCompiledError, UncommittedPublication};
use wave_application_core::{CompilerTerminal, PublicationCause, PublicationPhase};

use super::common::attempt;

pub(crate) fn publication_terminal(
    source: wave_application_core::SourceAuthority,
    recipe: nudox_compile_vocab::CompileRecipeFact,
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
