//! Bounded capture of both output streams from one owned child process.
//!
//! Unix polls nonblocking pipes without reader threads. The child remains
//! unreaped until both pipes close. A non-reaping exit observation then lets
//! capture retire the original process group even on ordinary success before
//! its leader PID is reused. Windows uses a separate
//! owned Job Object and overlapped-pipe implementation.
//! Unix targets must support waitid(WNOWAIT) and process groups; an OS refusal
//! is a typed capture failure, never an unbounded reader fallback. Targets
//! outside Unix and Windows return Unsupported before starting a child.
//! The capture deadline bounds reads and leader exit; Darwin's verified group
//! retirement has its own 500 ms cleanup bound after that terminal.

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
            // waitid(WNOWAIT) proves leader exit without reaping it. Even a
            // descendant that closed both pipes remains in the original group
            // until retirement, while the unreaped leader pins that PGID.
            if out_closed
                && err_closed
                && leader_finished(child.id()).map_err(CaptureError::Wait)?
            {
                return Ok((out, err));
            }
            if !progressed {
                thread::sleep(POLL_INTERVAL);
            }
        }
    })();
    match transaction {
        Ok((stdout, stderr)) => stop(&mut child)
            .map(|status| CapturedOutput {
                status,
                stdout,
                stderr,
            })
            .map_err(CaptureError::Wait),
        Err(primary) => match stop(&mut child) {
            Ok(_) => Err(primary),
            Err(source) => Err(CaptureError::Cleanup {
                primary: Box::new(primary),
                source,
            }),
        },
    }
}

