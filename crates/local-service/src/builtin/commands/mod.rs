//! Product command admission and the atomic intent/view/projection pipeline.

mod adapter;
mod browse_lane;
mod diff;
mod embedding_refresh;
mod graph;
mod index;
mod index_operation;
mod journal_readiness;
mod search_lane;
mod semantic_query;
mod semantic_shapes;
mod snapshot;

pub(in crate::builtin) use adapter::{CommandAdapter, Executed};
#[cfg(any(test, feature = "test-support"))]
pub(in crate::builtin) use adapter::{
    PreparedBuiltinIntent, commit_builtin_intent, commit_prepared_builtin_intent,
    prepare_builtin_intent,
};
#[cfg(test)]
pub(in crate::builtin) use index::package_source_membership_page_from_snapshot;
#[cfg(test)]
pub(in crate::builtin) use index::prepare_source_facts_changes;
pub(in crate::builtin) use index::{recover_awaiting_selection, recover_offered_reservation};
