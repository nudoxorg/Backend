//! Public surface for snapshot-pinned horizontal routing.
#![forbid(unsafe_code)]

mod authority;
mod merge;
mod planning;
mod worker;

pub use authority::{
    BoundsError, CANCELLATION_SAMPLE_INTERVAL, Cancellation, MAX_SEGMENTS, MAX_TOP_K, MAX_WORKERS,
    OrderingRecipe, Query, QueryDigest, RetryPolicy, RouteId, TopK,
};
pub use authority::{SegmentOrdinal, WorkerId};
pub use merge::{
    MergeError, MergeOutcome, MergedHit, MissingAssignment, ObservedHit, ReplyError, RoutedHit,
    RoutedHitSlot, merge_replies,
};
pub use planning::{Coordinator, PlanError, RoutePlan, SegmentAssignment, SegmentRange};
#[cfg(test)]
pub use worker::InMemoryWorker;
pub use worker::{
    AttemptOrdinal, ExecuteError, IdentifiedWorker, RouteAttempt, RoutedWorker, WorkerBackend,
    WorkerReply, WorkerRequest, WorkerResponse,
};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
