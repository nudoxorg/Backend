//! Defines publication behavior for `compiler-publication`, whose purpose is to publish verified compiler fragments as immutable generations.
//! This module owns the publication invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Durable compiler publication API, synchronous commit path, and verified reopen path.

mod open;
mod publish;
mod types;

pub use open::open_published;
pub use publish::publish_compiled;
pub use types::{
    OpenPublicationScratch, OpenPublishedError, OpenedCompilation, OpenedFragment,
    OpenedFragmentCursor, OpenedFragmentError, OpenedFragmentFactMismatch, OpenedFragmentView,
    PublicationScratch, PublishCompiledError, PublishControl, PublishedCompilation,
    UncommittedPublication, UncommittedPublicationFacts,
};
