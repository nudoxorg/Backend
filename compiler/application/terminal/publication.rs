//! Defines terminal publication behavior for `compiler-application`, whose purpose is to bind application requests to native compilation and durable publication.
//! This module owns the terminal publication invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Durable-publication terminal projection.

use compiler_publication::{PublishCompiledError, UncommittedPublication};
use interface_core::{CompilerTerminal, PublicationCause, PublicationPhase};

use super::common::attempt;

pub(crate) fn publication_terminal(
    source: interface_core::SourceAuthority,
    recipe: compiler_vocabulary::CompileRecipeFact,
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
