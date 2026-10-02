//! Committing a staged directory to its final name, and riding out the
//! operating system's transient denials.
//!
//! Every durable projection in this extension is built in a private staging
//! directory and made visible by renaming that directory to its
//! content-addressed name. That rename is the single publication point, so it
//! is routed through [`backend_platform::durable::replace_file`], the
//! retry-aware replacement the rest of the workspace uses: on Windows a virus
//! scanner or indexer that briefly holds a just-written file turns an
//! otherwise valid rename into a sharing or access error.
//!
//! A directory cannot replace an existing directory on Windows, while POSIX
//! only refuses a non-empty one, and the two report the refusal differently.
//! [`publish_directory`] hides that difference behind a closed outcome: the
//! caller learns that it published, or that an identical directory had already
//! been published, and never inspects an error kind.

use std::{io, path::Path, thread, time::Duration};

use backend_platform::durable::replace_file;

/// How a staged directory reached its final name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirectoryPublication {
    /// The rename created the destination.
    Published,
    /// A directory already occupied the destination, so another publisher won
    /// the race. The staged directory is untouched and still the caller's to
    /// remove; the destination must be opened and validated before it is
    /// trusted.
    AlreadyPresent,
}

/// Renames `staged` to `destination`, which must not be an existing entry.
///
/// # Errors
///
/// Returns the operating-system error for every failure other than an
/// existing destination directory.
pub(crate) fn publish_directory(
    staged: &Path,
    destination: &Path,
) -> io::Result<DirectoryPublication> {
    match replace_file(staged, destination) {
        Ok(()) => Ok(DirectoryPublication::Published),
        Err(error) if destination_won(&error, destination) => {
            Ok(DirectoryPublication::AlreadyPresent)
        }
        Err(error) => Err(error),
    }
}

/// Whether `error` means that a directory occupies `destination`.
///
/// POSIX reports a non-empty destination as `AlreadyExists` or
/// `DirectoryNotEmpty`. Windows reports every directory destination as access
/// denied, which is also what a genuine permission problem looks like, so the
/// destination itself decides.
fn destination_won(error: &io::Error, destination: &Path) -> bool {
    match error.kind() {
        io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty => true,
        io::ErrorKind::PermissionDenied => cfg!(windows) && destination.is_dir(),
        _ => false,
    }
}

/// What happened to an entry moved out of the way.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MovedAside {
    /// The entry now lives at the quarantine name.
    Moved,
    /// The entry was already gone, so there was nothing to move.
    Vanished,
}

/// Moves `entry` (a corrupt directory or a stray file) to the unused
/// `quarantine` name.
///
/// # Errors
///
/// Returns the operating-system error for every failure other than a missing
/// entry.
pub(crate) fn move_aside(entry: &Path, quarantine: &Path) -> io::Result<MovedAside> {
    match replace_file(entry, quarantine) {
        Ok(()) => Ok(MovedAside::Moved),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(MovedAside::Vanished),
        Err(error) => Err(error),
    }
}

/// Pauses between attempts to rebuild a stage the operating system refused.
const DENIAL_BACKOFF: [Duration; 4] = [
    Duration::from_millis(25),
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(200),
];

/// A failure that may be the operating system briefly refusing a file that a
/// scanner or indexer holds, rather than anything wrong with the request.
pub(crate) trait TransientDenial {
    /// Whether rebuilding from scratch can reasonably succeed.
    fn is_transient_denial(&self) -> bool;
}

/// Whether `error` carries one of the codes Windows reports while another
/// handle holds a file it needs: access denied, sharing violation, lock
/// violation, or a directory that cannot be removed yet because entries inside
/// it are still pending deletion. No other platform reports such a refusal, so nothing is retried
/// there.
pub(crate) fn io_is_transient_denial(error: &io::Error) -> bool {
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    const ERROR_DIR_NOT_EMPTY: i32 = 145;
    cfg!(windows)
        && matches!(
            error.raw_os_error(),
            Some(
                ERROR_ACCESS_DENIED
                    | ERROR_SHARING_VIOLATION
                    | ERROR_LOCK_VIOLATION
                    | ERROR_DIR_NOT_EMPTY
            )
        )
}

