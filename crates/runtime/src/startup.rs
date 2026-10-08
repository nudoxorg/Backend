//! A bounded private failure receipt for a detached owner startup.
//!
//! This channel contains one terminal startup cause, never an environment or
//! a lifetime stderr stream. It cannot keep the owner attached to a launching
//! CLI, block a pipe, or grow after that CLI has accepted the endpoint.

use std::fmt::{self, Write as _};
use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(any(windows, test))]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
pub(crate) mod admission;
#[cfg(unix)]
static OWNER_ADMISSION: std::sync::OnceLock<admission::StartupOwner> = std::sync::OnceLock::new();

/// Startup work observed before an endpoint accepts connections. No phase is readiness.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum StartupPhase {
    /// Selecting the private ephemeral admission directory.
    PreparingLaunch,
    /// Another detached owner holds startup admission.
    WaitingAdmission,
    /// Finding the installed companion executable.
    SelectingExecutable,
    /// Creating the detached process.
    Spawning,
    /// Admitting private workspace directories and the authority credential.
    PreparingWorkspace,
    /// Acquiring durable ownership and recovering the store.
    OpeningOwner,
    /// Capturing installed compiler tools and opening their host.
    SelectingCompilers,
    /// Recovering and reconciling semantic authority.
    RecoveringSemanticState,
    /// Recovering and publishing the derived view.
    PublishingView,
    /// Binding the endpoint after composition succeeds.
    BindingListener,
    /// Listener bound; a client must still connect to establish readiness.
    Listening,
}
impl StartupPhase {
    #[cfg(unix)]
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::PreparingLaunch => "preparing-launch",
            Self::WaitingAdmission => "waiting-admission",
            Self::SelectingExecutable => "selecting-executable",
            Self::Spawning => "spawning",
            Self::PreparingWorkspace => "preparing-workspace",
            Self::OpeningOwner => "opening-owner",
            Self::SelectingCompilers => "selecting-compilers",
            Self::RecoveringSemanticState => "recovering-semantic-state",
            Self::PublishingView => "publishing-view",
            Self::BindingListener => "binding-listener",
            Self::Listening => "listening",
        }
    }
    #[cfg(unix)]
    pub(crate) fn from_code(code: &str) -> Option<Self> {
        [
            Self::PreparingLaunch,
            Self::WaitingAdmission,
            Self::SelectingExecutable,
            Self::Spawning,
            Self::PreparingWorkspace,
            Self::OpeningOwner,
            Self::SelectingCompilers,
            Self::RecoveringSemanticState,
            Self::PublishingView,
            Self::BindingListener,
            Self::Listening,
        ]
        .into_iter()
        .find(|phase| phase.code() == code)
    }
}
impl fmt::Display for StartupPhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PreparingLaunch => "preparing launch",
            Self::WaitingAdmission => "waiting for workspace startup",
            Self::SelectingExecutable => "finding the local owner",
            Self::Spawning => "starting the local owner",
            Self::PreparingWorkspace => "preparing the private workspace",
            Self::OpeningOwner => "opening the workspace store",
            Self::SelectingCompilers => "selecting language tools",
            Self::RecoveringSemanticState => "recovering semantic state",
            Self::PublishingView => "publishing the workspace view",
            Self::BindingListener => "binding the local endpoint",
            Self::Listening => "checking the local endpoint",
        })
    }
}

/// Path-free startup observation measured with the caller's monotonic clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StartupProgress {
    /// Work currently reported by the launcher or its admitted owner.
    pub phase: StartupPhase,
    /// Time since this caller began ensuring the endpoint.
    pub elapsed: std::time::Duration,
    /// Local generation counter; phases only advance within one attempt.
    pub attempt: u64,
}

