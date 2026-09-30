//! Product command admission and the atomic intent/view/projection pipeline.

mod adapter;
mod diff;
mod graph;
mod index;
mod semantic_query;
mod snapshot;

pub(in crate::builtin) use adapter::{CommandAdapter, Executed, commit_builtin_intent};
pub(in crate::builtin) use index::{recover_awaiting_selection, recover_offered_reservation};
