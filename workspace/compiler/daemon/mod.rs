//! The compiler daemon's runtime modules: forge context and HTTP server logic.
//!
//! This module is compiled into the `compiler` library so the daemon binary
//! (`compiler-daemon`) can reach the `ForgeRuntime` without creating a
//! circular dependency.

pub mod forge;
