//! Durable-witness boundary construction and cancellation observation.

use nudox_index_graph_vector::Cancellation;
use nudox_index_publish::PublishedIndexSnapshot;

/// Runtime retrieval boundary pinned to one durable published snapshot witness.
///
/// The private witness field deliberately prevents a copyable descriptive snapshot view from
/// entering this server boundary without its owning durable publication proof.
pub struct RetrievalBoundary<'boundary, 'store, 'selection, PayloadOwner>
where
    PayloadOwner: AsRef<[u8]>,
{
    pub(super) published: &'boundary PublishedIndexSnapshot<'store, 'selection, PayloadOwner>,
    pub(super) cancellation: &'boundary Cancellation,
}

impl<'boundary, 'store, 'selection, PayloadOwner>
    RetrievalBoundary<'boundary, 'store, 'selection, PayloadOwner>
where
    PayloadOwner: AsRef<[u8]>,
{
    /// Binds runtime retrieval to one sealed durable publication and one cancellation authority.
    #[must_use]
    pub const fn new(
        published: &'boundary PublishedIndexSnapshot<'store, 'selection, PayloadOwner>,
        cancellation: &'boundary Cancellation,
    ) -> Self {
        Self {
            published,
            cancellation,
        }
    }
}
