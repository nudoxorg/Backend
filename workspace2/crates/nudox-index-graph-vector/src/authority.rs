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

/// Complete authority needed to interpret graph facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphAuthority {
    snapshot: IndexSnapshotId,
    projection: ProjectionId,
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

    /// Returns the pinned snapshot.
    #[must_use]
    pub const fn snapshot(self) -> IndexSnapshotId {
        self.snapshot
    }

    /// Returns the graph recipe authority.
    #[must_use]
    pub const fn projection(self) -> ProjectionId {
        self.projection
    }
}

/// Stable embedding model identity retained by every vector result.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModelId([u8; 16]);

impl ModelId {
    /// Preserves the complete model registry key.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
}

impl AsRef<[u8; 16]> for ModelId {
    fn as_ref(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Distance metric whose ordering gives a vector score meaning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Metric {
    /// Sum of squared coordinate differences; smaller values rank first.
    SquaredEuclidean,
    /// Negative dot-product ordering; smaller values rank first.
    NegativeDotProduct,
}

/// Complete authority needed to interpret vector facts and scores.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VectorAuthority {
    snapshot: IndexSnapshotId,
    model: ModelId,
    dimension: u16,
    metric: Metric,
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

    /// Returns the pinned snapshot.
    #[must_use]
    pub const fn snapshot(self) -> IndexSnapshotId {
        self.snapshot
    }

    /// Returns the embedding model identity.
    #[must_use]
    pub const fn model(self) -> ModelId {
        self.model
    }

    /// Returns the exact coordinate count.
    #[must_use]
    pub const fn dimension(self) -> u16 {
        self.dimension
    }

    /// Returns the score metric.
    #[must_use]
    pub const fn metric(self) -> Metric {
        self.metric
    }
}
