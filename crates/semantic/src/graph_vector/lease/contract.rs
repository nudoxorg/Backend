//! Defines lease contract behavior for `backend-semantic::graph_vector`, whose purpose is to execute typed graph and vector work through bounded leased storage.
//! This module owns the lease contract invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Closed graph-lease vocabulary, separated from storage and endpoint mechanics.

use crate::graph_vector::{GraphAuthority, MissingPartitions, PartitionId};

/// Per-partition retained capacity for one stack-owned graph lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LeaseCapacity {
    /// Maximum number of `GraphEdge` values retained for each selected partition.
    pub edges_per_partition: u8,
    /// Maximum in-memory byte charge for one partition's edge batch, measured as
    /// `edge_count * size_of::<GraphEdge>()`.
    pub bytes_per_partition: usize,
}

/// One coherent observation of work currently retained by a graph lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LeaseLoad {
    /// Number of graph edges in slots that are ready or currently borrowed by the consumer.
    pub edges: usize,
    /// In-memory byte charge for those edges (`edges * size_of::<GraphEdge>()`).
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
    InvalidItemCapacity {
        /// Maximum edge count supported for one partition batch.
        maximum: usize,
        /// Requested edge count from [`LeaseCapacity::edges_per_partition`].
        observed: usize,
    },
    /// The configured byte reserve cannot hold its declared item reserve.
    InvalidByteCapacity {
        /// Minimum in-memory bytes required for the requested edge reserve.
        required: usize,
        /// Configured per-partition byte limit.
        observed: usize,
    },
    /// Every bounded cancellation registration is occupied.
    CancellationCapacity {
        /// Maximum simultaneous lease registrations held by this cancellation object.
        maximum: usize,
    },
    /// The selected partition fan-out exceeds fixed slots.
    PartitionCapacity {
        /// Number of fixed partition slots available to one lease.
        maximum: usize,
        /// Number of partition IDs requested for this lease.
        observed: usize,
    },
    /// One selected partition occurred twice.
    DuplicatePartition {
        /// Zero-based position of the first occurrence in the selected-partition slice.
        first_index: usize,
        /// Zero-based position of the repeated occurrence in the selected-partition slice.
        index: usize,
        /// Partition ID present at both reported positions.
        partition: PartitionId,
    },
    /// `split()` was called more than once for one stack-owned lease.
    EndpointsAlreadyBorrowed,
    /// The partition has already published its one immutable batch.
    PartitionAlreadySettled {
        /// Selected partition whose one batch has already been published.
        partition: PartitionId,
    },
    /// The producer is closed by terminal publication, cancellation, or consumer drop.
    StreamClosed,
    /// The settled batch exceeds its exact item reserve.
    ItemCapacity {
        /// Maximum number of edges admitted in this partition's batch.
        maximum: usize,
        /// Number of edges in the submitted batch.
        observed: usize,
    },
    /// The settled batch exceeds its exact byte reserve.
    ByteCapacity {
        /// Configured in-memory byte limit for this partition's edge batch.
        maximum: usize,
        /// Actual in-memory size of the submitted edge slice, in bytes.
        observed: usize,
    },
    /// The completion names no selected partition.
    UnselectedPartition {
        /// Partition ID supplied by the producer but absent from this lease's selection.
        observed: PartitionId,
    },
    /// One settled edge belongs to another pinned graph authority.
    WrongAuthority {
        /// Zero-based position of the invalid edge in the submitted batch.
        edge_index: usize,
        /// Snapshot authority pinned when the lease was admitted.
        expected: GraphAuthority,
        /// Snapshot authority carried by the edge at `edge_index`.
        observed: GraphAuthority,
    },
    /// One settled edge does not belong to its completed partition.
    WrongPartition {
        /// Zero-based position of the invalid edge in the submitted batch.
        edge_index: usize,
        /// Partition ID whose batch is being settled.
        expected: PartitionId,
        /// Partition ID carried by the edge at `edge_index`.
        observed: PartitionId,
    },
    /// The declared exact absence exceeds selected partitions.
    MissingPartitionCapacity {
        /// Number of partitions selected for this lease, and therefore the maximum absence count.
        maximum: usize,
        /// Number of missing-partition IDs declared by the producer.
        observed: usize,
    },
    /// One declared missing partition occurred twice.
    DuplicateMissingPartition {
        /// Zero-based position of the first occurrence in the declared-missing slice.
        first_index: usize,
        /// Zero-based position of the repeated occurrence in the declared-missing slice.
        index: usize,
        /// Partition ID present at both reported positions.
        partition: PartitionId,
    },
    /// Declared absence did not equal the exact uncompleted selection.
    IncorrectMissingPartitions {
        /// Partition expected at `index`, or `None` when the expected list has ended.
        expected: Option<PartitionId>,
        /// Partition declared at `index`, or `None` when the supplied list has ended.
        observed: Option<PartitionId>,
        /// Zero-based first differing position; may identify the end of either list.
        index: usize,
    },
    /// The producer was dropped before publishing a terminal fact.
    ProducerDisconnected,
    /// An atomic state had no declared discriminant, so the lease failed closed before raw access.
    CorruptState {
        /// Lease state cell whose stored discriminant was not recognized.
        cell: LeaseStateCell,
    },
}

/// Typed provenance for a graph acquisition that remained snapshot-authoritative but degraded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphDegradation {
    /// The acquisition retried a stale route while retaining the same pinned graph authority.
    StaleRoute,
    /// A selected immutable partition source was unavailable.
    PartitionSourceUnavailable,
}

/// Terminal facts for the graph lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphTerminal {
    /// Every selected partition produced one complete immutable batch.
    Complete {
        /// Snapshot authority pinned by the lease when it was created.
        authority: GraphAuthority,
    },
    /// Cancellation won before a normal terminal could become observable.
    Cancelled {
        /// Snapshot authority pinned by the lease when it was created.
        authority: GraphAuthority,
    },
    /// Exact absent selected partitions in original selection order.
    Partial {
        /// Snapshot authority pinned by the lease when it was created.
        authority: GraphAuthority,
        /// Selected partitions that did not publish a batch, in selection order.
        missing: MissingPartitions,
    },
    /// All selected partitions arrived, but acquisition recovered through a degraded route.
    Degraded {
        /// Snapshot authority pinned by the lease when it was created.
        authority: GraphAuthority,
        /// Reason the acquisition path degraded while preserving that authority.
        reason: GraphDegradation,
    },
    /// A degraded acquisition retained exact selected partition absences in request order.
    DegradedPartial {
        /// Snapshot authority pinned by the lease when it was created.
        authority: GraphAuthority,
        /// Selected partitions that did not publish a batch, in selection order.
        missing: MissingPartitions,
        /// Reason the acquisition path degraded while preserving that authority.
        reason: GraphDegradation,
    },
    /// The unique producer disappeared without a terminal declaration.
    Failed {
        /// Snapshot authority pinned by the lease when it was created.
        authority: GraphAuthority,
        /// Exact stream failure that prevented a usable completion declaration.
        cause: StreamCapacityError,
    },
}
