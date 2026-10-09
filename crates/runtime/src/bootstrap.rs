//! An automatically spawned owner's bounded, process-private startup channel.
//! Durable setup belongs to that existing owner candidate, never the waiting
//! client. The channel is an anonymous pipe, not an authority credential.

use super::{
    ApplicationStateRoot, LIVE_START_TIMEOUT, POLL_INTERVAL, RuntimeError, WorkspacePaths,
    default_workspace_path, detach, locald_executable, normalize_identity, same_path_identity,
    startup, unlink_dead_endpoint,
};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const ARGUMENT: &str = "--runtime-startup-v1";
const PIPE_MAGIC: &[u8] = b"nudox.local-startup-pipe.v1\n";
const MAX_PIPE_BYTES: usize = 10 * 1024;
static AUTOMATIC_PIPE: AtomicBool = AtomicBool::new(false);

/// The last stage actually reported by one startup candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupPhase {
    /// The selected executable is being started; no child admission yet.
    Starting,
    /// The companion accepted the closed bootstrap grammar.
    Admitted,
    /// The child is checking and durably creating private directories.
    PrivateState,
    /// The child is admitting or durably publishing its authority credential.
    Authority,
    /// The child is preparing its endpoint namespace.
    Endpoint,
    /// The child has entered ordinary owner composition.
    OpeningOwner,
    /// This candidate exited with the existing typed owner-contention code.
    AwaitingOwner,
}

impl StartupPhase {
    /// Stable local startup spelling; never an indexing or publication claim.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Admitted => "admitted",
            Self::PrivateState => "private-state",
            Self::Authority => "authority",
            Self::Endpoint => "endpoint",
            Self::OpeningOwner => "opening-owner",
            Self::AwaitingOwner => "awaiting-owner",
        }
    }

    fn parse(text: &[u8]) -> Option<Self> {
        [
            Self::Admitted,
            Self::PrivateState,
            Self::Authority,
            Self::Endpoint,
            Self::OpeningOwner,
        ]
        .into_iter()
        .find(|phase| phase.as_str().as_bytes() == text)
    }
}

/// A readiness observation, not a durable operation receipt or owner lease.
#[derive(Debug)]
pub struct StartupPending {
    pub(crate) candidate_pid: Option<u32>,
    pub(crate) phase: StartupPhase,
    pub(crate) contender_exited: bool,
}

impl StartupPending {
    /// PID observed from the exact child handle, if process creation was reached.
    /// This is not a reusable signal capability or proof of workspace ownership.
    #[must_use]
    pub const fn candidate_pid(&self) -> Option<u32> {
        self.candidate_pid
    }
    /// Closed reason this observation remains pending.
    #[must_use]
    pub const fn cause(&self) -> StartupPendingCause {
        StartupPendingCause::ResponseBudget
    }
    /// The original command can be retried; it has not been submitted.
    #[must_use]
    pub const fn action(&self) -> StartupPendingAction {
        StartupPendingAction::RetryOriginalCommand
    }
    /// Last reported child stage, or the checked contender outcome.
    #[must_use]
    pub const fn phase(&self) -> StartupPhase {
        self.phase
    }
    /// Whether this candidate retired after conceding the existing owner lease.
    #[must_use]
    pub const fn contender_exited(&self) -> bool {
        self.contender_exited
    }
}

/// Closed reason for a bounded startup observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupPendingCause {
    /// The initiating client's response budget elapsed before endpoint readiness.
    ResponseBudget,
}
impl StartupPendingCause {
    /// Stable local response spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        "response-budget"
    }
}

/// Action permitted by a startup observation, without an operation receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupPendingAction {
    /// Retry the same command after the shared owner finishes opening.
    RetryOriginalCommand,
}
impl StartupPendingAction {
    /// Stable local response spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        "retry-original-command"
    }
}

/// Bounded startup either reaches an endpoint or hands back honest pending work.
#[derive(Debug)]
pub enum LocaldStartup {
    /// A local connection succeeded; callers still authenticate their session.
    Ready(PathBuf),
    /// The response budget elapsed. No user command has been submitted.
    Pending(StartupPending),
}

/// Frozen original path selection passed only to a matched owner executable.
/// Fields are private; default-root layout is closed and checked before writes.
#[derive(Debug)]
pub struct SpawnedOwnerBootstrap {
    paths: WorkspacePaths,
}

impl SpawnedOwnerBootstrap {
    fn configure(paths: &WorkspacePaths, command: &mut Command) -> Result<(), RuntimeError> {
        let (layout, anchor) = match paths.private_application_root.as_ref() {
            None => ("explicit", Path::new("")),
            Some(state) => {
                let layout = match state.suffix {
                    [] => "direct",
                    ["state"] => "xdg-data",
                    [".local", "state"] => "xdg-home",
                    ["Library", "Application Support"] => "mac-home",
                    _ => {
                        return Err(RuntimeError::InvalidPath(
                            "unsupported captured startup layout",
                        ));
                    }
                };
                (layout, state.anchor.as_path())
            }
        };
        command
            .arg(ARGUMENT)
            .arg(layout)
            .arg(anchor)
            .arg(paths.project())
            .arg(paths.data())
            .arg(paths.endpoint())
            .arg(paths.authority_secret());
        Ok(())
    }

