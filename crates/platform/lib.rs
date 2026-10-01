//! Audited operating-system seams for local process boundaries.
//! Unix builds re-export the standard Unix socket types unchanged, so every existing Unix path keeps its exact behaviour.
//! Windows builds provide the same stream, listener, and same-user identity surface over AF_UNIX sockets and Win32 security calls.
//! macOS's loaded-image identity check also uses a small audited `dladdr` boundary here.
//!
//! # Why this is its own crate
//!
//! Local surfaces trust a peer because the operating system says it runs as
//! the same user. On Unix that is one safe `rustix` call. On Windows it is a
//! socket ioctl, a process token, and a security descriptor, and each of those
//! is an unsafe FFI call. Keeping them here means the workspace-wide
//! `unsafe_code = "deny"` still holds everywhere else, and each platform trust
//! seam can be reviewed in one module.
#![cfg_attr(not(any(windows, target_os = "macos")), forbid(unsafe_code))]
#![cfg_attr(test, allow(clippy::expect_used))]

pub mod directory;
pub mod durability;
pub mod durable;
#[cfg_attr(target_os = "macos", allow(unsafe_code))]
pub mod executable_identity;
pub mod local;
mod native_path;
#[cfg(windows)]
pub mod win32;

pub use native_path::{NativePath, NativePathError, NativePathKey, NativePathWire};
