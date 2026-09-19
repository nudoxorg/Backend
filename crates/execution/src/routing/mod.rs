//! Public surface for snapshot-pinned horizontal routing.

mod authority;
mod merge;
mod planning;
mod worker;

pub use self::authority::{
    BoundsError, CANCELLATION_SAMPLE_INTERVAL, Cancellation, MAX_SEGMENTS, MAX_TOP_K, MAX_WORKERS,
    OrderingRecipe, Query, QueryDigest, RetryPolicy, RouteId, TopK,
};
pub use self::authority::{SegmentOrdinal, WorkerId};
pub use self::merge::{
    MergeError, MergeOutcome, MergedHit, MissingAssignment, ObservedHit, ReplyError, RoutedHit,
    RoutedHitSlot, merge_replies,
};
pub use self::planning::{Coordinator, PlanError, RoutePlan, SegmentAssignment, SegmentRange};
#[cfg(test)]
pub use self::worker::InMemoryWorker;
pub use self::worker::{
    AttemptOrdinal, ExecuteError, IdentifiedWorker, RouteAttempt, RoutedWorker, WorkerBackend,
    WorkerReply, WorkerRequest, WorkerResponse,
};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
