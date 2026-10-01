//! Cold process supervision with bounded file output and explicit reaping.

use crate::{
    Cancellation, CancellationObserver, ProcessError,
    process::{
        ExecutableLease, ProcessReceipt, ProcessStdin, ProcessTerminal, SupervisedCommand, receipt,
    },
};
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions, remove_file, symlink_metadata},
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const CHILD_REAP_TIMEOUT: Duration = Duration::from_millis(500);
// Includes every spawned child with unreleased Child custody, whether still
// caller-owned or queued for shared reaping. Admission fails before spawn.
const CHILD_REAPER_CAPACITY: usize = 32;

struct ChildReaperState {
    reserved: AtomicUsize,
    capacity: usize,
    jobs: Mutex<VecDeque<ChildReapJob>>,
    changed: Condvar,
    worker_started: Mutex<bool>,
    #[cfg(test)]
    shutdown: AtomicBool,
}

pub(crate) struct ChildReaperPermit {
    state: Arc<ChildReaperState>,
}

pub(crate) struct ChildReapTicket {
    completed: Arc<AtomicBool>,
}

/// Affine ownership of a spawned child, its reserved reaper slot, and the
/// state of its eventual reap. Keeping these parts together prevents callers
/// from dropping the `Child` while retaining a stale permit or claiming that
/// delegated custody has already been reaped.
pub(crate) struct OwnedChild(OwnedChildState);

enum OwnedChildState {
    Owned {
        child: Child,
        permit: ChildReaperPermit,
    },
    Delegated(ChildReapTicket),
    Reaped,
}

struct ChildReapJob {
    child: Child,
    _permit: ChildReaperPermit,
    completed: Arc<AtomicBool>,
}

static CHILD_REAPER: OnceLock<Mutex<Option<Arc<ChildReaperState>>>> = OnceLock::new();

#[cfg(test)]
static GROUP_RETIREMENT_ATTEMPTS: std::sync::Mutex<Vec<(u32, usize)>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(crate) fn group_retirement_attempts(process_id: u32) -> usize {
    GROUP_RETIREMENT_ATTEMPTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find_map(|(recorded_pid, attempts)| (*recorded_pid == process_id).then_some(*attempts))
        .unwrap_or(0)
}

/// A process supervisor bound to one validated command.
#[derive(Clone, Debug)]
pub struct ProcessSupervisor {
    command: SupervisedCommand,
}

impl ProcessSupervisor {
    /// Creates a supervisor for a validated command.
    #[must_use]
    pub const fn new(command: SupervisedCommand) -> Self {
        Self { command }
    }

    /// Returns the validated command owned by this supervisor.
    #[must_use]
    pub const fn command(&self) -> &SupervisedCommand {
        &self.command
    }

    /// Starts the validated command and returns its running lifecycle state.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when cancellation, a requested limit, identity
    /// verification, temporary I/O, process startup, or bounded child-custody
    /// admission fails. At most 32 children may be under this supervisor's
    /// custody at once; admission is refused before spawning when full.
    pub fn start(&self) -> Result<RunningProcess, ProcessError> {
        let (cancellation, _handle) = Cancellation::new();
        self.start_with_cancellation(&cancellation)
    }

    /// Starts the validated command after observing caller cancellation.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when cancellation, a requested limit, identity
    /// verification, temporary I/O, or process startup fails.
    pub fn start_with_cancellation(
        &self,
        cancellation: &Cancellation,
    ) -> Result<RunningProcess, ProcessError> {
        self.start_with_observer(cancellation)
    }

    /// Starts the validated command after observing a borrowed cancellation
    /// source.
    ///
    /// This supports caller-owned flags such as
    /// [`std::sync::atomic::AtomicBool`] without an intermediate cancellation
    /// thread.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when cancellation, a requested limit, identity
    /// verification, temporary I/O, or process startup fails.
    pub fn start_with_observer<C: CancellationObserver + ?Sized>(
        &self,
        cancellation: &C,
    ) -> Result<RunningProcess, ProcessError> {
        self.start_with_observer_optional_deadline(cancellation, None)
    }

    fn start_with_observer_optional_deadline<C: CancellationObserver + ?Sized>(
        &self,
        cancellation: &C,
        deadline: Option<Instant>,
    ) -> Result<RunningProcess, ProcessError> {
        cancellation.checkpoint().map_err(ProcessError::from)?;
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(ProcessError::Deadline);
        }
        self.command.limits().validate_supported()?;
        let workspace_baseline = match self.command.limits().workspace_limit() {
            Some(_) => workspace_size(self.command.workspace())?,
            None => 0,
        };
        let stdin = TempInput::create(self.command.stdin())?;
        let stdout = TempOutput::create("stdout")?;
        let stderr = TempOutput::create("stderr")?;
        self.command.verify_executable()?;
        let executable = ExecutableLease::prepare(&self.command)?;
        cancellation.checkpoint().map_err(ProcessError::from)?;
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(ProcessError::Deadline);
        }
        let reaper_permit = reserve_child_reaper()?;
        let child = OwnedChild::new(
            spawn(
                &self.command,
                executable.path(),
                stdin.as_ref(),
                &stdout.file,
                &stderr.file,
            )?,
            reaper_permit,
        );
        Ok(RunningProcess {
            command: self.command.clone(),
            group_retirement: ProcessGroupRetirement::new(child.id()?),
            child,
            _stdin: stdin,
            _executable: executable,
            stdout,
            stderr,
            workspace_baseline,
            group_retired: false,
        })
    }

    /// Starts a checked persistent byte-stream process for serialized bounded exchanges.
    ///
    /// The command must advertise a protocol with persistent-session support and use closed
    /// startup stdin. Each exchange supplies one bounded request and its exact expected response
    /// extent. Both pipe directions run concurrently under one wall deadline, so a blocked child
    /// stdin write is cancellable just like a blocked response read. Any exchange failure kills
    /// and retires its owned process group before reaping the leader; callers must discard the
    /// session. The containment boundary is the original process group: a descendant that
    /// deliberately changes process groups or creates a new session is outside it.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when command identity, process limits, startup, or pipe setup is
    /// rejected.
    #[cfg(not(all(
        unix,
        not(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        ))
    )))]
    pub(crate) fn start_byte_session(&self) -> Result<RunningByteSession, ProcessError> {
        let _ = self;
        Err(ProcessError::UnsupportedLimit(
            crate::UnsupportedLimit::ProcessGroup,
        ))
    }

    /// Starts a persistent session only where this supervisor can own and retire a process group.
    #[cfg(all(
        unix,
        not(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        ))
    ))]
    pub(crate) fn start_byte_session(&self) -> Result<RunningByteSession, ProcessError> {
        if !self.command.protocol().supports_persistent()
            || self.command.stdin().as_bytes().is_some()
        {
            return Err(ProcessError::Protocol);
        }
        self.command.limits().validate_supported()?;
        let workspace_baseline = match self.command.limits().workspace_limit() {
            Some(_) => workspace_size(self.command.workspace())?,
            None => 0,
        };
        self.command.verify_executable()?;
        let executable = ExecutableLease::prepare(&self.command)?;
        let reaper_permit = reserve_child_reaper()?;
        let mut process = authority_command(&self.command, executable.path());
        let child = process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| ProcessError::SpawnFailure)?;
        let mut child = OwnedChild::new(child, reaper_permit);
        let child_pid = child.id()?;
        let stop_workers = Arc::new(AtomicBool::new(false));
        let group_retirement =
            ProcessGroupRetirement::with_stop_workers(child_pid, Arc::clone(&stop_workers));
        let Some(stdin) = child.child_mut().ok().and_then(|child| child.stdin.take()) else {
            let _ = terminate_child_with_group(&mut child, &group_retirement);
            return Err(ProcessError::SpawnFailure);
        };
        let Some(stdout) = child.child_mut().ok().and_then(|child| child.stdout.take()) else {
            let _ = terminate_child_with_group(&mut child, &group_retirement);
            return Err(ProcessError::SpawnFailure);
        };
        let Some(stderr) = child.child_mut().ok().and_then(|child| child.stderr.take()) else {
            let _ = terminate_child_with_group(&mut child, &group_retirement);
            return Err(ProcessError::SpawnFailure);
        };
        let nonblocking = set_nonblocking_pipe(&stdin)
            .and_then(|()| set_nonblocking_pipe(&stdout))
            .and_then(|()| set_nonblocking_pipe(&stderr));
        if let Err(error) = nonblocking {
            stop_workers.store(true, Ordering::Release);
            let _ = terminate_child_with_group(&mut child, &group_retirement);
            return Err(error);
        }
        let stdout_observed = Arc::new(AtomicUsize::new(0));
        let stderr_observed = Arc::new(AtomicUsize::new(0));
        let output_observed = Arc::new(AtomicUsize::new(0));
        let stderr_exceeded = Arc::new(AtomicBool::new(false));
        let stdout_fault = Arc::new(AtomicU8::new(0));
        let stdout_credit = Arc::new(Mutex::new(SessionStdoutCredit::default()));
        let stdout_credit_changed = Arc::new(Condvar::new());
        #[cfg(test)]
        let stdout_reader_gate =
            Arc::new((Mutex::new(TestStdoutReaderGate::default()), Condvar::new()));
        let stderr_reader = match spawn_session_stderr_reader(
            stderr,
            group_retirement.clone(),
            Arc::clone(&stop_workers),
            Arc::clone(&stderr_observed),
            Arc::clone(&stderr_exceeded),
            Arc::clone(&output_observed),
            Arc::clone(&stdout_fault),
            self.command.limits().stderr(),
            self.command.limits().output_bytes(),
        ) {
            Ok(reader) => reader,
            Err(error) => {
                stop_workers.store(true, Ordering::Release);
                let _ = terminate_child_with_group(&mut child, &group_retirement);
                return Err(error);
            }
        };
        let (stdout_events, stdout_reader) = match spawn_session_stdout_reader(
            stdout,
            group_retirement.clone(),
            Arc::clone(&stop_workers),
            self.command.limits().stdout(),
            self.command.limits().output_bytes(),
            Arc::clone(&stdout_observed),
            Arc::clone(&output_observed),
            Arc::clone(&stdout_credit),
            Arc::clone(&stdout_credit_changed),
            #[cfg(test)]
            Arc::clone(&stdout_reader_gate),
            Arc::clone(&stdout_fault),
        ) {
            Ok(reader) => reader,
            Err(error) => {
                stop_workers.store(true, Ordering::Release);
                let teardown = terminate_child_with_group(&mut child, &group_retirement);
                if teardown.is_ok() {
                    let _ = stderr_reader.join();
                }
                return Err(error);
            }
        };
        let (stdin_sender, stdin_writer) =
            match spawn_session_stdin_writer(stdin, Arc::clone(&stop_workers)) {
                Ok(writer) => writer,
                Err(error) => {
                    stop_workers.store(true, Ordering::Release);
                    drop(stdout_events);
                    let teardown = terminate_child_with_group(&mut child, &group_retirement);
                    if teardown.is_ok() {
                        let _ = stdout_reader.join();
                        let _ = stderr_reader.join();
                    }
                    return Err(error);
                }
            };
        Ok(RunningByteSession {
            command: self.command.clone(),
            child,
            group_retirement,
            stop_workers,
            _executable: executable,
            stdin_sender: Some(stdin_sender),
            stdin_writer: Some(stdin_writer),
            stdout_events: Some(stdout_events),
            stdout_reader: Some(stdout_reader),
            stderr_observed,
            stderr_exceeded,
            stdout_observed,
            output_observed,
            stdout_credit,
            stdout_credit_changed,
            #[cfg(test)]
            stdout_reader_gate,
            stdout_fault,
            workspace_baseline,
            started: Instant::now(),
            exchanges: 0,
            request_bytes: 0,
            stderr_reader: Some(stderr_reader),
            group_retired: false,
        })
    }

    /// Runs the command with a fresh cancellation token.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when startup, execution, cancellation, or
    /// process reaping fails.
    pub fn run(&self) -> Result<ProcessReceipt, ProcessError> {
        let (cancellation, _handle) = Cancellation::new();
        self.run_with_cancellation(&cancellation)
    }

    /// Runs the command while observing caller cancellation.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when startup, cancellation, a configured limit,
    /// or process completion fails.
    pub fn run_with_cancellation(
        &self,
        cancellation: &Cancellation,
    ) -> Result<ProcessReceipt, ProcessError> {
        self.run_with_observer(cancellation)
    }

    /// Runs the command while polling a borrowed cancellation observer.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when startup, cancellation, a configured
    /// limit, or process completion fails.
    pub fn run_with_observer<C: CancellationObserver + ?Sized>(
        &self,
        cancellation: &C,
    ) -> Result<ProcessReceipt, ProcessError> {
        self.start_with_observer(cancellation)?
            .finish_with_observer(cancellation)
    }

    /// Runs the command while polling a borrowed cancellation source and an
    /// absolute caller deadline.
    ///
    /// The effective deadline is the earlier of this deadline and the
    /// command's configured wall-time limit. It is measured across executable
    /// verification, spawn, and process completion.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when startup, cancellation, a configured
    /// limit, or process completion fails.
    pub fn run_with_observer_until<C: CancellationObserver + ?Sized>(
        &self,
        cancellation: &C,
        deadline: Instant,
    ) -> Result<ProcessReceipt, ProcessError> {
        self.start_with_observer_optional_deadline(cancellation, Some(deadline))?
            .finish_with_observer_until(cancellation, deadline)
    }
}

