//! Product command admission and the atomic intent/view/projection pipeline.

mod adapter;
mod diff;
mod graph;
mod index;
mod semantic_query;
mod snapshot;

pub(super) use adapter::CommandAdapter;
