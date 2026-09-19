//! Incremental operator facade.
//!
//! Operators are exposed as one stable family of laws while their kernels
//! remain isolated from arrangement ownership and retention policy.

mod join;
mod recursion;
mod stateless;
mod support;
mod topk;

pub use join::*;
pub use recursion::*;
pub use stateless::*;
pub use support::*;
pub use topk::*;
