//! Windows implementations of the local process-boundary seams.
//! `socket` provides AF_UNIX streams and listeners; `identity` answers "does this peer or file belong to me?".
//! `security` restricts a file to its owner, and `random` fills buffers from the system generator.

pub(crate) mod durable;
pub(crate) mod file;
pub mod identity;
pub mod project_fs;
pub mod random;
pub mod security;
pub mod socket;
pub mod workspace_fs;

#[cfg(test)]
mod tests;
