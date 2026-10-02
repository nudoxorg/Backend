//! Bounded capture of both output streams from one owned child process.
//!
//! Unix polls nonblocking pipes without reader threads. The child remains
//! unreaped until both pipes close, so cancellation can retire the original
//! process group before its leader PID is reused. Windows uses a separate
//! owned Job Object and overlapped-pipe implementation.

use std::{
    ffi::OsString, io, path::PathBuf, process::ExitStatus, sync::atomic::AtomicBool, time::Instant,
};

#[cfg(unix)]
use std::{
    io::Read,
    os::fd::AsFd,
    process::{Command, Stdio},
    sync::atomic::Ordering,
    thread,
    time::Duration,
};

#[cfg(unix)]
const READ_CHUNK_BYTES: usize = 4096;
#[cfg(unix)]
const POLL_INTERVAL: Duration = Duration::from_millis(2);

/// The complete process input. Explicit fields let platform owners preserve
/// environment clearing and override/removal semantics without inspecting a
/// partially configured std Command.
#[derive(Clone, Debug)]
pub struct CaptureCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub environment: CaptureEnvironment,
    pub overrides: Vec<(OsString, Option<OsString>)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureEnvironment {
    Inherit,
    Clear,
}

/// One absolute clock and independent retained-byte limits.
#[derive(Clone, Copy, Debug)]
pub struct CaptureLimits {
    pub deadline: Instant,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
}

/// A complete child transaction; no reader thread or child remains live.
#[derive(Debug)]
pub struct CapturedOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

/// Exact bounded-child failure. Cleanup retains the original primary cause.
#[derive(Debug)]
pub enum CaptureError {
    Unsupported,
    /// Process-wide capture ownership slots are all occupied. No child spawned.
    Capacity {
        maximum: usize,
    },
    Spawn(io::Error),
    MissingPipe(OutputStream),
    Configure {
        stream: OutputStream,
        source: io::Error,
    },
    Read {
        stream: OutputStream,
        source: io::Error,
    },
    Wait(io::Error),
    OutputLimit {
        stream: OutputStream,
        observed: usize,
        maximum: usize,
    },
    Deadline,
    Cancelled,
    Cleanup {
        primary: Box<Self>,
        source: io::Error,
    },
}

/// Captures one process under a shared stdout/stderr/exit deadline.
/// Cancellation must be an atomic flag whose owner remains alive for the call.
pub fn capture(
    command: &CaptureCommand,
    limits: CaptureLimits,
    cancelled: &AtomicBool,
) -> Result<CapturedOutput, CaptureError> {
    #[cfg(unix)]
    return capture_unix(command, limits, cancelled);
    #[cfg(windows)]
    return crate::windows_child_output::capture(command, limits, cancelled);
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (command, limits, cancelled);
        Err(CaptureError::Unsupported)
    }
}

#[cfg(unix)]
fn capture_unix(
    spec: &CaptureCommand,
    limits: CaptureLimits,
    cancelled: &AtomicBool,
) -> Result<CapturedOutput, CaptureError> {
    use std::os::unix::process::CommandExt;

    if cancelled.load(Ordering::Acquire) {
        return Err(CaptureError::Cancelled);
    }
    if Instant::now() >= limits.deadline {
        return Err(CaptureError::Deadline);
    }
    let mut command = Command::new(&spec.program);
    command.args(&spec.args);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    if spec.environment == CaptureEnvironment::Clear {
        command.env_clear();
    }
    for (key, value) in &spec.overrides {
        if let Some(value) = value {
            command.env(key, value);
        } else {
            command.env_remove(key);
        }
    }
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(CaptureError::Spawn)?;
    let transaction = (|| {
        let mut stdout = child
            .stdout
            .take()
            .ok_or(CaptureError::MissingPipe(OutputStream::Stdout))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or(CaptureError::MissingPipe(OutputStream::Stderr))?;
        configure(&stdout).map_err(|source| CaptureError::Configure {
            stream: OutputStream::Stdout,
            source,
        })?;
        configure(&stderr).map_err(|source| CaptureError::Configure {
            stream: OutputStream::Stderr,
            source,
        })?;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut out_closed = false;
        let mut err_closed = false;
        let mut chunk = [0_u8; READ_CHUNK_BYTES];
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Err(CaptureError::Cancelled);
            }
            if Instant::now() >= limits.deadline {
                return Err(CaptureError::Deadline);
            }
            let mut progressed = false;
            if !out_closed {
                match read_available(&mut stdout, &mut chunk) {
                    Ok(Some(0)) => out_closed = true,
                    Ok(Some(count)) => {
                        append_bounded(
                            &mut out,
                            &chunk[..count],
                            limits.stdout_bytes,
                            OutputStream::Stdout,
                        )?;
                        progressed = true;
                    }
                    Ok(None) => {}
                    Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
                    Err(source) => {
                        return Err(CaptureError::Read {
                            stream: OutputStream::Stdout,
                            source,
                        });
                    }
                }
            }
            if !err_closed {
                match read_available(&mut stderr, &mut chunk) {
                    Ok(Some(0)) => err_closed = true,
                    Ok(Some(count)) => {
                        append_bounded(
                            &mut err,
                            &chunk[..count],
                            limits.stderr_bytes,
                            OutputStream::Stderr,
                        )?;
                        progressed = true;
                    }
                    Ok(None) => {}
                    Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
                    Err(source) => {
                        return Err(CaptureError::Read {
                            stream: OutputStream::Stderr,
                            source,
                        });
                    }
                }
            }
            // try_wait reaps on success. Never call it while a descendant
            // might retain a pipe: deadline cleanup needs the unreaped leader.
            if out_closed && err_closed {
                if let Some(status) = child.try_wait().map_err(CaptureError::Wait)? {
                    return Ok(CapturedOutput {
                        status,
                        stdout: out,
                        stderr: err,
                    });
                }
            }
            if !progressed {
                thread::sleep(POLL_INTERVAL);
            }
        }
    })();
    match transaction {
        Ok(output) => Ok(output),
        Err(primary) => match stop(&mut child) {
            Ok(()) => Err(primary),
            Err(source) => Err(CaptureError::Cleanup {
                primary: Box::new(primary),
                source,
            }),
        },
    }
}

