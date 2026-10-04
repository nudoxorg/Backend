//! Private, same-directory publication for immutable Tantivy projections.
//!
//! Every published generation is built beneath a private, versioned namespace.
//! A [`PreparedStage`] owns one direct child of that namespace until the
//! platform's no-replace directory rename commits. Transient pre-commit
//! denials retry that same admitted stage; build retries create a new stage
//! only after the previous one was successfully cleaned. A post-commit
//! durability failure is terminal because the destination may already exist.

use std::{
    io,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use backend_platform::{DirectoryCapability, DirectoryRenameError};

const DENIAL_BACKOFF: [Duration; 4] = [
    Duration::from_millis(25),
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(200),
];
const MAX_STAGE_CLEANUP_ENTRIES: usize = 1_000_000;

/// An owner-only namespace pinned by a directory handle.
///
/// `open_child` establishes a fresh private child beneath a caller-owned cache
/// root. The caller's root is never chmodded or replaced; all generations,
/// locks, and unpublished stages are direct descendants of this namespace.
pub(crate) struct PrivateNamespace {
    path: PathBuf,
    directory: DirectoryCapability,
}

impl PrivateNamespace {
    /// Creates or verifies one private namespace below an existing parent.
    pub(crate) fn open_child(parent: &Path, name: &str) -> io::Result<Self> {
        let path = parent.join(name);
        backend_platform::durable::ensure_private_child_directory(&path)?;
        let directory = DirectoryCapability::open(&path)?;
        directory.validate_private()?;
        Ok(Self { path, directory })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn open_private_file_read_write(
        &self,
        name: &str,
        create: bool,
    ) -> io::Result<std::fs::File> {
        self.directory.open_private_file_read_write(name, create)
    }

    pub(crate) fn open_private_dir(&self, name: &str) -> io::Result<DirectoryCapability> {
        self.directory.open_private_dir(name)
    }

    pub(crate) fn create_stage(&self, name: &str) -> io::Result<PreparedStage> {
        let stage_directory = match self.directory.create_private_dir(name) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Err(error),
            Err(error) => {
                return match self.directory.open_private_dir(name) {
                    Ok(_) => match remove_private_child(&self.directory, name) {
                        Ok(()) => Err(error),
                        Err(cleanup) => Err(io::Error::other(StageCreateRollbackFailure {
                            create: error,
                            cleanup,
                        })),
                    },
                    Err(check) if check.kind() == io::ErrorKind::NotFound => Err(error),
                    Err(check) => Err(io::Error::other(StageCreateRollbackFailure {
                        create: error,
                        cleanup: check,
                    })),
                };
            }
        };
        drop(stage_directory);
        Ok(PreparedStage {
            parent: self.directory.clone(),
            name: name.to_owned(),
            path: self.path.join(name),
            armed: true,
        })
    }

    pub(crate) fn remove_entry(&self, name: &str) -> io::Result<()> {
        match self.directory.open_dir(name) {
            Ok(_) => self
                .directory
                .remove_dir_all(name, MAX_STAGE_CLEANUP_ENTRIES),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(_) => match self.directory.remove_file(name) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                result => result,
            },
        }
    }

    /// Moves one private direct child to an unused quarantine name without
    /// replacing a concurrently-created destination.
    pub(crate) fn move_aside(&self, entry: &str, quarantine: &str) -> io::Result<MovedAside> {
        match self.directory.rename_with_outcome(entry, quarantine, false) {
            Ok(()) => Ok(MovedAside::Moved),
            Err(DirectoryRenameError::NotCommitted(error))
                if error.kind() == io::ErrorKind::NotFound =>
            {
                Ok(MovedAside::Vanished)
            }
            Err(error) => Err(error.into_io_error()),
        }
    }
}

#[derive(Debug)]
struct StageCreateRollbackFailure {
    create: io::Error,
    cleanup: io::Error,
}

impl std::fmt::Display for StageCreateRollbackFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "stage creation failed ({}), and exact-name rollback failed ({})",
            self.create, self.cleanup
        )
    }
}

impl std::error::Error for StageCreateRollbackFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.create)
    }
}

/// One direct, private, not-yet-published directory owned by its creator.
///
/// Dropping an armed stage removes only its exact direct-child name through the
/// pinned namespace handle. A successful rename or a post-commit durability
/// error disarms the guard before returning, so a live generation is never
/// removed by cleanup.
pub(crate) struct PreparedStage {
    parent: DirectoryCapability,
    name: String,
    path: PathBuf,
    armed: bool,
}