/// One supervised persistent process with an explicitly bounded byte exchange interface.
///
/// The caller owns this affine capability and must retain one per serialized session. Drop
/// retires its owned process group and reaps the leader. A descendant that deliberately changes
/// process groups or creates a new session is outside that boundary; nonblocking pumps still stop
/// without waiting for such a descendant to close inherited pipe descriptors.
pub(crate) struct RunningByteSession {
    command: SupervisedCommand,
    child: OwnedChild,
    group_retirement: ProcessGroupRetirement,
    stop_workers: Arc<AtomicBool>,
    _executable: ExecutableLease,
    stdin_sender: Option<SyncSender<SessionWriteCommand>>,
    stdin_writer: Option<JoinHandle<()>>,
    stdout_events: Option<Receiver<SessionPipeEvent>>,
    stdout_reader: Option<JoinHandle<()>>,
    stderr_observed: Arc<AtomicUsize>,
    stderr_exceeded: Arc<AtomicBool>,
    stdout_observed: Arc<AtomicUsize>,
    output_observed: Arc<AtomicUsize>,
    stdout_credit: Arc<Mutex<SessionStdoutCredit>>,
    stdout_credit_changed: Arc<Condvar>,
    #[cfg(test)]
    stdout_reader_gate: Arc<(Mutex<TestStdoutReaderGate>, Condvar)>,
    stdout_fault: Arc<AtomicU8>,
    workspace_baseline: usize,
    started: Instant,
    exchanges: usize,
    request_bytes: usize,
    stderr_reader: Option<JoinHandle<()>>,
    group_retired: bool,
}

#[derive(Clone, Copy)]
enum LeaderCustodyView {
    Owned,
    Reaped,
    Delegated,
    DelegatedComplete,
}

impl OwnedChild {
    pub(crate) fn new(child: Child, permit: ChildReaperPermit) -> Self {
        Self(OwnedChildState::Owned { child, permit })
    }

    fn view(&mut self) -> LeaderCustodyView {
        match &self.0 {
            OwnedChildState::Owned { .. } => LeaderCustodyView::Owned,
            OwnedChildState::Reaped => LeaderCustodyView::Reaped,
            OwnedChildState::Delegated(ticket) if ticket.is_complete() => {
                self.0 = OwnedChildState::Reaped;
                LeaderCustodyView::DelegatedComplete
            }
            OwnedChildState::Delegated(_) => LeaderCustodyView::Delegated,
        }
    }

    pub(crate) fn id(&self) -> Result<u32, ProcessError> {
        match &self.0 {
            OwnedChildState::Owned { child, .. } => Ok(child.id()),
            OwnedChildState::Delegated(_) | OwnedChildState::Reaped => Err(ProcessError::NotReaped),
        }
    }

    pub(crate) fn child_mut(&mut self) -> Result<&mut Child, ProcessError> {
        match &mut self.0 {
            OwnedChildState::Owned { child, .. } => Ok(child),
            OwnedChildState::Delegated(_) | OwnedChildState::Reaped => Err(ProcessError::NotReaped),
        }
    }

    fn mark_reaped(&mut self) {
        self.0 = OwnedChildState::Reaped;
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>, ProcessError> {
        let status = self.child_mut()?.try_wait().map_err(|_| ProcessError::Io)?;
        if status.is_some() {
            self.mark_reaped();
        }
        Ok(status)
    }

    fn wait_after_exit(&mut self) -> Result<ExitStatus, ProcessError> {
        let status = self
            .child_mut()?
            .wait()
            .map_err(|_| ProcessError::NotReaped)?;
        self.mark_reaped();
        Ok(status)
    }

    fn transfer_to_reaper(&mut self) -> Result<(), ProcessError> {
        let state = std::mem::replace(&mut self.0, OwnedChildState::Reaped);
        match state {
            OwnedChildState::Owned { child, permit } => {
                self.0 = OwnedChildState::Delegated(permit.handoff(child));
                Ok(())
            }
            other => {
                self.0 = other;
                Err(ProcessError::NotReaped)
            }
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let state = std::mem::replace(&mut self.0, OwnedChildState::Reaped);
        if let OwnedChildState::Owned { mut child, permit } = state {
            // The supervising owner normally retires the process group first.
            // This last-resort path protects the direct-child handle if an
            // early return or panic bypasses that owner-level cleanup.
            let _ = child.kill();
            let _ = permit.handoff(child);
        }
    }
}

fn supervised_child_pid(child: &OwnedChild) -> Result<u32, ProcessError> {
    child.id()
}

struct SessionWriteCommand {
    bytes: Vec<u8>,
    completed: SyncSender<Result<(), ProcessError>>,
}

#[derive(Clone)]
pub(crate) struct ProcessGroupRetirement {
    pid: u32,
    outcome: Arc<Mutex<Option<Result<(), ProcessError>>>>,
    stop_workers: Option<Arc<AtomicBool>>,
}

impl ProcessGroupRetirement {
    pub(crate) fn new(pid: u32) -> Self {
        Self {
            pid,
            outcome: Arc::new(Mutex::new(None)),
            stop_workers: None,
        }
    }

    pub(crate) fn with_stop_workers(pid: u32, stop_workers: Arc<AtomicBool>) -> Self {
        Self {
            pid,
            outcome: Arc::new(Mutex::new(None)),
            stop_workers: Some(stop_workers),
        }
    }

    pub(crate) fn retire(&self) -> Result<(), ProcessError> {
        if let Some(stop_workers) = &self.stop_workers {
            stop_workers.store(true, Ordering::Release);
        }
        let mut outcome = self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(result) = *outcome {
            return result;
        }
        let result = terminate_process_group(self.pid);
        *outcome = Some(result);
        result
    }
}

#[cfg(all(
    unix,
    not(any(
        target_os = "cygwin",
        target_os = "horizon",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    ))
))]
fn wait_for_child_exit_without_reaping(pid: u32) -> Result<bool, ProcessError> {
    let deadline = Instant::now()
        .checked_add(CHILD_REAP_TIMEOUT)
        .ok_or(ProcessError::NotReaped)?;
    loop {
        if child_exited_without_reaping(pid)? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn child_reaper_state() -> Result<Arc<ChildReaperState>, ProcessError> {
    let state_slot = CHILD_REAPER.get_or_init(|| Mutex::new(None));
    let mut state = state_slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Ok(Arc::clone(state.get_or_insert_with(|| {
        ChildReaperState::new(CHILD_REAPER_CAPACITY)
    })))
}

impl ChildReaperState {
    fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            reserved: AtomicUsize::new(0),
            capacity,
            jobs: Mutex::new(VecDeque::with_capacity(capacity)),
            changed: Condvar::new(),
            worker_started: Mutex::new(false),
            #[cfg(test)]
            shutdown: AtomicBool::new(false),
        })
    }
}

