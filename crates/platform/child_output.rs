//! Pollable reads from a spawned child's stdout pipe.
//!
//! A child can exit while one of its descendants retains stdout. A blocking
//! `read_to_end` or a reader-thread join would then outlive the child's own
//! deadline. This adapter exposes only bytes available now, or EOF, so the
//! caller can enforce one deadline for process exit and pipe retirement.
#![cfg_attr(
    windows,
    allow(
        unsafe_code,
        reason = "PeekNamedPipe is the audited Windows availability query for an anonymous child pipe"
    )
)]

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
    {
        let _ = stdout;
        // Anonymous pipes do not expose a nonblocking mode through the Rust
        // handle. `read_available` peeks before every bounded read instead.
    }
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
    {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED};
        use windows_sys::Win32::System::Pipes::PeekNamedPipe;

        let mut available = 0_u32;
        // SAFETY: `stdout` owns the pipe handle for this call. A null output
        // buffer requests only the kernel's available-byte count; the sole
        // writable pointer is a valid local `u32`. The handle remains alive
        // while the subsequent safe `Read` borrows `stdout` mutably.
        let peeked = unsafe {
            PeekNamedPipe(
                stdout.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if peeked == 0 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error().map(|code| code as u32) {
                Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED) => Ok(Some(0)),
                _ => Err(error),
            };
        }
        if available == 0 {
            return Ok(None);
        }
        let take = buffer.len().min(available as usize);
        stdout.read(&mut buffer[..take]).map(Some)
    }
}

/// Retires a timed-out child before returning from an output read.
///
/// On macOS this reuses the verified process-session retirement boundary, so
/// descendants still holding stdout are killed before the leader is reaped.
/// Other Unix children must have been started with `Command::process_group(0)`;
/// Windows kills and reaps the leader, while a separate Job Object would be
/// required to prove descendant retirement there.
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
