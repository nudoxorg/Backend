//! Closed graph-lease vocabulary, separated from storage and endpoint mechanics.

use crate::{GraphAuthority, MissingPartitions, PartitionId};

/// Per-partition retained capacity for one stack-owned graph lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LeaseCapacity {
    /// Maximum edges retained for each selected partition.
    pub edges_per_partition: u8,
    /// Maximum byte charge retained for each selected partition.
    pub bytes_per_partition: usize,
}

/// One coherent observation of work currently retained by a graph lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LeaseLoad {
    /// Number of ready or consumer-borrowed graph edges.
    pub edges: usize,
    /// Byte charge for those exact edges.
    pub bytes: usize,
}

/// Atomic cell whose unsupported representation was observed before a payload read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseStateCell {
    /// One selected partition's fixed edge slot.
    PartitionSlot,
    /// The one terminal publication cell.
    Terminal,
}

/// Exact admission, authority, capacity, or lifecycle rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamCapacityError {
    /// The requested per-partition item reserve cannot fit the fixed storage.
    InvalidItemCapacity { maximum: usize, observed: usize },
    /// The configured byte reserve cannot hold its declared item reserve.
    InvalidByteCapacity { required: usize, observed: usize },
    /// Every bounded cancellation registration is occupied.
    CancellationCapacity { maximum: usize },
    /// The selected partition fan-out exceeds fixed slots.
    PartitionCapacity { maximum: usize, observed: usize },
    /// One selected partition occurred twice.
    DuplicatePartition {
        first_index: usize,
        index: usize,
        partition: PartitionId,
    },
    /// `split()` was called more than once for one stack-owned lease.
    EndpointsAlreadyBorrowed,
    /// The partition has already published its one immutable batch.
    PartitionAlreadySettled { partition: PartitionId },
    /// The producer is closed by terminal publication, cancellation, or consumer drop.
    StreamClosed,
    /// The settled batch exceeds its exact item reserve.
    ItemCapacity { maximum: usize, observed: usize },
    /// The settled batch exceeds its exact byte reserve.
    ByteCapacity { maximum: usize, observed: usize },
    /// The completion names no selected partition.
    UnselectedPartition { observed: PartitionId },
    /// One settled edge belongs to another pinned graph authority.
    WrongAuthority {
        edge_index: usize,
        expected: GraphAuthority,
        observed: GraphAuthority,
    },
    /// One settled edge does not belong to its completed partition.
    WrongPartition {
        edge_index: usize,
        expected: PartitionId,
        observed: PartitionId,
    },
    /// The declared exact absence exceeds selected partitions.
    MissingPartitionCapacity { maximum: usize, observed: usize },
    /// One declared missing partition occurred twice.
    DuplicateMissingPartition {
        first_index: usize,
        index: usize,
        partition: PartitionId,
    },
    /// Declared absence did not equal the exact uncompleted selection.
    IncorrectMissingPartitions {
        expected: Option<PartitionId>,
        observed: Option<PartitionId>,
        index: usize,
    },
    /// The producer was dropped before publishing a terminal fact.
    ProducerDisconnected,
    /// An atomic state had no declared discriminant, so the lease failed closed before raw access.
    CorruptState { cell: LeaseStateCell },
}

/// Terminal facts for the graph lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphTerminal {
    /// Every selected partition produced one complete immutable batch.
    Complete { authority: GraphAuthority },
    /// Cancellation won before a normal terminal could become observable.
    Cancelled { authority: GraphAuthority },
    /// Exact absent selected partitions in original selection order.
    Partial {
        authority: GraphAuthority,
        missing: MissingPartitions,
    },
    /// The unique producer disappeared without a terminal declaration.
    Failed {
        authority: GraphAuthority,
        cause: StreamCapacityError,
    },
}