/// Adopts automatic Unix startup admission before composition or child spawning.
/// Direct/manual owners have no admission to adopt.
///
/// # Errors
/// Rejects an explicitly configured unsafe or mismatched inherited channel.
pub fn adopt_startup_admission() -> io::Result<bool> {
    #[cfg(unix)]
    {
        if let Some(owner) = admission::StartupOwner::from_environment()? {
            OWNER_ADMISSION
                .set(owner)
                .map_err(|_| io::Error::other("startup admission already adopted"))?;
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn admits_startup_paths(workspace: &Path, endpoint: &Path) -> bool {
    #[cfg(unix)]
    {
        OWNER_ADMISSION
            .get()
            .is_some_and(|owner| owner.admits_paths(workspace, endpoint))
    }
    #[cfg(not(unix))]
    {
        let _ = (workspace, endpoint);
        false
    }
}

/// Publishes advisory progress for an automatically launched owner. Failures
/// cannot alter durable ownership, composition or readiness.
pub fn report_startup_phase(phase: StartupPhase) {
    #[cfg(unix)]
    if let Some(owner) = OWNER_ADMISSION.get() {
        let _ = owner.phase(phase);
    }
    #[cfg(not(unix))]
    let _ = phase;
}

/// Private failure-receipt path installed only on an automatically spawned owner.
pub const STARTUP_DIAGNOSTIC_ENV: &str = "BACKEND_LOCALD_STARTUP_DIAGNOSTIC_FILE";
const MAGIC: &str = "nudox.local-startup-failure.v1\n";
const TERMINAL_PREFIX: &str = "terminal:";
const CONTENDED_PREFIX: &str = "owner-contended:";
const TERMINAL_FOOTER: &str = "\n--nudox.local-startup-failure.complete--\n";
const MAX_CAUSE_BYTES: usize = 8192;
const MAX_CAUSE_DIGITS: usize = 4;
#[cfg(any(windows, test))]
static ATTEMPT: AtomicU64 = AtomicU64::new(0);

/// The original owner refusal, bounded at its producer and checked on reading.
#[derive(Debug)]
pub struct StartupDiagnostic {
    cause: String,
    owner_contended: bool,
}
impl StartupDiagnostic {
    /// The producer explicitly reported durable-owner admission contention.
    /// This is a refusal classification, never proof that an owner is live.
    #[must_use]
    pub const fn owner_contended(&self) -> bool {
        self.owner_contended
    }
}

impl fmt::Display for StartupDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.cause)
    }
}

pub(crate) struct StartupAttempt {
    path: PathBuf,
    directory: backend_platform::DirectoryCapability,
    name: String,
    file: File,
    cleanup_on_drop: bool,
}

impl StartupAttempt {
    #[cfg(any(windows, test))]
    pub(crate) fn prepare(workspace: &Path) -> io::Result<Self> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let path = workspace.join(format!(
            ".startup.{}.{}.{}.status",
            std::process::id(),
            nonce,
            ATTEMPT.fetch_add(1, Ordering::Relaxed),
        ));
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "startup failure channel needs a Unicode file name",
                )
            })?
            .to_owned();
        Self::create(workspace, name)
    }

    /// One receipt name per workspace. A new admitted generation can remove
    /// only a known bounded stale receipt, never an unrelated private file.
    pub(crate) fn prepare_reserved(workspace: &Path, name: String) -> io::Result<Self> {
        let directory = backend_platform::DirectoryCapability::open(workspace)?;
        directory.validate_private()?;
        match directory.open_private_file_read_write(&name, false) {
            Ok(mut old) => {
                old.try_lock().map_err(io::Error::other)?;
                terminal_cause(&read_record(&mut old)?)?;
                let original = backend_platform::FileIdentity::of_file(&old)?;
                let named = directory.open_private_file_read_write(&name, false)?;
                if backend_platform::FileIdentity::of_file(&named)? != original {
                    return Err(io::Error::other("stale startup receipt inode changed"));
                }
                remove_ephemeral(&directory, workspace, &name)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        Self::create(workspace, name)
    }

    fn create(workspace: &Path, name: String) -> io::Result<Self> {
        let directory = backend_platform::DirectoryCapability::open(workspace)?;
        directory.validate_private()?;
        let file = directory.create_file_exclusive(&name)?;
        let mut attempt = Self {
            path: workspace.join(&name),
            directory,
            name,
            file,
            cleanup_on_drop: true,
        };
        attempt.file.write_all(MAGIC.as_bytes())?;
        Ok(attempt)
    }

    #[cfg(unix)]
    pub(crate) fn adopt_configured() -> io::Result<Option<Self>> {
        let Some(path) = std::env::var_os(STARTUP_DIAGNOSTIC_ENV).map(PathBuf::from) else {
            return Ok(None);
        };
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("startup receipt has no parent"))?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| io::Error::other("invalid startup receipt name"))?
            .to_owned();
        let directory = backend_platform::DirectoryCapability::open(parent)?;
        directory.validate_private()?;
        let mut file = directory.open_private_file_read_write(&name, false)?;
        if read_locked(&mut file)? != MAGIC.as_bytes() {
            return Err(io::Error::other("startup receipt is not pending"));
        }
        Ok(Some(Self {
            path,
            directory,
            name,
            file,
            cleanup_on_drop: true,
        }))
    }

    /// A detached child may report after the launcher deadline. Leave the
    /// single reserved record for its listener-success cleanup or the next
    /// kernel-admitted generation; this flag makes no liveness claim.
    #[cfg(unix)]
    pub(crate) fn preserve_for_detached_owner(&mut self) {
        self.cleanup_on_drop = false;
    }

    pub(crate) fn remove_if_original(&self) {
        if self.file.try_lock().is_err() {
            return;
        }
        let cleanup = (|| {
            let original = backend_platform::FileIdentity::of_file(&self.file)?;
            let named = self
                .directory
                .open_private_file_read_write(&self.name, false)?;
            if backend_platform::FileIdentity::of_file(&named)? != original {
                return Ok(());
            }
            // Private parent namespace, with the same full-identity check used
            // by launcher cleanup. The canonical admission inode is untouched.
            remove_ephemeral(
                &self.directory,
                self.path
                    .parent()
                    .ok_or_else(|| io::Error::other("startup receipt parent missing"))?,
                &self.name,
            )
        })();
        let _ = self.file.unlock();
        let _ = cleanup;
    }

    pub(crate) fn configure(&self, command: &mut Command) {
        command.env(STARTUP_DIAGNOSTIC_ENV, &self.path);
    }

    pub(crate) fn failure(&self) -> Option<StartupDiagnostic> {
        let mut file = self
            .directory
            .open_private_file_read_write(&self.name, false)
            .ok()?;
        if backend_platform::FileIdentity::of_file(&file).ok()?
            != backend_platform::FileIdentity::of_file(&self.file).ok()?
        {
            return None;
        }
        let bytes = read_locked(&mut file).ok()?;
        terminal_cause(&bytes)
            .ok()?
            .filter(|cause| !cause.is_empty())
            .map(|cause| StartupDiagnostic {
                cause,
                owner_contended: bytes.starts_with(format!("{MAGIC}{CONTENDED_PREFIX}").as_bytes()),
            })
    }
}

