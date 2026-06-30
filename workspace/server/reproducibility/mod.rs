//! REPRODUCIBILITY — registry is the most critical, durable layer, responsible
//! for persisting every store so the whole system can be rebuilt:
//!
//! - qdrant persistence (vectors)
//! - tantivy persistence (text indexes)
//! - terminus persistence (graph)
//! - blobs: the parsed code
//!
//! IMPLEMENT HERE: snapshot/restore for each backing store + the blobs.

pub mod blobs;
pub mod qdrant;
pub mod tantivy;
pub mod terminus;