impl PreparedStage {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Removes this exact unpublished child, retrying only transient cleanup
    /// denials. Missing means it was already removed and is success.
    pub(crate) fn discard(mut self) -> io::Result<()> {
        self.discard_inner()
    }

    fn discard_inner(&mut self) -> io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        retry_while_denied(|| remove_private_child(&self.parent, &self.name))?;
        self.armed = false;
        Ok(())
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    /// Publishes this stage under `destination`, using an atomic no-replace
    /// rename and a bounded retry of the same unchanged bytes.
    pub(crate) fn publish(
        self,
        destination: &str,
    ) -> Result<DirectoryPublication, PublicationFailure> {
        self.publish_with(
            destination,
            |parent, source, target| parent.rename_with_outcome(source, target, false),
            io_is_transient_denial,
            &DENIAL_BACKOFF,
            thread::sleep,
        )
    }

    fn publish_with(
        mut self,
        destination: &str,
        mut rename: impl FnMut(&DirectoryCapability, &str, &str) -> Result<(), DirectoryRenameError>,
        is_transient: impl Fn(&io::Error) -> bool,
        backoff: &[Duration],
        mut pause: impl FnMut(Duration),
    ) -> Result<DirectoryPublication, PublicationFailure> {
        let published = retry_publish_with(
            &self.parent,
            &self.name,
            destination,
            &mut rename,
            is_transient,
            backoff,
            &mut pause,
            &mut self.armed,
        );
        match published {
            Ok(DirectoryPublication::Published) => Ok(DirectoryPublication::Published),
            Ok(DirectoryPublication::AlreadyPresent) => {
                self.discard_inner()
                    .map_err(|source| PublicationFailure::Cleanup {
                        operation: None,
                        source,
                    })?;
                Ok(DirectoryPublication::AlreadyPresent)
            }
            Err(operation) => {
                if self.armed {
                    if let Err(source) = self.discard_inner() {
                        return Err(PublicationFailure::Cleanup {
                            operation: Some(Box::new(operation)),
                            source,
                        });
                    }
                }
                Err(operation)
            }
        }
    }
}

/// A preparation failure tagged according to whether retrying is safe after
/// cleanup. Cleanup failure is always terminal, even if the original build
/// error looked transient.
pub(crate) enum StagePreparationFailure<E> {
    Retryable(E),
    Terminal(E),
}

impl<E: TransientDenial> StagePreparationFailure<E> {
    pub(crate) fn for_attempt(error: E) -> Self {
        if error.is_transient_denial() {
            Self::Retryable(error)
        } else {
            Self::Terminal(error)
        }
    }
}

impl<E> StagePreparationFailure<E> {
    pub(crate) fn into_error(self) -> E {
        match self {
            Self::Retryable(error) | Self::Terminal(error) => error,
        }
    }
}

impl<E: TransientDenial> TransientDenial for StagePreparationFailure<E> {
    fn is_transient_denial(&self) -> bool {
        matches!(self, Self::Retryable(_))
    }
}

pub(crate) fn failed_stage_preparation<E>(
    stage: PreparedStage,
    operation: E,
) -> StagePreparationFailure<E>
where
    E: TransientDenial + From<io::Error>,
{
    failed_stage_preparation_with(stage, operation, E::from)
}

pub(crate) fn failed_stage_preparation_with<E>(
    stage: PreparedStage,
    operation: E,
    cleanup_failure: impl FnOnce(io::Error) -> E,
) -> StagePreparationFailure<E>
where
    E: TransientDenial,
{
    match stage.discard() {
        Ok(()) => StagePreparationFailure::for_attempt(operation),
        Err(cleanup) => StagePreparationFailure::Terminal(cleanup_failure(cleanup)),
    }
}

impl Drop for PreparedStage {
    fn drop(&mut self) {
        if self.armed {
            let _ = retry_while_denied(|| remove_private_child(&self.parent, &self.name));
            self.armed = false;
        }
    }
}

