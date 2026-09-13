//! Durable lexical projection storage split into codec, publication, and query seams.

mod codec;
mod model;
mod publication;
mod query;
mod store;

pub use model::{
    TantivyCandidate, TantivyProvenance, TantivyRowOrdinal, TantivySegment, TantivySegmentHit,
    TantivySegmentStore, TantivySegmentStoreError, TantivySnapshot,
};
