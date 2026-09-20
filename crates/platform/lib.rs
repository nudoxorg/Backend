//! Audited operating-system seams for local process boundaries.
//! Unix builds re-export the standard Unix socket types unchanged, so every existing Unix path keeps its exact behaviour.
//! Windows builds provide the same stream, listener, and same-user identity surface over AF_UNIX sockets and Win32 security calls.
//!
//! # Why this is its own crate
//!
//! Local surfaces trust a peer because the operating system says it runs as
//! the same user. On Unix that is one safe `rustix` call. On Windows it is a
//! socket ioctl, a process token, and a security descriptor, and each of those
//! is an unsafe FFI call. Keeping them here means the workspace-wide
//! `unsafe_code = "forbid"` still holds everywhere else — this crate's manifest
//! is the only place that softens it to `deny`, and only so the `win32` modules
//! can opt in by name — and the whole Windows trust seam can be reviewed in one
//! module.
#![cfg_attr(not(windows), forbid(unsafe_code))]
#![cfg_attr(test, allow(clippy::expect_used))]

pub mod durability;
pub mod local;
#[cfg(windows)]
pub mod win32;