#[cfg(unix)]
fn append_bounded(
    bytes: &mut Vec<u8>,
    chunk: &[u8],
    maximum: usize,
    stream: OutputStream,
) -> Result<(), CaptureError> {
    let observed = bytes.len().saturating_add(chunk.len());
    if observed > maximum {
        return Err(CaptureError::OutputLimit {
            stream,
            observed,
            maximum,
        });
    }
    bytes.extend_from_slice(chunk);
    Ok(())
}

#[cfg(unix)]
fn configure(pipe: &impl AsFd) -> io::Result<()> {
    use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
    let flags = fcntl_getfl(pipe)?;
    fcntl_setfl(pipe, flags | OFlags::NONBLOCK)?;
    Ok(())
}

#[cfg(unix)]
fn read_available(pipe: &mut impl Read, buffer: &mut [u8]) -> io::Result<Option<usize>> {
    match pipe.read(buffer) {
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        result => result.map(Some),
    }
}

/// Retires the original process group while the leader is still waitable,
/// then reaps it. Never call this after try_wait returned an exit status.
/// Descendants that escape the group/session require an outer OS sandbox.
#[cfg(unix)]
pub fn stop(child: &mut std::process::Child) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let group_result = crate::macos_process::retire_process_group(child.id())
        .map_err(|error| io::Error::other(format!("Darwin group retirement failed: {error:?}")));
    #[cfg(not(target_os = "macos"))]
    let group_result = i32::try_from(child.id())
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .ok_or_else(|| io::Error::other("invalid child process group"))
        .and_then(|group| {
            rustix::process::kill_process_group(group, rustix::process::Signal::KILL)
                .or_else(|error| {
                    if error == rustix::io::Errno::SRCH {
                        Ok(())
                    } else {
                        Err(error)
                    }
                })
                .map_err(io::Error::from)
        });
    let _ = child.kill();
    let reap_result = child.wait().map(|_| ());
    group_result.and(reap_result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[cfg(unix)]
    fn shell(
        script: &str,
        timeout: Duration,
        stdout_bytes: usize,
        stderr_bytes: usize,
        cancelled: &AtomicBool,
    ) -> Result<CapturedOutput, CaptureError> {
        capture(
            &CaptureCommand {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                cwd: None,
                environment: CaptureEnvironment::Inherit,
                overrides: Vec::new(),
            },
            CaptureLimits {
                deadline: Instant::now() + timeout,
                stdout_bytes,
                stderr_bytes,
            },
            cancelled,
        )
    }

    #[cfg(unix)]
    #[test]
    fn captures_both_streams_after_an_early_leader_exit() {
        let output = shell(
            "printf out; printf err >&2",
            Duration::from_secs(2),
            16,
            16,
            &AtomicBool::new(false),
        )
        .expect("bounded capture");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"err");
    }

    #[cfg(unix)]
    #[test]
    fn a_descendant_holding_both_pipes_reaches_one_deadline() {
        let started = Instant::now();
        let result = shell(
            "sleep 10 & exit 0",
            Duration::from_millis(100),
            16,
            16,
            &AtomicBool::new(false),
        );
        assert!(matches!(result, Err(CaptureError::Deadline)));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[cfg(unix)]
    #[test]
    fn oversized_stdout_and_stderr_are_rejected_independently() {
        assert!(matches!(
            shell(
                "printf 123456789",
                Duration::from_secs(2),
                4,
                16,
                &AtomicBool::new(false)
            ),
            Err(CaptureError::OutputLimit {
                stream: OutputStream::Stdout,
                ..
            })
        ));
        assert!(matches!(
            shell(
                "printf 123456789 >&2",
                Duration::from_secs(2),
                16,
                4,
                &AtomicBool::new(false)
            ),
            Err(CaptureError::OutputLimit {
                stream: OutputStream::Stderr,
                ..
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_retires_a_child_with_descendants() {
        use std::sync::{Arc, atomic::Ordering};
        let cancelled = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancelled);
        let timer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            trigger.store(true, Ordering::Release);
        });
        let started = Instant::now();
        let result = shell(
            "sleep 10 & wait",
            Duration::from_secs(2),
            16,
            16,
            &cancelled,
        );
        timer.join().expect("cancel trigger");
        assert!(matches!(result, Err(CaptureError::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