fn remove_private_child(parent: &DirectoryCapability, name: &str) -> io::Result<()> {
    match parent.remove_dir_all(name, MAX_STAGE_CLEANUP_ENTRIES) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn retry_publish_with(
    parent: &DirectoryCapability,
    source: &str,
    destination: &str,
    rename: &mut impl FnMut(&DirectoryCapability, &str, &str) -> Result<(), DirectoryRenameError>,
    is_transient: impl Fn(&io::Error) -> bool,
    backoff: &[Duration],
    pause: &mut impl FnMut(Duration),
    armed: &mut bool,
) -> Result<DirectoryPublication, PublicationFailure> {
    for delay in backoff {
        match publish_once(parent, source, destination, rename, armed) {
            Err(PublicationFailure::NotCommitted(error)) if is_transient(&error) => pause(*delay),
            outcome => return outcome,
        }
    }
    publish_once(parent, source, destination, rename, armed)
}

fn publish_once(
    parent: &DirectoryCapability,
    source: &str,
    destination: &str,
    rename: &mut impl FnMut(&DirectoryCapability, &str, &str) -> Result<(), DirectoryRenameError>,
    armed: &mut bool,
) -> Result<DirectoryPublication, PublicationFailure> {
    match rename(parent, source, destination) {
        Ok(()) => {
            *armed = false;
            Ok(DirectoryPublication::Published)
        }
        Err(DirectoryRenameError::CommittedButNotDurable(source)) => {
            *armed = false;
            Err(PublicationFailure::CommittedButNotDurable(source))
        }
        Err(DirectoryRenameError::NotCommitted(error)) => {
            if is_destination_conflict(&error) {
                match parent.open_private_dir(destination) {
                    Ok(_winner) => return Ok(DirectoryPublication::AlreadyPresent),
                    Err(check) if check.kind() == io::ErrorKind::NotFound => {}
                    Err(check) => {
                        return Err(PublicationFailure::NotCommitted(check));
                    }
                }
            }
            Err(PublicationFailure::NotCommitted(error))
        }
    }
}

fn is_destination_conflict(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty
    ) || (cfg!(windows) && error.kind() == io::ErrorKind::PermissionDenied)
}

/// Result of asking the no-replace rename to publish a stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirectoryPublication {
    /// The rename created the destination.
    Published,
    /// Another publisher already created a real private destination directory.
    /// The caller must reopen and validate its contents before trusting it.
    AlreadyPresent,
}

/// Closed publication failure that retains whether the generation rename
/// committed before durability failed.
#[derive(Debug)]
pub(crate) enum PublicationFailure {
    /// The rename did not commit; retry is safe only for classified transient
    /// errors while this exact stage remains present.
    NotCommitted(io::Error),
    /// The rename committed, but the namespace flush failed. The stage guard is
    /// disarmed and the operation must never be replayed.
    CommittedButNotDurable(io::Error),
    /// Stage cleanup failed. `operation` retains the publication result that
    /// led to cleanup; `None` means cleanup followed an already-present winner.
    Cleanup {
        operation: Option<Box<Self>>,
        source: io::Error,
    },
}

impl PublicationFailure {
    pub(crate) fn into_io_error(self) -> io::Error {
        match self {
            Self::NotCommitted(source) => io::Error::new(source.kind(), Self::NotCommitted(source)),
            Self::CommittedButNotDurable(source) => {
                io::Error::new(source.kind(), Self::CommittedButNotDurable(source))
            }
            Self::Cleanup { operation, source } => {
                io::Error::new(source.kind(), Self::Cleanup { operation, source })
            }
        }
    }
}

impl std::fmt::Display for PublicationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotCommitted(source) => {
                write!(formatter, "directory rename did not commit: {source}")
            }
            Self::CommittedButNotDurable(source) => write!(
                formatter,
                "directory rename committed but durability could not be confirmed: {source}"
            ),
            Self::Cleanup { operation, source } => {
                write!(formatter, "unpublished stage cleanup failed: {source}")?;
                if let Some(operation) = operation {
                    write!(formatter, " after {operation}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for PublicationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotCommitted(source) | Self::CommittedButNotDurable(source) => Some(source),
            Self::Cleanup { source, .. } => Some(source),
        }
    }
}

/// Result of moving a corrupt directory or stray file aside.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MovedAside {
    /// The entry now lives at the quarantine name.
    Moved,
    /// The entry was already gone, so there was nothing to move.
    Vanished,
}

/// A failure that may be the operating system briefly refusing a file that a
/// scanner or indexer holds, rather than anything wrong with the request.
pub(crate) trait TransientDenial {
    /// Whether repeating the operation after this failure may reasonably succeed.
    fn is_transient_denial(&self) -> bool;
}

impl TransientDenial for io::Error {
    fn is_transient_denial(&self) -> bool {
        io_is_transient_denial(self)
    }
}

/// Whether `error` carries one of the codes Windows reports while another
/// handle holds a file it needs. No other platform reports such a refusal.
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

