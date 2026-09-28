//! Private, durable worker compiler workspace snapshots.
//!
//! Workspace paths are never followed by name after their containing directory
//! has been opened. Unix uses `openat`/`mkdirat`/`unlinkat` relative to pinned
//! directory descriptors. Windows delegates to the platform's reviewed
//! handle-relative implementation; if it is unavailable, this module fails
//! closed rather than using path-based checks that have a replacement race.

use super::{AdmittedInputClosure, ClusterWorkerError, operation};
use backend_engine::application::{MAX_LOCAL_PACKAGE_SOURCE_BYTES, OwnedPackageSource};
use backend_engine::compiler_cluster_transport::{
    CompilerInputTreeRecordV2, CompilerWorkspaceFileRoleV2, validate_compiler_input_path,
};
use backend_store::UntrustedObjectId;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

const READY_MARKER: &[u8] = b"backend-worker-workspace-ready-v1\n";
static NEXT_WORKSPACE_SNAPSHOT: AtomicU64 = AtomicU64::new(0);

/// A complete immutable workspace snapshot. The guard retains the parent
/// directory handle so cleanup remains anchored if a path is replaced.
pub(super) struct MaterializedWorkspace {
    pub(super) root: Box<Path>,
    pub(super) sources: Box<[OwnedPackageSource]>,
    _cleanup: WorkspaceSnapshotGuard,
}

struct WorkspaceSnapshotGuard {
    #[cfg(unix)]
    parent: File,
    #[cfg(windows)]
    parent: backend_platform::win32::workspace_fs::WorkspaceRoot,
    root_name: String,
    ready_name: String,
}

impl std::fmt::Debug for WorkspaceSnapshotGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceSnapshotGuard")
            .field("root_name", &self.root_name)
            .field("ready_name", &self.ready_name)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for MaterializedWorkspace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MaterializedWorkspace")
            .field("root", &self.root)
            .field("sources", &self.sources.len())
            .field("_cleanup", &self._cleanup)
            .finish()
    }
}

impl Drop for WorkspaceSnapshotGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = remove_snapshot_tree_at(&self.parent, &self.root_name);
            let _ = remove_private_marker_at(&self.parent, &self.ready_name);
            let _ = remove_private_marker_at(&self.parent, &format!("{}.tmp", self.ready_name));
            let _ = self.parent.sync_all();
        }
        #[cfg(windows)]
        {
            let _ = self.parent.remove_dir_tree(&[&self.root_name]);
            let _ = self.parent.remove_file_relative(&[&self.ready_name]);
            let _ = self
                .parent
                .remove_file_relative(&[&format!("{}.tmp", self.ready_name)]);
            let _ = self.parent.flush_dir();
        }
        #[cfg(not(any(unix, windows)))]
        let _ = &self.root;
    }
}

/// Removes abandoned worker snapshots after durable assignment recovery has
/// settled. Only entries below an owner-private parent, with the worker's
/// reserved prefix and matching owner, are considered.
pub(super) fn reap_orphaned_workspace_snapshots(
    parent: impl AsRef<Path>,
) -> Result<usize, ClusterWorkerError> {
    #[cfg(unix)]
    {
        reap_unix(parent.as_ref())
    }
    #[cfg(windows)]
    {
        reap_windows(parent.as_ref())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = parent;
        Err(ClusterWorkerError::InvalidConfig)
    }
}

pub(super) fn materialize_workspace_snapshot(
    input: &AdmittedInputClosure,
    snapshot_parent: &Path,
    max_input_bytes: u64,
) -> Result<MaterializedWorkspace, ClusterWorkerError> {
    #[cfg(unix)]
    {
        materialize_unix(input, snapshot_parent, max_input_bytes)
    }
    #[cfg(windows)]
    {
        materialize_windows(input, snapshot_parent, max_input_bytes)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (input, snapshot_parent, max_input_bytes);
        Err(operation(
            "descriptor-relative workspace snapshots are unsupported on this platform",
        ))
    }
}

