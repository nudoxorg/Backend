//! Typed identities and bounded coalescing state used by the execution plane.
//!
//! Identity construction and runtime ownership are separate modules so the
//! public type surface stays explicit while the interner can enforce affine
//! follower and generation accounting privately.

mod identity;
mod interner;

pub use identity::*;
pub use interner::*;
