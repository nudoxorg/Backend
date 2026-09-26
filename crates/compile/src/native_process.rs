//! Persistent native process I/O and bounded frame readers.
use super::{NativeEnvelope, NativeProtocolError, NativeRunnerError};
use crate::process::ExecutableLease;
use crate::supervisor::{authority_command, terminate_process_tree, workspace_size};
use crate::{
    Cancellation, MAX_FRAME_BYTES, PROTOCOL_VERSION, ProcessError, SessionFrame, SupervisedCommand,
};
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[path = "native_process/run.rs"]
mod run;

pub(super) struct PersistentProcess {
    child: Child,
    _executable: ExecutableLease,
    stdin: ChildStdin,
    responses: Receiver<Result<Vec<u8>, ProcessError>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_observed: Arc<AtomicUsize>,
    stderr_exceeded: Arc<AtomicBool>,
    stdout_observed: Arc<AtomicUsize>,
    stdout_limit: usize,
    output_limit: usize,
    wall_time: Duration,
    workspace: PathBuf,
    workspace_baseline: usize,
    workspace_limit: Option<usize>,
    payload_offset: usize,
    _stdout_reader: JoinHandle<()>,
    _stderr_reader: JoinHandle<()>,
}

impl Drop for PersistentProcess {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

fn spawn_stdout_reader(
    mut stdout: ChildStdout,
    sender: SyncSender<Result<Vec<u8>, ProcessError>>,
    observed: Arc<AtomicUsize>,
    stdout_limit: usize,
    output_limit: usize,
) -> Result<JoinHandle<()>, NativeRunnerError> {
    thread::Builder::new()
        .name("backend-compile-native-stdout".into())
        .spawn(move || {
            loop {
                let mut header = [0_u8; 6];
                if stdout.read_exact(&mut header).is_err() {
                    let _ = sender.send(Err(ProcessError::Io));
                    return;
                }
                let frame_len = match frame_length(header) {
                    Ok(length) => length,
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        return;
                    }
                };
                if frame_len > stdout_limit || frame_len > output_limit {
                    let _ = sender.send(Err(ProcessError::OutputLimit));
                    return;
                }
                let Some(payload_len) = frame_len.checked_sub(header.len()) else {
                    let _ = sender.send(Err(ProcessError::Protocol));
                    return;
                };
                let mut bytes = Vec::with_capacity(frame_len);
                bytes.extend_from_slice(&header);
                let mut payload = vec![0_u8; payload_len];
                if stdout.read_exact(&mut payload).is_err() {
                    let _ = sender.send(Err(ProcessError::Io));
                    return;
                }
                bytes.extend_from_slice(&payload);
                let Some(total) = observe_bytes(&observed, frame_len) else {
                    let _ = sender.send(Err(ProcessError::OutputLimit));
                    return;
                };
                if total > stdout_limit || total > output_limit {
                    let _ = sender.send(Err(ProcessError::OutputLimit));
                    return;
                }
                if sender.send(Ok(bytes)).is_err() {
                    return;
                }
            }
        })
        .map_err(|_| NativeRunnerError::Process(ProcessError::Io))
}

fn spawn_stderr_reader(
    mut stderr: ChildStderr,
    bytes: Arc<Mutex<Vec<u8>>>,
    observed: Arc<AtomicUsize>,
    exceeded: Arc<AtomicBool>,
    limit: usize,
) -> Result<JoinHandle<()>, NativeRunnerError> {
    thread::Builder::new()
        .name("backend-compile-native-stderr".into())
        .spawn(move || {
            let mut chunk = [0_u8; 8192];
            loop {
                let Ok(read) = stderr.read(&mut chunk) else {
                    exceeded.store(true, Ordering::Release);
                    return;
                };
                if read == 0 {
                    return;
                }
                let Some(total) = observe_bytes(&observed, read) else {
                    exceeded.store(true, Ordering::Release);
                    continue;
                };
                if total > limit {
                    exceeded.store(true, Ordering::Release);
                }
                match bytes.lock() {
                    Ok(mut retained) => append_suffix(&mut retained, &chunk[..read], limit),
                    Err(poisoned) => {
                        append_suffix(&mut poisoned.into_inner(), &chunk[..read], limit);
                    }
                }
            }
        })
        .map_err(|_| NativeRunnerError::Process(ProcessError::Io))
}

/// Retains only the most recent diagnostic bytes. Native helpers are allowed
/// to write arbitrary diagnostics, so a bounded stderr policy must not turn a
/// failed helper into an unbounded allocator. Keeping the suffix is useful in
/// practice because the final diagnostic usually identifies the failure.
fn append_suffix(retained: &mut Vec<u8>, bytes: &[u8], limit: usize) {
    if limit == 0 {
        retained.clear();
        return;
    }
    if bytes.len() >= limit {
        retained.clear();
        retained.extend_from_slice(&bytes[bytes.len() - limit..]);
        return;
    }
    let overflow = retained
        .len()
        .saturating_add(bytes.len())
        .saturating_sub(limit);
    if overflow != 0 {
        retained.drain(..overflow);
    }
    retained.extend_from_slice(bytes);
}

fn output_exceeds_limit(stdout: usize, stderr: usize, limit: usize) -> bool {
    stdout > limit || stderr > limit || stdout > limit - stderr.min(limit)
}

fn workspace_growth_exceeds(current: usize, baseline: usize, limit: usize) -> bool {
    current > baseline && current - baseline > limit
}

fn observe_bytes(observed: &AtomicUsize, amount: usize) -> Option<usize> {
    observed
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(amount)
        })
        .ok()
        .and_then(|previous| previous.checked_add(amount))
}

fn frame_length(header: [u8; 6]) -> Result<usize, ProcessError> {
    if header[..3] != *b"BCF" {
        return Err(ProcessError::Protocol);
    }
    let version = u16::from_be_bytes([header[3], header[4]]);
    if version != PROTOCOL_VERSION {
        return Err(ProcessError::Protocol);
    }
    match header[5] {
        1 => Ok(6 + 32),
        2 | 5 => Ok(MAX_FRAME_BYTES),
        3 => Ok(6 + 8),
        4 => Ok(6),
        _ => Err(ProcessError::Protocol),
    }
}
