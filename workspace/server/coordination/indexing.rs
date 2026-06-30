//! The indexing flow.
//!
//! - runs the computer (drives `compiler::generation`);
//! - updates postgres status (tantivy polls postgres for the new work);
//! - generates blob information and fans it out to the sinks.
//!
//! IMPLEMENT HERE: the orchestration that turns a materialized package into a
//! fully-indexed, searchable one.
