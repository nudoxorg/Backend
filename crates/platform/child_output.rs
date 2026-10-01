//! Pollable reads from a spawned child's stdout pipe on Unix.
//!
//! A child can exit while one of its descendants retains stdout. A blocking
//! `read_to_end` or a reader-thread join would then outlive the child's own
//! deadline. This adapter exposes only bytes available now, or EOF, so the
//! caller can enforce one deadline for process exit and pipe retirement.
//! Windows' synchronous anonymous child pipes cannot make this promise:
//! `PeekNamedPipe` may itself block. Until an overlapped named-pipe capture
//! owns cancellation and descendant retirement, that platform fails closed.

use std::io::{self, Read as _};
use std::process::{Child, ChildStdout};

/// Prepares a child's stdout for deadline-driven reads.
///
/// # Errors
/// Returns an OS error when the pipe cannot be made nonblocking.
pub fn configure(stdout: &ChildStdout) -> io::Result<()> {
    #[cfg(unix)]
    {
        use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
        let flags = fcntl_getfl(stdout)?;
        fcntl_setfl(stdout, flags | OFlags::NONBLOCK)?;
    }
    #[cfg(windows)]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "bounded Windows child capture requires an overlapped pipe",
    ));
    #[cfg(unix)]
    Ok(())
}

/// Reads at most `buffer.len()` bytes without waiting for a writer.
///
/// `None` means no bytes are available yet; `Some(0)` means every writer has
/// closed the pipe. The caller retains its wall-clock and total-byte bounds.
///
/// # Errors
/// Returns an OS error for a broken pipe state other than normal EOF.
pub fn read_available(stdout: &mut ChildStdout, buffer: &mut [u8]) -> io::Result<Option<usize>> {
    if buffer.is_empty() {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        match stdout.read(buffer) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            result => result.map(Some),
        }
    }
    #[cfg(windows)]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "bounded Windows child capture requires an overlapped pipe",
    ))
}

/// Retires a timed-out child before returning from an output read.
///
/// On macOS this reuses the verified process-session retirement boundary, so
/// descendants still holding stdout are killed before the leader is reaped.
/// Other Unix children must have been started with `Command::process_group(0)`.
/// Windows does not enter this bounded-capture path until it has a Job Object.
pub fn stop(child: &mut Child) {
    #[cfg(target_os = "macos")]
    {
        let _ = crate::macos_process::retire_process_group(child.id());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = i32::try_from(child.id())
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .map(|group| rustix::process::kill_process_group(group, rustix::process::Signal::KILL));
    }
    let _ = child.kill();
    let _ = child.wait();
}
