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
use std::sync::atomic::{AtomicU64, Ordering};

/// Private failure-receipt path installed only on an automatically spawned owner.
pub const STARTUP_DIAGNOSTIC_ENV: &str = "BACKEND_LOCALD_STARTUP_DIAGNOSTIC_FILE";
const MAGIC: &str = "nudox.local-startup-failure.v1\n";
const TERMINAL_PREFIX: &str = "terminal:";
const TERMINAL_FOOTER: &str = "\n--nudox.local-startup-failure.complete--\n";
const MAX_CAUSE_BYTES: usize = 8192;
const MAX_CAUSE_DIGITS: usize = 4;
static ATTEMPT: AtomicU64 = AtomicU64::new(0);

/// The original owner refusal, bounded at its producer and checked on reading.
#[derive(Debug)]
pub struct StartupDiagnostic(String);

impl fmt::Display for StartupDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub(crate) struct StartupAttempt {
    path: PathBuf,
}

impl StartupAttempt {
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
        backend_platform::durable::write_private_atomic(&path, MAGIC.as_bytes())?;
        Ok(Self { path })
    }

    pub(crate) fn configure(&self, command: &mut Command) {
        command.env(STARTUP_DIAGNOSTIC_ENV, &self.path);
    }

    pub(crate) fn failure(&self) -> Option<StartupDiagnostic> {
        let mut file = open_channel_file(&self.path).ok()?;
        let bytes = read_locked(&mut file).ok()?;
        terminal_cause(&bytes)
            .ok()?
            .filter(|cause| !cause.is_empty())
            .map(StartupDiagnostic)
    }
}

impl Drop for StartupAttempt {
    fn drop(&mut self) {
        let _ = backend_platform::durable::remove_private(&self.path);
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
        let mut cause = BoundedCause {
            text: String::new(),
            full: false,
        };
        write!(&mut cause, "{error}").map_err(io::Error::other)?;
        let payload = terminal_payload(&cause.text);

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
    let name = path.file_name().and_then(|name| name.to_str()).ok_or_else(|| {
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
        + TERMINAL_PREFIX.len()
        + MAX_CAUSE_DIGITS
        + 1
        + MAX_CAUSE_BYTES
        + TERMINAL_FOOTER.len()
}

fn terminal_payload(cause: &str) -> Vec<u8> {
    let mut payload = format!("{TERMINAL_PREFIX}{}\n", cause.len()).into_bytes();
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
    let header_end = terminal.iter().position(|byte| *byte == b'\n').ok_or_else(invalid)?;
    let header = std::str::from_utf8(&terminal[..header_end]).map_err(|_| invalid())?;
    let declared_len = header.strip_prefix(TERMINAL_PREFIX).ok_or_else(invalid)?;
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
        let replacement_identity = FileIdentity::of_path_nofollow(&path)
            .expect("replacement identity");

        reporter
            .report(&"original owner cause")
            .expect("write only through the original descriptor");

        assert_eq!(
            FileIdentity::of_path_nofollow(&path).expect("replacement remains named"),
            replacement_identity
        );
        assert_eq!(std::fs::read(&path).expect("read replacement"), replacement);
        drop(attempt);
        std::fs::remove_dir_all(root).expect("remove workspace");
    }

    #[test]
    fn incomplete_terminal_record_is_never_reported_as_a_cause() {
        let root = workspace();
        let attempt = StartupAttempt::prepare(&root).expect("attempt");
        let incomplete = format!("{MAGIC}{TERMINAL_PREFIX}30\npartial");
        backend_platform::durable::write_private_atomic(&attempt.path, incomplete.as_bytes())
            .expect("write deliberately incomplete terminal frame");

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
        let mut reader = open_channel_file(&attempt.path).expect("reader descriptor");

        first.file.lock().expect("hold the reporter's exclusive lease");
        assert_eq!(
            second.file.try_lock().expect_err("duplicate writer must wait").kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            reader
                .try_lock_shared()
                .expect_err("reader must wait for the writer")
                .kind(),
            io::ErrorKind::WouldBlock
        );
        first.file.unlock().expect("release exclusive lease");

        first.report(&"first original cause").expect("first terminal report");
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
