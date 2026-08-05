//! Shared run loop: spawn, cgroup attach, wall timer, capped pipe reads.

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::cancel::CancelToken;
use crate::cgroup::Cgroup;
use crate::error::{KillReason, SandboxError};
use crate::limits::Limits;
use crate::spec::Output;

/// Drive a spawned child to completion under `limits`.
///
/// Honors `cancel` each poll iteration: writes `cgroup.kill` (when a cgroup is
/// owned) then kills the direct child. Forced kills (wall, output cap, cancel)
/// always go through [`force_kill`].
pub(crate) fn supervise(
    mut child: Child,
    limits: &Limits,
    cgroup: Option<Cgroup>,
    cancel: &CancelToken,
) -> Result<Output, SandboxError> {
    if let Some(ref cg) = cgroup
        && let Err(e) = cg.add_pid(child.id())
    {
        tracing::warn!(error = %e, "cgroup attach failed; relying on rlimits");
    }

    let start = Instant::now();
    let deadline = start + limits.wall;

    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    let mut stdout_done = false;
    let mut stderr_done = false;

    // Non-blocking-ish drain with deadline: use try_wait + read with short sleeps.
    make_nonblocking(child.stdout.as_mut());
    make_nonblocking(child.stderr.as_mut());

    loop {
        if cancel.is_cancelled() {
            force_kill(&mut child, cgroup.as_ref());
            return Err(SandboxError::Cancelled);
        }

        if Instant::now() >= deadline {
            force_kill(&mut child, cgroup.as_ref());
            let wall = start.elapsed();
            return Err(SandboxError::Killed {
                reason: KillReason::Wall,
                wall,
            });
        }

        if !stdout_done {
            if let Some(out) = child.stdout.as_mut() {
                match drain_cap(out, &mut stdout_buf, limits.max_stdout) {
                    Drain::WouldBlock => {}
                    Drain::Eof => stdout_done = true,
                    Drain::Capped => {
                        force_kill(&mut child, cgroup.as_ref());
                        let wall = start.elapsed();
                        return Err(SandboxError::Killed {
                            reason: KillReason::OutputCap,
                            wall,
                        });
                    }
                    Drain::Err(e) => return Err(SandboxError::Io(e)),
                }
            } else {
                stdout_done = true;
            }
        }

        if !stderr_done {
            if let Some(err) = child.stderr.as_mut() {
                match drain_cap(err, &mut stderr_buf, limits.max_stderr) {
                    Drain::WouldBlock => {}
                    Drain::Eof => stderr_done = true,
                    Drain::Capped => {
                        force_kill(&mut child, cgroup.as_ref());
                        let wall = start.elapsed();
                        return Err(SandboxError::Killed {
                            reason: KillReason::OutputCap,
                            wall,
                        });
                    }
                    Drain::Err(e) => return Err(SandboxError::Io(e)),
                }
            } else {
                stderr_done = true;
            }
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                // Final drain
                if let Some(out) = child.stdout.as_mut() {
                    let _ = out.read_to_end(&mut stdout_buf);
                }
                if let Some(err) = child.stderr.as_mut() {
                    let _ = err.read_to_end(&mut stderr_buf);
                }
                // Detect OOM via signal if possible
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    if status.signal() == Some(libc::SIGKILL) {
                        // Could be OOM killer or our kill — if cgroup peak near max, call it OOM.
                        if let Some(ref cg) = cgroup
                            && let Some(peak) = cg.peak_mem()
                            && peak >= limits.mem_bytes.get().saturating_mul(9) / 10
                        {
                            let wall = start.elapsed();
                            return Err(SandboxError::Killed {
                                reason: KillReason::Oom,
                                wall,
                            });
                        }
                    }
                    if status.signal() == Some(libc::SIGXCPU) {
                        let wall = start.elapsed();
                        return Err(SandboxError::Killed {
                            reason: KillReason::CpuTime,
                            wall,
                        });
                    }
                }

                let peak_mem = cgroup.as_ref().and_then(|c| c.peak_mem());
                let wall = start.elapsed();
                return Ok(Output {
                    stdout: stdout_buf,
                    stderr: stderr_buf,
                    end: crate::spec::ProcessEnd::Exited(status),
                    wall,
                    peak_mem,
                });
            }
            Ok(None) => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(SandboxError::Io(e)),
        }
    }
}

/// Kill the process tree: cgroup.kill first (covers forked guests), then the
/// direct child, then wait.
fn force_kill(child: &mut Child, cgroup: Option<&Cgroup>) {
    if let Some(cg) = cgroup {
        let _ = cg.kill_all();
    }
    let _ = child.kill();
    let _ = child.wait();
}

enum Drain {
    WouldBlock,
    Eof,
    Capped,
    Err(std::io::Error),
}

fn drain_cap(r: &mut impl Read, buf: &mut Vec<u8>, cap: usize) -> Drain {
    let mut tmp = [0u8; 8192];
    match r.read(&mut tmp) {
        Ok(0) => Drain::Eof,
        Ok(n) => {
            if buf.len() + n > cap {
                let keep = cap.saturating_sub(buf.len());
                buf.extend_from_slice(&tmp[..keep]);
                Drain::Capped
            } else {
                buf.extend_from_slice(&tmp[..n]);
                Drain::WouldBlock // more may come
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Drain::WouldBlock,
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Drain::WouldBlock,
        Err(e) => Drain::Err(e),
    }
}

fn make_nonblocking(pipe: Option<&mut impl std::os::fd::AsFd>) {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        if let Some(p) = pipe {
            let fd = p.as_fd().as_raw_fd();
            // SAFETY: fcntl on a pipe fd we own.
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                if flags >= 0 {
                    let _ = libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pipe;
    }
}

/// Apply rlimits in the current process (for `pre_exec`).
///
/// Best-effort: individual limit failures are ignored. macOS rejects some
/// `RLIMIT_AS` values with `EINVAL`; Linux may refuse raising hard limits.
///
/// **Memory:** cgroup `memory.max` is the real RAM ceiling. `RLIMIT_AS` caps
/// *virtual address space*, not RSS — compilers map large VA (LLVM arenas,
/// jemalloc). We set AS to **4×** `mem_bytes` as headroom so rustc is not
/// killed for legitimate mappings while still bounding runaway mmap.
/// soft==hard so the guest cannot raise its own budget.
pub(crate) fn apply_rlimits(limits: &Limits) -> Result<(), SandboxError> {
    #[cfg(unix)]
    {
        use rlimit::Resource;
        let ram = limits.mem_bytes.get();
        // VA headroom for toolchains (research: AS ≠ RAM).
        let as_lim = ram.saturating_mul(4).max(ram);
        let _ = Resource::AS.set(as_lim, as_lim);
        let cpu = limits.cpu_secs.get();
        let _ = Resource::CPU.set(cpu, cpu);
        let fsize = limits.fsize_bytes.get();
        let _ = Resource::FSIZE.set(fsize, fsize);
        let nofile = limits.nofile.get();
        let _ = Resource::NOFILE.set(nofile, nofile);
        // NPROC intentionally omitted — UID-global and multi-tenant footgun;
        // use cgroup pids.max instead.
    }
    #[cfg(not(unix))]
    {
        let _ = limits;
    }
    Ok(())
}

/// Shared setup: pipes, clear inherit.
pub(crate) fn base_command(program: &std::path::Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    cmd
}

/// Write a seatbelt / debug helper — unused placeholder for status fd.
#[allow(dead_code)]
pub(crate) fn write_all(w: &mut impl Write, data: &[u8]) -> Result<(), SandboxError> {
    w.write_all(data).map_err(SandboxError::Io)
}