pub(crate) fn reserve_child_reaper() -> Result<ChildReaperPermit, ProcessError> {
    let state = child_reaper_state()?;
    ensure_child_reaper_worker(&state)?;
    reserve_child_reaper_slot(state)
}

fn reserve_child_reaper_slot(
    state: Arc<ChildReaperState>,
) -> Result<ChildReaperPermit, ProcessError> {
    loop {
        let reserved = state.reserved.load(Ordering::Acquire);
        if reserved >= state.capacity {
            return Err(ProcessError::SupervisorCapacity);
        }
        if state
            .reserved
            .compare_exchange_weak(reserved, reserved + 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Ok(ChildReaperPermit { state });
        }
    }
}

impl ChildReaperPermit {
    fn handoff(self, child: Child) -> ChildReapTicket {
        let state = Arc::clone(&self.state);
        let ticket = self.queue_child(child);
        // A worker is normally active because permits are reserved before
        // spawn. If it died unexpectedly, custody remains in the queue while
        // this attempts a restart; a later admission retries worker startup.
        let _ = ensure_child_reaper_worker(&state);
        ticket
    }

    fn queue_child(self, child: Child) -> ChildReapTicket {
        let state = Arc::clone(&self.state);
        let completed = Arc::new(AtomicBool::new(false));
        state
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(ChildReapJob {
                child,
                _permit: self,
                completed: Arc::clone(&completed),
            });
        state.changed.notify_one();
        ChildReapTicket { completed }
    }
}

struct ChildReaperWorkerGuard(Arc<ChildReaperState>);

impl Drop for ChildReaperWorkerGuard {
    fn drop(&mut self) {
        *self
            .0
            .worker_started
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
        self.0.changed.notify_all();
    }
}

fn ensure_child_reaper_worker(state: &Arc<ChildReaperState>) -> Result<(), ProcessError> {
    let mut started = state
        .worker_started
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if *started {
        return Ok(());
    }
    *started = true;
    let worker_state = Arc::clone(state);
    if thread::Builder::new()
        .name("backend-child-reaper".into())
        .spawn(move || child_reaper_worker(worker_state))
        .is_err()
    {
        *started = false;
        return Err(ProcessError::NotReaped);
    }
    Ok(())
}

fn child_reaper_worker(state: Arc<ChildReaperState>) {
    let _guard = ChildReaperWorkerGuard(Arc::clone(&state));
    loop {
        let Some(mut job) = pop_child_reap_job(&state) else {
            break;
        };
        let ready = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            child_ready_to_reap(&mut job.child)
        }));
        if matches!(ready, Ok(Ok(true))) {
            job.completed.store(true, Ordering::Release);
            drop(job);
            continue;
        }

        // Rotate every still-live or uncertain process to the back of the
        // bounded queue. Polling is nonblocking, so one stuck child cannot
        // prevent an exited child behind it from being reaped.
        let mut jobs = state
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        jobs.push_back(job);
        let (jobs, _timeout) = state
            .changed
            .wait_timeout(jobs, Duration::from_millis(1))
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(jobs);
    }
}

fn pop_child_reap_job(state: &ChildReaperState) -> Option<ChildReapJob> {
    let mut jobs = state
        .jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        if let Some(job) = jobs.pop_front() {
            return Some(job);
        }
        #[cfg(test)]
        if state.shutdown.load(Ordering::Acquire) {
            return None;
        }
        jobs = state
            .changed
            .wait(jobs)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

#[cfg(test)]
fn stop_test_child_reaper(state: &ChildReaperState) {
    state.shutdown.store(true, Ordering::Release);
    state.changed.notify_all();
    let deadline = Instant::now() + Duration::from_secs(2);
    while *state
        .worker_started
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
    {
        assert!(Instant::now() < deadline, "test reaper did not stop");
        thread::sleep(Duration::from_millis(1));
    }
}

fn child_ready_to_reap(child: &mut Child) -> io::Result<bool> {
    #[cfg(all(
        unix,
        not(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        ))
    ))]
    {
        if !child_exited_without_reaping(child.id()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::Other,
                "failed to observe delegated child exit",
            )
        })? {
            return Ok(false);
        }
        child.wait().map(|_| true)
    }
    #[cfg(not(all(
        unix,
        not(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        ))
    )))]
    {
        child.try_wait().map(|status| status.is_some())
    }
}

impl Drop for ChildReaperPermit {
    fn drop(&mut self) {
        self.state.reserved.fetch_sub(1, Ordering::AcqRel);
    }
}

impl ChildReapTicket {
    fn is_complete(&self) -> bool {
        self.completed.load(Ordering::Acquire)
    }
}

#[cfg(all(
    unix,
    not(any(
        target_os = "cygwin",
        target_os = "horizon",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    ))
))]
pub(crate) fn set_nonblocking_pipe<Fd: std::os::fd::AsFd>(fd: Fd) -> Result<(), ProcessError> {
    let flags = rustix::fs::fcntl_getfl(&fd).map_err(|_| ProcessError::Io)?;
    rustix::fs::fcntl_setfl(&fd, flags | rustix::fs::OFlags::NONBLOCK).map_err(|_| ProcessError::Io)
}

#[cfg(all(
    unix,
    not(any(
        target_os = "cygwin",
        target_os = "horizon",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    ))
))]
pub(crate) fn child_exited_without_reaping(pid: u32) -> Result<bool, ProcessError> {
    use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};

    let raw_pid = i32::try_from(pid).map_err(|_| ProcessError::NotReaped)?;
    let pid = Pid::from_raw(raw_pid).ok_or(ProcessError::NotReaped)?;
    let status = waitid(
        WaitId::Pid(pid),
        WaitIdOptions::NOHANG | WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
    )
    .map_err(|_| ProcessError::Io)?;
    Ok(status.is_some_and(|status| status.exited() || status.killed() || status.dumped()))
}

#[cfg(not(all(
    unix,
    not(any(
        target_os = "cygwin",
        target_os = "horizon",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    ))
)))]
pub(crate) fn set_nonblocking_pipe<Fd>(fd: Fd) -> Result<(), ProcessError> {
    let _ = fd;
    Err(ProcessError::UnsupportedLimit(
        crate::UnsupportedLimit::ProcessGroup,
    ))
}

#[cfg(not(all(
    unix,
    not(any(
        target_os = "cygwin",
        target_os = "horizon",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    ))
)))]
pub(crate) fn child_exited_without_reaping(_pid: u32) -> Result<bool, ProcessError> {
    Err(ProcessError::UnsupportedLimit(
        crate::UnsupportedLimit::ProcessGroup,
    ))
}

#[derive(Clone, Copy, Debug)]
enum SessionPipeFault {
    Protocol = 1,
    OutputLimit = 2,
    Io = 3,
}

impl SessionPipeFault {
    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Protocol),
            2 => Some(Self::OutputLimit),
            3 => Some(Self::Io),
            _ => None,
        }
    }

    const fn process_error(self) -> ProcessError {
        match self {
            Self::Protocol => ProcessError::Protocol,
            Self::OutputLimit => ProcessError::OutputLimit,
            Self::Io => ProcessError::Io,
        }
    }
}

enum SessionPipeEvent {
    Bytes(Vec<u8>),
    Fault(SessionPipeFault),
}

#[derive(Default)]
struct SessionStdoutCredit {
    active: bool,
    remaining: usize,
    probe_requested: u64,
    probe_completed: u64,
}

