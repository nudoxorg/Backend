//! Exercises the `engine driver` tests native-compile contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
#[path = "native_compile/authority.rs"]
mod authority;
#[cfg(unix)]
#[path = "native_compile/bounded_native.rs"]
mod bounded_native;
#[path = "native_compile/lowering.rs"]
mod lowering;
#[path = "native_compile/matrix.rs"]
mod matrix;
#[path = "native_compile/rejection.rs"]
mod rejection;
#[path = "native_compile/support.rs"]
mod support;