// Ephemeral receipts have no recovery/durability authority. Keep their removal
// out of the durable directory API, which deliberately fsyncs every unlink.
fn remove_ephemeral(
    directory: &backend_platform::DirectoryCapability,
    parent: &Path,
    name: &str,
) -> io::Result<()> {
    directory.verify_path(parent)?;
    #[cfg(unix)]
    {
        std::fs::remove_file(parent.join(name))
    }
    #[cfg(not(unix))]
    {
        directory.remove_file(name)
    }
}

impl Drop for StartupAttempt {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            self.remove_if_original();
        }
    }
}

/// Writes a single failure to a launcher's already-created private channel.
/// Missing, completed, unsafe or unrelated files cannot be created by this API.
pub struct StartupFailureReporter {
    file: File,
}

impl StartupFailureReporter {
    /// Opens the transient channel inherited from the automatic launcher.
    ///
    /// # Errors
    /// Returns an I/O error if an explicitly configured channel is not the
    /// private, empty failure receipt created by the launcher.
    pub fn from_environment() -> io::Result<Option<Self>> {
        std::env::var_os(STARTUP_DIAGNOSTIC_ENV)
            .map(PathBuf::from)
            .map(Self::open)
            .transpose()
    }

    fn open(path: PathBuf) -> io::Result<Self> {
        let mut file = open_channel_file(&path)?;
        if read_locked(&mut file)? != MAGIC.as_bytes() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "startup failure channel is not pending",
            ));
        }
        Ok(Self { file })
    }

    /// Writes the bounded, printable terminal cause through the admitted file.
    ///
    /// # Errors
    /// Returns an I/O error if the private pending channel cannot be checked
    /// or the terminal receipt cannot be written. The caller still reports
    /// its original failure.
    pub fn report(self, error: &dyn fmt::Display) -> io::Result<()> {
        self.report_kind(error, false)
    }

    /// Reports an actual durable-owner lock refusal separately from listener
    /// contention or a generic startup failure. It does not assert liveness.
    ///
    /// # Errors
    /// Returns the original private-channel I/O or framing failure.
    pub fn report_owner_contention(self, error: &dyn fmt::Display) -> io::Result<()> {
        self.report_kind(error, true)
    }

    fn report_kind(self, error: &dyn fmt::Display, owner_contended: bool) -> io::Result<()> {
        let mut cause = BoundedCause {
            text: String::new(),
            full: false,
        };
        write!(&mut cause, "{error}").map_err(io::Error::other)?;
        let payload = terminal_payload_kind(&cause.text, owner_contended);

        let mut file = self.file;
        file.lock()?;
        if read_record(&mut file)? != MAGIC.as_bytes() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "startup failure channel is no longer pending",
            ));
        }
        file.seek(SeekFrom::End(0))?;
        file.write_all(&payload)?;
        file.sync_all()
    }
}