#[cfg(test)]
#[derive(Default)]
struct TestStdoutReaderGate {
    paused: bool,
    parked: bool,
}

impl RunningByteSession {
    /// Sends one bounded request and receives exactly the declared response bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] on cancellation, a limit, malformed command state, or any
    /// process/pipe failure. Every error retires and reaps this session.
    pub(crate) fn exchange_with_cancellation_flag(
        &mut self,
        request: Vec<u8>,
        response_bytes: usize,
        cancelled: &AtomicBool,
    ) -> Result<Vec<u8>, ProcessError> {
        let limits = self.command.limits();
        if request.is_empty()
            || request.len() > limits.input_bytes().saturating_sub(self.request_bytes)
        {
            return self.fail(ProcessError::InputLimit);
        }
        if response_bytes == 0
            || response_bytes > limits.stdout()
            || response_bytes > limits.output_bytes()
            || self.stdout_observed.load(Ordering::Acquire) > limits.stdout()
            || response_bytes
                > limits
                    .stdout()
                    .saturating_sub(self.stdout_observed.load(Ordering::Acquire))
        {
            return self.fail(ProcessError::OutputLimit);
        }
        if cancelled.load(Ordering::Acquire) {
            return self.fail(ProcessError::Cancelled);
        }
        if self.stderr_exceeded.load(Ordering::Acquire) {
            return self.fail(ProcessError::OutputLimit);
        }
        let output_bytes = self.output_observed.load(Ordering::Acquire);
        if output_bytes > limits.output_bytes()
            || response_bytes > limits.output_bytes().saturating_sub(output_bytes)
        {
            return self.fail(ProcessError::OutputLimit);
        }
        let deadline = Instant::now()
            .checked_add(limits.wall_time())
            .ok_or(ProcessError::Deadline)?;
        let request_len = request.len();
        if let Some(error) = self.session_health_error() {
            return self.fail(error);
        }
        if let Some(events) = self.stdout_events.as_ref() {
            let event = events.try_recv();
            match event {
                Err(TryRecvError::Empty) => {}
                Ok(SessionPipeEvent::Fault(fault)) => return self.fail(fault.process_error()),
                _ => return self.fail(ProcessError::Protocol),
            }
        } else {
            return self.fail(ProcessError::Io);
        }

        let mut response = Vec::new();
        if response.try_reserve_exact(response_bytes).is_err() {
            return self.fail(ProcessError::OutputLimit);
        }
        let (write_sender, write_receiver) = mpsc::sync_channel(1);
        let command = SessionWriteCommand {
            bytes: request,
            completed: write_sender,
        };
        let Some(stdin_sender) = self.stdin_sender.as_ref() else {
            return self.fail(ProcessError::Io);
        };
        {
            let mut credit = self
                .stdout_credit
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            credit.remaining = response_bytes;
            credit.active = true;
        }
        let queued = stdin_sender.try_send(command).is_ok();
        if !queued {
            return self.fail(ProcessError::Protocol);
        }
        let mut write_complete = false;
        loop {
            if cancelled.load(Ordering::Acquire) {
                return self.fail(ProcessError::Cancelled);
            }
            if let Some(error) = self.session_health_error() {
                return self.fail(error);
            }
            if let Some(limit) = limits.workspace_limit() {
                match workspace_size(self.command.workspace()) {
                    Ok(current)
                        if current > self.workspace_baseline
                            && current - self.workspace_baseline > limit =>
                    {
                        return self.fail(ProcessError::WorkspaceLimit);
                    }
                    Ok(_) => {}
                    Err(error) => return self.fail(error),
                }
            }
            match child_exited_without_reaping(supervised_child_pid(&self.child)?) {
                Ok(true) => return self.fail(ProcessError::Protocol),
                Ok(false) => {}
                Err(_) => return self.fail(ProcessError::Io),
            }
            if !write_complete {
                match write_receiver.try_recv() {
                    Ok(Ok(())) => write_complete = true,
                    Ok(Err(_)) => return self.fail(ProcessError::Io),
                    Err(TryRecvError::Disconnected) => return self.fail(ProcessError::Io),
                    Err(TryRecvError::Empty) => {}
                }
            }
            if response.len() == response_bytes && write_complete {
                break;
            }
            if Instant::now() >= deadline {
                return self.fail(ProcessError::Deadline);
            }
            let Some(events) = self.stdout_events.as_ref() else {
                return self.fail(ProcessError::Io);
            };
            let timeout = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(5));
            let event = events.recv_timeout(timeout);
            match event {
                Ok(SessionPipeEvent::Bytes(bytes)) => {
                    if response.len().saturating_add(bytes.len()) > response_bytes {
                        return self.fail(ProcessError::Protocol);
                    }
                    response.extend_from_slice(&bytes);
                }
                Ok(SessionPipeEvent::Fault(fault)) => return self.fail(fault.process_error()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return self.fail(ProcessError::Io),
            }
        }
        {
            let mut credit = self
                .stdout_credit
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            credit.active = false;
            credit.remaining = 0;
        }
        if let Err(error) = self.probe_stdout_idle_gate(deadline) {
            return self.fail(error);
        }
        if let Some(error) = self.session_health_error() {
            return self.fail(error);
        }

        // A persistent helper must remain alive after each complete response. A one-shot child
        // cannot be mistaken for a successful reusable session, even if its first frame was valid.
        match child_exited_without_reaping(supervised_child_pid(&self.child)?) {
            Ok(true) => self.fail(ProcessError::Protocol),
            Ok(false) => {
                self.exchanges = self.exchanges.saturating_add(1);
                self.request_bytes = self.request_bytes.saturating_add(request_len);
                Ok(response)
            }
            Err(_) => self.fail(ProcessError::Io),
        }
    }

    /// Returns whether the session should be retired before another request.
    #[must_use]
    pub(crate) fn should_recycle(&self) -> bool {
        self.session_health_error().is_some()
            || self.exchanges >= 64
            || self.started.elapsed() >= Duration::from_secs(15 * 60)
    }

    /// Returns whether one more exchange fits the remaining per-process byte allowances.
    #[must_use]
    pub(crate) fn can_exchange(&self, request_bytes: usize, response_bytes: usize) -> bool {
        let limits = self.command.limits();
        if self.session_health_error().is_some()
            || request_bytes == 0
            || request_bytes > limits.input_bytes().saturating_sub(self.request_bytes)
            || response_bytes == 0
            || response_bytes > limits.stdout()
            || self.stdout_observed.load(Ordering::Acquire) > limits.stdout()
            || response_bytes
                > limits
                    .stdout()
                    .saturating_sub(self.stdout_observed.load(Ordering::Acquire))
        {
            return false;
        }
        let stderr_bytes = self.stderr_observed.load(Ordering::Acquire);
        let output_bytes = self.output_observed.load(Ordering::Acquire);
        stderr_bytes <= limits.stderr()
            && output_bytes <= limits.output_bytes()
            && response_bytes <= limits.output_bytes().saturating_sub(output_bytes)
    }

