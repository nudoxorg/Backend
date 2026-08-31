//! Bounded scoped graph-edge leases.
//!
//! `GraphLease` owns every slot on the caller's stack. `split()` lends its one producer and one
//! consumer endpoint; neither endpoint allocates or owns a reference-counted state. The producer
//! is movable to one scoped worker and the consumer is polled by one task. A slot is published by
//! `Vacant -> Writing -> Ready`, borrowed by `Ready -> Reading`, and reused by
//! `Reading -> Vacant`. Release/acquire on those transitions is the sole payload hand-off.
//!
//! This is an SPSC structure, not an MPSC queue. Its operations are lock-free: a stalled writer
//! cannot prevent cancellation from closing a slot, and a stalled reader cannot prevent the
//! writer from closing the stream. Progress of a particular batch still depends on that batch's
//! owner releasing it, which is an intentional bounded-lease backpressure law.

mod cancellation;
mod channel;
mod contract;
mod storage;
mod wake;

pub use cancellation::Cancellation;
pub use channel::{
    EdgeBatchProducer, EdgeBatchStream, GraphLease, GraphStreamEvent, LeasedGraphBatch,
};
pub use contract::{GraphTerminal, LeaseCapacity, LeaseLoad, LeaseStateCell, StreamCapacityError};
