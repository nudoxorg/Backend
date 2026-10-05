//! Private, same-directory publication for immutable Tantivy projections.
//!
//! Every published generation is built beneath a private, versioned namespace.
//! A [`PreparedStage`] owns one direct child of that namespace until the
//! platform's no-replace directory rename commits. Transient pre-commit
//! denials retry that same admitted stage; build retries create a new stage
//! only after the previous one was successfully cleaned. A post-commit
//! durability or identity-verification failure is terminal because the
//! destination may already exist. Path-based Tantivy APIs are protected by
//! identity checks and the cooperating writer lock; these checks do not stop a
//! same-user process that bypasses that lock.

use std::{
    fs::{File, TryLockError},
    io,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use backend_platform::{
    CreatedDirectory, CreatedDirectoryRenameError, DirectoryCapability, DirectoryCreateFailure,
    DirectoryEntry, DirectoryRenameError, FileIdentity,
};

const DENIAL_BACKOFF: [Duration; 4] = [
    Duration::from_millis(25),
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(200),
];
const MAX_STAGE_CLEANUP_ENTRIES: usize = 1_000_000;
pub(crate) const STAGE_LEASE_FILE_PREFIX: &str = ".stage-active-";
const NAMESPACE_FENCE_WAIT: Duration = Duration::from_secs(5);
const NAMESPACE_FENCE_POLL: Duration = Duration::from_millis(2);

/// The two fixed lock files that serialize final namespace admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NamespaceFenceKind {
    /// Durable root-cache publication and pruning.
    DurableCache,
    /// Immutable lexical-segment publication.
    SegmentStore,
}

impl NamespaceFenceKind {
    fn file_name(self) -> &'static str {
        match self {
            Self::DurableCache => "durable-cache.lock",
            Self::SegmentStore => "segment-store.lock",
        }
    }
}

/// A bounded cooperative lock on one fixed namespace publication gate.
///
/// It binds the lock handle to its current direct name and the pinned parent
/// namespace. Same-user processes that bypass the lock remain outside the
/// guarantee.
pub(crate) struct NamespaceFence {
    file: File,
    identity: FileIdentity,
    directory: DirectoryCapability,
    namespace_path: PathBuf,
    kind: NamespaceFenceKind,
}

impl NamespaceFence {
    fn verify(&self) -> io::Result<()> {
        self.directory.verify_path(&self.namespace_path)?;
        if !self.file.metadata()?.is_file()
            || FileIdentity::of_file(&self.file)? != self.identity
            || FileIdentity::of_path_nofollow(
                &self.namespace_path.join(self.kind.file_name()),
            )? != self.identity
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "namespace fence identity changed",
            ));
        }
        Ok(())
    }

    /// Identifies only this held fence's direct control file after verifying its inode.
    pub(crate) fn is_control_entry(&self, path: &Path) -> io::Result<bool> {
        self.verify()?;
        Ok(path == self.namespace_path.join(self.kind.file_name()))
    }

    pub(crate) fn verify_for(&self, namespace: &PrivateNamespace) -> io::Result<()> {
        if self.namespace_path != namespace.path {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "namespace fence belongs to a different namespace",
            ));
        }
        namespace.verify_path()?;
        self.verify()
    }
}

pub(crate) fn stage_lease_file_name(stage_name: &str) -> String {
    format!("{STAGE_LEASE_FILE_PREFIX}{stage_name}")
}

fn verify_stage_lease_token(file: &mut File, stage_name: &str) -> io::Result<()> {
    let expected = stage_name.as_bytes();
    let metadata = file.metadata()?;
    let expected_len = u64::try_from(expected.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "stage name length overflow"))?;
    if !metadata.is_file() || metadata.len() != expected_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "stage active marker does not bind the expected stage name",
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut observed = vec![0; expected.len()];
    file.read_exact(&mut observed)?;
    if observed != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "stage active marker token changed",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StageSweepDisposition {
    Removed,
    Active,
    Vanished,
}

/// One stage's lifecycle disposition; a recoverable stage is left for a later
/// fence holder to sweep rather than removed by an unverified path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StageDisposition {
    /// The exact created stage is still being prepared.
    Prepared,
    /// The rename committed; cleanup must not touch the published generation.
    Published,
    /// The stage remains named for a later bounded recovery pass.
    PreservedForRecovery,
    /// The exact unpublished stage and its lease marker were removed.
    Removed,
}

#[derive(Debug)]
pub(crate) struct StageCleanupFailure {
    pub(crate) disposition: StageDisposition,
    pub(crate) stage_name: String,
    pub(crate) source: io::Error,
}

impl StageCleanupFailure {
    pub(crate) fn into_io_error(self) -> io::Error {
        io::Error::new(self.source.kind(), self)
    }
}

impl std::fmt::Display for StageCleanupFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "stage {} was {:?}: {}",
            self.stage_name, self.disposition, self.source
        )
    }
}

impl std::error::Error for StageCleanupFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug)]
struct StagePreparationCleanup<E> {
    operation: E,
    cleanup: StageCleanupFailure,
}

impl<E: std::fmt::Display> std::fmt::Display for StagePreparationCleanup<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "stage build failed ({}) and cleanup was {}",
            self.operation, self.cleanup
        )
    }
}

impl<E> std::error::Error for StagePreparationCleanup<E>
where
    E: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.operation)
    }
}

pub(crate) fn stage_preparation_cleanup_io_error<E>(
    operation: E,
    cleanup: StageCleanupFailure,
) -> io::Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    io::Error::new(
        cleanup.source.kind(),
        StagePreparationCleanup { operation, cleanup },
    )
}

