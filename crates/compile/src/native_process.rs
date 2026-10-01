//! Persistent native process I/O and bounded frame readers.
use super::{NativeEnvelope, NativeProtocolError, NativeRunnerError};
use crate::process::ExecutableLease;
use crate::supervisor::{
    OwnedChild, ProcessGroupRetirement, authority_command, child_exited_without_reaping,
    reserve_child_reaper, set_nonblocking_pipe, terminate_child_with_group, workspace_size,
};
use crate::{
    Cancellation, MAX_FRAME_BYTES, PROTOCOL_VERSION, ProcessError, SessionFrame, SupervisedCommand,
};
use std::{
    io::{self, Read, Write},
    path::PathBuf,
    process::{ChildStderr, ChildStdin, ChildStdout, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[path = "native_process/run.rs"]
mod run;

pub(super) struct PersistentProcess {
    child: OwnedChild,
    group_retirement: ProcessGroupRetirement,
    stop_workers: Arc<AtomicBool>,
    _executable: ExecutableLease,
    stdin: ChildStdin,
    responses: Receiver<Result<Vec<u8>, ProcessError>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_observed: Arc<AtomicUsize>,
    stderr_exceeded: Arc<AtomicBool>,
    stderr_fault: Arc<AtomicU8>,
    stdout_observed: Arc<AtomicUsize>,
    stdout_fault: Arc<AtomicU8>,
    output_observed: Arc<AtomicUsize>,
    stdout_drain: Arc<(Mutex<NativeDrainProbe>, std::sync::Condvar)>,
    stderr_drain: Arc<(Mutex<StderrDrainProbe>, std::sync::Condvar)>,
    #[cfg(test)]
    stdout_reader_gate: Arc<(Mutex<TestNativeStdoutGate>, std::sync::Condvar)>,
    stdout_limit: usize,
    output_limit: usize,
    workspace: PathBuf,
    workspace_baseline: usize,
    workspace_limit: Option<usize>,
    payload_offset: usize,
    stdout_reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct StderrDrainProbe {
    requested: u64,
    completed: u64,
}

#[derive(Default)]
struct NativeDrainProbe {
    requested: u64,
    completed: u64,
}

#[cfg(test)]
#[derive(Default)]
struct TestNativeStdoutGate {
    frames_seen: u64,
    pause_after_frame: Option<u64>,
    paused: bool,
    parked: bool,
}

impl Drop for PersistentProcess {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

fn spawn_stdout_reader(
    mut stdout: ChildStdout,
    sender: SyncSender<Result<Vec<u8>, ProcessError>>,
    group_retirement: ProcessGroupRetirement,
    stop: Arc<AtomicBool>,
    fault: Arc<AtomicU8>,
    observed: Arc<AtomicUsize>,
    output_observed: Arc<AtomicUsize>,
    stdout_drain: Arc<(Mutex<NativeDrainProbe>, std::sync::Condvar)>,
    #[cfg(test)] test_gate: Arc<(Mutex<TestNativeStdoutGate>, std::sync::Condvar)>,
    stdout_limit: usize,
    output_limit: usize,
) -> Result<JoinHandle<()>, NativeRunnerError> {
    thread::Builder::new()
        .name("backend-compile-native-stdout".into())
        .spawn(move || {
            loop {
                #[cfg(test)]
                await_native_stdout_reader(&test_gate, &stop);
                if stop.load(Ordering::Acquire) {
                    return;
                }
                let mut header = [0_u8; 6];
                if let Err(error) =
                    read_exact_stoppable(&mut stdout, &mut header, &stop, Some(&stdout_drain))
                {
                    if !stop.load(Ordering::Acquire) {
                        retire_native_pipe(error, &fault, &group_retirement, &sender);
                    }
                    return;
                }
                let frame_len = match frame_length(header) {
                    Ok(length) => length,
                    Err(error) => {
                        retire_native_pipe(error, &fault, &group_retirement, &sender);
                        return;
                    }
                };
                if frame_len > stdout_limit || frame_len > output_limit {
                    retire_native_pipe(
                        ProcessError::OutputLimit,
                        &fault,
                        &group_retirement,
                        &sender,
                    );
                    return;
                }
                let Some(payload_len) = frame_len.checked_sub(header.len()) else {
                    retire_native_pipe(ProcessError::Protocol, &fault, &group_retirement, &sender);
                    return;
                };
                let mut bytes = Vec::with_capacity(frame_len);
                bytes.extend_from_slice(&header);
                let mut payload = vec![0_u8; payload_len];
                if let Err(error) = read_exact_stoppable(&mut stdout, &mut payload, &stop, None) {
                    if !stop.load(Ordering::Acquire) {
                        retire_native_pipe(error, &fault, &group_retirement, &sender);
                    }
                    return;
                }
                bytes.extend_from_slice(&payload);
                let Some(total) = observe_bytes(&observed, frame_len) else {
                    retire_native_pipe(
                        ProcessError::OutputLimit,
                        &fault,
                        &group_retirement,
                        &sender,
                    );
                    return;
                };
                if total > stdout_limit
                    || reserve_output_bytes(&output_observed, frame_len, output_limit).is_err()
                {
                    retire_native_pipe(
                        ProcessError::OutputLimit,
                        &fault,
                        &group_retirement,
                        &sender,
                    );
                    return;
                }
                match sender.try_send(Ok(bytes)) {
                    Ok(()) => {
                        #[cfg(test)]
                        note_native_stdout_frame(&test_gate, &stop);
                    }
                    Err(mpsc::TrySendError::Full(_)) => {
                        retire_native_pipe(
                            ProcessError::Protocol,
                            &fault,
                            &group_retirement,
                            &sender,
                        );
                        return;
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => return,
                }
            }
        })
        .map_err(|_| NativeRunnerError::Process(ProcessError::Io))
}

fn spawn_stderr_reader(
    mut stderr: ChildStderr,
    bytes: Arc<Mutex<Vec<u8>>>,
    observed: Arc<AtomicUsize>,
    output_observed: Arc<AtomicUsize>,
    exceeded: Arc<AtomicBool>,
    fault: Arc<AtomicU8>,
    limit: usize,
    output_limit: usize,
    drain_probe: Arc<(Mutex<StderrDrainProbe>, std::sync::Condvar)>,
    group_retirement: ProcessGroupRetirement,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, NativeRunnerError> {
    thread::Builder::new()
        .name("backend-compile-native-stderr".into())
        .spawn(move || {
            let mut chunk = [0_u8; 8192];
            loop {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                let (drain_state, drain_changed) = &*drain_probe;
                let mut drain_state = drain_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let read = match stderr.read(&mut chunk) {
                    Ok(read) => read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        acknowledge_stderr_drain(&mut drain_state, drain_changed);
                        drop(drain_state);
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(_) => {
                        fault.store(1, Ordering::Release);
                        acknowledge_stderr_drain(&mut drain_state, drain_changed);
                        drop(drain_state);
                        let _ = group_retirement.retire();
                        return;
                    }
                };
                if read == 0 {
                    if !stop.load(Ordering::Acquire) {
                        fault.store(1, Ordering::Release);
                        acknowledge_stderr_drain(&mut drain_state, drain_changed);
                        drop(drain_state);
                        let _ = group_retirement.retire();
                    }
                    return;
                }
                drop(drain_state);
                let Some(total) = observe_bytes(&observed, read) else {
                    exceeded.store(true, Ordering::Release);
                    fault.store(2, Ordering::Release);
                    let _ = group_retirement.retire();
                    return;
                };
                if total > limit
                    || reserve_output_bytes(&output_observed, read, output_limit).is_err()
                {
                    exceeded.store(true, Ordering::Release);
                    fault.store(2, Ordering::Release);
                    let _ = group_retirement.retire();
                    return;
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

fn acknowledge_drain(state: &mut NativeDrainProbe, changed: &std::sync::Condvar) {
    if state.completed < state.requested {
        state.completed = state.requested;
        changed.notify_all();
    }
}

fn acknowledge_stderr_drain(state: &mut StderrDrainProbe, changed: &std::sync::Condvar) {
    if state.completed < state.requested {
        state.completed = state.requested;
        changed.notify_all();
    }
}

fn reserve_output_bytes(observed: &AtomicUsize, amount: usize, limit: usize) -> Result<usize, ()> {
    let mut current = observed.load(Ordering::Acquire);
    loop {
        let Some(next) = current.checked_add(amount) else {
            return Err(());
        };
        if next > limit {
            return Err(());
        }
        match observed.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Ok(next),
            Err(actual) => current = actual,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NativeDrainProbe, read_exact_stoppable, reserve_output_bytes};
    use std::io::{self, Read};
    use std::sync::{
        Arc, Barrier, Mutex, TryLockError,
        atomic::{AtomicBool, AtomicUsize},
        mpsc,
    };
    use std::thread;
    use std::time::Duration;

    #[test]
    fn concurrent_native_streams_cannot_overbook_combined_output() {
        let observed = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(Barrier::new(3));
        let stdout_total = Arc::clone(&observed);
        let stdout_gate = Arc::clone(&gate);
        let stdout = thread::spawn(move || {
            stdout_gate.wait();
            reserve_output_bytes(&stdout_total, 60, 100)
        });
        let stderr_total = Arc::clone(&observed);
        let stderr_gate = Arc::clone(&gate);
        let stderr = thread::spawn(move || {
            stderr_gate.wait();
            reserve_output_bytes(&stderr_total, 50, 100)
        });
        gate.wait();

        let stdout = stdout.join().expect("stdout accounting thread panicked");
        let stderr = stderr.join().expect("stderr accounting thread panicked");
        assert_ne!(stdout.is_ok(), stderr.is_ok());
        assert!(observed.load(std::sync::atomic::Ordering::Acquire) <= 100);
    }

    #[test]
    fn native_drain_generation_cannot_reuse_a_preceding_would_block() {
        struct WakeReaders {
            first: Option<mpsc::Sender<()>>,
            second: Option<mpsc::Sender<()>>,
        }

        impl Drop for WakeReaders {
            fn drop(&mut self) {
                if let Some(sender) = self.first.take() {
                    let _ = sender.send(());
                }
                if let Some(sender) = self.second.take() {
                    let _ = sender.send(());
                }
            }
        }

        impl WakeReaders {
            fn release_first(&mut self) {
                if let Some(sender) = self.first.take() {
                    sender.send(()).expect("first read is waiting");
                }
            }

            fn release_second(&mut self) {
                if let Some(sender) = self.second.take() {
                    sender.send(()).expect("second read is waiting");
                }
            }
        }

        struct PausedWouldBlockReader {
            entered: Option<mpsc::Sender<()>>,
            release: mpsc::Receiver<()>,
            calls: usize,
        }

        impl Read for PausedWouldBlockReader {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                let call = self.calls;
                self.calls = self.calls.saturating_add(1);
                if call == 0 {
                    self.entered
                        .take()
                        .expect("first read reports once")
                        .send(())
                        .expect("test is still waiting");
                    self.release
                        .recv()
                        .expect("test releases the observed empty read");
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                if call == 2 {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                output[0] = b'x';
                Ok(1)
            }
        }

        let probe = Arc::new((
            Mutex::new(NativeDrainProbe::default()),
            std::sync::Condvar::new(),
        ));
        let (read_entered_tx, read_entered_rx) = mpsc::channel();
        let (read_release_tx, read_release_rx) = mpsc::channel();
        let (read_done_tx, read_done_rx) = mpsc::channel();
        let (start_second_tx, start_second_rx) = mpsc::channel();
        let (second_done_tx, second_done_rx) = mpsc::channel();
        let mut wake_readers = WakeReaders {
            first: Some(read_release_tx),
            second: Some(start_second_tx),
        };
        let worker_probe = Arc::clone(&probe);
        let worker = thread::spawn(move || {
            let mut reader = PausedWouldBlockReader {
                entered: Some(read_entered_tx),
                release: read_release_rx,
                calls: 0,
            };
            let mut header = [0_u8; 1];
            let result = read_exact_stoppable(
                &mut reader,
                &mut header,
                &AtomicBool::new(false),
                Some(&worker_probe),
            );
            read_done_tx
                .send(result)
                .expect("test receives read result");
            start_second_rx
                .recv()
                .expect("test starts the next drain observation");
            let mut next_header = [0_u8; 1];
            let result = read_exact_stoppable(
                &mut reader,
                &mut next_header,
                &AtomicBool::new(false),
                Some(&worker_probe),
            );
            second_done_tx
                .send(result)
                .expect("test receives second read result");
        });

        read_entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("reader reached the controlled WouldBlock");
        let (request_attempt_tx, request_attempt_rx) = mpsc::channel();
        let (request_done_tx, request_done_rx) = mpsc::channel();
        let owner_probe = Arc::clone(&probe);
        let owner = thread::spawn(move || {
            let (state, changed) = &*owner_probe;
            let mut state = match state.try_lock() {
                Ok(state) => {
                    request_attempt_tx
                        .send(false)
                        .expect("test receives request lock outcome");
                    state
                }
                Err(TryLockError::WouldBlock) => {
                    request_attempt_tx
                        .send(true)
                        .expect("test receives request lock outcome");
                    state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                }
                Err(TryLockError::Poisoned(poisoned)) => {
                    request_attempt_tx
                        .send(false)
                        .expect("test receives request lock outcome");
                    poisoned.into_inner()
                }
            };
            state.requested = state.requested.saturating_add(1);
            let requested = state.requested;
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while state.completed < requested {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let (next, timeout) = changed
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state = next;
                if timeout.timed_out() && state.completed < requested {
                    break;
                }
            }
            request_done_tx
                .send(state.completed >= requested)
                .expect("test receives completed drain generation");
        });
        assert!(
            request_attempt_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("request attempts to acquire the probe lock"),
            "the nonblocking read and drain acknowledgement must share the request lock"
        );
        wake_readers.release_first();
        read_done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("reader completes after the controlled WouldBlock")
            .expect("one byte completes the header");
        // The old empty-read acknowledgement predates this request. The owner must
        // wait for a fresh drain observation rather than reusing that generation.
        let state = probe
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(state.requested, 1);
        assert_eq!(state.completed, 0);
        drop(state);
        let completion_before_fresh_observation = request_done_rx.try_recv().ok();
        wake_readers.release_second();
        let second_read_succeeded = second_done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("reader performs a fresh WouldBlock observation")
            .is_ok();
        let completed_after_fresh_observation = completion_before_fresh_observation
            .or_else(|| request_done_rx.recv_timeout(Duration::from_secs(5)).ok())
            .unwrap_or(false);
        worker.join().expect("stdout test reader exits");
        owner
            .join()
            .expect("drain owner exits after a fresh acknowledgement");
        assert!(completion_before_fresh_observation.is_none());
        assert!(second_read_succeeded);
        assert!(completed_after_fresh_observation);
    }
}

fn read_exact_stoppable(
    reader: &mut impl Read,
    mut output: &mut [u8],
    stop: &AtomicBool,
    idle_probe: Option<&Arc<(Mutex<NativeDrainProbe>, std::sync::Condvar)>>,
) -> Result<(), ProcessError> {
    let initial_len = output.len();
    while !output.is_empty() {
        if stop.load(Ordering::Acquire) {
            return Err(ProcessError::Cancelled);
        }
        let read = if let Some(idle_probe) = idle_probe {
            let (state, changed) = &**idle_probe;
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let read = reader.read(output);
            if output.len() == initial_len
                && let Err(error) = &read
                && error.kind() == io::ErrorKind::WouldBlock
            {
                acknowledge_drain(&mut state, changed);
            }
            read
        } else {
            reader.read(output)
        };
        match read {
            Ok(0) => return Err(ProcessError::Protocol),
            Ok(read) => output = &mut output[read..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(_) => return Err(ProcessError::Io),
        }
    }
    Ok(())
}

#[cfg(test)]
fn await_native_stdout_reader(
    gate: &(Mutex<TestNativeStdoutGate>, std::sync::Condvar),
    stop: &AtomicBool,
) {
    let (state, changed) = gate;
    let mut state = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if state.paused {
        state.parked = true;
        changed.notify_all();
        while state.paused && !stop.load(Ordering::Acquire) {
            let (next, _) = changed
                .wait_timeout(state, Duration::from_millis(2))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
        }
        state.parked = false;
        changed.notify_all();
    }
}

#[cfg(test)]
fn note_native_stdout_frame(
    gate: &(Mutex<TestNativeStdoutGate>, std::sync::Condvar),
    stop: &AtomicBool,
) {
    let (state, changed) = gate;
    let mut state = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.frames_seen = state.frames_seen.saturating_add(1);
    if state.pause_after_frame == Some(state.frames_seen) {
        state.parked = true;
        changed.notify_all();
        while state.paused && !stop.load(Ordering::Acquire) {
            let (next, _) = changed
                .wait_timeout(state, Duration::from_millis(2))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
        }
        state.parked = false;
        changed.notify_all();
    }
}

#[cfg(test)]
fn release_native_stdout_reader_for_test(gate: &(Mutex<TestNativeStdoutGate>, std::sync::Condvar)) {
    let (state, changed) = gate;
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .paused = false;
    changed.notify_all();
}

fn retire_native_pipe(
    error: ProcessError,
    fault: &AtomicU8,
    group_retirement: &ProcessGroupRetirement,
    sender: &SyncSender<Result<Vec<u8>, ProcessError>>,
) {
    let code = match error {
        ProcessError::OutputLimit => 2,
        ProcessError::Protocol => 3,
        _ => 1,
    };
    let _ = fault.compare_exchange(0, code, Ordering::AcqRel, Ordering::Acquire);
    let _ = sender.try_send(Err(error));
    let _ = group_retirement.retire();
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
