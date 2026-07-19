// nudox fork: crates.io qdrant-edge 0.7.2 with one change — the dead generic
// anonymize_collection_values{,_opt} helpers are removed from
// segment/common/anonymize.rs (their HRTB overflows the trait solver whenever
// objc2's recursive `&Retained<T>: IntoIterator` blanket impl is in the crate
// graph, e.g. via iroh/netdev on macOS).
#![allow(unexpected_cfgs)]
#![allow(dead_code, unused_imports)]
// #![warn(unnameable_types)] // TODO: re-enable when cleaning up the API
pub use edge::*;
mod bm25;
mod common;
mod edge;
mod gridstore;
mod posting_list;
mod quantization;
mod segment;
mod shard;
mod sparse;
mod wal;
