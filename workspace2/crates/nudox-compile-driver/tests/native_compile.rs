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
#[path = "native_compile/scanner.rs"]
mod scanner;
#[path = "native_compile/support.rs"]
mod support;