fn open_channel_file(path: &Path) -> io::Result<File> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "startup failure channel needs a Unicode file name",
            )
        })?;
    let directory = backend_platform::DirectoryCapability::open(parent)?;
    directory.validate_private()?;
    directory.open_private_file_read_write(name, false)
}

fn read_locked(file: &mut File) -> io::Result<Vec<u8>> {
    file.lock_shared()?;
    let record = read_record(file);
    let unlock = file.unlock();
    match record {
        Ok(record) => {
            unlock?;
            Ok(record)
        }
        Err(error) => {
            let _ = unlock;
            Err(error)
        }
    }
}

fn read_record(file: &mut File) -> io::Result<Vec<u8>> {
    let limit = maximum_record_bytes();
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    (&mut *file)
        .take(u64::try_from(limit + 1).map_err(io::Error::other)?)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "startup failure receipt exceeds its bound",
        ));
    }
    Ok(bytes)
}

fn maximum_record_bytes() -> usize {
    MAGIC.len()
        + CONTENDED_PREFIX.len()
        + MAX_CAUSE_DIGITS
        + 1
        + MAX_CAUSE_BYTES
        + TERMINAL_FOOTER.len()
}

fn terminal_payload_kind(cause: &str, owner_contended: bool) -> Vec<u8> {
    let prefix = if owner_contended {
        CONTENDED_PREFIX
    } else {
        TERMINAL_PREFIX
    };
    let mut payload = format!("{prefix}{}\n", cause.len()).into_bytes();
    payload.extend_from_slice(cause.as_bytes());
    payload.extend_from_slice(TERMINAL_FOOTER.as_bytes());
    payload
}

fn terminal_cause(record: &[u8]) -> io::Result<Option<String>> {
    if record == MAGIC.as_bytes() {
        return Ok(None);
    }
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "startup failure receipt is incomplete or invalid",
        )
    };
    let terminal = record.strip_prefix(MAGIC.as_bytes()).ok_or_else(invalid)?;
    let header_end = terminal
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(invalid)?;
    let header = std::str::from_utf8(&terminal[..header_end]).map_err(|_| invalid())?;
    let declared_len = header
        .strip_prefix(TERMINAL_PREFIX)
        .or_else(|| header.strip_prefix(CONTENDED_PREFIX))
        .ok_or_else(invalid)?;
    if declared_len.is_empty()
        || declared_len.len() > MAX_CAUSE_DIGITS
        || !declared_len.bytes().all(|byte| byte.is_ascii_digit())
        || (declared_len.len() > 1 && declared_len.starts_with('0'))
    {
        return Err(invalid());
    }
    let cause_len = declared_len.parse::<usize>().map_err(|_| invalid())?;
    if cause_len > MAX_CAUSE_BYTES {
        return Err(invalid());
    }
    let cause_start = header_end + 1;
    let cause_end = cause_start.checked_add(cause_len).ok_or_else(invalid)?;
    let record_end = cause_end
        .checked_add(TERMINAL_FOOTER.len())
        .ok_or_else(invalid)?;
    if terminal.len() != record_end || &terminal[cause_end..] != TERMINAL_FOOTER.as_bytes() {
        return Err(invalid());
    }
    let cause = std::str::from_utf8(&terminal[cause_start..cause_end]).map_err(|_| invalid())?;
    Ok(Some(cause.to_owned()))
}

struct BoundedCause {
    text: String,
    full: bool,
}

