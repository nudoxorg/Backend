//! How we manage access.
//! We don't provide any kind of access controls — server providers gate through a proxy or similar.
//! For our own services we simply provide a wall through an internal auth provider for rate limiting.

pub mod federation;
pub mod source;

pub use federation::{Federation, SourceRole, Sourced};
pub use source::{Source, SourceId};

/// The access-control policy every read/write is checked through.
///
/// We expose the contract here so the server can be generic over the
/// implementation — the concrete policy is injected at assembly time.
pub trait AccessPolicy: Send + Sync + 'static {}
