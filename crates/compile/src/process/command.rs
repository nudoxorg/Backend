//! Standard-input policy and complete supervised command identities.

use super::{
    ExecutableIdentity, ProcessEnvironment, ProcessLimits, ProcessReceipt, ToolchainArtifact,
};

mod spec;
use crate::{
    Cancellation, CommandId, CommandSchema, ProcessError, ProtocolDescriptor, SessionKey,
    ToolchainId, typed_of,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const MAX_ARGUMENTS: usize = 256;
const MAX_ARGUMENT_BYTES: usize = 64 * 1024;

/// Explicit standard-input policy for a supervised process.
///
/// A command never inherits the parent's standard input.  [`Self::Bytes`]
/// supplies a bounded, immutable request body; [`Self::Null`] closes the
/// stream at process start. Shared request images retain their allocation
/// while a supervised command consumes them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProcessStdin {
    /// Start with a closed standard-input stream.
    Null,
    /// Feed these bytes and then close standard input.
    Bytes(Vec<u8>),
    /// Feed an immutable shared request image and then close standard input.
    Shared(Arc<[u8]>),
}

impl ProcessStdin {
    /// Returns a closed standard-input policy.
    #[must_use]
    pub const fn null() -> Self {
        Self::Null
    }

    /// Creates a standard-input policy from owned bytes.
    #[must_use]
    pub fn bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self::Bytes(bytes.into())
    }

    /// Creates a standard-input policy borrowing an immutable shared image.
    #[must_use]
    pub fn shared_bytes(bytes: Arc<[u8]>) -> Self {
        Self::Shared(bytes)
    }

    /// Returns the supplied bytes, if this policy feeds standard input.
    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Null => None,
            Self::Bytes(bytes) => Some(bytes),
            Self::Shared(bytes) => Some(bytes),
        }
    }

    /// Returns the number of bytes supplied to standard input.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Null => 0,
            Self::Bytes(bytes) => bytes.len(),
            Self::Shared(bytes) => bytes.len(),
        }
    }

    /// Returns whether this policy supplies no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Alias for callers that use the protocol term “stdin”.
pub type StdinSpec = ProcessStdin;

/// Short alias for the explicit standard-input policy.
pub type Stdin = ProcessStdin;

/// Explicit command specification consumed by a [`crate::ProcessSupervisor`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupervisedCommand {
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    environment: ProcessEnvironment,
    stdin: ProcessStdin,
    toolchain: Option<ToolchainId>,
    executable_identity: Option<ExecutableIdentity>,
    toolchain_artifact: Option<ToolchainArtifact>,
    session_key: Option<SessionKey>,
    protocol: ProtocolDescriptor,
    limits: ProcessLimits,
}