#[cfg(unix)]
fn materialize_unix(
    input: &AdmittedInputClosure,
    snapshot_parent: &Path,
    max_input_bytes: u64,
) -> Result<MaterializedWorkspace, ClusterWorkerError> {
    use rustix::fs::{
        AtFlags, Mode, OFlags, RenameFlags, fchmod, mkdirat, open, openat, renameat_with,
    };
    use rustix::io::Errno;
    use std::os::unix::fs::MetadataExt;
    use std::path::Component;
    use std::time::{SystemTime, UNIX_EPOCH};

    if !snapshot_parent.is_absolute() {
        return Err(ClusterWorkerError::InvalidConfig);
    }
    let mut parent = open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(operation)?;
    for component in snapshot_parent.components() {
        let name = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => name,
            Component::ParentDir | Component::Prefix(_) => {
                return Err(ClusterWorkerError::InvalidConfig);
            }
        };
        parent = File::from(
            openat(
                &parent,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(operation)?,
        );
    }
    let parent_metadata = parent.metadata().map_err(operation)?;
    if !parent_metadata.is_dir() || parent_metadata.mode() & 0o077 != 0 {
        return Err(ClusterWorkerError::InvalidConfig);
    }
    let expected_uid = parent_metadata.uid();
    prove_current_owner(&parent, expected_uid)?;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(operation)?
        .as_nanos();
    let (root_name, root_directory) = create_snapshot_directory(&parent, timestamp)?;
    let root_path = snapshot_parent.join(&root_name);
    let ready_name = format!("{root_name}.ready");
    let cleanup = WorkspaceSnapshotGuard {
        parent,
        root_name: root_name.clone(),
        ready_name: ready_name.clone(),
    };

    let root_metadata = root_directory.metadata().map_err(operation)?;
    validate_owned_directory(&root_metadata, expected_uid)?;
    fchmod(&root_directory, Mode::RWXU).map_err(operation)?;

    let mut records = input
        .workspace_tree
        .pages()
        .map(|page| page.record().clone())
        .collect::<Vec<_>>();
    records.sort_by(|left, right| left.path().cmp(right.path()));
    for record in &records {
        if let CompilerInputTreeRecordV2::Directory { path } = record
            && !path.is_empty()
        {
            checked_segments(path)?;
            ensure_workspace_directory(&root_directory, path, expected_uid)?;
        }
    }

    let mut sources = Vec::new();
    let mut source_bytes = 0_u64;
    let mut total_bytes = 0_u64;
    for record in &records {
        let CompilerInputTreeRecordV2::File {
            path,
            role,
            object_id,
            length,
        } = record
        else {
            continue;
        };
        let segments = checked_segments(path)?;
        total_bytes = total_bytes
            .checked_add(*length)
            .filter(|total| *total <= max_input_bytes)
            .ok_or(ClusterWorkerError::InputBounds)?;
        let (parent_path, leaf) = segments.split_at(segments.len() - 1);
        let parent_directory = if parent_path.is_empty() {
            root_directory.try_clone().map_err(operation)?
        } else {
            open_workspace_directory(&root_directory, parent_path, expected_uid)?
        };
        let file = openat(
            &parent_directory,
            leaf[0],
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map(File::from)
        .map_err(operation)?;
        let metadata = file.metadata().map_err(operation)?;
        validate_owned_regular_file(&metadata, expected_uid)?;
        let mut file = file;
        let verified = input
            .store
            .write_verified_object_payload(
                UntrustedObjectId::from_bytes(*object_id),
                *length,
                &mut file,
            )
            .map_err(operation)?;
        if verified.id().as_bytes() != object_id || verified.payload_len() != *length {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        file.sync_all().map_err(operation)?;
        fchmod(&file, Mode::RUSR).map_err(operation)?;
        file.sync_all().map_err(operation)?;
        if *role == CompilerWorkspaceFileRoleV2::Source {
            source_bytes = source_bytes
                .checked_add(*length)
                .filter(|total| *total <= MAX_LOCAL_PACKAGE_SOURCE_BYTES as u64)
                .ok_or(ClusterWorkerError::InputBounds)?;
            let source_len =
                usize::try_from(*length).map_err(|_| ClusterWorkerError::InputBounds)?;
            file.seek(SeekFrom::Start(0)).map_err(operation)?;
            let mut source = String::new();
            source.try_reserve(source_len).map_err(operation)?;
            file.take(length.saturating_add(1))
                .read_to_string(&mut source)
                .map_err(operation)?;
            if source.len() as u64 != *length {
                return Err(ClusterWorkerError::InvalidInputClosure);
            }
            sources.push(OwnedPackageSource::from_string(path, source).map_err(operation)?);
        }
    }
    if sources.is_empty() || total_bytes > max_input_bytes {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }

    let mut directories = records
        .iter()
        .filter_map(|record| match record {
            CompilerInputTreeRecordV2::Directory { path } => Some(path.as_ref()),
            CompilerInputTreeRecordV2::File { path, .. } => {
                path.rsplit_once('/').map(|(parent, _)| parent)
            }
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    directories.extend(std::iter::once(""));
    for directory in directories
        .into_iter()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let handle = if directory.is_empty() {
            root_directory.try_clone().map_err(operation)?
        } else {
            let segments = checked_segments(directory)?;
            open_workspace_directory(&root_directory, &segments, expected_uid)?
        };
        fchmod(&handle, Mode::RUSR | Mode::XUSR).map_err(operation)?;
        handle.sync_all().map_err(operation)?;
    }
    root_directory.sync_all().map_err(operation)?;

    publish_ready_marker_unix(&cleanup.parent, &ready_name, &root_name)?;
    cleanup.parent.sync_all().map_err(operation)?;

    // Keep ownership of the root handle until the marker has reached the parent.
    drop(root_directory);
    Ok(MaterializedWorkspace {
        root: root_path.into_boxed_path(),
        sources: sources.into_boxed_slice(),
        _cleanup: cleanup,
    })
}

#[cfg(unix)]
fn create_snapshot_directory(
    parent: &File,
    timestamp: u128,
) -> Result<(String, File), ClusterWorkerError> {
    use rustix::fs::{Mode, OFlags, mkdirat, openat};
    use rustix::io::Errno;

    for _ in 0..32 {
        let ordinal = NEXT_WORKSPACE_SNAPSHOT.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            "worker-snapshot-{}-{timestamp}-{ordinal}",
            std::process::id()
        );
        match mkdirat(parent, name.as_str(), Mode::RWXU) {
            Ok(()) => {
                let directory = openat(
                    parent,
                    name.as_str(),
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map(File::from)
                .map_err(operation)?;
                return Ok((name, directory));
            }
            Err(Errno::EXIST) => continue,
            Err(error) => return Err(operation(error)),
        }
    }
    Err(ClusterWorkerError::InvalidConfig)
}

#[cfg(unix)]
fn publish_ready_marker_unix(
    parent: &File,
    ready_name: &str,
    root_name: &str,
) -> Result<(), ClusterWorkerError> {
    use rustix::fs::{Mode, OFlags, RenameFlags, fchmod, openat, renameat_with};

    let temporary_name = format!("{ready_name}.tmp");
    let file = openat(
        parent,
        temporary_name.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map(File::from)
    .map_err(operation)?;
    let mut file = file;
    file.write_all(READY_MARKER).map_err(operation)?;
    file.write_all(root_name.as_bytes()).map_err(operation)?;
    file.write_all(b"\n").map_err(operation)?;
    file.sync_all().map_err(operation)?;
    fchmod(&file, Mode::RUSR).map_err(operation)?;
    file.sync_all().map_err(operation)?;
    drop(file);
    renameat_with(
        parent,
        temporary_name.as_str(),
        parent,
        ready_name,
        RenameFlags::NOREPLACE,
    )
    .map_err(operation)
}

#[cfg(unix)]
fn checked_segments(path: &str) -> Result<Vec<&str>, ClusterWorkerError> {
    validate_compiler_input_path(path).map_err(operation)?;
    Ok(path.split('/').collect())
}

#[cfg(unix)]
fn ensure_workspace_directory(
    root: &File,
    path: &str,
    expected_uid: u32,
) -> Result<(), ClusterWorkerError> {
    use rustix::fs::{Mode, OFlags, mkdirat, openat};
    use rustix::io::Errno;

    let segments = checked_segments(path)?;
    let mut current = root.try_clone().map_err(operation)?;
    for component in segments {
        match mkdirat(&current, component, Mode::RWXU) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(error) => return Err(operation(error)),
        }
        let next = File::from(
            openat(
                &current,
                &*component,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(operation)?,
        );
        validate_owned_directory(&next.metadata().map_err(operation)?, expected_uid)?;
        current = next;
    }
    Ok(())
}

#[cfg(unix)]
fn open_workspace_directory(
    root: &File,
    segments: &[&str],
    expected_uid: u32,
) -> Result<File, ClusterWorkerError> {
    use rustix::fs::{Mode, OFlags, openat};

    let mut current = root.try_clone().map_err(operation)?;
    for component in segments {
        let next = File::from(
            openat(
                &current,
                *component,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(operation)?,
        );
        validate_owned_directory(&next.metadata().map_err(operation)?, expected_uid)?;
        current = next;
    }
    Ok(current)
}

#[cfg(unix)]
fn validate_owned_directory(
    metadata: &std::fs::Metadata,
    expected_uid: u32,
) -> Result<(), ClusterWorkerError> {
    use std::os::unix::fs::MetadataExt;
    if !metadata.is_dir() || metadata.uid() != expected_uid || metadata.mode() & 0o077 != 0 {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    Ok(())
}

#[cfg(unix)]
fn validate_owned_regular_file(
    metadata: &std::fs::Metadata,
    expected_uid: u32,
) -> Result<(), ClusterWorkerError> {
    use std::os::unix::fs::MetadataExt;
    if !metadata.is_file()
        || metadata.uid() != expected_uid
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
    {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    Ok(())
}

#[cfg(unix)]
fn reap_unix(parent_path: &Path) -> Result<usize, ClusterWorkerError> {
    use rustix::fs::{AtFlags, Dir, Mode, OFlags, open, openat, unlinkat};
    use std::os::unix::fs::MetadataExt;
    use std::path::Component;

    if !parent_path.is_absolute() {
        return Err(ClusterWorkerError::InvalidConfig);
    }
    let mut parent = File::from(
        open(
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(operation)?,
    );
    for component in parent_path.components() {
        let name = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => name,
            Component::ParentDir | Component::Prefix(_) => {
                return Err(ClusterWorkerError::InvalidConfig);
            }
        };
        parent = File::from(
            openat(
                &parent,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(operation)?,
        );
    }
    let metadata = parent.metadata().map_err(operation)?;
    if !metadata.is_dir() || metadata.mode() & 0o077 != 0 {
        return Err(ClusterWorkerError::InvalidConfig);
    }
    let expected_uid = metadata.uid();
    prove_current_owner(&parent, expected_uid)?;
    let mut directory = Dir::read_from(&parent).map_err(operation)?;
    let mut names = Vec::<String>::new();
    while let Some(entry) = directory.read() {
        let entry = entry.map_err(operation)?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        if let Ok(name) = std::str::from_utf8(bytes)
            && (name.starts_with("worker-snapshot-") || name.starts_with(".worker-snapshot-owner-"))
        {
            names.push(name.to_owned());
        }
    }
    names.sort();

    let mut removed = 0_usize;
    let roots = names
        .iter()
        .filter(|name| !name.ends_with(".ready") && !name.ends_with(".ready.tmp"))
        .cloned()
        .collect::<Vec<_>>();
    for root_name in roots {
        match openat(
            &parent,
            root_name.as_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(root) => {
                let root = File::from(root);
                validate_owned_directory(&root.metadata().map_err(operation)?, expected_uid)?;
                remove_snapshot_contents(&root, expected_uid)?;
                unlinkat(&parent, root_name.as_str(), AtFlags::REMOVEDIR).map_err(operation)?;
                removed = removed
                    .checked_add(1)
                    .ok_or(ClusterWorkerError::InputBounds)?;
            }
            Err(error) if error == rustix::io::Errno::NOENT => continue,
            Err(error) if error == rustix::io::Errno::LOOP => {
                return Err(ClusterWorkerError::InvalidInputClosure);
            }
            Err(error) if error == rustix::io::Errno::NOTDIR => {
                // A prefix-matching non-directory is not a worker snapshot.
                continue;
            }
            Err(error) => return Err(operation(error)),
        }
        remove_private_marker_at(&parent, &format!("{root_name}.ready")).map_err(operation)?;
        remove_private_marker_at(&parent, &format!("{root_name}.ready.tmp")).map_err(operation)?;
    }
    // Clean crash-left marker temporaries only if they are plain, single-link
    // files created by this worker identity.
    for name in names
        .iter()
        .filter(|name| name.ends_with(".ready") || name.ends_with(".ready.tmp"))
    {
        remove_private_marker_at(&parent, name).map_err(operation)?;
    }
    for name in names
        .iter()
        .filter(|name| name.starts_with(".worker-snapshot-owner-"))
    {
        remove_private_marker_at(&parent, name).map_err(operation)?;
    }
    parent.sync_all().map_err(operation)?;
    Ok(removed)
}

#[cfg(unix)]
fn remove_snapshot_tree_at(parent: &File, root_name: &str) -> std::io::Result<()> {
    use rustix::fs::{AtFlags, Mode, OFlags, openat, unlinkat};
    use std::os::unix::fs::MetadataExt;

    let root = File::from(openat(
        parent,
        root_name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let metadata = root.metadata()?;
    if !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "workspace snapshot root changed type",
        ));
    }
    let expected_uid = parent.metadata()?.uid();
    if metadata.uid() != expected_uid {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "workspace snapshot root is not owned by its private parent owner",
        ));
    }
    remove_snapshot_contents_io(&root, expected_uid)?;
    Ok(unlinkat(parent, root_name, AtFlags::REMOVEDIR)?)
}

#[cfg(unix)]
fn remove_snapshot_contents(directory: &File, expected_uid: u32) -> Result<(), ClusterWorkerError> {
    remove_snapshot_contents_io(directory, expected_uid).map_err(operation)
}

#[cfg(unix)]
fn remove_snapshot_contents_io(directory: &File, expected_uid: u32) -> std::io::Result<()> {
    use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, fchmod, openat, statat, unlinkat};
    use std::os::unix::fs::MetadataExt;

    let metadata = directory.metadata()?;
    if !metadata.is_dir() || metadata.uid() != expected_uid || metadata.mode() & 0o077 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "workspace snapshot directory is not worker-owned",
        ));
    }
    fchmod(directory, Mode::RWXU)?;
    let mut entries = Dir::read_from(directory)?;
    let mut names = Vec::new();
    while let Some(entry) = entries.read() {
        let entry = entry?;
        let bytes = entry.file_name().to_bytes();
        if bytes != b"." && bytes != b".." {
            names.push(entry.file_name().to_owned());
        }
    }
    for name in names {
        match openat(
            directory,
            &name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(child) => {
                let child = File::from(child);
                let child_metadata = child.metadata()?;
                if !child_metadata.is_dir()
                    || child_metadata.uid() != expected_uid
                    || child_metadata.mode() & 0o077 != 0
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "workspace snapshot contains a foreign directory",
                    ));
                }
                remove_snapshot_contents_io(&child, expected_uid)?;
                child.sync_all()?;
                unlinkat(directory, &name, AtFlags::REMOVEDIR)?;
            }
            Err(error)
                if error == rustix::io::Errno::NOTDIR || error == rustix::io::Errno::LOOP =>
            {
                // On Unix, opening a symlink with O_DIRECTORY|O_NOFOLLOW may
                // report ENOTDIR instead of ELOOP. Inspect the directory
                // entry without following it so both syscall results take
                // the same safe unlink path.
                let entry = statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW)?;
                match FileType::from_raw_mode(entry.st_mode) {
                    FileType::Symlink => {
                        if entry.st_uid != expected_uid {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::PermissionDenied,
                                "workspace snapshot contains a foreign symbolic link",
                            ));
                        }
                        unlinkat(directory, &name, AtFlags::empty())?;
                    }
                    FileType::RegularFile => {
                        let file = File::from(openat(
                            directory,
                            &name,
                            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                            Mode::empty(),
                        )?);
                        let file_metadata = file.metadata()?;
                        if !file_metadata.is_file()
                            || file_metadata.uid() != expected_uid
                            || file_metadata.nlink() != 1
                            || file_metadata.mode() & 0o077 != 0
                        {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "workspace snapshot contains a foreign or aliased file",
                            ));
                        }
                        unlinkat(directory, &name, AtFlags::empty())?;
                    }
                    _ => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "workspace snapshot contains an unsupported filesystem entry",
                        ));
                    }
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    directory.sync_all()
}