/// The transient-denial classification of a Tantivy failure that wraps a file
/// system error.
pub(crate) fn backend_is_transient_denial(error: &tantivy::TantivyError) -> bool {
    use tantivy::{
        TantivyError,
        directory::error::{OpenDirectoryError, OpenReadError, OpenWriteError},
    };
    match error {
        TantivyError::IoError(error) => io_is_transient_denial(error),
        TantivyError::OpenWriteError(OpenWriteError::IoError { io_error, .. })
        | TantivyError::OpenReadError(OpenReadError::IoError { io_error, .. }) => {
            io_is_transient_denial(io_error)
        }
        TantivyError::OpenDirectoryError(OpenDirectoryError::IoError { io_error, .. }) => {
            io_is_transient_denial(io_error)
        }
        _ => false,
    }
}

/// Runs an attempt again while it fails with a transient denial.
///
/// `attempt` must be self-contained and safe to repeat: a stage-build-publish
/// attempt creates its own staging directory, removes it on failure and
/// publishes nothing before it succeeds; a removal is idempotent. A repeated
/// attempt therefore cannot observe the previous one. The wait is short and
/// bounded; the last error is returned unchanged once the schedule is spent, so
/// a genuine permission problem still fails closed.
pub(crate) fn retry_while_denied<T, E: TransientDenial>(
    attempt: impl FnMut() -> Result<T, E>,
) -> Result<T, E> {
    retry_with(attempt, &DENIAL_BACKOFF, thread::sleep)
}