    /// Consumes only the versioned automatic-start prefix. Old companions reject
    /// that argument before initializing anything; ordinary locald argv is unchanged.
    /// This must run before process configuration or filesystem initialization.
    ///
    /// # Errors
    /// Refuses incomplete, unknown or inconsistent original path selections.
    pub fn extract(
        args: impl IntoIterator<Item = String>,
    ) -> Result<(Option<Self>, Vec<String>), RuntimeError> {
        let mut args = args.into_iter();
        let Some(first) = args.next() else {
            return Ok((None, Vec::new()));
        };
        if first != ARGUMENT {
            return Ok((None, std::iter::once(first).chain(args).collect()));
        }
        let operands = args.by_ref().take(6).collect::<Vec<_>>();
        let [layout, anchor, project, data, endpoint, authority] = operands.as_slice() else {
            return Err(RuntimeError::InvalidPath(
                "automatic startup prefix is incomplete",
            ));
        };
        let bootstrap = Self::from_operands(layout, anchor, project, data, endpoint, authority)?;
        AUTOMATIC_PIPE.store(true, Ordering::Release);
        let _ = io::stdout().lock().write_all(PIPE_MAGIC);
        report_phase(StartupPhase::Admitted);
        Ok((Some(bootstrap), args.collect()))
    }

    fn from_operands(
        layout: &str,
        anchor: &str,
        project: &str,
        data: &str,
        endpoint: &str,
        authority: &str,
    ) -> Result<Self, RuntimeError> {
        let root = match layout {
            "explicit" if anchor.is_empty() => None,
            "direct" => Some(ApplicationStateRoot::new(PathBuf::from(anchor), &[])),
            "xdg-data" if cfg!(not(target_os = "macos")) => {
                Some(ApplicationStateRoot::new(PathBuf::from(anchor), &["state"]))
            }
            "xdg-home" if cfg!(not(target_os = "macos")) => Some(ApplicationStateRoot::new(
                PathBuf::from(anchor),
                &[".local", "state"],
            )),
            "mac-home" if cfg!(target_os = "macos") => Some(ApplicationStateRoot::new(
                PathBuf::from(anchor),
                &["Library", "Application Support"],
            )),
            _ => {
                return Err(RuntimeError::InvalidPath(
                    "automatic startup layout is invalid for this platform",
                ));
            }
        };
        let paths = WorkspacePaths {
            project: project.into(),
            data: data.into(),
            endpoint: endpoint.into(),
            authority_secret: authority.into(),
            private_application_root: root,
        };
        if [
            &paths.project,
            &paths.data,
            &paths.endpoint,
            &paths.authority_secret,
        ]
        .iter()
        .any(|path| !path.is_absolute())
            || paths.private_application_root.as_ref().is_some_and(|root| {
                !root.anchor.is_absolute()
                    || !same_path_identity(
                        &normalize_identity(&default_workspace_path(&paths.project, &root.path())),
                        &paths.data,
                    )
            })
        {
            return Err(RuntimeError::InvalidPath(
                "automatic startup paths do not match their captured identity",
            ));
        }
        Ok(Self { paths })
    }

    /// Checks the parsed owner operands against the original selection, then
    /// performs every existing private-directory, credential and fsync check.
    ///
    /// # Errors
    /// Refuses changed operands or any original private-state/credential check.
    pub fn initialize(
        &self,
        workspace: &Path,
        endpoint: &Path,
        authority: Option<&Path>,
    ) -> Result<(), RuntimeError> {
        if !same_path_identity(workspace, self.paths.data())
            || !same_path_identity(endpoint, self.paths.endpoint())
            || authority != Some(self.paths.authority_secret())
            || !same_path_identity(
                &normalize_identity(&std::env::current_dir().map_err(RuntimeError::Io)?),
                self.paths.project(),
            )
        {
            return Err(RuntimeError::InvalidPath(
                "automatic startup operands differ from the captured project session",
            ));
        }
        report_phase(StartupPhase::PrivateState);
        self.paths.initialize_data_directory()?;
        report_phase(StartupPhase::Authority);
        super::ensure_authority_secret(self.paths.authority_secret())?;
        report_phase(StartupPhase::Endpoint);
        if let Some(parent) = self.paths.endpoint().parent() {
            std::fs::create_dir_all(parent).map_err(RuntimeError::Io)?;
        }
        unlink_dead_endpoint(self.paths.endpoint());
        report_phase(StartupPhase::OpeningOwner);
        Ok(())
    }
}

fn report_phase(phase: StartupPhase) {
    if AUTOMATIC_PIPE.load(Ordering::Acquire) {
        let _ = writeln!(io::stdout().lock(), "phase:{}", phase.as_str());
    }
}

/// Reports a bounded terminal cause through an admitted automatic pipe.
/// A disconnected launcher never prevents the daemon from retiring normally.
#[must_use]
pub fn report_automatic_startup_failure(error: &dyn std::fmt::Display) -> bool {
    if !AUTOMATIC_PIPE.load(Ordering::Acquire) {
        return false;
    }
    let _ = io::stdout()
        .lock()
        .write_all(&startup::failure_record(error));
    true
}