impl fmt::Write for BoundedCause {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if self.full {
            return Ok(());
        }
        for character in text.chars() {
            let character = if character.is_control() && !matches!(character, '\n' | '\t') {
                '�'
            } else {
                character
            };
            if self.text.len() + character.len_utf8() > MAX_CAUSE_BYTES {
                self.full = true;
                break;
            }
            self.text.push(character);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> PathBuf {
        let root = super::super::tests::test_directory("startup-failure");
        super::super::tests::create_private_fixture(&root);
        root
    }

    #[cfg(unix)]
    #[test]
    fn reserved_receipt_is_bounded_across_canceled_attempts_and_preserves_unknown_files() {
        let root = workspace();
        let name = "workspace.failure".to_owned();
        for _ in 0..3 {
            let attempt =
                StartupAttempt::prepare_reserved(&root, name.clone()).expect("reserved receipt");
            // Close the abandoned original file without deleting its name,
            // modeling a terminated launcher whose Drop never executes.
            let mut attempt = attempt;
            let original = std::mem::replace(
                &mut attempt.file,
                File::open("/dev/null").expect("dummy descriptor"),
            );
            drop(original);
            drop(attempt);
            assert!(root.join(&name).exists());
        }
        assert_eq!(
            std::fs::read_dir(&root).expect("receipt directory").count(),
            1
        );
        std::fs::write(root.join(&name), b"unrelated private bytes").expect("unknown receipt");
        assert!(StartupAttempt::prepare_reserved(&root, name.clone()).is_err());
        assert_eq!(
            std::fs::read(root.join(name)).expect("unchanged unknown file"),
            b"unrelated private bytes"
        );
        std::fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn durable_owner_contention_is_typed_and_generic_listener_failures_are_not() {
        let root = workspace();
        let attempt = StartupAttempt::prepare(&root).expect("attempt");
        StartupFailureReporter::open(attempt.path.clone())
            .expect("reporter")
            .report_owner_contention(&"durable owner admission refused")
            .expect("typed refusal");
        assert!(attempt.failure().expect("typed receipt").owner_contended());
        drop(attempt);
        let attempt = StartupAttempt::prepare(&root).expect("listener attempt");
        StartupFailureReporter::open(attempt.path.clone())
            .expect("reporter")
            .report(&"listener already running")
            .expect("listener failure");
        assert!(
            !attempt
                .failure()
                .expect("generic receipt")
                .owner_contended()
        );
        drop(attempt);
        std::fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn original_startup_cause_is_private_bounded_and_removed_with_the_attempt() {
        let root = workspace();
        let attempt = StartupAttempt::prepare(&root).expect("private attempt");
        let path = attempt.path.clone();
        let reporter = StartupFailureReporter::open(path.clone()).expect("pending channel");
        let message = format!(
            "Go toolchain identity refused: {}",
            "é".repeat(MAX_CAUSE_BYTES)
        );
        reporter.report(&message).expect("bounded failure");
        let cause = attempt.failure().expect("actual cause").to_string();
        assert!(cause.starts_with("Go toolchain identity refused: "));
        assert!(cause.len() <= MAX_CAUSE_BYTES);
        assert_eq!(cause.chars().last(), Some('é'), "UTF-8 is never split");
        assert!(
            StartupFailureReporter::open(path.clone()).is_err(),
            "a terminal receipt cannot be reused"
        );
        drop(attempt);
        assert!(!path.exists());
        std::fs::remove_dir_all(root).expect("remove private workspace");
    }

    #[test]
    fn diagnostic_channel_does_not_create_a_missing_or_unrelated_file() {
        let root = workspace();
        let missing = root.join("missing");
        assert!(StartupFailureReporter::open(missing.clone()).is_err());
        assert!(!missing.exists());
        let unrelated = root.join("unrelated");
        backend_platform::durable::write_private_atomic(&unrelated, b"existing state")
            .expect("state");
        assert!(StartupFailureReporter::open(unrelated.clone()).is_err());
        assert_eq!(
            std::fs::read(unrelated).expect("unchanged state"),
            b"existing state"
        );
        std::fs::remove_dir_all(root).expect("remove private workspace");
    }

    #[test]
    fn reporting_after_attempt_removal_does_not_recreate_the_path() {
        let root = workspace();
        let attempt = StartupAttempt::prepare(&root).expect("attempt");
        let path = attempt.path.clone();
        let reporter = StartupFailureReporter::open(path.clone()).expect("reporter");

        drop(attempt);
        reporter
            .report(&"original owner cause")
            .expect("write through the held descriptor");

        assert!(matches!(
            std::fs::symlink_metadata(&path),
            Err(error) if error.kind() == io::ErrorKind::NotFound
        ));
        std::fs::remove_dir_all(root).expect("remove workspace");
    }

    #[test]
    fn reporting_after_path_replacement_does_not_modify_the_replacement() {
        use backend_platform::FileIdentity;

        let root = workspace();
        let attempt = StartupAttempt::prepare(&root).expect("attempt");
        let path = attempt.path.clone();
        let reporter = StartupFailureReporter::open(path.clone()).expect("reporter");
        let replacement = b"replacement remains byte-for-byte unchanged";
        backend_platform::durable::write_private_atomic(&path, replacement)
            .expect("replace the path after the reporter opened its file");
        let replacement_identity =
            FileIdentity::of_path_nofollow(&path).expect("replacement identity");

        reporter
            .report(&"original owner cause")
            .expect("write only through the original descriptor");

        drop(attempt);
        assert_eq!(
            FileIdentity::of_path_nofollow(&path).expect("replacement remains named"),
            replacement_identity,
            "attempt cleanup must preserve a different named identity"
        );
        assert_eq!(
            std::fs::read(&path).expect("read replacement after cleanup"),
            replacement
        );
        std::fs::remove_dir_all(root).expect("remove workspace");
    }

    #[test]
    fn incomplete_terminal_record_is_never_reported_as_a_cause() {
        let root = workspace();
        let mut attempt = StartupAttempt::prepare(&root).expect("attempt");
        let incomplete = format!("{MAGIC}{TERMINAL_PREFIX}30\npartial");
        attempt.file.set_len(0).expect("truncate pending frame");
        attempt
            .file
            .seek(SeekFrom::Start(0))
            .expect("rewind pending frame");
        attempt
            .file
            .write_all(incomplete.as_bytes())
            .expect("write deliberately incomplete terminal frame");
        attempt.file.sync_all().expect("flush incomplete frame");

        assert!(attempt.failure().is_none());
        let noncanonical = format!("{MAGIC}{TERMINAL_PREFIX}01\nx{TERMINAL_FOOTER}");
        assert!(terminal_cause(noncanonical.as_bytes()).is_err());
        drop(attempt);
        std::fs::remove_dir_all(root).expect("remove workspace");
    }

    #[test]
    fn duplicate_reporters_serialize_on_the_admitted_file() {
        let root = workspace();
        let attempt = StartupAttempt::prepare(&root).expect("attempt");
        let path = attempt.path.clone();
        let first = StartupFailureReporter::open(path.clone()).expect("first reporter");
        let second = StartupFailureReporter::open(path).expect("second reporter");
        let reader = open_channel_file(&attempt.path).expect("reader descriptor");

        first
            .file
            .lock()
            .expect("hold the reporter's exclusive lease");
        assert!(
            matches!(
                second.file.try_lock(),
                Err(std::fs::TryLockError::WouldBlock)
            ),
            "duplicate writer must wait"
        );
        assert!(
            matches!(
                reader.try_lock_shared(),
                Err(std::fs::TryLockError::WouldBlock)
            ),
            "reader must wait for the writer"
        );
        first.file.unlock().expect("release exclusive lease");

        first
            .report(&"first original cause")
            .expect("first terminal report");
        assert!(second.report(&"second cause").is_err());
        assert_eq!(
            attempt.failure().expect("complete cause").to_string(),
            "first original cause"
        );

        drop(attempt);
        std::fs::remove_dir_all(root).expect("remove workspace");
    }

    #[cfg(unix)]
    #[test]
    fn diagnostic_channel_refuses_symlinks_and_unprintable_control_sequences() {
        let root = workspace();
        let attempt = StartupAttempt::prepare(&root).expect("attempt");
        let link = root.join("alias");
        std::os::unix::fs::symlink(&attempt.path, &link).expect("alias");
        assert!(StartupFailureReporter::open(link).is_err());
        StartupFailureReporter::open(attempt.path.clone())
            .expect("pending channel")
            .report(&"original cause\n\u{1b}[2J\0")
            .expect("printable cause");
        let cause = attempt.failure().expect("cause").to_string();
        assert!(cause.contains("original cause\n"));
        assert!(!cause.contains('\u{1b}') && !cause.contains('\0'));
        drop(attempt);
        std::fs::remove_dir_all(root).expect("remove private workspace");
    }
}