/// Retries an isolated operation after a classified transient denial. Callers
/// building stages must clean that attempt before returning a retryable error.
pub(crate) fn retry_while_denied<T, E: TransientDenial>(
    attempt: impl FnMut() -> Result<T, E>,
) -> Result<T, E> {
    retry_with(attempt, &DENIAL_BACKOFF, thread::sleep)
}

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
    use std::{cell::Cell, fs};

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

    fn namespace(parent: &Path) -> PrivateNamespace {
        PrivateNamespace::open_child(parent, "private-v1").expect("private namespace")
    }

    fn stage(namespace: &PrivateNamespace, name: &str, content: &str) -> PreparedStage {
        let stage = namespace.create_stage(name).expect("stage");
        fs::write(stage.path().join("payload"), content).expect("stage payload");
        stage
    }

    #[test]
    fn publishes_one_immutable_winner_and_discards_only_the_losing_stage() {
        let root = scratch("winner");
        let namespace = namespace(&root);
        let winner = stage(&namespace, ".winner", "winner bytes");
        assert_eq!(
            winner.publish("generation").expect("first publisher"),
            DirectoryPublication::Published
        );
        let loser = stage(&namespace, ".loser", "loser bytes");
        assert_eq!(
            loser.publish("generation").expect("second publisher"),
            DirectoryPublication::AlreadyPresent
        );
        assert_eq!(
            fs::read(namespace.path().join("generation/payload")).expect("winner payload"),
            b"winner bytes"
        );
        assert!(!namespace.path().join(".loser").exists());
        drop(namespace);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn transient_rename_retries_the_same_stage_bytes_without_rebuilding() {
        let root = scratch("same-stage-retry");
        let namespace = namespace(&root);
        let stage = stage(&namespace, ".stage", "admitted bytes");
        let source_name = stage.name().to_owned();
        let attempts = Cell::new(0_u32);
        let pauses = Cell::new(Vec::new());
        let schedule = [Duration::from_millis(3), Duration::from_millis(7)];
        let result = stage.publish_with(
            "generation",
            |directory, source, destination| {
                attempts.set(attempts.get() + 1);
                assert_eq!(source, source_name);
                assert_eq!(
                    fs::read(namespace.path().join(source).join("payload")).expect("stage bytes"),
                    b"admitted bytes"
                );
                assert!(!namespace.path().join(destination).exists());
                if attempts.get() < 3 {
                    Err(DirectoryRenameError::NotCommitted(
                        io::Error::from_raw_os_error(32),
                    ))
                } else {
                    directory.rename_with_outcome(source, destination, false)
                }
            },
            |error| error.raw_os_error() == Some(32),
            &schedule,
            |delay| {
                let mut seen = pauses.take();
                seen.push(delay);
                pauses.set(seen);
            },
        );
        assert_eq!(
            result.expect("transient refusal clears"),
            DirectoryPublication::Published
        );
        assert_eq!(attempts.get(), 3);
        assert_eq!(pauses.take(), schedule);
        assert_eq!(
            fs::read(namespace.path().join("generation/payload")).expect("published bytes"),
            b"admitted bytes"
        );
        assert!(!namespace.path().join(".stage").exists());
        drop(namespace);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn exhausted_denials_remove_only_the_owned_unpublished_stage() {
        let root = scratch("denial-exhaustion");
        let namespace = namespace(&root);
        let candidate = stage(&namespace, ".stage", "admitted bytes");
        let previous = stage(&namespace, "previous", "previous generation");
        let attempts = Cell::new(0_u32);
        let pauses = Cell::new(Vec::new());
        let schedule = [Duration::from_millis(2), Duration::from_millis(5)];
        let result = candidate.publish_with(
            "next",
            |_, _, _| {
                attempts.set(attempts.get() + 1);
                Err(DirectoryRenameError::NotCommitted(
                    io::Error::from_raw_os_error(32),
                ))
            },
            |error| error.raw_os_error() == Some(32),
            &schedule,
            |delay| {
                let mut seen = pauses.take();
                seen.push(delay);
                pauses.set(seen);
            },
        );
        assert!(matches!(result, Err(PublicationFailure::NotCommitted(_))));
        assert_eq!(attempts.get(), schedule.len() as u32 + 1);
        assert_eq!(pauses.take(), schedule);
        assert!(!namespace.path().join(".stage").exists());
        assert!(!namespace.path().join("next").exists());
        assert_eq!(
            fs::read(previous.path().join("payload")).expect("prior generation"),
            b"previous generation"
        );
        drop(previous);
        drop(namespace);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn an_empty_real_destination_wins_without_being_overwritten() {
        let root = scratch("empty-winner");
        let namespace = namespace(&root);
        let destination = namespace
            .directory
            .create_private_dir("generation")
            .expect("empty private destination");
        drop(destination);
        let stage = stage(&namespace, ".stage", "candidate bytes");
        assert_eq!(
            stage.publish("generation").expect("no-replace winner"),
            DirectoryPublication::AlreadyPresent
        );
        assert_eq!(
            fs::read_dir(namespace.path().join("generation"))
                .expect("empty destination")
                .count(),
            0,
            "a content-invalid winner is left untouched for the caller to reject"
        );
        assert!(!namespace.path().join(".stage").exists());
        drop(namespace);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn postcommit_durability_failure_is_terminal_and_disarms_the_stage() {
        let root = scratch("commit-uncertain");
        let namespace = namespace(&root);
        let stage = stage(&namespace, ".stage", "committed bytes");
        let attempts = Cell::new(0_u32);
        let result = stage.publish_with(
            "generation",
            |directory, source, destination| {
                attempts.set(attempts.get() + 1);
                directory
                    .rename_with_outcome(source, destination, false)
                    .expect("the injected post-commit failure follows a real rename");
                Err(DirectoryRenameError::CommittedButNotDurable(
                    io::Error::from_raw_os_error(32),
                ))
            },
            |_| true,
            &[Duration::from_millis(1), Duration::from_millis(2)],
            |_| panic!("a post-commit durability failure must not retry"),
        );
        assert!(matches!(
            result,
            Err(PublicationFailure::CommittedButNotDurable(_))
        ));
        assert_eq!(attempts.get(), 1);
        assert_eq!(
            fs::read(namespace.path().join("generation/payload")).expect("committed payload"),
            b"committed bytes"
        );
        drop(namespace);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn private_namespace_rejects_symlink_and_preexisting_nonprivate_child() {
        use std::os::unix::fs::symlink;

        let root = scratch("namespace-reject");
        let outside = root.join("outside");
        fs::create_dir(&outside).expect("outside target");
        symlink(&outside, root.join("private-v1")).expect("namespace symlink");
        assert!(PrivateNamespace::open_child(&root, "private-v1").is_err());
        fs::remove_file(root.join("private-v1")).expect("remove namespace link");

        let public = root.join("private-v1");
        fs::create_dir(&public).expect("public namespace");
        assert!(PrivateNamespace::open_child(&root, "private-v1").is_err());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn private_move_aside_never_replaces_a_quarantine_winner() {
        let root = scratch("quarantine-noreplace");
        let namespace = namespace(&root);
        let entry = namespace
            .directory
            .create_private_dir("entry")
            .expect("entry");
        fs::write(namespace.path().join("entry/payload"), b"corrupt").expect("entry payload");
        drop(entry);
        let winner = namespace
            .directory
            .create_private_dir("quarantine")
            .expect("winner");
        fs::write(namespace.path().join("quarantine/payload"), b"winner").expect("winner payload");
        drop(winner);
        assert!(namespace.move_aside("entry", "quarantine").is_err());
        assert_eq!(
            fs::read(namespace.path().join("quarantine/payload")).expect("winner payload"),
            b"winner"
        );
        assert!(namespace.path().join("entry").is_dir());
        drop(namespace);
        fs::remove_dir_all(root).expect("cleanup");
    }

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

    #[test]
    fn preparation_retry_waits_only_after_a_transient_error() {
        let attempts = Cell::new(0_u32);
        let pauses = Cell::new(Vec::new());
        let schedule = [Duration::from_millis(3), Duration::from_millis(7)];
        let result = retry_with(
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() == 1 {
                    Err(Failure::Transient)
                } else {
                    Ok("ready")
                }
            },
            &schedule,
            |delay| {
                let mut seen = pauses.take();
                seen.push(delay);
                pauses.set(seen);
            },
        );
        assert_eq!(result, Ok("ready"));
        assert_eq!(attempts.get(), 2);
        assert_eq!(pauses.take(), [schedule[0]]);
        let permanent = retry_with(|| Err::<(), _>(Failure::Permanent), &schedule, |_| panic!());
        assert_eq!(permanent, Err(Failure::Permanent));
    }

    #[cfg(not(windows))]
    #[test]
    fn no_io_failure_is_transient_off_windows() {
        assert!(!io_is_transient_denial(&io::Error::from_raw_os_error(13)));
    }

    #[cfg(windows)]
    #[test]
    fn windows_sharing_codes_are_transient_but_unrelated_failures_are_not() {
        for code in [5, 32, 33, 145] {
            assert!(io_is_transient_denial(&io::Error::from_raw_os_error(code)));
        }
        for code in [2, 3, 80, 183] {
            assert!(!io_is_transient_denial(&io::Error::from_raw_os_error(code)));
        }
    }
}
