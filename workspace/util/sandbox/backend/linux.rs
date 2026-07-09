//! Linux bubblewrap backend — implementation lives in [`crate::cage`].
//!
//! Re-exported here so `backend::select` and historical imports keep working.

pub use crate::cage::LinuxNamespaces as LinuxBwrap;