/// An owner-only namespace pinned by a directory handle.
///
/// `open_child` establishes a fresh private child beneath a caller-owned cache
/// root. The caller's root is never chmodded or replaced; all generations,
/// locks, and unpublished stages are direct descendants of this namespace.
/// Path-based index APIs are checked against the pinned namespace before and
/// after use. Builders hold a per-stage active lease while outside the short
/// publication fence. Final admission, quarantine, rename, and reopen use the
/// fixed namespace fence. These checks coordinate cooperating writers; they
/// do not create a security boundary against a same-user process that ignores
/// the protocol.
#[derive(Clone)]
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
        directory.verify_path(&path)?;
        Ok(Self { path, directory })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Verifies the stored path still resolves to this pinned namespace.
    /// Path-based index operations require the caller's cooperative cache lock
    /// to remain held around the check and the operation.
    pub(crate) fn verify_path(&self) -> io::Result<()> {
        self.directory.verify_path(&self.path)
    }

    /// Acquires one fixed, owner-only namespace gate with a bounded wait.
    pub(crate) fn acquire_fence(&self, kind: NamespaceFenceKind) -> io::Result<NamespaceFence> {
        self.acquire_fence_with_wait(kind, NAMESPACE_FENCE_WAIT)
    }

    fn acquire_fence_with_wait(
        &self,
        kind: NamespaceFenceKind,
        wait: Duration,
    ) -> io::Result<NamespaceFence> {
        self.verify_path()?;
        let file = self
            .directory
            .open_private_file_read_write(kind.file_name(), true)?;
        let identity = FileIdentity::of_file(&file)?;
        let fence = NamespaceFence {
            file,
            identity,
            directory: self.directory.clone(),
            namespace_path: self.path.clone(),
            kind,
        };
        fence.verify_for(self)?;
        let started = Instant::now();
        loop {
            match fence.file.try_lock() {
                Ok(()) => {
                    fence.verify_for(self)?;
                    return Ok(fence);
                }
                Err(TryLockError::WouldBlock) if started.elapsed() < wait => {
                    thread::sleep(NAMESPACE_FENCE_POLL);
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "namespace publication fence remained busy",
                    ));
                }
                Err(TryLockError::Error(error)) => return Err(error),
            }
        }
    }

    /// Verifies a raw direct-child path against the namespace and child handles.
    pub(crate) fn verify_child_path(&self, name: &str) -> io::Result<()> {
        self.verify_path()?;
        let child = self.directory.open_private_dir(name)?;
        child.verify_path(&self.path.join(name))
    }

    pub(crate) fn open_private_dir(&self, name: &str) -> io::Result<DirectoryCapability> {
        self.directory.open_private_dir(name)
    }

    pub(crate) fn create_stage(
        &self,
        name: &str,
        fence: &NamespaceFence,
    ) -> io::Result<PreparedStage> {
        fence.verify_for(self)?;
        let created = match self.directory.create_private_dir_tracked(name) {
            Ok(created) => created,
            Err(DirectoryCreateFailure::NotCreated(source)) => return Err(source),
            Err(DirectoryCreateFailure::CreatedButUnpinned(source)) => {
                return Err(io::Error::other(DirectoryCreateFailure::CreatedButUnpinned(
                    source,
                )));
            }
            Err(DirectoryCreateFailure::CreatedButUnready { directory, source }) => {
                return match directory.remove_all(MAX_STAGE_CLEANUP_ENTRIES) {
                    Ok(()) => Err(source),
                    Err(cleanup) => Err(io::Error::other(StageCreateRollbackFailure {
                        create: source,
                        cleanup,
                    })),
                };
            }
        };
        let lease_name = stage_lease_file_name(name);
        let mut lease_file = match self.directory.create_file_exclusive(&lease_name) {
            Ok(file) => file,
            Err(create) => {
                return match created.remove_all(MAX_STAGE_CLEANUP_ENTRIES) {
                    Ok(()) => Err(create),
                    Err(cleanup) => Err(io::Error::other(StageCreateRollbackFailure {
                        create,
                        cleanup,
                    })),
                };
            }
        };
        let lease_identity = match FileIdentity::of_file(&lease_file) {
            Ok(identity) => identity,
            Err(create) => {
                drop(lease_file);
                let cleanup = created.remove_all(MAX_STAGE_CLEANUP_ENTRIES);
                return match cleanup {
                    Ok(()) => Err(create),
                    Err(cleanup) => Err(io::Error::other(StageCreateRollbackFailure {
                        create,
                        cleanup,
                    })),
                };
            }
        };
        if let Err(create) = lease_file
            .write_all(name.as_bytes())
            .and_then(|()| lease_file.sync_all())
        {
            drop(lease_file);
            return match self.remove_stage_and_lease(&created, &lease_name, lease_identity) {
                Ok(()) => Err(create),
                Err(cleanup) => Err(io::Error::other(StageCreateRollbackFailure {
                    create,
                    cleanup,
                })),
            };
        }
        let named_identity_error = match
            FileIdentity::of_path_nofollow(&self.path.join(&lease_name))
        {
            Ok(identity) if identity == lease_identity => None,
            Ok(_) => Some(io::Error::new(
                io::ErrorKind::InvalidData,
                "new stage lease name does not identify its held file",
            )),
            Err(error) => Some(error),
        };
        if let Some(create) = named_identity_error {
            drop(lease_file);
            return match self.remove_stage_and_lease(&created, &lease_name, lease_identity) {
                Ok(()) => Err(create),
                Err(cleanup) => Err(io::Error::other(StageCreateRollbackFailure {
                    create,
                    cleanup,
                })),
            };
        }
        if let Err(error) = lease_file.try_lock() {
            let create = match error {
                TryLockError::WouldBlock => io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "new stage lease could not be locked",
                ),
                TryLockError::Error(error) => error,
            };
            drop(lease_file);
            let cleanup = self.remove_stage_and_lease(&created, &lease_name, lease_identity);
            return match cleanup {
                Ok(()) => Err(create),
                Err(cleanup) => Err(io::Error::other(StageCreateRollbackFailure {
                    create,
                    cleanup,
                })),
            };
        }
        if let Err(error) = fence.verify_for(self).and_then(|()| {
            if FileIdentity::of_file(&lease_file)? != lease_identity
                || FileIdentity::of_path_nofollow(&self.path.join(&lease_name))? != lease_identity
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "stage lease identity changed during acquisition",
                ));
            }
            Ok(())
        }) {
            drop(lease_file);
            // The stage and marker stay named for the next fence holder. This
            // failure has no proof that cleanup can safely target the names.
            return Err(error);
        }
        Ok(PreparedStage {
            parent: self.directory.clone(),
            created,
            name: name.to_owned(),
            path: self.path.join(name),
            namespace: self.clone(),
            namespace_path: self.path.clone(),
            fence_kind: fence.kind,
            lease_name,
            lease_file: Some(lease_file),
            lease_identity,
            disposition: StageDisposition::Prepared,
            armed: true,
        })
    }

    fn remove_stage_and_lease(
        &self,
        created: &CreatedDirectory,
        lease_name: &str,
        lease_identity: FileIdentity,
    ) -> io::Result<()> {
        created.remove_all(MAX_STAGE_CLEANUP_ENTRIES)?;
        match FileIdentity::of_path_nofollow(&self.path.join(lease_name)) {
            Ok(identity) if identity == lease_identity => self.directory.remove_file(lease_name),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "stage lease name changed identity during cleanup",
            )),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn entries(
        &self,
        maximum: usize,
        fence: &NamespaceFence,
    ) -> io::Result<Vec<DirectoryEntry>> {
        fence.verify_for(self)?;
        let entries = self.directory.entries(maximum)?;
        fence.verify_for(self)?;
        Ok(entries)
    }

    /// Removes a direct stage only when its lease can be acquired, proving no
    /// cooperating builder still owns that unique name.
    pub(crate) fn sweep_stage(
        &self,
        stage_name: &str,
        fence: &NamespaceFence,
    ) -> io::Result<StageSweepDisposition> {
        fence.verify_for(self)?;
        match self.directory.open_private_dir(stage_name) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(StageSweepDisposition::Vanished);
            }
            Err(error) => return Err(error),
            Ok(stage) => stage.verify_path(&self.path.join(stage_name))?,
        }

        let lease_name = stage_lease_file_name(stage_name);
        let lease_file = match self
            .directory
            .open_private_file_read_write(&lease_name, false)
        {
            Ok(file) => Some(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let lease_identity = if let Some(file) = &lease_file {
            let identity = FileIdentity::of_file(file)?;
            if FileIdentity::of_path_nofollow(&self.path.join(&lease_name))? != identity {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "stage lease name changed during sweep admission",
                ));
            }
            let mut marker = file.try_clone()?;
            verify_stage_lease_token(&mut marker, stage_name)?;
            match marker.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => {
                    fence.verify_for(self)?;
                    return Ok(StageSweepDisposition::Active);
                }
                Err(TryLockError::Error(error)) => return Err(error),
            }
            if FileIdentity::of_file(&marker)? != identity
                || FileIdentity::of_path_nofollow(&self.path.join(&lease_name))? != identity
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "stage lease identity changed after sweep acquisition",
                ));
            }
            verify_stage_lease_token(&mut marker, stage_name)?;
            Some(identity)
        } else {
            None
        };

        self.remove_entry(stage_name, fence)?;
        if let Some(identity) = lease_identity {
            match FileIdentity::of_path_nofollow(&self.path.join(&lease_name)) {
                Ok(named) if named == identity => self.directory.remove_file(&lease_name)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "stage lease name changed during sweep cleanup",
                    ));
                }
                Err(error) => return Err(error),
            }
        }
        fence.verify_for(self)?;
        Ok(StageSweepDisposition::Removed)
    }

    /// Removes a lease sidecar left after its stage disappeared during cleanup.
    pub(crate) fn sweep_orphan_stage_lease(
        &self,
        lease_name: &str,
        stage_name: &str,
        fence: &NamespaceFence,
    ) -> io::Result<StageSweepDisposition> {
        fence.verify_for(self)?;
        match self.directory.open_private_dir(stage_name) {
            Ok(_) => return Ok(StageSweepDisposition::Active),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let file = match self
            .directory
            .open_private_file_read_write(lease_name, false)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(StageSweepDisposition::Vanished);
            }
            Err(error) => return Err(error),
        };
        let identity = FileIdentity::of_file(&file)?;
        if FileIdentity::of_path_nofollow(&self.path.join(lease_name))? != identity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "orphan stage lease changed identity during sweep admission",
            ));
        }
        let mut file = file;
        verify_stage_lease_token(&mut file, stage_name)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                fence.verify_for(self)?;
                return Ok(StageSweepDisposition::Active);
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }
        if FileIdentity::of_file(&file)? != identity
            || FileIdentity::of_path_nofollow(&self.path.join(lease_name))? != identity
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "orphan stage lease changed identity after sweep acquisition",
            ));
        }
        verify_stage_lease_token(&mut file, stage_name)?;
        self.directory.remove_file(lease_name)?;
        fence.verify_for(self)?;
        Ok(StageSweepDisposition::Removed)
    }

    pub(crate) fn remove_entry(
        &self,
        name: &str,
        fence: &NamespaceFence,
    ) -> io::Result<()> {
        fence.verify_for(self)?;
        let result = match self.directory.open_dir(name) {
            Ok(_) => self
                .directory
                .remove_dir_all(name, MAX_STAGE_CLEANUP_ENTRIES),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(_) => match self.directory.remove_file(name) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                result => result,
            },
        };
        fence.verify_for(self)?;
        result
    }

    /// Moves one private direct child to an unused quarantine name without
    /// replacing a concurrently-created destination.
    pub(crate) fn move_aside(
        &self,
        entry: &str,
        quarantine: &str,
        fence: &NamespaceFence,
    ) -> io::Result<MovedAside> {
        fence.verify_for(self)?;
        let result = match self.directory.rename_with_outcome(entry, quarantine, false) {
            Ok(()) => Ok(MovedAside::Moved),
            Err(DirectoryRenameError::NotCommitted(error))
                if error.kind() == io::ErrorKind::NotFound =>
            {
                Ok(MovedAside::Vanished)
            }
            Err(error) => Err(error.into_io_error()),
        };
        fence.verify_for(self)?;
        result
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
            "stage creation failed ({}), and receipt-authorized cleanup failed ({})",
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
/// Explicit cleanup requires the fixed namespace fence and the pinned creation
/// receipt. Drop only releases the stage's active marker and leaves a recoverable
/// stage for the next fence holder. On Unix the receipt follows `mkdirat` with a
/// separate open, so a same-user actor can replace the name in that interval and
/// become the identity represented by the receipt. The cooperating fence excludes
/// conforming writers across that interval; actors that bypass it remain outside
/// the guarantee.
pub(crate) struct PreparedStage {
    parent: DirectoryCapability,
    created: CreatedDirectory,
    name: String,
    path: PathBuf,
    namespace: PrivateNamespace,
    namespace_path: PathBuf,
    fence_kind: NamespaceFenceKind,
    lease_name: String,
    lease_file: Option<File>,
    lease_identity: FileIdentity,
    disposition: StageDisposition,
    armed: bool,
}

impl PreparedStage {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Confirms that the raw stage path still reaches its pinned parent and
    /// child. Path-based backends are bracketed by this check; the cache lock
    /// coordinates cooperating writers but is not a hostile same-user boundary.
    pub(crate) fn verify_path(&self) -> io::Result<()> {
        self.created.verify_path(&self.path)
    }

    /// Flushes the stage through its held directory capability rather than
    /// reopening the raw stage path.
    pub(crate) fn sync(&self) -> io::Result<()> {
        self.created.capability().sync_all()
    }

    #[cfg(test)]
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    #[cfg(test)]
    pub(crate) const fn disposition(&self) -> StageDisposition {
        self.disposition
    }

    /// Removes the receipt's pinned unpublished child, retrying only transient
    /// cleanup denials. A missing or replaced name is a terminal cleanup
    /// refusal because the receipt can no longer prove which object occupies it.
    pub(crate) fn discard(mut self) -> Result<(), StageCleanupFailure> {
        let fence = match self.namespace.acquire_fence(self.fence_kind) {
            Ok(fence) => fence,
            Err(source) => {
                self.disposition = StageDisposition::PreservedForRecovery;
                return Err(StageCleanupFailure {
                    disposition: self.disposition,
                    stage_name: self.name.clone(),
                    source,
                });
            }
        };
        self.discard_inner(&fence)
    }

    pub(crate) fn discard_under(
        mut self,
        fence: &NamespaceFence,
    ) -> Result<(), StageCleanupFailure> {
        self.discard_inner(fence)
    }

    fn discard_inner(&mut self, fence: &NamespaceFence) -> Result<(), StageCleanupFailure> {
        if !self.armed {
            return Ok(());
        }
        if let Err(source) = self.verify_fence_and_lease(fence) {
            self.disposition = StageDisposition::PreservedForRecovery;
            return Err(StageCleanupFailure {
                disposition: self.disposition,
                stage_name: self.name.clone(),
                source,
            });
        }
        let cleanup = retry_while_denied(|| self.created.remove_all(MAX_STAGE_CLEANUP_ENTRIES));
        let lease_cleanup = if cleanup.is_ok() {
            self.remove_lease_under(fence)
        } else {
            Ok(())
        };
        self.armed = false;
        match (cleanup, lease_cleanup) {
            (Ok(()), Ok(())) => {
                self.disposition = StageDisposition::Removed;
                Ok(())
            }
            (Err(source), _) | (Ok(()), Err(source)) => {
                self.disposition = StageDisposition::PreservedForRecovery;
                Err(StageCleanupFailure {
                    disposition: self.disposition,
                    stage_name: self.name.clone(),
                    source,
                })
            }
        }
    }

    fn prepare_for_publication(&mut self, fence: &NamespaceFence) -> io::Result<()> {
        self.verify_fence_and_lease(fence)?;
        self.remove_lease_under(fence)?;
        self.created.capability().sync_all()?;
        self.parent.sync_all()?;
        fence.verify_for(&self.namespace)
    }

    /// Publishes this stage under `destination`, using an identity-fenced
    /// atomic no-replace rename and a bounded retry of the same unchanged bytes.
    /// The source name is checked through its retained parent immediately
    /// before the capability-relative rename, and the resulting path is checked
    /// after commit. These separate operations require the cooperative
    /// namespace lock; they do not provide atomic exclusion against a same-user
    /// process that bypasses that protocol.
    pub(crate) fn publish(
        self,
        destination: &str,
        fence: &NamespaceFence,
    ) -> Result<DirectoryPublication, PublicationFailure> {
        self.publish_with(
            destination,
            fence,
            |created, target| created.rename_noreplace(target),
            io_is_transient_denial,
            &DENIAL_BACKOFF,
            thread::sleep,
        )
    }

    fn publish_with(
        mut self,
        destination: &str,
        fence: &NamespaceFence,
        mut rename: impl FnMut(&CreatedDirectory, &str) -> Result<(), CreatedDirectoryRenameError>,
        is_transient: impl Fn(&io::Error) -> bool,
        backoff: &[Duration],
        mut pause: impl FnMut(Duration),
    ) -> Result<DirectoryPublication, PublicationFailure> {
        if let Err(source) = self.prepare_for_publication(fence) {
            let operation = PublicationFailure::NotCommitted(source);
            if self.armed {
            if let Err(cleanup) = self.discard_inner(fence) {
                    return Err(PublicationFailure::Cleanup {
                        operation: Some(Box::new(operation)),
                        source: cleanup.into_io_error(),
                    });
                }
            }
            return Err(operation);
        }
        let published = retry_publish_with(
            &self.namespace,
            &self.created,
            &self.parent,
            &self.path,
            &self.path.with_file_name(destination),
            destination,
            fence,
            &mut rename,
            is_transient,
            backoff,
            &mut pause,
            &mut self.armed,
        );
        if !self.armed {
            self.disposition = StageDisposition::Published;
        }
        match published {
            Ok(DirectoryPublication::Published) => Ok(DirectoryPublication::Published),
            Ok(DirectoryPublication::AlreadyPresent) => {
                self.discard_inner(fence)
                    .map_err(|source| PublicationFailure::Cleanup {
                        operation: None,
                        source: source.into_io_error(),
                    })?;
                Ok(DirectoryPublication::AlreadyPresent)
            }
            Err(operation) => {
                if self.armed {
                    if let Err(source) = self.discard_inner(fence) {
                        return Err(PublicationFailure::Cleanup {
                            operation: Some(Box::new(operation)),
                            source: source.into_io_error(),
                        });
                    }
                }
                Err(operation)
            }
        }
    }

    fn verify_fence_and_lease(&self, fence: &NamespaceFence) -> io::Result<()> {
        if fence.namespace_path != self.namespace_path || fence.kind != self.fence_kind {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "stage and publication fence belong to different namespaces",
            ));
        }
        fence.verify_for(&self.namespace)?;
        self.created.verify_named()?;
        self.created.verify_path(&self.path)?;
        if let Some(file) = &self.lease_file {
            if FileIdentity::of_file(file)? != self.lease_identity
                || FileIdentity::of_path_nofollow(&self.namespace_path.join(&self.lease_name))?
                    != self.lease_identity
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "stage active-lease identity changed",
                ));
            }
            verify_stage_lease_token(&mut file.try_clone()?, &self.name)?;
        }
        Ok(())
    }

    fn remove_lease_under(&mut self, fence: &NamespaceFence) -> io::Result<()> {
        fence.verify_for(&self.namespace)?;
        if let Some(file) = &self.lease_file {
            if FileIdentity::of_file(file)? != self.lease_identity {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "stage active-lease handle changed identity before removal",
                ));
            }
            verify_stage_lease_token(&mut file.try_clone()?, &self.name)?;
        }
        match FileIdentity::of_path_nofollow(&self.namespace_path.join(&self.lease_name)) {
            Ok(identity) if identity == self.lease_identity => {
                self.parent.remove_file(&self.lease_name)?;
                self.lease_file.take();
                fence.verify_for(&self.namespace)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.lease_file.take();
                Ok(())
            }
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "stage active-lease name changed identity",
            )),
            Err(error) => Err(error),
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
    E: TransientDenial + From<io::Error> + std::error::Error + Send + Sync + 'static,
{
    match stage.discard() {
        Ok(()) => StagePreparationFailure::for_attempt(operation),
        Err(cleanup) => StagePreparationFailure::Terminal(E::from(
            stage_preparation_cleanup_io_error(operation, cleanup),
        )),
    }
}

