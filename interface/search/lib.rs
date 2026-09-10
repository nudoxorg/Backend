//! The `interface-search` crate exists to define one honest multi-lane search vocabulary and its ranking over every retrieval backend.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//!
//! Four lanes answer one request: exact keys, lexical names, the Trustfall relation graph, and
//! Qdrant vectors. Each lane reports its own [`Coverage`]; a lane that could not run says so rather
//! than contributing zero rows silently. The merged hit list is what every surface shows, and the
//! lane reports are what every surface shows *beside* it.

mod graph;
mod lane;
mod parse;
mod request;
mod score;
mod terminal;

pub use graph::{
    Depth, GraphEdge, GraphRequest, GraphSource, GraphTerminal, LINK_KINDS, MAX_GRAPH_DEPTH,
    RelationLabel, parse_relation, relation_label,
};
pub use lane::{Coverage, Degradation, Lane, LaneReport, LaneSet, Micros, Unavailability};
pub use parse::{QueryShape, classify, query_term};
pub use request::{
    Cursor, DEFAULT_RESULT_LIMIT, KindSet, MAX_QUERY_BYTES, MAX_RESULT_LIMIT, QueryText,
    QueryTextError, ResultLimit, SearchRequest, SearchScope,
};
pub use score::{
    CONTAINED_NAME_SCORE, EXACT_NAME_SCORE, FOLDED_NAME_SCORE, FOLDED_PREFIX_SCORE,
    PREFIX_NAME_SCORE, name_score,
};
pub use terminal::{Hit, Score, SearchTerminal, Truncation, merge_lanes};