    /// Checks whether the persistent child is still available for a new exchange.
    pub(crate) fn is_alive(&mut self) -> Result<bool, ProcessError> {
        if self.group_retired {
            return Ok(false);
        }
        if let Some(error) = self.session_health_error() {
            self.terminate()?;
            return Err(error);
        }
        let probe_deadline = Instant::now()
            .checked_add(Duration::from_millis(50))
            .ok_or(ProcessError::Deadline)?;
        if let Err(error) = self.probe_stdout_idle_gate(probe_deadline) {
            return self.fail(error);
        }
        if let Some(error) = self.session_health_error() {
            return self.fail(error);
        }
        match child_exited_without_reaping(supervised_child_pid(&self.child)?)? {
            false => Ok(true),
            true => {
                self.terminate()?;
                Ok(false)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn pause_stdout_reader_for_test(&self) -> bool {
        let (state, changed) = &*self.stdout_reader_gate;
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.paused = true;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !state.parked {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.paused = false;
                changed.notify_all();
                return false;
            }
            let (next, timeout) = changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
            if timeout.timed_out() && !state.parked {
                state.paused = false;
                changed.notify_all();
                return false;
            }
        }
        true
    }

    /// Terminates and reaps the child and drops the private executable lease.
    pub(crate) fn retire(&mut self) -> Result<(), ProcessError> {
        self.terminate()
    }

    fn fail<T>(&mut self, error: ProcessError) -> Result<T, ProcessError> {
        match self.terminate() {
            Ok(()) => Err(error),
            Err(teardown) => Err(teardown),
        }
    }

    fn terminate(&mut self) -> Result<(), ProcessError> {
        self.stop_workers.store(true, Ordering::Release);
        #[cfg(test)]
        release_test_stdout_reader(&self.stdout_reader_gate);
        {
            let mut credit = self
                .stdout_credit
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            credit.active = false;
            credit.remaining = 0;
        }
        self.stdout_events.take();
        self.stdin_sender.take();
        let result = if self.group_retired {
            Ok(())
        } else {
            let result = terminate_child_with_group(&mut self.child, &self.group_retirement);
            self.group_retired = result.is_ok();
            result
        };
        if let Some(writer) = self.stdin_writer.take()
            && writer.join().is_err()
        {
            if result.is_ok() {
                return Err(ProcessError::Io);
            }
        }
        if let Some(reader) = self.stderr_reader.take() {
            if reader.join().is_err() && result.is_ok() {
                return Err(ProcessError::Io);
            }
        }
        if let Some(reader) = self.stdout_reader.take()
            && reader.join().is_err()
            && result.is_ok()
        {
            return Err(ProcessError::Io);
        }
        result
    }

    fn session_health_error(&self) -> Option<ProcessError> {
        if self.stderr_exceeded.load(Ordering::Acquire) {
            return Some(ProcessError::OutputLimit);
        }
        if let Some(fault) = SessionPipeFault::from_code(self.stdout_fault.load(Ordering::Acquire))
        {
            return Some(fault.process_error());
        }
        let limits = self.command.limits();
        let stderr = self.stderr_observed.load(Ordering::Acquire);
        let stdout = self.stdout_observed.load(Ordering::Acquire);
        let output = self.output_observed.load(Ordering::Acquire);
        if stderr > limits.stderr() || stdout > limits.stdout() || output > limits.output_bytes() {
            Some(ProcessError::OutputLimit)
        } else {
            None
        }
    }

    /// Waits for the stdout pump to observe an empty nonblocking pipe while no response credit is
    /// armed. This makes cache-only health checks account for bytes already queued at the gate.
    /// A helper can still write after the acknowledgement; the protocol boundary is the instant
    /// the pump reports `WouldBlock`, not a promise about future asynchronous writes.
    fn probe_stdout_idle_gate(&mut self, deadline: Instant) -> Result<(), ProcessError> {
        let requested = {
            let mut credit = self
                .stdout_credit
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if credit.active {
                return Err(ProcessError::Protocol);
            }
            credit.probe_requested = credit
                .probe_requested
                .checked_add(1)
                .ok_or(ProcessError::Protocol)?;
            credit.probe_requested
        };
        #[cfg(test)]
        release_test_stdout_reader(&self.stdout_reader_gate);
        let mut credit = self
            .stdout_credit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while credit.probe_completed < requested {
            let now = Instant::now();
            if now >= deadline {
                return Err(ProcessError::Deadline);
            }
            let wait = deadline.saturating_duration_since(now);
            let (next, timeout) = self
                .stdout_credit_changed
                .wait_timeout(credit, wait)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            credit = next;
            if timeout.timed_out() && credit.probe_completed < requested {
                return Err(ProcessError::Deadline);
            }
        }
        drop(credit);
        let Some(events) = self.stdout_events.as_ref() else {
            return Err(ProcessError::Io);
        };
        match events.try_recv() {
            Err(TryRecvError::Empty) => Ok(()),
            Ok(SessionPipeEvent::Fault(fault)) => Err(fault.process_error()),
            Ok(SessionPipeEvent::Bytes(_)) => Err(ProcessError::Protocol),
            Err(TryRecvError::Disconnected) => Err(ProcessError::Io),
        }
    }
}

impl Drop for RunningByteSession {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

fn spawn_session_stderr_reader(
    mut stderr: ChildStderr,
    group_retirement: ProcessGroupRetirement,
    stop: Arc<AtomicBool>,
    observed: Arc<AtomicUsize>,
    exceeded: Arc<AtomicBool>,
    output_observed: Arc<AtomicUsize>,
    fault_state: Arc<AtomicU8>,
    limit: usize,
    output_limit: usize,
) -> Result<JoinHandle<()>, ProcessError> {
    thread::Builder::new()
        .name("backend-compile-session-stderr".into())
        .spawn(move || {
            let mut chunk = [0_u8; 8192];
            loop {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                let read = match stderr.read(&mut chunk) {
                    Ok(0) => {
                        if !stop.load(Ordering::Acquire) {
                            poison_session_pipe(
                                SessionPipeFault::Io,
                                &group_retirement,
                                &fault_state,
                                None,
                            );
                        }
                        return;
                    }
                    Ok(read) => read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(_) => {
                        if !stop.load(Ordering::Acquire) {
                            poison_session_pipe(
                                SessionPipeFault::Io,
                                &group_retirement,
                                &fault_state,
                                None,
                            );
                        }
                        return;
                    }
                };
                if account_session_bytes(&observed, &output_observed, read, limit, output_limit)
                    .is_err()
                {
                    exceeded.store(true, Ordering::Release);
                    poison_session_pipe(
                        SessionPipeFault::OutputLimit,
                        &group_retirement,
                        &fault_state,
                        None,
                    );
                    return;
                }
            }
        })
        .map_err(|_| ProcessError::Io)
}

fn spawn_session_stdin_writer(
    mut stdin: ChildStdin,
    stop: Arc<AtomicBool>,
) -> Result<(SyncSender<SessionWriteCommand>, JoinHandle<()>), ProcessError> {
    let (sender, receiver) = mpsc::sync_channel::<SessionWriteCommand>(1);
    let worker = thread::Builder::new()
        .name("backend-compile-session-stdin".into())
        .spawn(move || {
            while let Ok(command) = receiver.recv() {
                let result = write_session_request(&mut stdin, &command.bytes, &stop);
                let failed = result.is_err();
                let _ = command.completed.send(result);
                if failed {
                    return;
                }
            }
        })
        .map_err(|_| ProcessError::Io)?;
    Ok((sender, worker))
}

fn write_session_request(
    stdin: &mut ChildStdin,
    bytes: &[u8],
    stop: &AtomicBool,
) -> Result<(), ProcessError> {
    let mut written = 0;
    while written < bytes.len() {
        if stop.load(Ordering::Acquire) {
            return Err(ProcessError::Cancelled);
        }
        match stdin.write(&bytes[written..]) {
            Ok(0) => return Err(ProcessError::Io),
            Ok(count) => written += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(_) => return Err(ProcessError::Io),
        }
    }
    Ok(())
}

fn spawn_session_stdout_reader(
    mut stdout: ChildStdout,
    group_retirement: ProcessGroupRetirement,
    stop: Arc<AtomicBool>,
    stdout_limit: usize,
    output_limit: usize,
    observed: Arc<AtomicUsize>,
    output_observed: Arc<AtomicUsize>,
    credit: Arc<Mutex<SessionStdoutCredit>>,
    credit_changed: Arc<Condvar>,
    #[cfg(test)] test_gate: Arc<(Mutex<TestStdoutReaderGate>, Condvar)>,
    fault_state: Arc<AtomicU8>,
) -> Result<(Receiver<SessionPipeEvent>, JoinHandle<()>), ProcessError> {
    let (sender, receiver) = mpsc::sync_channel(4);
    let worker = thread::Builder::new()
        .name("backend-compile-session-stdout".into())
        .spawn(move || {
            let mut chunk = [0_u8; 8192];
            loop {
                #[cfg(test)]
                await_test_stdout_reader(&test_gate, &stop);
                if stop.load(Ordering::Acquire) {
                    return;
                }
                // The state lock covers the nonblocking read and its classification. Arming a
                // new exchange takes the same lock, so an idle byte already in the pipe cannot
                // become part of a later request merely because the pump was descheduled.
                let mut state = credit
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let read_limit = if state.active && state.remaining > 0 {
                    state.remaining.min(chunk.len())
                } else {
                    1
                };
                let read = match stdout.read(&mut chunk[..read_limit]) {
                    Ok(0) => {
                        record_session_pipe_fault(&fault_state, SessionPipeFault::Protocol);
                        acknowledge_stdout_probe(&mut state, &credit_changed);
                        drop(state);
                        if !stop.load(Ordering::Acquire) {
                            poison_session_pipe(
                                SessionPipeFault::Protocol,
                                &group_retirement,
                                &fault_state,
                                Some(&sender),
                            );
                        }
                        return;
                    }
                    Ok(read) => read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                        drop(state);
                        continue;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        if !state.active {
                            acknowledge_stdout_probe(&mut state, &credit_changed);
                        }
                        drop(state);
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(_) => {
                        record_session_pipe_fault(&fault_state, SessionPipeFault::Io);
                        acknowledge_stdout_probe(&mut state, &credit_changed);
                        drop(state);
                        if !stop.load(Ordering::Acquire) {
                            poison_session_pipe(
                                SessionPipeFault::Io,
                                &group_retirement,
                                &fault_state,
                                Some(&sender),
                            );
                        }
                        return;
                    }
                };
                // Count every byte removed from the OS pipe, including unsolicited bytes that
                // will poison the protocol below. Resource accounting is independent from frame
                // validity and never ignores padding or idle writes.
                if account_session_bytes(
                    &observed,
                    &output_observed,
                    read,
                    stdout_limit,
                    output_limit,
                )
                .is_err()
                {
                    record_session_pipe_fault(&fault_state, SessionPipeFault::OutputLimit);
                    acknowledge_stdout_probe(&mut state, &credit_changed);
                    drop(state);
                    poison_session_pipe(
                        SessionPipeFault::OutputLimit,
                        &group_retirement,
                        &fault_state,
                        Some(&sender),
                    );
                    return;
                }
                if !state.active || read > state.remaining {
                    record_session_pipe_fault(&fault_state, SessionPipeFault::Protocol);
                    acknowledge_stdout_probe(&mut state, &credit_changed);
                    drop(state);
                    poison_session_pipe(
                        SessionPipeFault::Protocol,
                        &group_retirement,
                        &fault_state,
                        Some(&sender),
                    );
                    return;
                }
                state.remaining -= read;
                drop(state);
                if sender
                    .send(SessionPipeEvent::Bytes(chunk[..read].to_vec()))
                    .is_err()
                {
                    return;
                }
            }
        })
        .map_err(|_| ProcessError::Io)?;
    Ok((receiver, worker))
}

fn acknowledge_stdout_probe(state: &mut SessionStdoutCredit, changed: &Condvar) {
    if state.probe_completed < state.probe_requested {
        state.probe_completed = state.probe_requested;
        changed.notify_all();
    }
}

fn record_session_pipe_fault(state: &AtomicU8, fault: SessionPipeFault) {
    let _ = state.compare_exchange(0, fault as u8, Ordering::AcqRel, Ordering::Acquire);
}

#[cfg(test)]
fn await_test_stdout_reader(gate: &(Mutex<TestStdoutReaderGate>, Condvar), stop: &AtomicBool) {
    let (state, changed) = gate;
    let mut state = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if state.paused {
        state.parked = true;
        changed.notify_all();
        while state.paused && !stop.load(Ordering::Acquire) {
            state = changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.parked = false;
        changed.notify_all();
    }
}

#[cfg(test)]
fn release_test_stdout_reader(gate: &(Mutex<TestStdoutReaderGate>, Condvar)) {
    let (state, changed) = gate;
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .paused = false;
    changed.notify_all();
}

fn account_session_bytes(
    stream_observed: &AtomicUsize,
    output_observed: &AtomicUsize,
    bytes: usize,
    stream_limit: usize,
    output_limit: usize,
) -> Result<(), SessionPipeFault> {
    let stream_total = stream_observed
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            Some(current.saturating_add(bytes))
        })
        .unwrap_or(usize::MAX)
        .saturating_add(bytes);
    let mut current = output_observed.load(Ordering::Acquire);
    let output_within_limit = loop {
        let Some(next) = current.checked_add(bytes) else {
            break false;
        };
        if next > output_limit {
            break false;
        }
        match output_observed.compare_exchange_weak(
            current,
            next,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => break true,
            Err(observed) => current = observed,
        }
    };
    if stream_total > stream_limit || !output_within_limit {
        Err(SessionPipeFault::OutputLimit)
    } else {
        Ok(())
    }
}

fn poison_session_pipe(
    fault: SessionPipeFault,
    group_retirement: &ProcessGroupRetirement,
    state: &AtomicU8,
    sender: Option<&SyncSender<SessionPipeEvent>>,
) {
    let _ = state.compare_exchange(0, fault as u8, Ordering::AcqRel, Ordering::Acquire);
    if let Some(sender) = sender {
        let _ = sender.try_send(SessionPipeEvent::Fault(fault));
    }
    let _ = group_retirement.retire();
}

/// A spawned process that has not yet reached its reaped terminal state.
///
/// This type is intentionally affine: consuming it through
/// [`RunningProcess::finish`] or [`RunningProcess::finish_with_cancellation`]
/// is the only way to obtain a [`ProcessReceipt`].
pub struct RunningProcess {
    command: SupervisedCommand,
    child: OwnedChild,
    group_retirement: ProcessGroupRetirement,
    _stdin: Option<TempInput>,
    _executable: ExecutableLease,
    stdout: TempOutput,
    stderr: TempOutput,
    workspace_baseline: usize,
    group_retired: bool,
}

impl std::fmt::Debug for RunningProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningProcess")
            .field("program", &self.command.program())
            .field("pid", &self.child.id().ok())
            .finish_non_exhaustive()
    }
}