pub(crate) fn failed_stage_preparation_with<E>(
    stage: PreparedStage,
    operation: E,
    cleanup_failure: impl FnOnce(E, StageCleanupFailure) -> E,
) -> StagePreparationFailure<E>
where
    E: TransientDenial,
{
    match stage.discard() {
        Ok(()) => StagePreparationFailure::for_attempt(operation),
        Err(cleanup) => StagePreparationFailure::Terminal(cleanup_failure(operation, cleanup)),
    }
}

impl Drop for PreparedStage {
    fn drop(&mut self) {
        if self.armed {
            // Releasing this file handle makes the direct stage discoverable
            // as stale to the next fence holder. Drop never deletes by name;
            // explicit discard/publish paths carry the namespace fence and
            // report a preserved-for-recovery disposition on refusal.
            self.disposition = StageDisposition::PreservedForRecovery;
        }
    }
}

fn retry_publish_with(
    namespace: &PrivateNamespace,
    created: &CreatedDirectory,
    parent: &DirectoryCapability,
    source_path: &Path,
    destination_path: &Path,
    destination: &str,
    fence: &NamespaceFence,
    rename: &mut impl FnMut(&CreatedDirectory, &str) -> Result<(), CreatedDirectoryRenameError>,
    is_transient: impl Fn(&io::Error) -> bool,
    backoff: &[Duration],
    pause: &mut impl FnMut(Duration),
    armed: &mut bool,
) -> Result<DirectoryPublication, PublicationFailure> {
    for delay in backoff {
        match publish_once(
            namespace,
            created,
            parent,
            source_path,
            destination_path,
            destination,
            fence,
            rename,
            armed,
        ) {
            Err(PublicationFailure::NotCommitted(error)) if is_transient(&error) => pause(*delay),
            outcome => return outcome,
        }
    }
    publish_once(
        namespace,
        created,
        parent,
        source_path,
        destination_path,
        destination,
        fence,
        rename,
        armed,
    )
}

