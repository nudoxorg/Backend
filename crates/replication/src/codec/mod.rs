//! Canonical bounded replication message codec.
//!
//! The wire grammar is split into shared bounded primitives and message-family
//! payloads while this facade preserves the original encode/decode API.

mod messages;
mod primitives;

pub use messages::{decode_message, encode_message};