impl Drop for RunningProcess {
    fn drop(&mut self) {
        // Dropping a running capability must not orphan the authority. A
        // completed child returns immediately; an active one is torn down as
        // a best-effort cleanup because `Drop` cannot report an error.
        let _ = self.terminate();
    }
}

impl RunningProcess {
    /// Finishes the process with a fresh cancellation token and returns its
    /// fully reaped receipt.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when cancellation, a deadline, an output or
    /// workspace bound, or process reaping fails.
    pub fn finish(self) -> Result<ProcessReceipt, ProcessError> {
        let (cancellation, _handle) = Cancellation::new();
        self.finish_with_cancellation(&cancellation)
    }

    /// Polls until the process exits, enforcing cancellation, deadlines, and
    /// output limits, then returns a fully reaped receipt.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when cancellation, a deadline, an output or
    /// workspace bound, or process reaping fails.
    pub fn finish_with_cancellation(
        self,
        cancellation: &Cancellation,
    ) -> Result<ProcessReceipt, ProcessError> {
        self.finish_with_observer(cancellation)
    }

    /// Polls until the process exits while observing a borrowed cancellation
    /// source, enforcing deadlines and output/workspace bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when cancellation, a deadline, an output or
    /// workspace bound, or process reaping fails.
    pub fn finish_with_observer<C: CancellationObserver + ?Sized>(
        self,
        cancellation: &C,
    ) -> Result<ProcessReceipt, ProcessError> {
        self.finish_with_optional_deadline(cancellation, None)
    }

    /// Polls until the process exits while observing a borrowed cancellation
    /// source and absolute caller deadline.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when cancellation, a deadline, an output or
    /// workspace bound, or process reaping fails.
    pub fn finish_with_observer_until<C: CancellationObserver + ?Sized>(
        self,
        cancellation: &C,
        deadline: Instant,
    ) -> Result<ProcessReceipt, ProcessError> {
        self.finish_with_optional_deadline(cancellation, Some(deadline))
    }

    fn finish_with_optional_deadline<C: CancellationObserver + ?Sized>(
        mut self,
        cancellation: &C,
        caller_deadline: Option<Instant>,
    ) -> Result<ProcessReceipt, ProcessError> {
        let start = Instant::now();
        let command_deadline = start
            .checked_add(self.command.limits().wall_time())
            .ok_or(ProcessError::Deadline)?;
        let deadline =
            caller_deadline.map_or(command_deadline, |deadline| deadline.min(command_deadline));

        let status = loop {
            if cancellation.is_cancelled() {
                self.terminate()?;
                return Err(ProcessError::Cancelled);
            }

            let stdout_size = output_size(&self.stdout.file)?;
            let stderr_size = output_size(&self.stderr.file)?;
            if stdout_size > self.command.limits().stdout()
                || stderr_size > self.command.limits().stderr()
                || output_exceeds_limit(
                    stdout_size,
                    stderr_size,
                    self.command.limits().output_bytes(),
                )
            {
                // Preserve the resource violation as the primary result even
                // when the child exits between the poll and tree teardown.
                let _ = self.terminate();
                return Err(ProcessError::OutputLimit);
            }

            if self.workspace_grew_beyond_limit()? {
                self.terminate()?;
                return Err(ProcessError::WorkspaceLimit);
            }

            #[cfg(all(
                unix,
                not(any(
                    target_os = "cygwin",
                    target_os = "horizon",
                    target_os = "openbsd",
                    target_os = "redox",
                    target_os = "wasi"
                ))
            ))]
            if child_exited_without_reaping(supervised_child_pid(&self.child)?)? {
                break self.retire_completed_child()?;
            }
            #[cfg(not(all(
                unix,
                not(any(
                    target_os = "cygwin",
                    target_os = "horizon",
                    target_os = "openbsd",
                    target_os = "redox",
                    target_os = "wasi"
                ))
            )))]
            if let Some(status) = self.child.try_wait()? {
                break status;
            }

            if Instant::now() >= deadline {
                let _ = self.terminate();
                return Err(ProcessError::Deadline);
            }
            thread::sleep(Duration::from_millis(2));
        };

        if self.workspace_grew_beyond_limit()? {
            return Err(ProcessError::WorkspaceLimit);
        }
        let out = read_bounded(&self.stdout.file, self.command.limits().stdout())?;
        let err = read_bounded(&self.stderr.file, self.command.limits().stderr())?;
        if output_exceeds_limit(out.len(), err.len(), self.command.limits().output_bytes()) {
            return Err(ProcessError::OutputLimit);
        }
        Ok(receipt(
            if status.success() {
                ProcessTerminal::Success
            } else {
                ProcessTerminal::Exit
            },
            status.code(),
            out,
            err,
        ))
    }

    fn workspace_grew_beyond_limit(&self) -> Result<bool, ProcessError> {
        let Some(limit) = self.command.limits().workspace_limit() else {
            return Ok(false);
        };
        let current = workspace_size(self.command.workspace())?;
        Ok(current > self.workspace_baseline && current - self.workspace_baseline > limit)
    }

    fn retire_completed_child(&mut self) -> Result<ExitStatus, ProcessError> {
        let child_pid = supervised_child_pid(&self.child)?;
        #[cfg(all(
            unix,
            not(any(
                target_os = "cygwin",
                target_os = "horizon",
                target_os = "openbsd",
                target_os = "redox",
                target_os = "wasi"
            ))
        ))]
        let group_result = self.group_retirement.retire();
        #[cfg(not(all(
            unix,
            not(any(
                target_os = "cygwin",
                target_os = "horizon",
                target_os = "openbsd",
                target_os = "redox",
                target_os = "wasi"
            ))
        )))]
        let group_result: Result<(), ProcessError> = Ok(());

        #[cfg(all(
            unix,
            not(any(
                target_os = "cygwin",
                target_os = "horizon",
                target_os = "openbsd",
                target_os = "redox",
                target_os = "wasi"
            ))
        ))]
        if !matches!(wait_for_child_exit_without_reaping(child_pid), Ok(true)) {
            self.group_retired = group_result.is_ok();
            return Err(ProcessError::NotReaped);
        }

        let status = self.child.wait_after_exit()?;
        #[cfg(all(
            unix,
            not(any(
                target_os = "cygwin",
                target_os = "horizon",
                target_os = "openbsd",
                target_os = "redox",
                target_os = "wasi"
            ))
        ))]
        {
            self.group_retired = group_result.is_ok();
        }
        group_result?;
        Ok(status)
    }

    fn terminate(&mut self) -> Result<(), ProcessError> {
        match self.child.view() {
            LeaderCustodyView::Reaped => {
                return if self.group_retired {
                    Ok(())
                } else {
                    self.group_retirement.retire()
                };
            }
            LeaderCustodyView::DelegatedComplete => {
                return if self.group_retired {
                    Ok(())
                } else {
                    self.group_retirement.retire()
                };
            }
            LeaderCustodyView::Delegated => return Err(ProcessError::NotReaped),
            LeaderCustodyView::Owned => {}
        }
        let child_pid = supervised_child_pid(&self.child)?;
        #[cfg(not(all(
            unix,
            not(any(
                target_os = "cygwin",
                target_os = "horizon",
                target_os = "openbsd",
                target_os = "redox",
                target_os = "wasi"
            ))
        )))]
        {
            let _ = self.child.child_mut()?.kill();
            let deadline = Instant::now()
                .checked_add(CHILD_REAP_TIMEOUT)
                .ok_or(ProcessError::NotReaped)?;
            loop {
                if self.child.try_wait()?.is_some() {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    let _ = self.child.transfer_to_reaper();
                    return Err(ProcessError::NotReaped);
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
        #[cfg(all(
            unix,
            not(any(
                target_os = "cygwin",
                target_os = "horizon",
                target_os = "openbsd",
                target_os = "redox",
                target_os = "wasi"
            ))
        ))]
        {
            let group_result = self.group_retirement.retire();
            if !child_exited_without_reaping(child_pid).unwrap_or(false) {
                let _ = self.child.child_mut()?.kill();
            }
            if !matches!(wait_for_child_exit_without_reaping(child_pid), Ok(true)) {
                let _ = self.child.transfer_to_reaper();
                self.group_retired = group_result.is_ok();
                return Err(ProcessError::NotReaped);
            }
            self.child.wait_after_exit()?;
            self.group_retired = group_result.is_ok();
            group_result?;
            Ok(())
        }
    }
}

