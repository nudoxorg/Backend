//! Pure projections from library values into the shapes views render.
//! Identity, page, shelf, signature, prose, status, and fault live here.
//! Nothing in this module touches GPUI, the filesystem, or the network.
//!
//! This is the layer a concurrent presentation crate is meant to replace. It
//! is kept deliberately small and value-shaped — every type is `Clone + Eq`,
//! every function is total, and there is one constructor per input shape — so
//! that swapping it for a shared crate is a matter of changing imports rather
//! than rewriting views. Until such a crate exists and compiles, this is the
//! desktop's own copy and is tested on its own.

pub(crate) mod fault;
pub(crate) mod identity;
pub(crate) mod page;
pub(crate) mod prose;
pub(crate) mod shelf;
pub(crate) mod signature;
pub(crate) mod status;
