//! Defines terminal publication behavior for the `backend-engine` application, whose purpose is to bind application requests to native compilation and durable publication.
//! This module owns the terminal publication invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Durable-publication terminal projection.

use crate::publication::{
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
        PublishSemanticError::Canonical(_)
        | PublishSemanticError::ArtifactDescriptorAllocation(_) => {
            rejected(source, recipe, PublicationPhase::Canonical)
        }
        PublishSemanticError::ManifestWrite(_) => {
            rejected(source, recipe, PublicationPhase::Manifest)
        }
        PublishSemanticError::FragmentStorageOwner(_)
        | PublishSemanticError::FragmentStorage { .. }
        | PublishSemanticError::FragmentManifest { .. }
        | PublishSemanticError::FragmentValidation { .. } => {
            rejected(source, recipe, PublicationPhase::Fragment)
        }
        PublishSemanticError::ImageCountMismatch { .. }
        | PublishSemanticError::ImageRegionOffset { .. }
        | PublishSemanticError::ImageBytesLength { .. } => {
            rejected(source, recipe, PublicationPhase::SemanticImage)
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

#[cfg(test)]
mod tests {
    use backend_library::interface::{
        CompilerTerminal, PublicationCause, PublicationPhase, SourceAuthority,
    };
    use backend_semantic::vocabulary::{
        CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage,
    };
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};

    use crate::publication::PublishSemanticError;

    use super::semantic_publication_terminal;

    fn source_and_recipe() -> (SourceAuthority, CompileRecipeFact) {
        let source = SourceAuthority {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"terminal"),
            byte_len: 8,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"terminal-toolchain"),
        );
        (source, recipe)
    }

    fn assert_phase(error: PublishSemanticError, expected: PublicationPhase) {
        let (source, recipe) = source_and_recipe();
        let CompilerTerminal::Publication { cause, .. } =
            semantic_publication_terminal(source, recipe, error)
        else {
            panic!("semantic publication maps to a publication terminal");
        };
        assert_eq!(cause, PublicationCause::Rejected(expected));
    }

    #[test]
    fn staged_byte_failures_keep_their_publication_phase() {
        let allocation = Vec::<u8>::new()
            .try_reserve_exact(usize::MAX)
            .expect_err("oversized descriptor reservation is rejected");
        assert_phase(
            PublishSemanticError::ArtifactDescriptorAllocation(allocation),
            PublicationPhase::Canonical,
        );
        assert_phase(
            PublishSemanticError::FragmentValidation {
                ordinal: 0,
                source: backend_semantic::ir::FragmentError::Magic { actual: *b"bad!" },
            },
            PublicationPhase::Fragment,
        );
        assert_phase(
            PublishSemanticError::ImageCountMismatch {
                fragments: 1,
                semantic_images: 0,
            },
            PublicationPhase::SemanticImage,
        );
        assert_phase(
            PublishSemanticError::ImageRegionOffset {
                ordinal: 0,
                expected: 0,
                observed: 1,
            },
            PublicationPhase::SemanticImage,
        );
        assert_phase(
            PublishSemanticError::ImageBytesLength {
                expected: 8,
                observed: 9,
            },
            PublicationPhase::SemanticImage,
        );
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
