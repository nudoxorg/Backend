//! Validated command specifications and terminal process receipts.

mod command;
mod identity;
mod limits;
mod receipt;

pub use command::{ProcessStdin, Stdin, StdinSpec, SupervisedCommand};
pub use identity::{
    ExecutableIdentity, MAX_EXECUTABLE_BYTES, ToolchainArtifact, VerifiedExecutable,
};
pub use limits::{ProcessEnvironment, ProcessLimits};
pub use receipt::{ProcessReceipt, ProcessTerminal};

pub(crate) use identity::ExecutableLease;
pub(crate) use receipt::receipt;
