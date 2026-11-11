//! Product command admission and the atomic intent/view/projection pipeline.

mod adapter;
mod browse_lane;
mod diff;
mod graph;
pub(super) mod index;
mod index_input_workers;
mod index_operation;
mod journal_readiness;
mod local_search_lane;
mod package_profile;
mod search_lane;
mod semantic_query;
mod semantic_shapes;
mod semantic_source;
mod structural_source;
mod snapshot;
mod stylo_query;
pub(in crate::builtin) mod sql_query;
// Kept private until Vue owner replay and public-route admission are complete.
mod vue_query;
mod vue_source;
mod vue_references;
mod stylo_impact_capture;
mod impact_retention;
mod impact_diff;
mod stylo_impact_diff;
mod impact_freshness;
mod impact_comparison_owner;
mod stylo_impact_walk;
mod stylo_impact_adapter;
mod impact_framework;
mod impact_css;
mod impact_code;
mod impact_sql;

pub(in crate::builtin) use adapter::{CommandAdapter, Executed, ExpectedNativeSourceOrigin, selected_native_policy_for_row};
#[cfg(test)]
pub(in crate::builtin) use adapter::test_native_document_freshness;
#[cfg(any(test, feature = "test-support"))]
pub(in crate::builtin) use adapter::{
    commit_builtin_intent, commit_prepared_builtin_intent,
    prepare_builtin_intent,
};
pub(in crate::builtin) use index::package_source_membership_page_from_snapshot;
#[cfg(test)]
pub(in crate::builtin) use index::prepare_source_facts_changes;
#[cfg(test)]
pub(in crate::builtin) use index::semantic_versions;
pub(in crate::builtin) use index::{recover_awaiting_selection, recover_offered_reservation};
