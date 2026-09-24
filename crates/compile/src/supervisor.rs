//! Cold process supervision with bounded file output and explicit reaping.

use crate::{
    Cancellation, ProcessError,
    process::{
        ExecutableLease, ProcessReceipt, ProcessStdin, ProcessTerminal, SupervisedCommand, receipt,
    },
};
use std::{
    fs::{File, OpenOptions, remove_file, symlink_metadata},
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    /// verification, temporary I/O, or process startup fails.
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
        cancellation.checkpoint().map_err(ProcessError::from)?;
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
        let child = spawn(
            &self.command,
            executable.path(),
            stdin.as_ref(),
            &stdout.file,
            &stderr.file,
        )?;
        Ok(RunningProcess {
            command: self.command.clone(),
            child,
            _stdin: stdin,
            _executable: executable,
            stdout,
            stderr,
            workspace_baseline,
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
        self.start_with_cancellation(cancellation)?
            .finish_with_cancellation(cancellation)
    }
}

/// A spawned process that has not yet reached its reaped terminal state.
///
/// This type is intentionally affine: consuming it through
/// [`RunningProcess::finish`] or [`RunningProcess::finish_with_cancellation`]
/// is the only way to obtain a [`ProcessReceipt`].
pub struct RunningProcess {
    command: SupervisedCommand,
    child: Child,
    _stdin: Option<TempInput>,
    _executable: ExecutableLease,
    stdout: TempOutput,
    stderr: TempOutput,
    workspace_baseline: usize,
}

impl std::fmt::Debug for RunningProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningProcess")
            .field("program", &self.command.program())
            .field("pid", &self.child.id())
            .finish_non_exhaustive()
    }
}

impl Drop for RunningProcess {
    fn drop(&mut self) {
        // Dropping a running capability must not orphan the authority. A
        // completed child returns immediately; an active one is torn down as
        // a best-effort cleanup because `Drop` cannot report an error.
        let _ = terminate_process_tree(&mut self.child);
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
        mut self,
        cancellation: &Cancellation,
    ) -> Result<ProcessReceipt, ProcessError> {
        let start = Instant::now();
        let deadline = start
            .checked_add(self.command.limits().wall_time())
            .ok_or(ProcessError::Deadline)?;

        let status = loop {
            if cancellation.is_cancelled() {
                terminate_process_tree(&mut self.child)?;
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
                let _ = terminate_process_tree(&mut self.child);
                return Err(ProcessError::OutputLimit);
            }

            if self.workspace_grew_beyond_limit()? {
                terminate_process_tree(&mut self.child)?;
                return Err(ProcessError::WorkspaceLimit);
            }

            if let Some(status) = self.child.try_wait().map_err(|_| ProcessError::Io)? {
                break status;
            }

            if Instant::now() >= deadline {
                let _ = terminate_process_tree(&mut self.child);
                return Err(ProcessError::Deadline);
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
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
        // NixOS does not provide /bin/sh. The authority host supplies an
        // absolute, pinned shell when resource limits need the POSIX wrapper.
        let shell = std::env::var_os("NUDOX_PROCESS_SHELL")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/bin/sh"));
        let mut process = Command::new(shell);
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

pub(crate) fn terminate_process_tree(child: &mut Child) -> Result<(), ProcessError> {
    if child
        .try_wait()
        .map_err(|_| ProcessError::NotReaped)?
        .is_some()
    {
        return Ok(());
    }
    let group_result = terminate_process_group(child.id());
    let kill_result = child.kill();
    let wait_result = child.wait();
    if wait_result.is_err() {
        return Err(ProcessError::NotReaped);
    }
    // A child can exit between the initial poll and the group signal. In
    // that race kill reports an absent direct child even though wait
    // successfully reaped it; preserve the caller's primary limit/cancel
    // error. If the direct kill succeeded while group teardown was
    // unavailable, retain the typed capability failure so callers cannot
    // mistake a direct-child kill for process-tree cleanup.
    if let Err(error) = group_result
        && kill_result.is_ok()
        && matches!(error, ProcessError::UnsupportedLimit(_))
    {
        return Err(error);
    }
    Ok(())
}

pub(crate) fn configure_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = command;
    }
}

fn terminate_process_group(pid: u32) -> Result<(), ProcessError> {
    #[cfg(unix)]
    {
        let pid = i32::try_from(pid).map_err(|_| ProcessError::NotReaped)?;
        let argument = format!("-{pid}");
        for executable in [Path::new("/bin/kill"), Path::new("/usr/bin/kill")] {
            if !executable.is_file() {
                continue;
            }
            let result = Command::new(executable)
                .arg("-KILL")
                .arg("--")
                .arg(&argument)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if result.is_ok_and(|status| status.success()) {
                return Ok(());
            }
        }
        Err(ProcessError::UnsupportedLimit(
            crate::UnsupportedLimit::ProcessGroup,
        ))
    }
    #[cfg(not(unix))]
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
