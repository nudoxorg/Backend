//! Reaped process terminal states and bounded output receipts.

/// Supervisor terminal state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessTerminal {
    /// The child exited successfully.
    Success,
    /// The child exited with a non-success status.
    Exit,
    /// The caller requested cancellation.
    Cancelled,
    /// The wall-time limit elapsed.
    Deadline,
    /// An output limit was exceeded.
    OutputLimit,
    /// The child could not be spawned.
    SpawnFailure,
    /// A protocol or cleanup invariant failed.
    ProtocolFailure,
}

/// A fully reaped, bounded process result.
///
/// The fields are private so a caller cannot manufacture a receipt claiming
/// successful cleanup without actually running the supervisor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessReceipt {
    terminal: ProcessTerminal,
    status: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_observed: usize,
    stderr_observed: usize,
    reaped: bool,
}

impl ProcessReceipt {
    /// Returns the terminal child status.
    #[must_use]
    pub const fn terminal(&self) -> ProcessTerminal {
        self.terminal
    }

    /// Returns the OS exit code, if one was supplied.
    #[must_use]
    pub const fn status(&self) -> Option<i32> {
        self.status
    }

    /// Returns bounded stdout bytes.
    #[must_use]
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    /// Returns bounded stderr bytes.
    #[must_use]
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    /// Returns the observed stdout byte count.
    #[must_use]
    pub const fn stdout_observed(&self) -> usize {
        self.stdout_observed
    }

    /// Returns the observed stderr byte count.
    #[must_use]
    pub const fn stderr_observed(&self) -> usize {
        self.stderr_observed
    }

    /// Returns whether the supervisor proved child cleanup and reaping.
    #[must_use]
    pub const fn reaped(&self) -> bool {
        self.reaped
    }

    /// Consumes the receipt and returns bounded stdout and stderr ownership.
    #[must_use]
    pub fn into_output(self) -> (Vec<u8>, Vec<u8>) {
        (self.stdout, self.stderr)
    }
}

pub(crate) fn receipt(
    terminal: ProcessTerminal,
    status: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
) -> ProcessReceipt {
    ProcessReceipt {
        stdout_observed: stdout.len(),
        stderr_observed: stderr.len(),
        terminal,
        status,
        stdout,
        stderr,
        reaped: true,
    }
}
