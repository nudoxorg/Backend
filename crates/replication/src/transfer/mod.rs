//! Immutable object transfer protocol and typestate facade.
//!
//! The implementation is intentionally split by ownership boundary: wire
//! frames, resumable checkpoints, assembly, and verified object owners each
//! live in their own module while this facade preserves the historical public
//! re-exports.

mod assembly;
mod cas;
mod checkpoint;
mod lease;
mod object;
mod protocol;
mod receipt;
mod receiving_cas;

pub use assembly::*;
pub use cas::*;
pub use checkpoint::*;
pub use lease::*;
pub use object::*;
pub use protocol::*;
pub use receipt::*;
pub use receiving_cas::*;