#[cfg(unix)]
fn leader_finished(id: u32) -> io::Result<bool> {
    use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
    let pid = i32::try_from(id)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::other("invalid child PID"))?;
    match waitid(
        WaitId::Pid(pid),
        WaitIdOptions::NOHANG | WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
    ) {
        Ok(Some(status)) => Ok(status.exited() || status.killed() || status.dumped()),
        Ok(None) => Ok(false),
        Err(error) if error == rustix::io::Errno::INTR => Ok(false),
        Err(error) => Err(error.into()),
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
pub fn stop(child: &mut std::process::Child) -> io::Result<ExitStatus> {
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
    let status = child.wait()?;
    group_result?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    // Every test here drives the Unix pipe poller; the Windows capture has its
    // own tests beside its implementation in `windows_child_output`.
    #[cfg(unix)]
    use super::*;
    #[cfg(unix)]
    use std::time::Duration;

    #[cfg(unix)]
    const FIXTURE: &str = "child_output::tests::native_fixture";

    #[cfg(unix)]
    fn fixture(
        mode: &str,
        root: Option<&std::path::Path>,
        timeout: Duration,
        stdout_bytes: usize,
        stderr_bytes: usize,
        cancelled: &AtomicBool,
    ) -> Result<CapturedOutput, CaptureError> {
        let mut overrides = vec![("BACKEND_CAPTURE_MODE".into(), Some(mode.into()))];
        if let Some(root) = root {
            overrides.push((
                "BACKEND_CAPTURE_ROOT".into(),
                Some(root.as_os_str().to_os_string()),
            ));
        }
        capture(
            &CaptureCommand {
                program: std::env::current_exe()
                    .expect("test executable")
                    .into_os_string(),
                args: vec!["--exact".into(), FIXTURE.into(), "--nocapture".into()],
                cwd: None,
                environment: CaptureEnvironment::Inherit,
                overrides,
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
    fn native_fixture() {
        use std::{fs, io::Write as _, process::Stdio};
        let Ok(mode) = std::env::var("BACKEND_CAPTURE_MODE") else {
            return;
        };
        match mode.as_str() {
            "dual" => {
                std::io::stdout()
                    .write_all(b"capture-stdout-token")
                    .expect("stdout");
                std::io::stderr()
                    .write_all(b"capture-stderr-token")
                    .expect("stderr");
            }
            "large-out" => std::io::stdout()
                .write_all(&[b'x'; 16 * 1024])
                .expect("stdout"),
            "large-err" => std::io::stderr()
                .write_all(&[b'y'; 16 * 1024])
                .expect("stderr"),
            "leader-inherit" | "leader-closed" => {
                let root = std::path::PathBuf::from(
                    std::env::var_os("BACKEND_CAPTURE_ROOT").expect("fixture root"),
                );
                let mut command =
                    std::process::Command::new(std::env::current_exe().expect("executable"));
                command
                    .args(["--exact", FIXTURE, "--nocapture"])
                    .env(
                        "BACKEND_CAPTURE_MODE",
                        if mode == "leader-closed" {
                            "sleep-marker"
                        } else {
                            "sleep-marker-long"
                        },
                    )
                    .env("BACKEND_CAPTURE_ROOT", &root)
                    .stdin(Stdio::null());
                if mode == "leader-closed" {
                    command.stdout(Stdio::null()).stderr(Stdio::null());
                } else {
                    command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
                }
                let _descendant = command.spawn().expect("descendant");
                let deadline = Instant::now() + Duration::from_secs(2);
                while !root.join("started").exists() {
                    assert!(Instant::now() < deadline, "descendant did not start");
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            "sleep-marker" | "sleep-marker-long" => {
                let root = std::path::PathBuf::from(
                    std::env::var_os("BACKEND_CAPTURE_ROOT").expect("fixture root"),
                );
                fs::write(root.join("started"), b"started").expect("started marker");
                std::thread::sleep(if mode == "sleep-marker" {
                    Duration::from_secs(2)
                } else {
                    Duration::from_secs(5)
                });
                fs::write(root.join("survived"), b"survived").expect("survival marker");
            }
            _ => panic!("unknown fixture mode"),
        }
    }

    #[cfg(unix)]
    fn scratch(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "backend-capture-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("scratch root");
        root
    }

    #[cfg(unix)]
    #[test]
    fn captures_both_streams_after_an_early_leader_exit() {
        let output = fixture(
            "dual",
            None,
            Duration::from_secs(2),
            4096,
            4096,
            &AtomicBool::new(false),
        )
        .expect("bounded capture");
        assert!(output.status.success());
        assert!(
            output
                .stdout
                .windows(b"capture-stdout-token".len())
                .any(|part| part == b"capture-stdout-token")
        );
        assert!(
            output
                .stderr
                .windows(b"capture-stderr-token".len())
                .any(|part| part == b"capture-stderr-token")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_descendant_holding_both_pipes_reaches_one_deadline() {
        let root = scratch("held-pipes");
        let started = Instant::now();
        let result = fixture(
            "leader-inherit",
            Some(&root),
            Duration::from_secs(1),
            4096,
            4096,
            &AtomicBool::new(false),
        );
        assert!(matches!(result, Err(CaptureError::Deadline)));
        assert!(root.join("started").exists(), "descendant fixture started");
        assert!(started.elapsed() < Duration::from_secs(3));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn successful_leader_exit_retires_a_descendant_that_closed_both_pipes() {
        let root = scratch("closed-pipes");
        let output = fixture(
            "leader-closed",
            Some(&root),
            Duration::from_secs(2),
            4096,
            4096,
            &AtomicBool::new(false),
        )
        .expect("successful capture");
        assert!(output.status.success());
        assert!(root.join("started").exists(), "descendant fixture started");
        std::thread::sleep(Duration::from_millis(2300));
        assert!(
            !root.join("survived").exists(),
            "contained descendant survived successful capture"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn oversized_stdout_and_stderr_are_rejected_independently() {
        assert!(matches!(
            fixture(
                "large-out",
                None,
                Duration::from_secs(2),
                1024,
                65536,
                &AtomicBool::new(false)
            ),
            Err(CaptureError::OutputLimit {
                stream: OutputStream::Stdout,
                ..
            })
        ));
        assert!(matches!(
            fixture(
                "large-err",
                None,
                Duration::from_secs(2),
                65536,
                1024,
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
        let root = scratch("cancel");
        let cancelled = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancelled);
        let marker = root.join("started");
        let timer = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(1);
            while !marker.exists() {
                assert!(Instant::now() < deadline, "descendant did not start");
                std::thread::sleep(Duration::from_millis(2));
            }
            trigger.store(true, Ordering::Release);
        });
        let started = Instant::now();
        let result = fixture(
            "leader-inherit",
            Some(&root),
            Duration::from_secs(3),
            4096,
            4096,
            &cancelled,
        );
        timer.join().expect("cancel trigger");
        assert!(matches!(result, Err(CaptureError::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(3));
        let _ = std::fs::remove_dir_all(root);
    }
}