fn spawn(
    command: &SupervisedCommand,
    executable: &Path,
    stdin: Option<&TempInput>,
    stdout: &File,
    stderr: &File,
) -> Result<Child, ProcessError> {
    let stdout = stdout.try_clone().map_err(|_| ProcessError::Io)?;
    let stderr = stderr.try_clone().map_err(|_| ProcessError::Io)?;
    let stdin = match stdin {
        Some(input) => Stdio::from(input.open()?),
        None => Stdio::null(),
    };
    let mut process = authority_command(command, executable);
    process
        .stdin(stdin)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|_| ProcessError::SpawnFailure)
}

/// Builds the exact child command, adding Unix resource ceilings before the
/// authority's executable is `exec`'d.  The wrapper receives every executable
/// argument as a positional parameter, so paths and values never pass through
/// shell interpolation.
pub(crate) fn authority_command(command: &SupervisedCommand, executable: &Path) -> Command {
    let mut process = if command.limits().uses_unix_resource_limits() {
        let mut process = Command::new("/bin/sh");
        process
            .arg("-c")
            .arg(RESOURCE_LIMIT_SCRIPT)
            .arg("backend-compile-resource-wrapper")
            .arg(cpu_limit_argument(command))
            .arg(memory_limit_argument(command))
            .arg(process_count_limit_argument(command))
            .arg(executable);
        process.args(command.args());
        process
    } else {
        let mut process = Command::new(executable);
        process.args(command.args());
        process
    };
    configure_process_group(&mut process);
    process
        .current_dir(command.cwd())
        .env_clear()
        .envs(command.environment().as_envs());
    process
}

const RESOURCE_LIMIT_SCRIPT: &str = r#"
set -e
if [ "$1" != "-" ]; then ulimit -t "$1"; fi
if [ "$2" != "-" ]; then ulimit -v "$2"; fi
if [ "$3" != "-" ]; then ulimit -u "$3"; fi
shift 3
exec "$@"
"#;

fn cpu_limit_argument(command: &SupervisedCommand) -> String {
    command.limits().cpu_time_limit().map_or_else(
        || "-".to_owned(),
        |limit| {
            let seconds = limit.as_secs();
            let seconds = if limit.subsec_nanos() > 0 && seconds < u64::MAX {
                seconds + 1
            } else {
                seconds
            };
            seconds.max(1).to_string()
        },
    )
}

fn memory_limit_argument(command: &SupervisedCommand) -> String {
    #[cfg(target_os = "linux")]
    {
        command.limits().memory_bytes_limit().map_or_else(
            || "-".to_owned(),
            |bytes| {
                let kib = bytes.checked_add(1023).unwrap_or(usize::MAX) / 1024;
                kib.max(1).to_string()
            },
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = command;
        "-".to_owned()
    }
}

fn process_count_limit_argument(command: &SupervisedCommand) -> String {
    command
        .limits()
        .process_count_limit()
        .map_or_else(|| "-".to_owned(), |limit| limit.to_string())
}

fn output_size(file: &File) -> Result<usize, ProcessError> {
    let length = file.metadata().map_err(|_| ProcessError::Io)?.len();
    usize::try_from(length).map_err(|_| ProcessError::OutputLimit)
}

fn read_bounded(file: &File, limit: usize) -> Result<Vec<u8>, ProcessError> {
    let mut file = file.try_clone().map_err(|_| ProcessError::Io)?;
    file.seek(io::SeekFrom::Start(0))
        .map_err(|_| ProcessError::Io)?;
    let mut output = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let read = file.read(&mut chunk).map_err(|_| ProcessError::Io)?;
        if read == 0 {
            break;
        }
        if output.len() > limit || read > limit - output.len() {
            return Err(ProcessError::OutputLimit);
        }
        output.extend_from_slice(&chunk[..read]);
    }
    Ok(output)
}

fn output_exceeds_limit(stdout: usize, stderr: usize, limit: usize) -> bool {
    stdout > limit || stderr > limit || stdout > limit - stderr.min(limit)
}

pub(crate) fn terminate_child_with_group(
    child: &mut OwnedChild,
    group_retirement: &ProcessGroupRetirement,
) -> Result<(), ProcessError> {
    match child.view() {
        LeaderCustodyView::Reaped => return group_retirement.retire(),
        LeaderCustodyView::DelegatedComplete => return group_retirement.retire(),
        LeaderCustodyView::Delegated => return Err(ProcessError::NotReaped),
        LeaderCustodyView::Owned => {}
    }
    let child_pid = supervised_child_pid(child)?;
    // Keep the leader waitable while retiring its group so Darwin can census
    // the pinned original PGID before the caller reaps it.
    let group_result = group_retirement.retire();
    if !child_exited_without_reaping(child_pid).unwrap_or(false) {
        let _ = child.child_mut()?.kill();
    }
    if !matches!(wait_for_child_exit_without_reaping(child_pid), Ok(true)) {
        child.transfer_to_reaper()?;
        return Err(ProcessError::NotReaped);
    }
    child.wait_after_exit()?;
    group_result?;
    Ok(())
}

pub(crate) fn configure_process_group(command: &mut Command) {
    #[cfg(target_os = "macos")]
    backend_platform::macos_process::configure_process_session(command);
    #[cfg(all(
        unix,
        not(target_os = "macos"),
        not(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        ))
    ))]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(any(
        target_os = "macos",
        all(
            unix,
            not(any(
                target_os = "cygwin",
                target_os = "horizon",
                target_os = "openbsd",
                target_os = "redox",
                target_os = "wasi"
            ))
        )
    )))]
    {
        let _ = command;
    }
}

