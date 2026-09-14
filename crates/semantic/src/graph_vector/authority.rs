//! Defines authority behavior for `backend-semantic::graph_vector`, whose purpose is to execute typed graph and vector work through bounded leased storage.
//! This module owns the authority invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::ops::Deref;

use crate::index_vocabulary::IndexSnapshotId;

/// Immutable graph projection recipe identity within a snapshot.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProjectionId {
    /// Stable recipe code selected by the graph projection owner.
    pub raw: u16,
}

impl ProjectionId {
    /// Names one concrete graph recipe.
    #[must_use]
    pub const fn new(raw: u16) -> Self {
        Self { raw }
    }
}

/// Immutable partition coordinate within one projection.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PartitionId {
    /// Stable coordinate assigned by the projection builder.
    pub raw: u16,
}

impl PartitionId {
    /// Names one projection partition.
    #[must_use]
    pub const fn new(raw: u16) -> Self {
        Self { raw }
    }
}

/// A non-empty bounded set of missing immutable partition coordinates.
///
/// Graph and vector terminals share this representation because its invariant is only partition
/// cardinality and selection order; the enclosing terminal keeps the query family and authority.
/// Closed cardinality variants make a sentinel array plus mismatched length unrepresentable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MissingPartitions(MissingPartitionsRepr);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MissingPartitionsRepr {
    One(PartitionId),
    Two([PartitionId; 2]),
    Three([PartitionId; 3]),
    Four([PartitionId; 4]),
}

impl MissingPartitions {
    /// Derives exact absent partitions from a validated selected/reachable partition relation.
    ///
    /// The returned partitions preserve the original selected order. Every reachable partition
    /// must occur exactly once in the selected bounded set, so a caller cannot turn a foreign or
    /// duplicate partition into an apparent absence fact.
    pub fn from_selected_reachable(
        selected: &[PartitionId],
        reachable: &[PartitionId],
    ) -> Result<Option<Self>, MissingPartitionsError> {
        if selected.len() > crate::graph_vector::MAX_PARTITIONS {
            return Err(MissingPartitionsError::SelectedCapacity {
                maximum: crate::graph_vector::MAX_PARTITIONS,
                observed: selected.len(),
            });
        }
        if reachable.len() > crate::graph_vector::MAX_PARTITIONS {
            return Err(MissingPartitionsError::ReachableCapacity {
                maximum: crate::graph_vector::MAX_PARTITIONS,
                observed: reachable.len(),
            });
        }
        for (index, partition) in selected.iter().copied().enumerate() {
            if let Some(first_index) = selected[..index]
                .iter()
                .position(|first| *first == partition)
            {
                return Err(MissingPartitionsError::DuplicateSelected {
                    first_index,
                    index,
                    partition,
                });
            }
        }
        for (index, partition) in reachable.iter().copied().enumerate() {
            if let Some(first_index) = reachable[..index]
                .iter()
                .position(|first| *first == partition)
            {
                return Err(MissingPartitionsError::DuplicateReachable {
                    first_index,
                    index,
                    partition,
                });
            }
            if !selected.contains(&partition) {
                return Err(MissingPartitionsError::ReachableNotSelected { index, partition });
            }
        }
        let mut absent = [PartitionId::new(0); crate::graph_vector::MAX_PARTITIONS];
        let mut absent_len = 0_usize;
        for partition in selected.iter().copied() {
            if !reachable.contains(&partition) {
                absent[absent_len] = partition;
                absent_len += 1;
            }
        }
        Ok(Self::from_prefix(absent, absent_len))
    }

    pub(crate) fn from_prefix(
        values: [PartitionId; crate::graph_vector::MAX_PARTITIONS],
        length: usize,
    ) -> Option<Self> {
        match length {
            0 => None,
            1 => Some(Self(MissingPartitionsRepr::One(values[0]))),
            2 => Some(Self(MissingPartitionsRepr::Two([values[0], values[1]]))),
            3 => Some(Self(MissingPartitionsRepr::Three([
                values[0], values[1], values[2],
            ]))),
            4 => Some(Self(MissingPartitionsRepr::Four([
                values[0], values[1], values[2], values[3],
            ]))),
            _ => None,
        }
    }
}

