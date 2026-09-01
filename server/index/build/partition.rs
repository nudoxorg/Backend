//! Defines partition-space behavior for `server-index-build`, whose purpose is to project reopened compiler IR into immutable exact and lexical segments.
//! This module owns the shared partition-range proof used by graph and vector projections.
//! Its narrow surface prevents coordinate arithmetic and boundary policy from drifting apart.

use server_index_graph_vector::PartitionId;

/// Reports whether `required_partitions` fit from `base` through the end of partition space.
pub(crate) fn partition_range_fits(base: PartitionId, required_partitions: usize) -> bool {
    usize::from(base.raw)
        .checked_add(required_partitions)
        .is_some_and(|end| end <= usize::from(u16::MAX) + 1)
}

/// Resolves one admitted partition ordinal without narrowing or overflow.
pub(crate) fn partition_at(base: PartitionId, ordinal: usize) -> Option<PartitionId> {
    usize::from(base.raw)
        .checked_add(ordinal)
        .and_then(|raw| u16::try_from(raw).ok())
        .map(PartitionId::new)
}
