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

impl PersistentProcess {
    pub(super) fn spawn(command: &SupervisedCommand) -> Result<Self, NativeRunnerError> {
        command
            .limits()
            .validate_supported()
            .map_err(NativeRunnerError::Process)?;
        let workspace_baseline = match command.limits().workspace_limit() {
            Some(_) => workspace_size(command.workspace()).map_err(NativeRunnerError::Process)?,
            None => 0,
        };
        command
            .verify_executable()
            .map_err(NativeRunnerError::Process)?;
        let executable = ExecutableLease::prepare(command).map_err(NativeRunnerError::Process)?;
        let mut process = authority_command(command, executable.path());
        let mut child = process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| NativeRunnerError::Process(ProcessError::SpawnFailure))?;
        let Some(stdin) = child.stdin.take() else {
            let _ = terminate_process_tree(&mut child);
            return Err(NativeRunnerError::Process(ProcessError::SpawnFailure));
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = terminate_process_tree(&mut child);
            return Err(NativeRunnerError::Process(ProcessError::SpawnFailure));
        };
        let Some(child_stderr) = child.stderr.take() else {
            let _ = terminate_process_tree(&mut child);
            return Err(NativeRunnerError::Process(ProcessError::SpawnFailure));
        };
        let (sender, responses) = mpsc::sync_channel(1);
        let stdout_observed = Arc::new(AtomicUsize::new(0));
        let stdout_reader = match spawn_stdout_reader(
            stdout,
            sender,
            Arc::clone(&stdout_observed),
            command.limits().stdout(),
            command.limits().output_bytes(),
        ) {
            Ok(reader) => reader,
            Err(error) => {
                let _ = terminate_process_tree(&mut child);
                return Err(error);
            }
        };
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let stderr_observed = Arc::new(AtomicUsize::new(0));
        let stderr_exceeded = Arc::new(AtomicBool::new(false));
        let stderr_reader = match spawn_stderr_reader(
            child_stderr,
            Arc::clone(&stderr),
            Arc::clone(&stderr_observed),
            Arc::clone(&stderr_exceeded),
            command.limits().stderr(),
        ) {
            Ok(reader) => reader,
            Err(error) => {
                let _ = terminate_process_tree(&mut child);
                return Err(error);
            }
        };
        Ok(Self {
            child,
            _executable: executable,
            stdin,
            responses,
            stderr,
            stderr_observed,
            stderr_exceeded,
            stdout_observed,
            stdout_limit: command.limits().stdout(),
            output_limit: command.limits().output_bytes(),
            wall_time: command.limits().wall_time(),
            workspace: command.workspace().to_owned(),
            workspace_baseline,
            workspace_limit: command.limits().workspace_limit(),
            payload_offset: 0,
            _stdout_reader: stdout_reader,
            _stderr_reader: stderr_reader,
        })
    }

    pub(super) fn send(&mut self, frame: &SessionFrame) -> Result<(), NativeRunnerError> {
        self.send_bytes(&frame.encode())
    }

    pub(super) fn send_bytes(&mut self, bytes: &[u8]) -> Result<(), NativeRunnerError> {
        self.stdin
            .write_all(bytes)
            .map_err(|_| NativeRunnerError::Process(ProcessError::Io))?;
        self.stdin
            .flush()
            .map_err(|_| NativeRunnerError::Process(ProcessError::Io))?;
        Ok(())
    }

    pub(super) fn receive(
        &mut self,
        cancellation: &Cancellation,
        response_payload: bool,
    ) -> Result<(Vec<u8>, Option<NativeEnvelope>), NativeRunnerError> {
        let deadline = Instant::now()
            .checked_add(self.command_limits_wall_time())
            .ok_or(NativeRunnerError::Process(ProcessError::Deadline))?;
        loop {
            if cancellation.is_cancelled() {
                self.terminate().map_err(NativeRunnerError::Process)?;
                return Err(NativeRunnerError::Process(ProcessError::Cancelled));
            }
            if self.stderr_exceeded.load(Ordering::Acquire)
                || output_exceeds_limit(
                    self.stdout_observed.load(Ordering::Acquire),
                    self.stderr_observed.load(Ordering::Acquire),
                    self.output_limit,
                )
            {
                self.terminate().map_err(NativeRunnerError::Process)?;
                return Err(NativeRunnerError::Process(ProcessError::OutputLimit));
            }
            if let Some(limit) = self.workspace_limit {
                let current =
                    workspace_size(&self.workspace).map_err(NativeRunnerError::Process)?;
                if workspace_growth_exceeds(current, self.workspace_baseline, limit) {
                    self.terminate().map_err(NativeRunnerError::Process)?;
                    return Err(NativeRunnerError::Process(ProcessError::WorkspaceLimit));
                }
            }
            match self.responses.recv_timeout(Duration::from_millis(2)) {
                Ok(Ok(bytes)) => {
                    if self.stderr_exceeded.load(Ordering::Acquire)
                        || bytes.len() > self.stdout_limit
                        || output_exceeds_limit(
                            bytes.len(),
                            self.stderr_observed.load(Ordering::Acquire),
                            self.output_limit,
                        )
                    {
                        self.terminate().map_err(NativeRunnerError::Process)?;
                        return Err(NativeRunnerError::Process(ProcessError::OutputLimit));
                    }
                    let payload = if response_payload && bytes.get(5) == Some(&5) {
                        Some(self.receive_payload(cancellation, deadline)?)
                    } else {
                        None
                    };
                    return Ok((bytes, payload));
                }
                Ok(Err(error)) => {
                    let _ = self.terminate();
                    return Err(NativeRunnerError::Process(error));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = self.terminate();
                    return Err(NativeRunnerError::Unavailable);
                }
                Err(RecvTimeoutError::Timeout) => {
                    if Instant::now() >= deadline {
                        self.terminate().map_err(NativeRunnerError::Process)?;
                        return Err(NativeRunnerError::Process(ProcessError::Deadline));
                    }
                    if self
                        .child
                        .try_wait()
                        .map_err(|_| NativeRunnerError::Process(ProcessError::Io))?
                        .is_some()
                    {
                        let _ = self.terminate();
                        return Err(NativeRunnerError::Unavailable);
                    }
                }
            }
        }
    }

    fn command_limits_wall_time(&self) -> Duration {
        self.wall_time
    }

    fn receive_payload(
        &mut self,
        cancellation: &Cancellation,
        deadline: Instant,
    ) -> Result<NativeEnvelope, NativeRunnerError> {
        loop {
            if cancellation.is_cancelled() {
                self.terminate().map_err(NativeRunnerError::Process)?;
                return Err(NativeRunnerError::Process(ProcessError::Cancelled));
            }
            if self.stderr_exceeded.load(Ordering::Acquire)
                || output_exceeds_limit(
                    self.stdout_observed.load(Ordering::Acquire),
                    self.stderr_observed.load(Ordering::Acquire),
                    self.output_limit,
                )
            {
                self.terminate().map_err(NativeRunnerError::Process)?;
                return Err(NativeRunnerError::Process(ProcessError::OutputLimit));
            }
            if let Some(limit) = self.workspace_limit {
                let current =
                    workspace_size(&self.workspace).map_err(NativeRunnerError::Process)?;
                if workspace_growth_exceeds(current, self.workspace_baseline, limit) {
                    self.terminate().map_err(NativeRunnerError::Process)?;
                    return Err(NativeRunnerError::Process(ProcessError::WorkspaceLimit));
                }
            }
            if let Some(payload) = self.decode_stderr_payload()? {
                return Ok(payload);
            }
            if Instant::now() >= deadline {
                self.terminate().map_err(NativeRunnerError::Process)?;
                return Err(NativeRunnerError::Process(ProcessError::Deadline));
            }
            if self
                .child
                .try_wait()
                .map_err(|_| NativeRunnerError::Process(ProcessError::Io))?
                .is_some()
            {
                let _ = self.terminate();
                return Err(NativeRunnerError::Unavailable);
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn decode_stderr_payload(&mut self) -> Result<Option<NativeEnvelope>, NativeRunnerError> {
        let stderr = match self.stderr.lock() {
            Ok(stderr) => stderr,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(candidate) = stderr.get(self.payload_offset..) else {
            return Ok(None);
        };
        if candidate.is_empty() {
            return Ok(None);
        }
        match NativeEnvelope::decode(candidate) {
            Ok(payload) => {
                self.payload_offset = stderr.len();
                Ok(Some(payload))
            }
            Err(NativeProtocolError::Truncated) => Ok(None),
            Err(_) => {
                drop(stderr);
                self.terminate().map_err(NativeRunnerError::Process)?;
                Err(NativeRunnerError::Protocol)
            }
        }
    }

    pub(super) fn take_stderr(&mut self) -> Vec<u8> {
        self.payload_offset = 0;
        match self.stderr.lock() {
            Ok(mut bytes) => std::mem::take(&mut *bytes),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        }
    }

    pub(super) fn terminate(&mut self) -> Result<(), ProcessError> {
        terminate_process_tree(&mut self.child)
    }
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