fn publish_once(
    namespace: &PrivateNamespace,
    created: &CreatedDirectory,
    parent: &DirectoryCapability,
    source_path: &Path,
    destination_path: &Path,
    destination: &str,
    fence: &NamespaceFence,
    rename: &mut impl FnMut(&CreatedDirectory, &str) -> Result<(), CreatedDirectoryRenameError>,
    armed: &mut bool,
) -> Result<DirectoryPublication, PublicationFailure> {
    fence
        .verify_for(namespace)
        .map_err(PublicationFailure::OwnershipChanged)?;
    created
        .verify_named()
        .and_then(|()| created.verify_path(source_path))
        .map_err(PublicationFailure::OwnershipChanged)?;
    match rename(created, destination) {
        Ok(()) => {
            *armed = false;
            if let Err(verification) = fence
                .verify_for(namespace)
                .and_then(|()| created.verify_path(destination_path))
            {
                return Err(PublicationFailure::CommittedButOwnershipUnverified {
                    rename: None,
                    verification,
                });
            }
            Ok(DirectoryPublication::Published)
        }
        Err(CreatedDirectoryRenameError::Rename(
            DirectoryRenameError::CommittedButNotDurable(source),
        )) => {
            *armed = false;
            if let Err(verification) = fence
                .verify_for(namespace)
                .and_then(|()| created.verify_path(destination_path))
            {
                Err(PublicationFailure::CommittedButOwnershipUnverified {
                    rename: Some(source),
                    verification,
                })
            } else {
                Err(PublicationFailure::CommittedButNotDurable(source))
            }
        }
        Err(CreatedDirectoryRenameError::NameChanged(error)) => {
            Err(PublicationFailure::OwnershipChanged(error))
        }
        Err(CreatedDirectoryRenameError::Rename(DirectoryRenameError::NotCommitted(error))) => {
            fence
                .verify_for(namespace)
                .map_err(PublicationFailure::OwnershipChanged)?;
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
    /// The receipt name or its raw path stopped resolving to the held stage
    /// before the rename committed.
    OwnershipChanged(io::Error),
    /// The rename committed, but the namespace flush failed. The stage guard is
    /// disarmed and the operation must never be replayed.
    CommittedButNotDurable(io::Error),
    /// The rename committed, but the raw destination path no longer resolves
    /// to the held stage. The commit is retained and must never be replayed.
    CommittedButOwnershipUnverified {
        /// Rename durability error, if the commit also reported one.
        rename: Option<io::Error>,
        /// The failed post-commit identity check.
        verification: io::Error,
    },
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
            Self::OwnershipChanged(source) => {
                io::Error::new(source.kind(), Self::OwnershipChanged(source))
            }
            Self::CommittedButNotDurable(source) => {
                io::Error::new(source.kind(), Self::CommittedButNotDurable(source))
            }
            Self::CommittedButOwnershipUnverified {
                rename,
                verification,
            } => io::Error::new(
                verification.kind(),
                Self::CommittedButOwnershipUnverified {
                    rename,
                    verification,
                },
            ),
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
            Self::OwnershipChanged(source) => write!(
                formatter,
                "created stage identity changed before publication: {source}"
            ),
            Self::CommittedButNotDurable(source) => write!(
                formatter,
                "directory rename committed but durability could not be confirmed: {source}"
            ),
            Self::CommittedButOwnershipUnverified {
                rename,
                verification,
            } => {
                write!(
                    formatter,
                    "directory rename committed but destination identity could not be verified: {verification}"
                )?;
                if let Some(rename) = rename {
                    write!(formatter, " (rename durability error: {rename})")?;
                }
                Ok(())
            }
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
            Self::NotCommitted(source)
            | Self::OwnershipChanged(source)
            | Self::CommittedButNotDurable(source) => Some(source),
            Self::CommittedButOwnershipUnverified { verification, .. } => Some(verification),
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

    use backend_platform::FileIdentity;
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
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::DurableCache)
            .expect("test stage gate");
        let stage = namespace.create_stage(name, &fence).expect("stage");
        drop(fence);
        assert_eq!(stage.disposition(), StageDisposition::Prepared);
        fs::write(stage.path().join("payload"), content).expect("stage payload");
        stage
    }

    fn publish(
        stage: PreparedStage,
        destination: &str,
    ) -> Result<DirectoryPublication, PublicationFailure> {
        let namespace = stage.namespace.clone();
        let fence = namespace
            .acquire_fence(stage.fence_kind)
            .map_err(PublicationFailure::NotCommitted)?;
        stage.publish(destination, &fence)
    }

    #[test]
    fn publishes_one_immutable_winner_and_discards_only_the_losing_stage() {
        let root = scratch("winner");
        let namespace = namespace(&root);
        let winner = stage(&namespace, ".winner", "winner bytes");
        assert_eq!(
            publish(winner, "generation").expect("first publisher"),
            DirectoryPublication::Published
        );
        let loser = stage(&namespace, ".loser", "loser bytes");
        assert_eq!(
            publish(loser, "generation").expect("second publisher"),
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
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::DurableCache)
            .expect("publication gate");
        let result = stage.publish_with(
            "generation",
            &fence,
            |created, destination| {
                attempts.set(attempts.get() + 1);
                assert_eq!(created.name(), source_name.as_str());
                assert_eq!(
                    fs::read(namespace.path().join(&source_name).join("payload"))
                        .expect("stage bytes"),
                    b"admitted bytes"
                );
                assert!(!namespace.path().join(destination).exists());
                if attempts.get() < 3 {
                    Err(CreatedDirectoryRenameError::Rename(
                        DirectoryRenameError::NotCommitted(io::Error::from_raw_os_error(32)),
                    ))
                } else {
                    created.rename_noreplace(destination)
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
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::DurableCache)
            .expect("publication gate");
        let result = candidate.publish_with(
            "next",
            &fence,
            |_, _| {
                attempts.set(attempts.get() + 1);
                Err(CreatedDirectoryRenameError::Rename(
                    DirectoryRenameError::NotCommitted(io::Error::from_raw_os_error(32)),
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
    fn failed_receipt_cleanup_is_terminal_and_does_not_start_a_fresh_stage() {
        #[derive(Debug, Eq, PartialEq)]
        enum AttemptFailure {
            Retryable,
            CleanupRefused,
        }

        impl TransientDenial for AttemptFailure {
            fn is_transient_denial(&self) -> bool {
                matches!(self, Self::Retryable)
            }
        }

        let root = scratch("cleanup-terminal");
        let namespace = namespace(&root);
        let attempts = Cell::new(0_u32);
        let schedule = [Duration::from_millis(1), Duration::from_millis(2)];
        let result = retry_with(
            || {
                attempts.set(attempts.get() + 1);
                let stage = stage(&namespace, ".stage", "admitted bytes");
                fs::rename(
                    namespace.path().join(".stage"),
                    namespace.path().join("moved-created-object"),
                )
                .expect("move the owned stage away from its receipt name");
                let replacement = namespace
                    .directory
                    .create_private_dir(".stage")
                    .expect("create a replacement at the old name");
                fs::write(namespace.path().join(".stage/payload"), b"replacement")
                    .expect("replacement payload");
                drop(replacement);

                Err::<(), _>(failed_stage_preparation_with(
                    stage,
                    AttemptFailure::Retryable,
                    |_, _| AttemptFailure::CleanupRefused,
                ))
            },
            &schedule,
            |_| panic!("a cleanup refusal must not be retried as a transient build failure"),
        );

        assert_eq!(attempts.get(), 1);
        assert!(matches!(
            result,
            Err(StagePreparationFailure::Terminal(
                AttemptFailure::CleanupRefused
            ))
        ));
        assert_eq!(
            fs::read(namespace.path().join(".stage/payload")).expect("replacement survives"),
            b"replacement"
        );
        drop(namespace);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn publication_refuses_a_replaced_stage_name_and_preserves_both_objects() {
        let root = scratch("replaced-stage-name");
        let namespace = namespace(&root);
        let candidate = stage(&namespace, ".stage", "original staged bytes");
        let stage_path = candidate.path().to_path_buf();
        let original_identity = FileIdentity::of_path_nofollow(&stage_path)
            .expect("original stage identity");
        let moved_path = namespace.path().join("moved-original-stage");
        fs::rename(&stage_path, &moved_path).expect("move original stage away");
        let replacement = namespace
            .directory
            .create_private_dir(".stage")
            .expect("create replacement stage name");
        fs::write(stage_path.join("payload"), b"replacement bytes")
            .expect("write replacement stage");
        drop(replacement);
        let replacement_identity =
            FileIdentity::of_path_nofollow(&stage_path).expect("replacement stage identity");
        assert_ne!(original_identity, replacement_identity);

        let failure = publish(candidate, "generation")
            .expect_err("publication must reject a changed stage name");
        let PublicationFailure::Cleanup {
            operation: Some(operation),
            ..
        } = failure
        else {
            panic!("identity mismatch must remain distinct from rename refusal");
        };
        assert!(matches!(
            *operation,
            PublicationFailure::OwnershipChanged(_)
        ));
        assert!(!namespace.path().join("generation").exists());
        assert_eq!(
            FileIdentity::of_path_nofollow(&moved_path).expect("moved original identity"),
            original_identity
        );
        assert_eq!(
            FileIdentity::of_path_nofollow(&stage_path).expect("replacement identity"),
            replacement_identity
        );
        assert_eq!(
            fs::read(stage_path.join("payload")).expect("replacement bytes survive"),
            b"replacement bytes"
        );
        drop(namespace);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn publication_refences_the_stage_name_after_its_initial_path_check() {
        let root = scratch("stage-name-replaced-after-preflight");
        let namespace = namespace(&root);
        let candidate = stage(&namespace, ".stage", "original staged bytes");
        let stage_path = candidate.path().to_path_buf();
        let moved_path = namespace.path().join("moved-original-stage");
        let attempts = Cell::new(0_u32);
        let pauses = Cell::new(0_u32);
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::DurableCache)
            .expect("publication gate");
        let failure = candidate
            .publish_with(
                "generation",
                &fence,
                |created, destination| {
                    attempts.set(attempts.get() + 1);
                    // This callback runs after publish_once's initial path
                    // checks. It models a same-user actor that ignores the
                    // cooperating namespace lock before the receipt's final
                    // direct-name check.
                    fs::rename(&stage_path, &moved_path).expect("move original stage");
                    let replacement = namespace
                        .directory
                        .create_private_dir(created.name())
                        .expect("install replacement at the source name");
                    fs::write(stage_path.join("payload"), b"replacement bytes")
                        .expect("write replacement payload");
                    drop(replacement);
                    created.rename_noreplace(destination)
                },
                |_| true,
                &[Duration::from_millis(1)],
                |_| pauses.set(pauses.get() + 1),
            )
            .expect_err("the receipt must reject the changed direct name");

        let PublicationFailure::Cleanup {
            operation: Some(operation),
            ..
        } = failure
        else {
            panic!("changed-name cleanup must preserve the original operation");
        };
        assert!(matches!(*operation, PublicationFailure::OwnershipChanged(_)));
        assert_eq!(attempts.get(), 1, "identity failures are never retried");
        assert_eq!(pauses.get(), 0);
        assert!(!namespace.path().join("generation").exists());
        assert_eq!(
            fs::read(moved_path.join("payload")).expect("original stage remains available"),
            b"original staged bytes"
        );
        assert_eq!(
            fs::read(stage_path.join("payload")).expect("replacement is untouched"),
            b"replacement bytes"
        );
        drop(namespace);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn publication_refuses_parent_path_replacement_but_receipt_cleans_original() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = scratch("replaced-stage-parent");
        let namespace = namespace(&root);
        let candidate = stage(&namespace, ".stage", "original staged bytes");
        let namespace_path = namespace.path().to_path_buf();
        let moved_namespace = root.join("moved-private-v1");
        let original_parent_identity =
            FileIdentity::of_path_nofollow(&namespace_path).expect("original parent identity");
        fs::rename(&namespace_path, &moved_namespace).expect("move pinned namespace");
        fs::create_dir(&namespace_path).expect("install replacement namespace");
        fs::set_permissions(
            &namespace_path,
            fs::Permissions::from_mode(0o700),
        )
        .expect("make replacement namespace private");
        let replacement_parent_identity =
            FileIdentity::of_path_nofollow(&namespace_path).expect("replacement parent identity");
        assert_ne!(original_parent_identity, replacement_parent_identity);
        let replacement_parent = DirectoryCapability::open(&namespace_path)
            .expect("open replacement namespace capability");
        let replacement = replacement_parent
            .create_private_dir(".stage")
            .expect("install replacement child");
        fs::write(namespace_path.join(".stage/payload"), b"replacement bytes")
            .expect("write replacement bytes");
        drop(replacement);
        drop(replacement_parent);

        let failure = publish(candidate, "generation")
            .expect_err("raw stage path must still reach its retained parent");
        assert!(matches!(failure, PublicationFailure::OwnershipChanged(_)));
        assert!(!moved_namespace.join(".stage").exists());
        assert_eq!(
            fs::read(namespace_path.join(".stage/payload")).expect("replacement bytes survive"),
            b"replacement bytes"
        );
        drop(namespace);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn parent_path_replacement_after_preflight_commits_only_through_the_pinned_parent() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = scratch("parent-replaced-after-preflight");
        let namespace = namespace(&root);
        let stage = stage(&namespace, ".stage", "committed bytes");
        let namespace_path = namespace.path().to_path_buf();
        let moved_namespace = root.join("moved-private-v1");
        let attempts = Cell::new(0_u32);
        let pauses = Cell::new(0_u32);
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::DurableCache)
            .expect("publication gate");
        let result = stage.publish_with(
            "generation",
            &fence,
            |created, destination| {
                attempts.set(attempts.get() + 1);
                // publish_once has checked the original raw parent path. The
                // receipt rename remains relative to its pinned parent even
                // when that raw path is changed before the syscall.
                fs::rename(&namespace_path, &moved_namespace).expect("move pinned namespace");
                fs::create_dir(&namespace_path).expect("install replacement namespace");
                fs::set_permissions(&namespace_path, fs::Permissions::from_mode(0o700))
                    .expect("make replacement namespace private");
                let replacement_parent = DirectoryCapability::open(&namespace_path)
                    .expect("pin replacement namespace");
                let replacement = replacement_parent
                    .create_private_dir("generation")
                    .expect("install replacement generation name");
                fs::write(
                    namespace_path.join("generation/payload"),
                    b"replacement bytes",
                )
                .expect("write replacement generation");
                drop(replacement);
                drop(replacement_parent);
                created.rename_noreplace(destination)
            },
            |_| true,
            &[Duration::from_millis(1)],
            |_| pauses.set(pauses.get() + 1),
        );

        assert!(matches!(
            result,
            Err(PublicationFailure::CommittedButOwnershipUnverified { rename: None, .. })
        ));
        assert_eq!(attempts.get(), 1, "a post-commit ambiguity is terminal");
        assert_eq!(pauses.get(), 0);
        assert_eq!(
            fs::read(moved_namespace.join("generation/payload")).expect("pinned-parent commit"),
            b"committed bytes"
        );
        assert_eq!(
            fs::read(namespace_path.join("generation/payload")).expect("replacement survives"),
            b"replacement bytes"
        );
        drop(namespace);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn postcommit_parent_path_replacement_is_reported_as_committed_unverified() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = scratch("postcommit-parent-replacement");
        let namespace = namespace(&root);
        let stage = stage(&namespace, ".stage", "committed bytes");
        let namespace_path = namespace.path().to_path_buf();
        let moved_namespace = root.join("moved-private-v1");
        let result = stage.publish_with(
            "generation",
            &namespace
                .acquire_fence(NamespaceFenceKind::DurableCache)
                .expect("publication gate"),
            |created, destination| {
                created.rename_noreplace(destination)?;
                fs::rename(&namespace_path, &moved_namespace).expect("move committed namespace");
                fs::create_dir(&namespace_path).expect("install replacement namespace");
                fs::set_permissions(
                    &namespace_path,
                    fs::Permissions::from_mode(0o700),
                )
                .expect("make replacement namespace private");
                Ok(())
            },
            |_| false,
            &[],
            |_| panic!("postcommit identity mismatch must never retry"),
        );
        assert!(matches!(
            result,
            Err(PublicationFailure::CommittedButOwnershipUnverified { rename: None, .. })
        ));
        assert_eq!(
            fs::read(moved_namespace.join("generation/payload")).expect("committed bytes remain"),
            b"committed bytes"
        );
        assert!(!namespace_path.join("generation").exists());
        drop(namespace);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn postcommit_destination_replacement_is_reported_without_removing_either_object() {
        let root = scratch("postcommit-destination-replacement");
        let namespace = namespace(&root);
        let stage = stage(&namespace, ".stage", "committed bytes");
        let destination_path = namespace.path().join("generation");
        let moved_commit = namespace.path().join("moved-committed-generation");
        let attempts = Cell::new(0_u32);
        let pauses = Cell::new(0_u32);
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::DurableCache)
            .expect("publication gate");
        let result = stage.publish_with(
            "generation",
            &fence,
            |created, destination| {
                attempts.set(attempts.get() + 1);
                created.rename_noreplace(destination)?;
                fs::rename(&destination_path, &moved_commit).expect("move committed generation");
                let replacement = namespace
                    .directory
                    .create_private_dir(destination)
                    .expect("install a replacement destination");
                fs::write(destination_path.join("payload"), b"replacement bytes")
                    .expect("write replacement destination");
                drop(replacement);
                Ok(())
            },
            |_| true,
            &[Duration::from_millis(1)],
            |_| pauses.set(pauses.get() + 1),
        );

        assert!(matches!(
            result,
            Err(PublicationFailure::CommittedButOwnershipUnverified {
                rename: None,
                ..
            })
        ));
        assert_eq!(attempts.get(), 1, "the committed rename is never replayed");
        assert_eq!(pauses.get(), 0);
        assert_eq!(
            fs::read(moved_commit.join("payload")).expect("committed bytes remain"),
            b"committed bytes"
        );
        assert_eq!(
            fs::read(destination_path.join("payload")).expect("replacement remains"),
            b"replacement bytes"
        );
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
            publish(stage, "generation").expect("no-replace winner"),
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
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::DurableCache)
            .expect("publication gate");
        let result = stage.publish_with(
            "generation",
            &fence,
            |created, destination| {
                attempts.set(attempts.get() + 1);
                // The real no-replace rename commits first; the seam then
                // injects the platform's ambiguous post-commit disposition.
                created
                    .rename_noreplace(destination)
                    .expect("the injected post-commit failure follows a real rename");
                Err(CreatedDirectoryRenameError::Rename(
                    DirectoryRenameError::CommittedButNotDurable(
                        io::Error::from_raw_os_error(32),
                    ),
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
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::DurableCache)
            .expect("publication gate");
        assert!(namespace.move_aside("entry", "quarantine", &fence).is_err());
        assert_eq!(
            fs::read(namespace.path().join("quarantine/payload")).expect("winner payload"),
            b"winner"
        );
        assert!(namespace.path().join("entry").is_dir());
        drop(namespace);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn a_busy_or_replaced_namespace_fence_never_admits_a_stale_handle() {
        let root = scratch("fence-replacement");
        let namespace = namespace(&root);
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::DurableCache)
            .expect("first gate");
        let busy = namespace.acquire_fence_with_wait(
            NamespaceFenceKind::DurableCache,
            Duration::from_millis(5),
        );
        assert!(matches!(busy, Err(ref error) if error.kind() == io::ErrorKind::WouldBlock));

        let gate_path = namespace.path().join("durable-cache.lock");
        let old_identity = fence.identity;
        fs::rename(&gate_path, namespace.path().join("displaced-gate"))
            .expect("move the held gate name");
        let replacement = namespace
            .directory
            .create_file_exclusive("durable-cache.lock")
            .expect("create replacement gate");
        assert_ne!(FileIdentity::of_file(&replacement).expect("replacement identity"), old_identity);
        assert!(fence.verify_for(&namespace).is_err());
        drop(replacement);
        drop(fence);
        drop(namespace);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn an_active_stage_is_skipped_and_unwound_stage_is_recovered_under_the_fence() {
        let root = scratch("active-stage-sweep");
        let namespace = namespace(&root);
        let stage_name = ".0000000000000000000000000000000000000000000000000000000000000000.building-1-2";
        let stage = stage(&namespace, stage_name, "still building");
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::DurableCache)
            .expect("sweep gate");
        assert_eq!(
            namespace.sweep_stage(stage_name, &fence).expect("active sweep"),
            StageSweepDisposition::Active
        );
        assert!(stage.path().join("payload").is_file());
        drop(fence);

        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            drop(stage);
            panic!("simulate builder unwind after its active lease was dropped");
        }));
        assert!(unwound.is_err());
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::DurableCache)
            .expect("recovery gate");
        assert_eq!(
            namespace.sweep_stage(stage_name, &fence).expect("stale sweep"),
            StageSweepDisposition::Removed
        );
        assert!(!namespace.path().join(stage_name).exists());
        assert!(!namespace
            .path()
            .join(stage_lease_file_name(stage_name))
            .exists());
        drop(fence);
        drop(namespace);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn replaced_active_marker_refuses_publication_and_preserves_both_objects() {
        let root = scratch("replaced-active-marker");
        let namespace = namespace(&root);
        let stage_name = ".stage";
        let stage = stage(&namespace, stage_name, "stage bytes");
        let marker_name = stage_lease_file_name(stage_name);
        let marker_path = namespace.path().join(&marker_name);
        let displaced = namespace.path().join("displaced-active-marker");
        let held_identity = stage.lease_identity;
        fs::rename(&marker_path, &displaced).expect("move original active marker");
        let replacement = namespace
            .directory
            .create_file_exclusive(&marker_name)
            .expect("replace active marker");
        let replacement_identity = FileIdentity::of_file(&replacement).expect("replacement identity");
        assert_ne!(held_identity, replacement_identity);
        drop(replacement);

        let fence = namespace
            .acquire_fence(NamespaceFenceKind::DurableCache)
            .expect("publication gate");
        let failure = stage
            .publish("generation", &fence)
            .expect_err("the owner receipt rejects a replaced active marker");
        assert!(matches!(
            failure,
            PublicationFailure::Cleanup {
                operation: Some(_),
                ..
            }
        ));
        assert!(!namespace.path().join("generation").exists());
        assert!(namespace.path().join(stage_name).is_dir());
        assert_eq!(
            FileIdentity::of_path_nofollow(&marker_path).expect("replacement survives"),
            replacement_identity
        );
        assert_eq!(
            FileIdentity::of_path_nofollow(&displaced).expect("original marker survives"),
            held_identity
        );
        assert!(namespace.sweep_stage(stage_name, &fence).is_err());
        assert!(namespace.path().join(stage_name).is_dir());
        drop(fence);
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
