//! The search/read flow.
//!
//! Routes a read request to the right surface — default precise (tantivy) text
//! search, explicitly-gated semantic (qdrant) search, registry search, and
//! graph expansion — and returns the responses.
//!
//! IMPLEMENT HERE: dispatch + response assembly for reads.
