#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Blocking Qdrant projection for immutable vector snapshots.
//!
//! The adapter owns only the disposable remote projection. A point's payload retains the complete
//! snapshot/model/metric/partition/entity authority, while its Qdrant integer ID is a physical
//! coordinate checked against that payload on every write and readback. Network calls are
//! deliberately synchronous: callers that need an async stream can place this named blocking edge
//! behind their bounded runtime adapter.

mod adapter;
mod admission;
mod config;
mod contract;
mod identity;
mod limits;
mod mutation;
mod query;
mod scoring;
mod transport;
mod wire;

pub use adapter::QdrantBlockingAdapter;
pub use contract::*;
