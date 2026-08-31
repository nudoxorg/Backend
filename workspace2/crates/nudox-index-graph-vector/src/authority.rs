use core::ops::Deref;

use nudox_index_vocab::IndexSnapshotId;

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
    pub(crate) fn from_prefix(
        values: [PartitionId; crate::MAX_PARTITIONS],
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
