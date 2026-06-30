//! Keeping the text index fresh: tantivy polls postgres for newly-indexed work
//! and pulls it into the index (rather than postgres pushing into tantivy).
//!
//! IMPLEMENT HERE: the postgres → tantivy poll loop.
