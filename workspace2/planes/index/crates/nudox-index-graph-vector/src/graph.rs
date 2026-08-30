use nudox_ir_vocab::EntityId;

use crate::{GraphAuthority, MAX_PARTITIONS, PartitionId};

/// One immutable directed graph edge with complete projection authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphEdge {
    authority: GraphAuthority,
    partition: PartitionId,
    source: EntityId,
    target: EntityId,
}

impl GraphEdge {
    /// Creates one edge fact. A graph view validates its authority before admission.
    #[must_use]
    pub const fn new(
        authority: GraphAuthority,
        partition: PartitionId,
        source: EntityId,
        target: EntityId,
    ) -> Self {
        Self {
            authority,
            partition,
            source,
            target,
        }
    }

    /// Returns the complete immutable graph authority.
    #[must_use]
    pub const fn authority(self) -> GraphAuthority {
        self.authority
    }

    /// Returns the owning partition.
    #[must_use]
    pub const fn partition(self) -> PartitionId {
        self.partition
    }

    /// Returns the source entity.
    #[must_use]
    pub const fn source(self) -> EntityId {
        self.source
    }

    /// Returns the target entity.
    #[must_use]
    pub const fn target(self) -> EntityId {
        self.target
    }
}

/// Borrowed graph facts supplied by one immutable partition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphRow<'facts> {
    partition: PartitionId,
    edges: &'facts [GraphEdge],
}

impl<'facts> GraphRow<'facts> {
    /// Binds a complete borrowed edge slice to its partition coordinate.
    #[must_use]
    pub const fn new(partition: PartitionId, edges: &'facts [GraphEdge]) -> Self {
        Self { partition, edges }
    }

    /// Returns the partition coordinate.
    #[must_use]
    pub const fn partition(self) -> PartitionId {
        self.partition
    }

    /// Lends the complete immutable edge slice.
    #[must_use]
    pub const fn edges(self) -> &'facts [GraphEdge] {
        self.edges
    }
}

/// Exact reason a borrowed graph was rejected before entering the query adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionError {
    /// Global fan-out bound; checked before duplicate or edge work.
    TooManyRows {
        /// Maximum selected partition rows.
        maximum: usize,
        /// Complete observed row count.
        observed: usize,
    },
    /// Two rows claim the same partition.
    DuplicatePartition {
        /// First row owning the partition.
        first_row_index: usize,
        /// Later duplicate row.
        row_index: usize,
        /// Repeated coordinate.
        partition: PartitionId,
    },
    /// A row coordinate and edge coordinate disagree.
    WrongEdgePartition {
        /// Row containing the rejected edge.
        row_index: usize,
        /// Edge coordinate inside the row.
        edge_index: usize,
        /// Row partition.
        expected: PartitionId,
        /// Edge partition.
        observed: PartitionId,
    },
    /// An edge belongs to another snapshot or graph recipe.
    WrongGraphAuthority {
        /// Row containing the rejected edge.
        row_index: usize,
        /// Edge coordinate inside the row.
        edge_index: usize,
        /// Pinned query authority.
        expected: GraphAuthority,
        /// Rejected edge authority.
        observed: GraphAuthority,
    },
}

/// Validated borrowed graph view consumed by the nested Trustfall adapter.
#[derive(Debug, Eq, PartialEq)]
pub struct TrustfallGraph<'facts> {
    authority: GraphAuthority,
    rows: &'facts [GraphRow<'facts>],
}

impl<'facts> TrustfallGraph<'facts> {
    /// Validates global work bounds before scanning duplicates or facts.
    pub fn try_new(
        authority: GraphAuthority,
        rows: &'facts [GraphRow<'facts>],
    ) -> Result<Self, AdmissionError> {
        if rows.len() > MAX_PARTITIONS {
            return Err(AdmissionError::TooManyRows {
                maximum: MAX_PARTITIONS,
                observed: rows.len(),
            });
        }
        for row_index in 0..rows.len() {
            for first_row_index in 0..row_index {
                if rows[first_row_index].partition == rows[row_index].partition {
                    return Err(AdmissionError::DuplicatePartition {
                        first_row_index,
                        row_index,
                        partition: rows[row_index].partition,
                    });
                }
            }
        }
        for (row_index, row) in rows.iter().copied().enumerate() {
            for (edge_index, edge) in row.edges.iter().copied().enumerate() {
                if edge.authority != authority {
                    return Err(AdmissionError::WrongGraphAuthority {
                        row_index,
                        edge_index,
                        expected: authority,
                        observed: edge.authority,
                    });
                }
                if edge.partition != row.partition {
                    return Err(AdmissionError::WrongEdgePartition {
                        row_index,
                        edge_index,
                        expected: row.partition,
                        observed: edge.partition,
                    });
                }
            }
        }
        Ok(Self { authority, rows })
    }

    /// Returns the graph authority pinned by validation.
    #[must_use]
    pub const fn authority(&self) -> GraphAuthority {
        self.authority
    }

    /// Lends validated partition rows without copying their edges.
    #[must_use]
    pub const fn rows(&self) -> &'facts [GraphRow<'facts>] {
        self.rows
    }
}