struct Channel {
    bytes: Vec<u8>,
    observed: usize,
    header: bool,
    phase: StartupPhase,
    failure: Option<super::StartupDiagnostic>,
}

impl Channel {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            observed: 0,
            header: false,
            phase: StartupPhase::Starting,
            failure: None,
        }
    }
    fn read(
        &mut self,
        pipe: &mut impl io::Read,
        progress: &mut impl FnMut(StartupPhase),
    ) -> Result<(), RuntimeError> {
        let mut chunk = [0; 1024];
        loop {
            let Some(count) = backend_platform::child_output::read_available(pipe, &mut chunk)
                .map_err(RuntimeError::Io)?
            else {
                break;
            };
            if count == 0 {
                return self.finish();
            }
            self.observed = self.observed.saturating_add(count);
            if self.observed > MAX_PIPE_BYTES {
                return Err(RuntimeError::StartupChannel(
                    "startup channel exceeds its bound",
                ));
            }
            if self.failure.is_some() {
                return Err(RuntimeError::StartupChannel(
                    "record after terminal startup cause",
                ));
            }
            self.bytes.extend_from_slice(&chunk[..count]);
            if !self.header {
                if self.bytes.len() < PIPE_MAGIC.len() {
                    continue;
                }
                if !self.bytes.starts_with(PIPE_MAGIC) {
                    return Err(RuntimeError::CompanionProtocolMismatch);
                }
                self.bytes.drain(..PIPE_MAGIC.len());
                self.header = true;
            }
            while self.bytes.starts_with(b"phase:") {
                let Some(end) = self.bytes.iter().position(|byte| *byte == b'\n') else {
                    break;
                };
                let phase = StartupPhase::parse(&self.bytes[6..end])
                    .ok_or(RuntimeError::StartupChannel("unknown startup phase"))?;
                let expected = match self.phase {
                    StartupPhase::Starting => Some(StartupPhase::Admitted),
                    StartupPhase::Admitted => Some(StartupPhase::PrivateState),
                    StartupPhase::PrivateState => Some(StartupPhase::Authority),
                    StartupPhase::Authority => Some(StartupPhase::Endpoint),
                    StartupPhase::Endpoint => Some(StartupPhase::OpeningOwner),
                    StartupPhase::OpeningOwner | StartupPhase::AwaitingOwner => None,
                };
                if expected != Some(phase) {
                    return Err(RuntimeError::StartupChannel(
                        "startup phases are out of order",
                    ));
                }
                self.phase = phase;
                progress(phase);
                self.bytes.drain(..=end);
            }
            if !self.bytes.is_empty()
                && !self.bytes.starts_with(b"phase:")
                && !b"phase:".starts_with(&self.bytes)
            {
                if self.bytes.ends_with(startup::TERMINAL_FOOTER.as_bytes()) {
                    if self.phase == StartupPhase::Starting {
                        return Err(RuntimeError::StartupChannel(
                            "startup cause before companion admission",
                        ));
                    }
                    self.failure = Some(startup::StartupDiagnostic(
                        startup::terminal_cause(&self.bytes)
                            .map_err(RuntimeError::Io)?
                            .ok_or(RuntimeError::StartupChannel(
                                "missing terminal startup cause",
                            ))?,
                    ));
                    self.bytes.clear();
                } else if !startup::MAGIC.as_bytes().starts_with(&self.bytes)
                    && !self.bytes.starts_with(startup::MAGIC.as_bytes())
                {
                    return Err(RuntimeError::StartupChannel("invalid startup record"));
                }
            }
        }
        Ok(())
    }

    fn finish(&self) -> Result<(), RuntimeError> {
        // EOF after complete phases is legal: pipe lifetime is independent of
        // endpoint readiness. Buffered bytes, however, can never become a
        // complete phase or terminal record after the writer has closed.
        if self.bytes.is_empty() {
            return Ok(());
        }
        if !self.header {
            return Err(RuntimeError::CompanionProtocolMismatch);
        }
        Err(RuntimeError::StartupChannel(
            "startup channel ended with an incomplete or trailing record",
        ))
    }
}

