//! Validated command specifications and terminal process receipts.

mod command;
mod identity;
mod limits;
#[cfg(target_os = "linux")]
mod native_library;
mod receipt;

pub use command::{ProcessStdin, Stdin, StdinSpec, SupervisedCommand};
pub use identity::{
    ExecutableIdentity, MAX_EXECUTABLE_BYTES, ToolchainArtifact, VerifiedExecutable,
};
pub use limits::{ProcessEnvironment, ProcessLimits};
#[cfg(target_os = "linux")]
pub use native_library::NativeLibraryLease;
pub use receipt::{ProcessReceipt, ProcessTerminal};

pub(crate) use identity::ExecutableLease;
pub(crate) use receipt::receipt;
