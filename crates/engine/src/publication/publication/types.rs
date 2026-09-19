//! Defines publication types behavior for the `backend-engine` publication, whose purpose is to publish verified compiler fragments as immutable generations.
//! This module owns the publication types invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Durable compiler publication from driver-issued compact fragments only.

mod open;
mod publish;

pub use open::{
    OpenPublicationScratch, OpenPublishedError, OpenSemanticPublicationScratch, OpenedCompilation,
    OpenedFragment, OpenedFragmentCursor, OpenedFragmentError, OpenedFragmentFactMismatch,
    OpenedFragmentView, OpenedSemanticArtifact, OpenedSemanticArtifactCursor,
    OpenedSemanticArtifactError, OpenedSemanticCompilation, OpenedSemanticGeneration,
    SemanticGenerationRequirements,
};
pub use publish::{
    PublicationScratch, PublishCompiledError, PublishControl, PublishSemanticError,
    PublishedCompilation, SemanticPublicationScratch, UncommittedPublication,
    UncommittedPublicationFacts,
};