/// The retry loop with its schedule and clock injected, so tests drive it
/// without sleeping.
fn retry_with<T, E: TransientDenial>(
    mut attempt: impl FnMut() -> Result<T, E>,
    backoff: &[Duration],
    mut pause: impl FnMut(Duration),
) -> Result<T, E> {
    for delay in backoff {
        match attempt() {
            Err(error) if error.is_transient_denial() => pause(*delay),
            outcome => return outcome,
        }
    }
    attempt()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use std::{cell::Cell, fs, path::PathBuf};

    #[derive(Debug, Eq, PartialEq)]
    enum Failure {
        Transient,
        Permanent,
    }

    impl TransientDenial for Failure {
        fn is_transient_denial(&self) -> bool {
            matches!(self, Self::Transient)
        }
    }

    fn scratch(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "nudox-tantivy-publish-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        fs::create_dir_all(&path).expect("scratch directory");
        path
    }

    fn staged(parent: &Path, name: &str, content: &str) -> PathBuf {
        let path = parent.join(name);
        fs::create_dir(&path).expect("staged directory");
        fs::write(path.join("payload"), content).expect("staged payload");
        path
    }

    #[test]
    fn the_first_publisher_wins_and_the_second_learns_it_lost() {
        let parent = scratch("race");
        let destination = parent.join("final");
        let first = staged(&parent, ".first", "one");
        let second = staged(&parent, ".second", "two");

        assert_eq!(
            publish_directory(&first, &destination).expect("first publish"),
            DirectoryPublication::Published
        );
        assert_eq!(
            publish_directory(&second, &destination).expect("second publish"),
            DirectoryPublication::AlreadyPresent,
            "an existing destination directory is a lost race, on every platform"
        );
        assert_eq!(
            fs::read_to_string(destination.join("payload")).expect("winner payload"),
            "one",
            "the loser must not disturb the published directory"
        );
        assert!(second.is_dir(), "the loser's stage stays for its owner");
        fs::remove_dir_all(parent).expect("cleanup");
    }

    #[test]
    fn many_threads_publishing_one_destination_agree_on_one_winner() {
        let parent = scratch("storm");
        let destination = parent.join("final");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
        let workers = (0..12)
            .map(|index| {
                let staged = staged(&parent, &format!(".stage-{index}"), &index.to_string());
                let destination = destination.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    publish_directory(&staged, &destination)
                })
            })
            .collect::<Vec<_>>();
        let outcomes = workers
            .into_iter()
            .map(|worker| worker.join().expect("publisher").expect("publication"))
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == DirectoryPublication::Published)
                .count(),
            1,
            "exactly one rename may create the destination: {outcomes:?}"
        );
        assert!(destination.join("payload").is_file());
        fs::remove_dir_all(parent).expect("cleanup");
    }

    #[test]
    fn a_missing_stage_is_an_error_not_a_lost_race() {
        let parent = scratch("missing");
        let error = publish_directory(&parent.join(".absent"), &parent.join("final"))
            .expect_err("nothing to publish");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        fs::remove_dir_all(parent).expect("cleanup");
    }

    #[test]
    fn moving_aside_distinguishes_a_moved_entry_from_a_vanished_one() {
        let parent = scratch("aside");
        let entry = staged(&parent, "corrupt", "x");
        assert_eq!(
            move_aside(&entry, &parent.join(".corrupt-1")).expect("move"),
            MovedAside::Moved
        );
        assert!(!entry.exists() && parent.join(".corrupt-1").is_dir());
        assert_eq!(
            move_aside(&entry, &parent.join(".corrupt-2")).expect("already moved"),
            MovedAside::Vanished
        );
        fs::remove_dir_all(parent).expect("cleanup");
    }

    #[test]
    fn a_transient_denial_is_rebuilt_with_the_scheduled_pauses_until_it_clears() {
        let attempts = Cell::new(0_u32);
        let pauses = Cell::new(Vec::new());
        let outcome = retry_with(
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() < 4 {
                    Err(Failure::Transient)
                } else {
                    Ok("published")
                }
            },
            &DENIAL_BACKOFF,
            |delay| {
                let mut seen = pauses.take();
                seen.push(delay);
                pauses.set(seen);
            },
        );
        assert_eq!(outcome, Ok("published"));
        assert_eq!(attempts.get(), 4);
        assert_eq!(pauses.take(), DENIAL_BACKOFF[..3]);
    }

    #[test]
    fn a_permanent_failure_is_returned_at_once_without_waiting() {
        let attempts = Cell::new(0_u32);
        let outcome: Result<(), Failure> = retry_with(
            || {
                attempts.set(attempts.get() + 1);
                Err(Failure::Permanent)
            },
            &DENIAL_BACKOFF,
            |_| panic!("a permanent failure must not be waited on"),
        );
        assert_eq!(outcome, Err(Failure::Permanent));
        assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn a_denial_that_outlasts_the_schedule_is_reported_after_bounded_attempts() {
        let attempts = Cell::new(0_usize);
        let waited = Cell::new(Duration::ZERO);
        let outcome: Result<(), Failure> = retry_with(
            || {
                attempts.set(attempts.get() + 1);
                Err(Failure::Transient)
            },
            &DENIAL_BACKOFF,
            |delay| waited.set(waited.get() + delay),
        );
        assert_eq!(outcome, Err(Failure::Transient));
        assert_eq!(attempts.get(), DENIAL_BACKOFF.len() + 1);
        assert_eq!(waited.get(), DENIAL_BACKOFF.iter().sum::<Duration>());
    }

    #[cfg(windows)]
    #[test]
    fn windows_denial_codes_are_transient_and_other_failures_are_not() {
        use std::sync::Arc;
        use tantivy::{TantivyError, directory::error::OpenWriteError};

        for code in [5, 32, 33, 145] {
            let denied = TantivyError::OpenWriteError(OpenWriteError::IoError {
                io_error: Arc::new(io::Error::from_raw_os_error(code)),
                filepath: PathBuf::from("segment.idx"),
            });
            assert!(backend_is_transient_denial(&denied), "code {code}");
        }
        for code in [2, 3, 80, 183] {
            let other = TantivyError::OpenWriteError(OpenWriteError::IoError {
                io_error: Arc::new(io::Error::from_raw_os_error(code)),
                filepath: PathBuf::from("segment.idx"),
            });
            assert!(!backend_is_transient_denial(&other), "code {code}");
        }
        assert!(!backend_is_transient_denial(&TantivyError::InternalError(
            "not a file system error".to_owned()
        )));
    }

    #[cfg(not(windows))]
    #[test]
    fn no_failure_is_transient_off_windows() {
        // Only Windows reports a scanner-held file as a sharing refusal.
        assert!(!io_is_transient_denial(&io::Error::from_raw_os_error(13)));
    }
}
