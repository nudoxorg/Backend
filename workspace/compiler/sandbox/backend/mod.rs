//! Backend run loop and the real smolvm runtime.
//!
//! [`supervisor`] is the shared run loop (spawn, cgroup attach, wall timer,
//! capped pipe reads) used by dev passthrough. [`smolvm`] is the real
//! [`crate::vm::VmRuntime`] over the vendored smolvm crate.

pub mod smolvm;
pub(crate) mod supervisor;
