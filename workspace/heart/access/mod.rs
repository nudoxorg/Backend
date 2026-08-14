//! Source federation and provider identity (no access policy yet).
//! We don't provide any kind of access controls — server providers gate through a proxy or similar.
//! For our own services we simply provide a wall through an internal auth provider for rate limiting.
//!
//! Hosted multi-tenancy will reintroduce a real access-policy trait when needed;
//! until then there is no empty marker / `Arc<dyn …>` to thread around.

pub mod federation;
pub mod source;

pub use federation::{Federation, SourceRole, Sourced};
pub use source::{Source, SourceId};
