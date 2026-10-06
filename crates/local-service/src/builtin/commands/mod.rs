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
#[cfg(test)]
pub(in crate::builtin) use index::prepare_source_facts_changes;
#[cfg(any(test, feature = "test-support"))]
pub(in crate::builtin) use adapter::{
    PreparedBuiltinIntent, commit_builtin_intent, commit_prepared_builtin_intent,
    prepare_builtin_intent,
};
pub(in crate::builtin) use index::{
    recover_awaiting_selection, recover_offered_reservation, source_capture_receipt_for_root,
};
