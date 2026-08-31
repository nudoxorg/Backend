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
