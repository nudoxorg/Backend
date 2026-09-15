//! One local stream, listener, and address type for every process-boundary crate.
//! On Unix these are the standard library's Unix socket types, re-exported unchanged.
//! On Windows they are AF_UNIX sockets with the same method names, so call sites need no platform branches.

#[cfg(unix)]
pub use std::os::unix::net::{
    SocketAddr as LocalAddr, UnixListener as LocalListener, UnixStream as LocalStream,
};

#[cfg(windows)]
pub use crate::win32::socket::{LocalAddr, LocalListener, LocalStream};