/// Starts the same selected locald, with all durable setup inside that child.
/// The response clock starts before attachment and executable discovery.
/// A pending result does not submit, accept or complete the user's command.
/// Existing owner-lease contention and detached owner idle retirement remain authoritative.
/// Individual OS path lookup/spawn calls are not claimed to be interruptible.
/// Windows retains its existing synchronous [`super::ensure_locald`] route.
///
/// # Errors
/// Returns executable/spawn/protocol failures or the original owner startup refusal.
pub fn start_locald(
    paths: &WorkspacePaths,
    response_budget: Duration,
    mut progress: impl FnMut(StartupPhase),
) -> Result<LocaldStartup, RuntimeError> {
    let started = Instant::now();
    let deadline = started
        .checked_add(response_budget.min(LIVE_START_TIMEOUT))
        .unwrap_or(started);
    if response_budget.is_zero() {
        return Ok(not_started());
    }
    if backend_platform::local::connect_timeout(
        paths.endpoint(),
        POLL_INTERVAL.min(response_budget),
    )
    .is_ok()
    {
        return Ok(LocaldStartup::Ready(paths.endpoint().to_path_buf()));
    }
    progress(StartupPhase::Starting);
    // Capture relative overrides once in the launcher's CWD. No directory or
    // credential is created here, and an already-running owner takes the branch above.
    let paths = WorkspacePaths {
        project: paths.project.clone(),
        data: paths.data.clone(),
        private_application_root: paths.private_application_root.clone(),
        endpoint: super::lexically_absolute(paths.endpoint()),
        authority_secret: super::lexically_absolute(paths.authority_secret()),
    };
    super::validate_endpoint_length(paths.endpoint())?;
    let executable = locald_executable()?;
    let mut command = Command::new(&executable);
    SpawnedOwnerBootstrap::configure(&paths, &mut command)?;
    command
        .current_dir(paths.project())
        .arg("--endpoint")
        .arg(paths.endpoint())
        .arg("--workspace")
        .arg(paths.data())
        .arg("--authority-secret-file")
        .arg(paths.authority_secret())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // The pipe supersedes only this launcher's transient file receipt.
    command.env_remove(super::STARTUP_DIAGNOSTIC_ENV);
    detach(&mut command);
    if Instant::now() >= deadline {
        return Ok(not_started());
    }
    let child = command
        .spawn()
        .map_err(|source| RuntimeError::Spawn { executable, source })?;
    observe_owned_candidate(&paths, child, deadline, &mut progress)
}

/// Only Ready/Pending relinquishes the exact child to the existing owner
/// lifecycle. Errors and unwinding retain custody through kill and kernel wait.
/// Kernel process retirement, like spawn itself, is not claimed interruptible.
struct StartupChild {
    child: Child,
    handed_off: bool,
}
impl Drop for StartupChild {
    fn drop(&mut self) {
        if !self.handed_off {
            // Child retains its cached exit status after try_wait, so this
            // cannot signal a PID that was already reaped and reused.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn observe_owned_candidate(
    paths: &WorkspacePaths,
    child: Child,
    deadline: Instant,
    progress: &mut impl FnMut(StartupPhase),
) -> Result<LocaldStartup, RuntimeError> {
    let mut candidate = StartupChild {
        child,
        handed_off: false,
    };
    let mut pipe = candidate
        .child
        .stdout
        .take()
        .ok_or(RuntimeError::StartupChannel("startup pipe missing"))?;
    backend_platform::child_output::configure(&pipe).map_err(RuntimeError::Io)?;
    let result = wait_for_owner(paths, &mut candidate.child, &mut pipe, deadline, progress)?;
    candidate.handed_off = true;
    Ok(result)
}

fn not_started() -> LocaldStartup {
    LocaldStartup::Pending(StartupPending {
        candidate_pid: None,
        phase: StartupPhase::Starting,
        contender_exited: false,
    })
}

fn wait_for_owner(
    paths: &WorkspacePaths,
    child: &mut Child,
    pipe: &mut ChildStdout,
    mut deadline: Instant,
    progress: &mut impl FnMut(StartupPhase),
) -> Result<LocaldStartup, RuntimeError> {
    let mut channel = Channel::new();
    let mut child_exit = None;
    loop {
        channel.read(pipe, progress)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero()
            && backend_platform::local::connect_timeout(
                paths.endpoint(),
                remaining.min(POLL_INTERVAL),
            )
            .is_ok()
        {
            return Ok(LocaldStartup::Ready(paths.endpoint().to_path_buf()));
        }
        if child_exit.is_none()
            && let Some(status) = child.try_wait().map_err(RuntimeError::Io)?
        {
            child_exit = Some(status.code());
            deadline = super::deadline_after_exit(deadline, Instant::now(), status.code());
            channel.read(pipe, progress)?;
            if !channel.header {
                return Err(RuntimeError::CompanionProtocolMismatch);
            }
            if status.code() == Some(i32::from(super::OWNER_CONTENDED_EXIT_CODE)) {
                channel.phase = StartupPhase::AwaitingOwner;
                progress(channel.phase);
            }
        }
        if Instant::now() >= deadline {
            if let Some(code) = child_exit
                && code != Some(i32::from(super::OWNER_CONTENDED_EXIT_CODE))
            {
                return Err(RuntimeError::DaemonExited {
                    code,
                    diagnostic: channel.failure,
                });
            }
            return Ok(LocaldStartup::Pending(StartupPending {
                candidate_pid: Some(child.id()),
                phase: channel.phase,
                contender_exited: child_exit.is_some(),
            }));
        }
        std::thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn paths(root: &Path) -> WorkspacePaths {
        WorkspacePaths {
            project: normalize_identity(&std::env::current_dir().expect("fixture cwd")),
            data: root.join("absent-state"),
            endpoint: root.join("owner.sock"),
            authority_secret: root.join("absent-state/authority.secret"),
            private_application_root: None,
        }
    }

    struct OwnedChild(Child);
    impl std::ops::Deref for OwnedChild {
        type Target = Child;
        fn deref(&self) -> &Child {
            &self.0
        }
    }
    impl std::ops::DerefMut for OwnedChild {
        fn deref_mut(&mut self) -> &mut Child {
            &mut self.0
        }
    }
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
            }
            let _ = self.0.wait();
        }
    }

