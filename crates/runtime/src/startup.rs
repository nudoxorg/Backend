//! A bounded private failure receipt for a detached owner startup.
//!
//! This channel contains one terminal startup cause, never an environment or
//! a lifetime stderr stream. It cannot keep the owner attached to a launching
//! CLI, block a pipe, or grow after that CLI has accepted the endpoint.

use std::fmt::{self, Write as _};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// Private failure-receipt path installed only on an automatically spawned owner.
pub const STARTUP_DIAGNOSTIC_ENV: &str = "BACKEND_LOCALD_STARTUP_DIAGNOSTIC_FILE";
const MAGIC: &str = "nudox.local-startup-failure.v1\n";
const MAX_CAUSE_BYTES: usize = 8192;
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
        let bytes = read_record(&self.path).ok()?;
        let text = std::str::from_utf8(&bytes).ok()?.strip_prefix(MAGIC)?;
        (!text.is_empty()).then(|| StartupDiagnostic(text.to_owned()))
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
    path: PathBuf,
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
        if read_record(&path)? != MAGIC.as_bytes() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "startup failure channel is not pending",
            ));
        }
        Ok(Self { path })
    }

    /// Atomically publishes the bounded, printable terminal cause.
    ///
    /// # Errors
    /// Returns an I/O error if the private pending channel cannot be checked
    /// or atomically replaced. The caller still reports its original failure.
    pub fn report(self, error: &dyn fmt::Display) -> io::Result<()> {
        if read_record(&self.path)? != MAGIC.as_bytes() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "startup failure channel is no longer pending",
            ));
        }
        let mut cause = BoundedCause {
            text: String::new(),
            full: false,
        };
        write!(&mut cause, "{error}").map_err(io::Error::other)?;
        let mut record = String::from(MAGIC);
        record.push_str(&cause.text);
        backend_platform::durable::write_private_atomic(&self.path, record.as_bytes())
    }
}

fn read_record(path: &Path) -> io::Result<Vec<u8>> {
    let file = backend_platform::durable::open_private_read(path)?;
    let limit = MAGIC.len() + MAX_CAUSE_BYTES;
    let mut bytes = Vec::new();
    file.take(u64::try_from(limit + 1).map_err(io::Error::other)?)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "startup failure receipt exceeds its bound",
        ));
    }
    Ok(bytes)
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