fn terminate_process_group(pid: u32) -> Result<(), ProcessError> {
    #[cfg(test)]
    {
        let mut attempts = GROUP_RETIREMENT_ATTEMPTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, count)) = attempts
            .iter_mut()
            .find(|(recorded_pid, _)| *recorded_pid == pid)
        {
            *count = count.saturating_add(1);
        } else {
            attempts.push((pid, 1));
        }
    }
    #[cfg(target_os = "macos")]
    {
        backend_platform::macos_process::retire_process_group(pid)
            .map_err(|_| ProcessError::UnsupportedLimit(crate::UnsupportedLimit::ProcessGroup))
    }
    #[cfg(all(
        unix,
        not(target_os = "macos"),
        not(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        ))
    ))]
    {
        use rustix::{
            io::Errno,
            process::{Pid, Signal, kill_process_group},
        };

        let raw_pid = i32::try_from(pid).map_err(|_| ProcessError::NotReaped)?;
        let group_id = Pid::from_raw(raw_pid).ok_or(ProcessError::NotReaped)?;
        match kill_process_group(group_id, Signal::KILL) {
            Ok(()) | Err(Errno::SRCH) => Ok(()),
            Err(error) => {
                #[cfg(test)]
                {
                    eprintln!(
                        "killpg {group_id:?} returned {error:?}; observed group={:?}",
                        rustix::process::getpgid(Some(group_id))
                    );
                }
                Err(ProcessError::UnsupportedLimit(
                    crate::UnsupportedLimit::ProcessGroup,
                ))
            }
        }
    }
    #[cfg(not(any(
        target_os = "macos",
        all(
            unix,
            not(any(
                target_os = "cygwin",
                target_os = "horizon",
                target_os = "openbsd",
                target_os = "redox",
                target_os = "wasi"
            ))
        )
    )))]
    {
        let _ = pid;
        Err(ProcessError::UnsupportedLimit(
            crate::UnsupportedLimit::ProcessGroup,
        ))
    }
}

pub(crate) fn workspace_size(root: &Path) -> Result<usize, ProcessError> {
    let metadata = symlink_metadata(root).map_err(|_| ProcessError::Io)?;
    if metadata.is_file() {
        return usize::try_from(metadata.len()).map_err(|_| ProcessError::WorkspaceLimit);
    }
    if !metadata.is_dir() {
        return Ok(0);
    }
    let mut total = 0_usize;
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).map_err(|_| ProcessError::Io)? {
            let entry = entry.map_err(|_| ProcessError::Io)?;
            let metadata = symlink_metadata(entry.path()).map_err(|_| ProcessError::Io)?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                total = total
                    .checked_add(
                        usize::try_from(metadata.len())
                            .map_err(|_| ProcessError::WorkspaceLimit)?,
                    )
                    .ok_or(ProcessError::WorkspaceLimit)?;
            }
        }
    }
    Ok(total)
}

struct TempOutput {
    path: PathBuf,
    file: File,
    remove_on_drop: bool,
}

struct TempInput {
    path: PathBuf,
}

impl TempInput {
    fn create(stdin: &ProcessStdin) -> Result<Option<Self>, ProcessError> {
        let Some(bytes) = stdin.as_bytes() else {
            return Ok(None);
        };
        let mut output = TempOutput::create("stdin")?;
        output.file.write_all(bytes).map_err(|_| ProcessError::Io)?;
        output.file.flush().map_err(|_| ProcessError::Io)?;
        let path = output.keep_path();
        Ok(Some(Self { path }))
    }

    fn open(&self) -> Result<File, ProcessError> {
        File::open(&self.path).map_err(|_| ProcessError::Io)
    }
}

impl Drop for TempInput {
    fn drop(&mut self) {
        let _ = remove_file(&self.path);
    }
}

impl TempOutput {
    fn create(label: &str) -> Result<Self, ProcessError> {
        let base = std::env::temp_dir();
        for _ in 0..16 {
            let nonce = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!(
                "backend-compile-{}-{nonce}-{label}.out",
                std::process::id()
            ));
            match OpenOptions::new()
                .create_new(true)
                .write(true)
                .read(true)
                .open(&path)
            {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file,
                        remove_on_drop: true,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(ProcessError::TemporaryFile),
            }
        }
        Err(ProcessError::TemporaryFile)
    }

    fn keep_path(&mut self) -> PathBuf {
        self.remove_on_drop = false;
        self.path.clone()
    }
}

impl Drop for TempOutput {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_reaper_reservations_are_bounded_and_released_by_permit_drop() {
        let state = ChildReaperState::new(2);
        let first = reserve_child_reaper_slot(Arc::clone(&state)).expect("first slot");
        let second = reserve_child_reaper_slot(Arc::clone(&state)).expect("second slot");
        assert!(matches!(
            reserve_child_reaper_slot(Arc::clone(&state)),
            Err(ProcessError::SupervisorCapacity)
        ));
        assert_eq!(state.reserved.load(Ordering::Acquire), 2);

        drop(first);
        let replacement = reserve_child_reaper_slot(Arc::clone(&state))
            .expect("a released slot can be reserved again");
        assert_eq!(state.reserved.load(Ordering::Acquire), 2);

        drop((second, replacement));
        assert_eq!(state.reserved.load(Ordering::Acquire), 0);
    }

    #[cfg(unix)]
    #[test]
    fn owned_child_keeps_reserved_custody_until_the_shared_reaper_finishes() {
        let state = ChildReaperState::new(1);
        let permit = reserve_child_reaper_slot(Arc::clone(&state)).expect("reserve slot");
        let child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn child");
        let mut owned = OwnedChild::new(child, permit);
        let pid = owned.id().expect("owned PID");
        owned
            .child_mut()
            .expect("owned child handle")
            .kill()
            .expect("kill child");
        owned.transfer_to_reaper().expect("delegate child custody");

        assert_eq!(state.reserved.load(Ordering::Acquire), 1);
        assert!(matches!(
            reserve_child_reaper_slot(Arc::clone(&state)),
            Err(ProcessError::SupervisorCapacity)
        ));
        let deadline = Instant::now() + Duration::from_secs(2);
        while state.reserved.load(Ordering::Acquire) != 0 {
            assert!(Instant::now() < deadline, "delegated child was not reaped");
            thread::sleep(Duration::from_millis(1));
        }
        assert!(matches!(owned.view(), LeaderCustodyView::DelegatedComplete));
        assert!(matches!(owned.view(), LeaderCustodyView::Reaped));

        let replacement = reserve_child_reaper_slot(Arc::clone(&state))
            .expect("reaped custody releases its admission slot");
        drop(replacement);
        drop(owned);

        let permit = reserve_child_reaper_slot(Arc::clone(&state)).expect("reserve drop slot");
        let child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn child for fallback drop");
        drop(OwnedChild::new(child, permit));
        let deadline = Instant::now() + Duration::from_secs(2);
        while state.reserved.load(Ordering::Acquire) != 0 {
            assert!(
                Instant::now() < deadline,
                "drop fallback lost child custody"
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert!(pid > 0);
        stop_test_child_reaper(&state);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn delegated_child_reaper_recovers_queued_custody_and_is_fair() {
        let state = ChildReaperState::new(2);
        let permit = reserve_child_reaper_slot(Arc::clone(&state)).expect("reserve slot");
        let mut child = Command::new("/bin/sh")
            .args(["-c", "IFS= read -r line"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("spawn child blocked on stdin");
        let blocked_pid = child.id();
        let input_gate = child.stdin.take().expect("take child stdin");
        let blocked_ticket = permit.queue_child(child);
        {
            let jobs = state
                .jobs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(jobs.front().map(|job| job.child.id()), Some(blocked_pid));
        }
        assert!(!blocked_ticket.is_complete());
        assert_eq!(state.reserved.load(Ordering::Acquire), 1);
        ensure_child_reaper_worker(&state).expect("restart test reaper with queued custody");

        let permit = reserve_child_reaper_slot(Arc::clone(&state)).expect("reserve second slot");
        assert_eq!(state.reserved.load(Ordering::Acquire), 2);
        let exited_child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn exited child");
        let exited_ticket = permit.handoff(exited_child);

        assert!(!blocked_ticket.is_complete());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !exited_ticket.is_complete() {
            assert!(
                Instant::now() < deadline,
                "reaper did not service later exit"
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert!(!blocked_ticket.is_complete());
        while state.reserved.load(Ordering::Acquire) != 1 {
            assert!(Instant::now() < deadline, "reaped slot was not released");
            thread::sleep(Duration::from_millis(1));
        }

        drop(input_gate);
        while !blocked_ticket.is_complete() {
            assert!(Instant::now() < deadline, "reaper did not finish the child");
            thread::sleep(Duration::from_millis(1));
        }
        while state.reserved.load(Ordering::Acquire) != 0 {
            assert!(Instant::now() < deadline, "reaper did not release its slot");
            thread::sleep(Duration::from_millis(1));
        }
        stop_test_child_reaper(&state);
        state.shutdown.store(false, Ordering::Release);
        ensure_child_reaper_worker(&state).expect("restart test reaper");
        stop_test_child_reaper(&state);
    }

    #[test]
    fn concurrent_stdout_and_stderr_cannot_overbook_combined_output_limit() {
        let stdout = Arc::new(AtomicUsize::new(0));
        let stderr = Arc::new(AtomicUsize::new(0));
        let total = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(std::sync::Barrier::new(3));
        let stdout_thread = {
            let stdout = Arc::clone(&stdout);
            let total = Arc::clone(&total);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                account_session_bytes(&stdout, &total, 60, 100, 100).is_ok()
            })
        };
        let stderr_thread = {
            let stderr = Arc::clone(&stderr);
            let total = Arc::clone(&total);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                account_session_bytes(&stderr, &total, 60, 100, 100).is_ok()
            })
        };
        start.wait();
        let stdout_admitted = stdout_thread.join().expect("stdout accounting thread");
        let stderr_admitted = stderr_thread.join().expect("stderr accounting thread");
        assert_ne!(stdout_admitted, stderr_admitted);
        assert_eq!(total.load(Ordering::Acquire), 60);
    }
}
