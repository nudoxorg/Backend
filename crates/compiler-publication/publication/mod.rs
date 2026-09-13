//! Defines publication behavior for `compiler-publication`, whose purpose is to publish verified compiler fragments as immutable generations.
//! This module owns the publication invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Durable compiler publication API, synchronous commit path, and verified reopen path.

mod open;
mod publish;
mod types;

pub use open::{
    open_published, open_published_semantic, open_semantic_generation,
    semantic_generation_requirements,
};
pub use publish::{publish_compiled, publish_semantic};
pub use types::{
    OpenPublicationScratch, OpenPublishedError, OpenSemanticPublicationScratch, OpenedCompilation,
    OpenedFragment, OpenedFragmentCursor, OpenedFragmentError, OpenedFragmentFactMismatch,
    OpenedFragmentView, OpenedSemanticArtifact, OpenedSemanticArtifactCursor,
    OpenedSemanticArtifactError, OpenedSemanticCompilation, OpenedSemanticGeneration,
    PublicationScratch, PublishCompiledError, PublishControl, PublishSemanticError,
    PublishedCompilation, SemanticGenerationRequirements, SemanticPublicationScratch,
    UncommittedPublication, UncommittedPublicationFacts,
};