/// Exact selected/reachable partition relation rejected before an absence fact was created.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MissingPartitionsError {
    /// Selected partitions exceeded the fixed request bound.
    SelectedCapacity {
        /// Maximum selected partition count.
        maximum: usize,
        /// Complete selected partition count.
        observed: usize,
    },
    /// Reachable partitions exceeded the fixed request bound.
    ReachableCapacity {
        /// Maximum reachable partition count.
        maximum: usize,
        /// Complete reachable partition count.
        observed: usize,
    },
    /// One selected partition occurred twice.
    DuplicateSelected {
        /// Earlier selected position.
        first_index: usize,
        /// Later selected position.
        index: usize,
        /// Repeated partition.
        partition: PartitionId,
    },
    /// One reachable partition occurred twice.
    DuplicateReachable {
        /// Earlier reachable position.
        first_index: usize,
        /// Later reachable position.
        index: usize,
        /// Repeated partition.
        partition: PartitionId,
    },
    /// A reachable partition was absent from the selected authoritative relation.
    ReachableNotSelected {
        /// Reachable position.
        index: usize,
        /// Foreign partition.
        partition: PartitionId,
    },
}

impl AsRef<[PartitionId]> for MissingPartitions {
    fn as_ref(&self) -> &[PartitionId] {
        self
    }
}

impl Deref for MissingPartitions {
    type Target = [PartitionId];

    fn deref(&self) -> &Self::Target {
        match &self.0 {
            MissingPartitionsRepr::One(partition) => core::slice::from_ref(partition),
            MissingPartitionsRepr::Two(partitions) => partitions,
            MissingPartitionsRepr::Three(partitions) => partitions,
            MissingPartitionsRepr::Four(partitions) => partitions,
        }
    }
}

/// Complete authority needed to interpret graph facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphAuthority {
    /// Immutable snapshot pinned by the graph query.
    pub snapshot: IndexSnapshotId,
    /// Graph projection recipe that interprets the snapshot facts.
    pub projection: ProjectionId,
}

impl GraphAuthority {
    /// Binds a graph projection recipe to one immutable snapshot.
    #[must_use]
    pub const fn new(snapshot: IndexSnapshotId, projection: ProjectionId) -> Self {
        Self {
            snapshot,
            projection,
        }
    }
}

/// Stable embedding model identity retained by every vector result.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModelId {
    /// Complete model registry key, preserved without normalization.
    pub raw: [u8; 16],
}

impl ModelId {
    /// Preserves the complete model registry key.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self { raw: bytes }
    }
}

impl From<[u8; 16]> for ModelId {
    /// Wraps the complete model registry key without changing any byte.
    fn from(raw: [u8; 16]) -> Self {
        Self::new(raw)
    }
}

impl Deref for ModelId {
    type Target = [u8; 16];

    fn deref(&self) -> &Self::Target {
        &self.raw
    }
}

impl AsRef<[u8; 16]> for ModelId {
    fn as_ref(&self) -> &[u8; 16] {
        &self.raw
    }
}

/// Distance metric whose ordering gives a vector score meaning.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Metric {
    /// Sum of squared coordinate differences; smaller values rank first.
    SquaredEuclidean = 0,
    /// Negative dot-product ordering; smaller values rank first.
    NegativeDotProduct = 1,
}

/// Complete authority needed to interpret vector facts and scores.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VectorAuthority {
    /// Immutable snapshot containing the vector facts.
    pub snapshot: IndexSnapshotId,
    /// Embedding model registry key for the vector coordinates.
    pub model: ModelId,
    /// Coordinate count required by the model and projection recipe.
    pub dimension: u16,
    /// Distance recipe that gives scores their ordering meaning.
    pub metric: Metric,
}

impl VectorAuthority {
    /// Binds model shape and score recipe to one immutable snapshot.
    #[must_use]
    pub const fn new(
        snapshot: IndexSnapshotId,
        model: ModelId,
        dimension: u16,
        metric: Metric,
    ) -> Self {
        Self {
            snapshot,
            model,
            dimension,
            metric,
        }
    }
}
