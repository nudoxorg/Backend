//! Product command admission and the atomic intent/view/projection pipeline.

mod adapter;
mod browse_lane;
mod diff;
mod graph;
mod index;
mod index_operation;
mod semantic_query;
mod semantic_shapes;
mod snapshot;

pub(in crate::builtin) use adapter::{CommandAdapter, Executed};
#[cfg(any(test, feature = "test-support"))]
pub(in crate::builtin) use adapter::commit_builtin_intent;
pub(in crate::builtin) use index::{recover_awaiting_selection, recover_offered_reservation};