    struct Chunks<'a> {
        bytes: &'a [u8],
        size: usize,
        pause: bool,
    }
    impl io::Read for Chunks<'_> {
        fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
            if self.pause {
                self.pause = false;
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let count = self.bytes.len().min(into.len()).min(self.size);
            into[..count].copy_from_slice(&self.bytes[..count]);
            self.bytes = &self.bytes[count..];
            self.pause = true;
            Ok(count)
        }
    }
    fn receive(bytes: &[u8], size: usize) -> Result<(Channel, Vec<StartupPhase>), RuntimeError> {
        let mut reader = Chunks {
            bytes,
            size,
            pause: false,
        };
        let mut channel = Channel::new();
        let mut phases = Vec::new();
        while !reader.bytes.is_empty() {
            channel.read(&mut reader, &mut |phase| phases.push(phase))?;
        }
        // Deliver actual EOF, including for a record received in one chunk.
        reader.pause = false;
        channel.read(&mut reader, &mut |phase| phases.push(phase))?;
        Ok((channel, phases))
    }
    fn all_phases() -> Vec<u8> {
        let mut record = PIPE_MAGIC.to_vec();
        record.extend_from_slice(b"phase:admitted\nphase:private-state\nphase:authority\nphase:endpoint\nphase:opening-owner\n");
        record
    }

    #[test]
    fn fragmented_phases_and_original_bounded_cause_have_one_exact_interpretation() {
        let mut bytes = all_phases();
        let original = format!("original authority refusal: {}\u{0}", "é".repeat(8192));
        bytes.extend_from_slice(&startup::failure_record(&original));
        for size in [1, 2, 5, PIPE_MAGIC.len(), 1024, bytes.len()] {
            let (channel, phases) = receive(&bytes, size).expect("every byte split is valid");
            assert_eq!(
                phases,
                [
                    StartupPhase::Admitted,
                    StartupPhase::PrivateState,
                    StartupPhase::Authority,
                    StartupPhase::Endpoint,
                    StartupPhase::OpeningOwner
                ]
            );
            let cause = channel
                .failure
                .expect("original terminal cause")
                .to_string();
            assert!(cause.starts_with("original authority refusal: "));
            assert!(cause.len() <= 8192);
            assert_eq!(cause.chars().last(), Some('é'));
            assert!(channel.bytes.is_empty());
        }
        for size in 1..=all_phases().len() {
            let (channel, _) = receive(&all_phases(), size).expect("healthy phase-only EOF");
            assert_eq!(channel.phase, StartupPhase::OpeningOwner);
            assert!(
                channel.failure.is_none(),
                "EOF is not an invented owner failure"
            );
        }
        let bounded = startup::failure_record(&"bounded\0\u{1b}cause");
        assert_eq!(
            startup::terminal_cause(&bounded).expect("bounded cause"),
            Some("bounded��cause".to_owned())
        );
    }

    #[test]
    fn invalid_or_repeated_phase_and_oversized_or_extra_terminal_records_refuse() {
        for body in [
            b"phase:authority\n".as_slice(),
            b"phase:unknown\n",
            b"phase:admitted\nphase:admitted\n",
            b"phase:admitted\nphase:endpoint\n",
        ] {
            let mut bytes = PIPE_MAGIC.to_vec();
            bytes.extend_from_slice(body);
            assert!(receive(&bytes, 1).is_err());
        }
        let mut oversized = all_phases();
        oversized.extend_from_slice(startup::MAGIC.as_bytes());
        oversized.extend_from_slice(b"terminal:8192\n");
        oversized.extend(std::iter::repeat_n(b'x', MAX_PIPE_BYTES));
        let error = receive(&oversized, 1024)
            .err()
            .expect("input bound must refuse");
        assert!(error.to_string().contains("exceeds its bound"));
        let failure = startup::failure_record(&"original é cause");
        for trailing in [
            b"phase:opening-owner\n".as_slice(),
            b"x",
            startup::MAGIC.as_bytes(),
        ] {
            let mut extra = all_phases();
            extra.extend_from_slice(&failure);
            extra.extend_from_slice(trailing);
            // All chunk sizes include the one-read case that must finalize
            // buffered terminal-plus-trailing bytes instead of waiting forever.
            for size in 1..=extra.len() {
                assert!(
                    receive(&extra, size).is_err(),
                    "trailing bytes at chunk size {size}"
                );
            }
        }
        for end in 1..failure.len() {
            let mut incomplete = all_phases();
            incomplete.extend_from_slice(&failure[..end]);
            for size in [1, incomplete.len()] {
                assert!(
                    receive(&incomplete, size).is_err(),
                    "incomplete terminal prefix {end}, chunk {size}"
                );
            }
        }
        for suffix in [b"p".as_slice(), b"phase:", b"phase:authority"] {
            let mut incomplete = PIPE_MAGIC.to_vec();
            incomplete.extend_from_slice(b"phase:admitted\n");
            incomplete.extend_from_slice(suffix);
            assert!(receive(&incomplete, incomplete.len()).is_err());
        }
        assert!(matches!(
            receive(b"unmatched executable stdout\n", 1),
            Err(RuntimeError::CompanionProtocolMismatch)
        ));
    }

    #[test]
    fn captured_default_root_and_owner_operands_are_checked_before_any_write() {
        let root = super::super::tests::test_directory("bootstrap-frozen-default");
        std::fs::create_dir_all(&root).expect("owned fixture root");
        let project = normalize_identity(&std::env::current_dir().expect("cwd"));
        let root = normalize_identity(&root);
        let data = default_workspace_path(&project, &root);
        let endpoint = root.join("owner.sock");
        let authority = data.join("authority.secret");
        let text = |path: &Path| path.to_str().expect("fixture Unicode").to_owned();
        let bootstrap = SpawnedOwnerBootstrap::from_operands(
            "direct",
            &text(&root),
            &text(&project),
            &text(&data),
            &text(&endpoint),
            &text(&authority),
        )
        .expect("captured original layout");
        assert_eq!(
            bootstrap.paths.private_application_root,
            Some(ApplicationStateRoot::new(root.clone(), &[]))
        );
        // Discovery normalizes the data identity but preserves the original
        // anchor spelling for private-capability admission in the child.
        let anchor_alias = root.join(".");
        let alias_bootstrap = SpawnedOwnerBootstrap::from_operands(
            "direct",
            &text(&anchor_alias),
            &text(&project),
            &text(&data),
            &text(&endpoint),
            &text(&authority),
        )
        .expect("same captured root through an original lexical alias");
        assert_eq!(alias_bootstrap.paths.data(), bootstrap.paths.data());
        assert_eq!(
            alias_bootstrap.paths.private_application_root,
            Some(ApplicationStateRoot::new(anchor_alias, &[]))
        );
        assert!(
            !data.exists(),
            "decoding an alias proof still performs no writes"
        );
        assert!(
            SpawnedOwnerBootstrap::from_operands(
                "direct",
                &text(&root),
                &text(&project),
                &text(&root.join("wrong-project-key")),
                &text(&endpoint),
                &text(&authority)
            )
            .is_err()
        );
        assert!(
            SpawnedOwnerBootstrap::from_operands(
                "unknown",
                &text(&root),
                &text(&project),
                &text(&data),
                &text(&endpoint),
                &text(&authority)
            )
            .is_err()
        );
        assert!(
            SpawnedOwnerBootstrap::from_operands(
                "explicit",
                "",
                &text(&project),
                "relative",
                &text(&endpoint),
                &text(&authority)
            )
            .is_err()
        );
        assert!(
            bootstrap
                .initialize(&data, &root.join("wrong.sock"), Some(&authority))
                .is_err()
        );
        assert!(
            bootstrap
                .initialize(&data, &endpoint, Some(&root.join("wrong-secret")))
                .is_err()
        );
        assert!(
            !data.exists(),
            "operand refusals must not create the private tree"
        );
        bootstrap
            .initialize(&data, &endpoint, Some(&authority))
            .expect("same durable initialization in child seam");
        assert_eq!(std::fs::metadata(&authority).expect("credential").len(), 32);
        assert_eq!(
            std::fs::metadata(&authority)
                .expect("credential permissions")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&data)
                .expect("private workspace")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let before = std::fs::read(&authority).expect("credential bytes");
        bootstrap
            .initialize(&data, &endpoint, Some(&authority))
            .expect("reopen same admitted credential");
        assert_eq!(std::fs::read(&authority).expect("same credential"), before);
        std::fs::remove_dir_all(root).expect("remove owned fixture");
    }

    #[test]
    fn already_running_and_zero_budget_paths_create_no_state_or_candidate() {
        let root = super::super::tests::socket_test_directory("startup-live-or-zero");
        std::fs::create_dir_all(&root).expect("fixture root");
        let paths = paths(&root);
        let LocaldStartup::Pending(pending) = start_locald(&paths, Duration::ZERO, |_| {
            panic!("zero budget must not dispatch")
        })
        .expect("pending without dispatch") else {
            panic!("not ready");
        };
        assert_eq!(pending.candidate_pid(), None);
        assert_eq!(pending.phase(), StartupPhase::Starting);
        let listener = std::os::unix::net::UnixListener::bind(paths.endpoint())
            .expect("live existing endpoint");
        let result = start_locald(&paths, Duration::from_secs(1), |_| {
            panic!("existing owner must not dispatch")
        })
        .expect("attach existing");
        assert!(
            matches!(result, LocaldStartup::Ready(endpoint) if endpoint.as_path() == paths.endpoint())
        );
        assert!(!paths.data().exists());
        assert!(!paths.authority_secret().exists());
        drop(listener);
        std::fs::remove_dir_all(root).expect("remove fixture");
    }

    // This controlled pipe peer is not an installed product fixture. It proves
    // the waiting client hands back Pending while its exact admitted child is
    // blocked before filesystem setup, without a helper thread or leaked child.
    #[test]
    fn owned_child_can_remain_pending_before_private_state_and_then_retire_exactly() {
        let root = super::super::tests::socket_test_directory("startup-pending-child");
        std::fs::create_dir_all(&root).expect("fixture root");
        let paths = paths(&root);
        let mut child = OwnedChild(Command::new("/bin/sh")
            .args(["-c", "printf 'nudox.local-startup-pipe.v1\\nphase:admitted\\nphase:private-state\\n'; exec 1>&-; IFS= read -r release"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().expect("owned pipe peer"));
        let mut pipe = child.stdout.take().expect("pipe");
        backend_platform::child_output::configure(&pipe).expect("nonblocking pipe");
        let started = Instant::now();
        let mut phases = Vec::new();
        let result = wait_for_owner(
            &paths,
            &mut child,
            &mut pipe,
            started + Duration::from_millis(200),
            &mut |phase| phases.push(phase),
        );
        let still_running = child.try_wait().expect("observe exact child").is_none();
        // Kernel retirement precedes assertions, including any failed result.
        if let Some(mut input) = child.stdin.take() {
            let _ = input.write_all(b"release\n");
        }
        let status = child.wait().expect("kernel-wait own child");
        assert!(status.success());
        let LocaldStartup::Pending(pending) = result.expect("bounded pending observation") else {
            panic!("not ready");
        };
        assert_eq!(pending.candidate_pid(), Some(child.id()));
        assert_eq!(pending.phase(), StartupPhase::PrivateState);
        assert!(!pending.contender_exited());
        assert_eq!(pending.cause(), StartupPendingCause::ResponseBudget);
        assert_eq!(pending.action(), StartupPendingAction::RetryOriginalCommand);
        assert!(still_running);
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(!paths.data().exists());
        assert_eq!(phases, [StartupPhase::Admitted, StartupPhase::PrivateState]);
        std::fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn unmatched_companion_exit_has_a_rebuild_action_without_creating_state() {
        let root = super::super::tests::socket_test_directory("startup-old-companion");
        std::fs::create_dir_all(&root).expect("fixture root");
        let paths = paths(&root);
        let mut child = OwnedChild(
            Command::new("/bin/sh")
                .args(["-c", "exit 64"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("old protocol fixture"),
        );
        let mut pipe = child.stdout.take().expect("pipe");
        backend_platform::child_output::configure(&pipe).expect("nonblocking pipe");
        let result = wait_for_owner(
            &paths,
            &mut child,
            &mut pipe,
            Instant::now() + Duration::from_secs(1),
            &mut |_| {},
        );
        child.wait().expect("kernel-wait fixture");
        let error = result.expect_err("old companion must not masquerade as pending");
        assert!(matches!(error, RuntimeError::CompanionProtocolMismatch));
        assert!(
            error
                .to_string()
                .contains("install matched CLI and locald or rebuild")
        );
        assert!(!paths.data().exists());
        std::fs::remove_dir_all(root).expect("remove fixture");
    }
    #[test]
    fn original_failed_child_and_retired_contender_have_distinct_outcomes() {
        let root = super::super::tests::socket_test_directory("startup-failure-or-contender");
        std::fs::create_dir_all(&root).expect("fixture root");
        let paths = paths(&root);
        for (code, expected_cause) in [
            (70, Some("original checked credential refusal")),
            (75, None),
        ] {
            let mut record = PIPE_MAGIC.to_vec();
            record.extend_from_slice(b"phase:admitted\n");
            if let Some(cause) = expected_cause {
                record.extend_from_slice(&startup::failure_record(&cause));
            }
            let mut child = OwnedChild(
                Command::new("/bin/sh")
                    .args([
                        "-c",
                        "printf '%s' \"$1\"; exit \"$2\"",
                        "fixture",
                        std::str::from_utf8(&record).expect("UTF8 record"),
                        &code.to_string(),
                    ])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("owned protocol peer"),
            );
            let mut pipe = child.stdout.take().expect("pipe");
            backend_platform::child_output::configure(&pipe).expect("nonblocking pipe");
            let result = wait_for_owner(
                &paths,
                &mut child,
                &mut pipe,
                Instant::now() + Duration::from_millis(200),
                &mut |_| {},
            );
            child.wait().expect("kernel-wait owned peer");
            if let Some(cause) = expected_cause {
                let RuntimeError::DaemonExited { code, diagnostic } =
                    result.expect_err("actual failure")
                else {
                    panic!("typed original cause required");
                };
                assert_eq!(code, Some(70));
                assert_eq!(diagnostic.expect("original cause").to_string(), cause);
            } else {
                let LocaldStartup::Pending(pending) = result.expect("wait for actual lease winner")
                else {
                    panic!("no winner endpoint");
                };
                assert_eq!(pending.phase(), StartupPhase::AwaitingOwner);
                assert_eq!(pending.candidate_pid(), Some(child.id()));
                assert!(pending.contender_exited());
            }
            assert!(!paths.data().exists());
        }
        std::fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn retired_contender_waits_for_winner_endpoint_before_reporting_ready() {
        use std::os::unix::net::UnixListener;
        use std::sync::mpsc;

        let root = super::super::tests::socket_test_directory("startup-contender-winner");
        std::fs::create_dir_all(&root).expect("fixture root");
        let paths = paths(&root);
        let fixture_deadline = Instant::now() + Duration::from_secs(2);
        let mut child = OwnedChild(
            Command::new("/bin/sh")
                .args([
                    "-c",
                    "printf 'nudox.local-startup-pipe.v1\\nphase:admitted\\n'; exit 75",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("owned contending candidate"),
        );
        let mut pipe = child.stdout.take().expect("startup pipe");
        backend_platform::child_output::configure(&pipe).expect("nonblocking pipe");

        // Bind the simulated winner only after the loser reports exit 75. The
        // child's status is a wait condition, never evidence of readiness.
        let (contended_tx, contended_rx) = mpsc::channel();
        let mut contended_tx = Some(contended_tx);
        let endpoint = paths.endpoint().to_path_buf();
        let winner = std::thread::spawn(move || {
            contended_rx
                .recv_timeout(fixture_deadline.saturating_duration_since(Instant::now()))
                .expect("contender outcome observed");
            let listener = UnixListener::bind(endpoint).expect("winner endpoint bind");
            listener
                .set_nonblocking(true)
                .expect("bounded winner accept");
            loop {
                match listener.accept() {
                    Ok((connection, _)) => {
                        drop(connection);
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < fixture_deadline,
                            "launcher must connect before the fixture deadline"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("winner accept failed: {error}"),
                }
            }
        });
        let result = wait_for_owner(
            &paths,
            &mut child,
            &mut pipe,
            fixture_deadline,
            &mut |phase| {
                if phase == StartupPhase::AwaitingOwner
                    && let Some(contended_tx) = contended_tx.take()
                {
                    contended_tx.send(()).expect("signal winner fixture");
                }
            },
        );
        child.wait().expect("kernel-wait contending candidate");
        winner.join().expect("winner accepted one readiness probe");
        assert!(
            matches!(
                result,
                Ok(LocaldStartup::Ready(endpoint)) if endpoint.as_path() == paths.endpoint()
            ),
            "exit 75 must wait for a successful connection to the winner"
        );
        assert!(!paths.data().exists());
        std::fs::remove_dir_all(root).expect("remove fixture");
    }

    // The PID remains this test's direct, unreaped child until waitpid below.
    // This guard never uses a name, discovery scan, or another owner's PID.
    struct UnreapedPeer(rustix::process::Pid);
    impl Drop for UnreapedPeer {
        fn drop(&mut self) {
            use rustix::process::{Signal, WaitOptions, kill_process, waitpid};
            if matches!(waitpid(Some(self.0), WaitOptions::NOHANG), Ok(None)) {
                let _ = kill_process(self.0, Signal::KILL);
                let _ = waitpid(Some(self.0), WaitOptions::empty());
            }
        }
    }

    #[test]
    fn live_malformed_or_unwinding_candidate_is_kernel_retired() {
        use rustix::process::{Pid, WaitOptions, waitpid};
        let root = super::super::tests::socket_test_directory("startup-live-invalid");
        std::fs::create_dir_all(&root).expect("fixture root");
        let paths = paths(&root);
        for unwind in [false, true] {
            let script = if unwind {
                "printf 'nudox.local-startup-pipe.v1\\nphase:admitted\\n'; kill -STOP $$"
            } else {
                "printf 'invalid-live-companion-protocol\\n'; kill -STOP $$"
            };
            let child = Command::new("/bin/sh")
                .args(["-c", script])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("owned live peer");
            let pid = Pid::from_raw(child.id().cast_signed()).expect("child PID");
            let peer = UnreapedPeer(pid);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                observe_owned_candidate(
                    &paths,
                    child,
                    Instant::now() + Duration::from_secs(2),
                    &mut |_| {
                        if unwind {
                            panic!("controlled progress callback unwind");
                        }
                    },
                )
            }));
            if unwind {
                assert!(result.is_err());
            } else {
                assert!(matches!(
                    result.expect("ordinary error"),
                    Err(RuntimeError::CompanionProtocolMismatch)
                ));
            }
            assert!(
                matches!(
                    waitpid(Some(pid), WaitOptions::NOHANG),
                    Err(rustix::io::Errno::CHILD)
                ),
                "the owning guard must kernel-wait its exact invalid child before returning"
            );
            drop(peer);
        }
        assert!(!paths.data().exists());
        std::fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn owned_guard_explicitly_hands_off_a_live_valid_pending_candidate() {
        use rustix::process::{Pid, WaitOptions, waitpid};
        let root = super::super::tests::socket_test_directory("startup-live-valid");
        std::fs::create_dir_all(&root).expect("fixture root");
        let paths = paths(&root);
        let child = Command::new("/bin/sh")
            .args(["-c", "printf 'nudox.local-startup-pipe.v1\\nphase:admitted\\nphase:private-state\\n'; kill -STOP $$"])
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().expect("owned valid peer");
        let pid = Pid::from_raw(child.id().cast_signed()).expect("child PID");
        let peer = UnreapedPeer(pid);
        let result = observe_owned_candidate(
            &paths,
            child,
            Instant::now() + Duration::from_millis(300),
            &mut |_| {},
        );
        let live = matches!(waitpid(Some(pid), WaitOptions::NOHANG), Ok(None));
        drop(peer); // exact owned child is killed and kernel-waited before assertions
        let LocaldStartup::Pending(pending) = result.expect("valid pending handoff") else {
            panic!("no endpoint");
        };
        assert!(
            live,
            "valid Pending must not silently retire the owner candidate"
        );
        assert_eq!(
            pending.candidate_pid(),
            Some(pid.as_raw_pid().cast_unsigned())
        );
        assert_eq!(pending.phase(), StartupPhase::PrivateState);
        assert!(!pending.contender_exited());
        assert!(!paths.data().exists());
        std::fs::remove_dir_all(root).expect("remove fixture");
    }
}