#[cfg(unix)]
fn remove_private_marker_at(parent: &File, name: &str) -> std::io::Result<()> {
    use rustix::fs::{AtFlags, Mode, OFlags, openat, unlinkat};
    use std::os::unix::fs::MetadataExt;

    let file = match openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(file) => File::from(file),
        Err(error) if error == rustix::io::Errno::NOENT => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let parent_uid = parent.metadata()?.uid();
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != parent_uid
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "workspace readiness marker is not a private single-link file",
        ));
    }
    unlinkat(parent, name, AtFlags::empty()).map_err(Into::into)
}

#[cfg(unix)]
fn prove_current_owner(parent: &File, expected_uid: u32) -> Result<(), ClusterWorkerError> {
    use rustix::fs::{AtFlags, Mode, OFlags, openat, unlinkat};
    use std::os::unix::fs::MetadataExt;

    let probe_name = format!(
        ".worker-snapshot-owner-{}-{}",
        std::process::id(),
        NEXT_WORKSPACE_SNAPSHOT.fetch_add(1, Ordering::Relaxed)
    );
    let probe = File::from(
        openat(
            parent,
            probe_name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(operation)?,
    );
    let probe_metadata = probe.metadata();
    drop(probe);
    unlinkat(parent, probe_name.as_str(), AtFlags::empty()).map_err(operation)?;
    let probe_metadata = probe_metadata.map_err(operation)?;
    if probe_metadata.uid() == expected_uid && probe_metadata.nlink() == 1 {
        Ok(())
    } else {
        Err(ClusterWorkerError::InvalidConfig)
    }
}

#[cfg(windows)]
fn materialize_windows(
    input: &AdmittedInputClosure,
    snapshot_parent: &Path,
    max_input_bytes: u64,
) -> Result<MaterializedWorkspace, ClusterWorkerError> {
    use backend_platform::win32::workspace_fs::WorkspaceRoot;
    use std::time::{SystemTime, UNIX_EPOCH};

    let parent = WorkspaceRoot::open(snapshot_parent).map_err(operation)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(operation)?
        .as_nanos();
    let mut created = None;
    for _ in 0..32 {
        let ordinal = NEXT_WORKSPACE_SNAPSHOT.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            "worker-snapshot-{}-{timestamp}-{ordinal}",
            std::process::id()
        );
        match parent.create_child_dir_exclusive(&name) {
            Ok(root) => {
                created = Some((name, root));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(operation(error)),
        }
    }
    let (root_name, root) = created.ok_or(ClusterWorkerError::InvalidConfig)?;
    let root_path = snapshot_parent.join(&root_name);
    let ready_name = format!("{root_name}.ready");
    let cleanup = WorkspaceSnapshotGuard {
        parent,
        root_name: root_name.clone(),
        ready_name: ready_name.clone(),
    };

    let mut records = input
        .workspace_tree
        .pages()
        .map(|page| page.record().clone())
        .collect::<Vec<_>>();
    records.sort_by(|left, right| left.path().cmp(right.path()));
    for record in &records {
        if let CompilerInputTreeRecordV2::Directory { path } = record
            && !path.is_empty()
        {
            let segments = checked_windows_segments(path)?;
            ensure_windows_directory(&root, &segments)?;
        }
    }

    let mut sources = Vec::new();
    let mut source_bytes = 0_u64;
    let mut total_bytes = 0_u64;
    for record in &records {
        let CompilerInputTreeRecordV2::File {
            path,
            role,
            object_id,
            length,
        } = record
        else {
            continue;
        };
        let segments = checked_windows_segments(path)?;
        total_bytes = total_bytes
            .checked_add(*length)
            .filter(|total| *total <= max_input_bytes)
            .ok_or(ClusterWorkerError::InputBounds)?;
        let mut file = root.create_file_exclusive(&segments).map_err(operation)?;
        let verified = input
            .store
            .write_verified_object_payload(
                UntrustedObjectId::from_bytes(*object_id),
                *length,
                &mut file,
            )
            .map_err(operation)?;
        if verified.id().as_bytes() != object_id || verified.payload_len() != *length {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        file.sync_all().map_err(operation)?;
        let mut permissions = file.metadata().map_err(operation)?.permissions();
        permissions.set_readonly(true);
        file.set_permissions(permissions).map_err(operation)?;
        file.sync_all().map_err(operation)?;
        if *role == CompilerWorkspaceFileRoleV2::Source {
            source_bytes = source_bytes
                .checked_add(*length)
                .filter(|total| *total <= MAX_LOCAL_PACKAGE_SOURCE_BYTES as u64)
                .ok_or(ClusterWorkerError::InputBounds)?;
            let source_len =
                usize::try_from(*length).map_err(|_| ClusterWorkerError::InputBounds)?;
            file.seek(SeekFrom::Start(0)).map_err(operation)?;
            let mut source = String::new();
            source.try_reserve(source_len).map_err(operation)?;
            file.take(length.saturating_add(1))
                .read_to_string(&mut source)
                .map_err(operation)?;
            if source.len() as u64 != *length {
                return Err(ClusterWorkerError::InvalidInputClosure);
            }
            sources.push(OwnedPackageSource::from_string(path, source).map_err(operation)?);
        }
    }
    if sources.is_empty() || total_bytes > max_input_bytes {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    let directories = records
        .iter()
        .filter_map(|record| match record {
            CompilerInputTreeRecordV2::Directory { path } if !path.is_empty() => {
                Some(path.to_string())
            }
            CompilerInputTreeRecordV2::File { path, .. } => {
                path.rsplit_once('/').map(|(parent, _)| parent.to_owned())
            }
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    let mut directories = directories.iter().map(String::as_str).collect::<Vec<_>>();
    directories.sort_by_key(|path| std::cmp::Reverse(path.matches('/').count()));
    for directory in directories {
        let segments = checked_windows_segments(directory)?;
        root.flush_dir_relative(&segments).map_err(operation)?;
    }
    root.flush_dir().map_err(operation)?;
    publish_ready_marker_windows(&cleanup.parent, &ready_name, &root_name)?;
    cleanup.parent.flush_dir().map_err(operation)?;
    Ok(MaterializedWorkspace {
        root: root_path.into_boxed_path(),
        sources: sources.into_boxed_slice(),
        _cleanup: cleanup,
    })
}

#[cfg(windows)]
fn publish_ready_marker_windows(
    parent: &backend_platform::win32::workspace_fs::WorkspaceRoot,
    ready_name: &str,
    root_name: &str,
) -> Result<(), ClusterWorkerError> {
    let temporary_name = format!("{ready_name}.tmp");
    let mut file = parent
        .create_file_exclusive(&[&temporary_name])
        .map_err(operation)?;
    file.write_all(READY_MARKER).map_err(operation)?;
    file.write_all(root_name.as_bytes()).map_err(operation)?;
    file.write_all(b"\n").map_err(operation)?;
    file.sync_all().map_err(operation)?;
    drop(file);
    parent
        .rename_relative(&[&temporary_name], &[ready_name], false)
        .map_err(operation)
}

#[cfg(windows)]
fn ensure_windows_directory(
    root: &backend_platform::win32::workspace_fs::WorkspaceRoot,
    segments: &[&str],
) -> Result<(), ClusterWorkerError> {
    let mut current = root.clone();
    for component in segments {
        match current.open_dir_checked(&[*component]) {
            Ok(directory) => current = directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                current = current
                    .create_child_dir_exclusive(*component)
                    .map_err(operation)?;
            }
            Err(error) => return Err(operation(error)),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn checked_windows_segments(path: &str) -> Result<Vec<&str>, ClusterWorkerError> {
    validate_compiler_input_path(path).map_err(operation)?;
    Ok(path.split('/').collect())
}

#[cfg(windows)]
fn reap_windows(parent_path: &Path) -> Result<usize, ClusterWorkerError> {
    use backend_platform::win32::workspace_fs::WorkspaceRoot;

    let parent = WorkspaceRoot::open(parent_path).map_err(operation)?;
    let entries = parent.read_dir_checked(&[]).map_err(operation)?;
    let mut roots = entries
        .iter()
        .filter(|entry| {
            entry.kind == backend_platform::win32::workspace_fs::EntryKind::Directory
                && entry.name.starts_with("worker-snapshot-")
        })
        .map(|entry| entry.name.clone())
        .collect::<Vec<_>>();
    roots.sort();
    let mut removed = 0_usize;
    for root_name in &roots {
        parent
            .remove_dir_tree(&[root_name.as_str()])
            .map_err(operation)?;
        parent
            .remove_file_relative(&[&format!("{root_name}.ready")])
            .or_else(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(error)
                }
            })
            .map_err(operation)?;
        parent
            .remove_file_relative(&[&format!("{root_name}.ready.tmp")])
            .or_else(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(error)
                }
            })
            .map_err(operation)?;
        removed = removed
            .checked_add(1)
            .ok_or(ClusterWorkerError::InputBounds)?;
    }
    for entry in entries.iter().filter(|entry| {
        entry.kind == backend_platform::win32::workspace_fs::EntryKind::File
            && entry.name.starts_with("worker-snapshot-")
            && (entry.name.ends_with(".ready") || entry.name.ends_with(".ready.tmp"))
    }) {
        let stem = entry
            .name
            .strip_suffix(".ready.tmp")
            .or_else(|| entry.name.strip_suffix(".ready"))
            .unwrap_or_default();
        if !roots.iter().any(|root_name| root_name == stem) {
            parent
                .remove_file_relative(&[&entry.name])
                .map_err(operation)?;
        }
    }
    parent.flush_dir().map_err(operation)?;
    Ok(removed)
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::reap_orphaned_workspace_snapshots;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::PathBuf;

    fn fixture(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        // `/var` is a symlink to `/private/var` on macOS. The reaper walks
        // every path component with O_NOFOLLOW, so give it the canonical
        // owner-private parent just as persisted worker configuration does.
        fs::canonicalize(std::env::temp_dir())
            .expect("canonicalize temporary directory")
            .join(format!(
                "backend-worker-snapshot-{label}-{}-{nonce}",
                std::process::id()
            ))
    }

    fn private_directory(path: &std::path::Path) {
        fs::create_dir(path).expect("create private fixture");
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("restrict fixture");
    }

    #[test]
    fn cold_start_reaps_only_private_snapshot_trees_and_is_idempotent() {
        let parent = fixture("cold-reopen");
        private_directory(&parent);
        let orphan = parent.join("worker-snapshot-after-kill");
        fs::create_dir_all(orphan.join("src")).expect("create orphan tree");
        fs::write(orphan.join("src/lib.rs"), b"pub fn value() -> u8 { 1 }\n")
            .expect("write orphan source");
        fs::set_permissions(orphan.join("src/lib.rs"), fs::Permissions::from_mode(0o400))
            .expect("make snapshot file read-only");
        let outside = parent.join("outside-sentinel");
        fs::write(&outside, b"keep").expect("write unrelated sentinel");
        // Install the hostile link while the root is still writable, then
        // seal the orphan. The reaper must unlink the link itself and never
        // follow it, even though it cannot traverse the source tree.
        symlink(&outside, orphan.join("sentinel-link")).expect("add hostile symbolic link");
        fs::set_permissions(orphan.join("src"), fs::Permissions::from_mode(0o500))
            .expect("seal source directory");
        fs::set_permissions(&orphan, fs::Permissions::from_mode(0o500))
            .expect("seal snapshot root");

        assert_eq!(
            reap_orphaned_workspace_snapshots(&parent).expect("reap cold-start orphan"),
            1
        );
        assert!(!orphan.exists());
        assert_eq!(fs::read(&outside).expect("read sentinel"), b"keep");
        assert_eq!(
            reap_orphaned_workspace_snapshots(&parent).expect("repeat cold-start reaping"),
            0
        );
        fs::remove_dir_all(parent).expect("remove fixture");
    }

    #[test]
    fn orphan_reaper_rejects_a_hardlink_alias_without_unlinking_it() {
        let parent = fixture("hardlink");
        private_directory(&parent);
        let outside = parent.join("outside-sentinel");
        fs::write(&outside, b"keep").expect("write sentinel");
        let orphan = parent.join("worker-snapshot-aliased");
        fs::create_dir(&orphan).expect("create orphan root");
        fs::hard_link(&outside, orphan.join("alias")).expect("create hardlink alias");

        assert!(reap_orphaned_workspace_snapshots(&parent).is_err());
        assert_eq!(fs::read(&outside).expect("read sentinel"), b"keep");
        assert_eq!(fs::read(orphan.join("alias")).expect("read alias"), b"keep");
        fs::remove_dir_all(parent).expect("remove fixture");
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::reap_orphaned_workspace_snapshots;
    use backend_platform::local::LocalListener;
    use backend_platform::win32::security::restrict_to_current_user;
    use backend_platform::win32::workspace_fs::WorkspaceRoot;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    fn fixture(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path = std::env::temp_dir().join(format!("bws-{label}-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).expect("create Windows fixture");
        restrict_to_current_user(&path).expect("make fixture owner-private");
        path
    }

    fn write_private_file(root: &WorkspaceRoot, path: &[&str], bytes: &[u8]) {
        use std::io::Write as _;
        let mut file = root
            .create_file_exclusive(path)
            .expect("create private fixture file");
        file.write_all(bytes).expect("write private fixture file");
        file.sync_all().expect("flush private fixture file");
    }

    #[test]
    fn windows_cold_start_reopens_and_reaps_a_private_snapshot() {
        let parent = fixture("cold-reopen");
        let root = WorkspaceRoot::open(&parent).expect("open private fixture");
        let orphan_name = "worker-snapshot-after-kill";
        let orphan = root
            .create_child_dir_exclusive(orphan_name)
            .expect("create orphan tree");
        let source_dir = orphan
            .create_child_dir_exclusive("src")
            .expect("create source directory");
        write_private_file(&source_dir, &["lib.rs"], b"pub fn value() -> u8 { 1 }\n");
        drop(source_dir);
        let outside_path = ["outside-sentinel"];
        write_private_file(&root, &outside_path, b"keep");
        drop(orphan);
        drop(root);
        let outside = parent.join("outside-sentinel");
        let orphan = parent.join(orphan_name);
        assert_eq!(
            fs::read(orphan.join("src/lib.rs")).expect("cold-reopen orphan source"),
            b"pub fn value() -> u8 { 1 }\n"
        );

        assert_eq!(
            reap_orphaned_workspace_snapshots(&parent).expect("cold-start reap"),
            1
        );
        assert!(!orphan.exists());
        assert_eq!(fs::read(&outside).expect("read sentinel"), b"keep");
        assert_eq!(
            reap_orphaned_workspace_snapshots(&parent).expect("idempotent cold-start reap"),
            0
        );
        fs::remove_dir_all(parent).expect("remove fixture");
    }

    #[test]
    fn windows_orphan_reaper_rejects_a_junction_and_preserves_its_target() {
        let parent = fixture("junction");
        let root = WorkspaceRoot::open(&parent).expect("open private fixture");
        let outside_root = root
            .create_child_dir_exclusive("outside")
            .expect("create outside directory");
        write_private_file(&outside_root, &["sentinel"], b"keep");
        drop(outside_root);
        let outside = parent.join("outside");
        let orphan_root = root
            .create_child_dir_exclusive("worker-snapshot-junction")
            .expect("create orphan root");
        drop(orphan_root);
        drop(root);
        let orphan = parent.join("worker-snapshot-junction");
        let junction = orphan.join("junction");
        let output = Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .output()
            .expect("run Windows junction fixture command");
        assert!(
            output.status.success(),
            "mklink /J should succeed: {output:?}"
        );

        assert!(reap_orphaned_workspace_snapshots(&parent).is_err());
        assert_eq!(
            fs::read(outside.join("sentinel")).expect("read outside sentinel"),
            b"keep"
        );
        fs::remove_dir_all(parent).expect("remove fixture");
    }

    #[test]
    fn windows_orphan_reaper_rejects_an_af_unix_reparse_point() {
        let parent = fixture("reparse");
        let root = WorkspaceRoot::open(&parent).expect("open private fixture");
        write_private_file(&root, &["outside-sentinel"], b"keep");
        let orphan_root = root
            .create_child_dir_exclusive("worker-snapshot-r")
            .expect("create orphan root");
        drop(orphan_root);
        drop(root);
        let outside = parent.join("outside-sentinel");
        let orphan = parent.join("worker-snapshot-r");
        let endpoint = orphan.join("ep");
        let listener = LocalListener::bind(&endpoint).expect("create reparse endpoint");

        assert!(reap_orphaned_workspace_snapshots(&parent).is_err());
        assert_eq!(fs::read(&outside).expect("read sentinel"), b"keep");
        drop(listener);
        let _ = fs::remove_file(endpoint);
        fs::remove_dir_all(parent).expect("remove fixture");
    }

    #[test]
    fn windows_orphan_reaper_rejects_a_hardlink_alias_without_deleting_the_target() {
        let parent = fixture("hardlink");
        let root = WorkspaceRoot::open(&parent).expect("open private fixture");
        write_private_file(&root, &["outside-sentinel"], b"keep");
        let orphan_root = root
            .create_child_dir_exclusive("worker-snapshot-hardlink")
            .expect("create orphan root");
        drop(orphan_root);
        drop(root);
        let outside = parent.join("outside-sentinel");
        let orphan = parent.join("worker-snapshot-hardlink");
        fs::hard_link(&outside, orphan.join("alias")).expect("create hardlink alias");

        assert!(reap_orphaned_workspace_snapshots(&parent).is_err());
        assert_eq!(fs::read(&outside).expect("read sentinel"), b"keep");
        assert_eq!(fs::read(orphan.join("alias")).expect("read alias"), b"keep");
        fs::remove_dir_all(parent).expect("remove fixture");
    }
}
