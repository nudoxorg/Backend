//! Audited operating-system seams for local process boundaries.
//! Unix builds re-export the standard Unix socket types unchanged, so every existing Unix path keeps its exact behaviour.
//! macOS adds narrowly scoped process retirement and loaded-image identity
//! adapters. Windows provides the same stream, listener, and same-user identity
//! surface over AF_UNIX sockets and Win32 security calls.
//!
//! # Why this is its own crate
//!
//! Local surfaces trust a peer because the operating system says it runs as
//! the same user. On Unix that is one safe `rustix` call. Windows uses socket,
//! process-token, and security-descriptor FFI. macOS process retirement and
//! loaded-image checks also need native interfaces. Keeping these seams here
//! means the workspace-wide `unsafe_code = "deny"` holds everywhere else,
//! and each platform boundary can be reviewed in one module.
#![cfg_attr(not(any(windows, target_os = "macos")), forbid(unsafe_code))]
#![cfg_attr(test, allow(clippy::expect_used))]

pub mod directory;
pub mod durability;
pub mod durable;
#[cfg_attr(target_os = "macos", allow(unsafe_code))]
pub mod executable_identity;
pub mod local;
#[cfg(target_os = "macos")]
pub mod macos_process;
mod native_path;
#[cfg(windows)]
pub mod win32;

pub use directory::{DirectoryCapability, DirectoryEntry, DirectoryRenameError, EntryKind};
pub use native_path::{NativePath, NativePathError, NativePathKey, NativePathWire};
