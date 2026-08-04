//! Shared backend utilities.
//!
//! [`supervisor`] is the shared run loop (spawn, cgroup attach, wall timer,
//! capped pipe reads). The old `Backend` trait and per-backend structs have
//! been collapsed into the [`crate::cage`] module.

pub(crate) mod supervisor;
