//! Exercises the `compiler-application` tests local-compiler contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Public local-compilation journey through real Rust lowering and durable publication.

#[path = "local_compiler/journey.rs"]
mod journey;
#[path = "local_compiler/packages.rs"]
mod packages;
#[path = "local_compiler/support.rs"]
mod support;
#[path = "local_compiler/toolchains.rs"]
mod toolchains;
