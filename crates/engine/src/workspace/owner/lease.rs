//! Filesystem lease, epoch, and full-width fencing capability.

use super::WorkspaceError;
use backend_platform::{DirectoryCapability, OwnedWorkspaceDirectory};
use backend_store::{PublicationAuthorityError, StorePublicationAuthority};
use blake3::Hasher;
use std::fmt;
#[cfg(test)]
use std::fs;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static OWNER_STATE_TEMPORARY: AtomicU64 = AtomicU64::new(0);

const LOCK_FILE: &str = "OWNER.lock";
const OWNER_STATE_FILE: &str = "OWNER.state";
const OWNER_STATE_MAGIC: &[u8] = b"LUNA_OWNER_STATE_V1\0";
const OWNER_STATE_BYTES: usize = OWNER_STATE_MAGIC.len() + 8 + 32 + 32;

/// Filesystem owner lock with durable monotonic epoch and full-width fence.
pub struct OwnerLease {
    directory: PathBuf,
    authority: StorePublicationAuthority,
    root: DirectoryCapability,
    epoch: u64,
    fence: [u8; 32],
}

impl fmt::Debug for OwnerLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnerLease")
            .field("directory", &self.directory)
            .field("authority", &self.authority)
            .field("epoch", &self.epoch)
            .field("fence", &self.fence)
            .finish()
    }
}

/// Immutable observation of the owner's epoch and fence. This has no kernel
/// lock or store publication capability and cannot select a workspace head.
#[derive(Clone, Debug)]
pub struct OwnerLeaseIdentity {
    directory: PathBuf,
    root: DirectoryCapability,
    epoch: u64,
    fence: [u8; 32],
}

impl OwnerLeaseIdentity {
    /// Returns the epoch observed while the exclusive lease was held.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Returns the full-width observed owner fence.
    #[must_use]
    pub const fn fence(&self) -> [u8; 32] {
        self.fence
    }

    /// Checks the durable identity without creating or duplicating a writer.
    pub fn assert_current(&self) -> Result<(), WorkspaceError> {
        assert_owner_identity(&self.directory, &self.root, self.epoch, self.fence)
    }
}

impl OwnerLease {
    pub(super) fn identity(&self) -> OwnerLeaseIdentity {
        OwnerLeaseIdentity {
            directory: self.directory.clone(),
            root: self.root.clone(),
            epoch: self.epoch,
            fence: self.fence,
        }
    }
    /// Acquires the owner lock and durably advances the epoch.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn acquire(directory: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        let directory = directory.as_ref().to_owned();
        let owned = open_workspace_directory(&directory, 64).map_err(WorkspaceError::io)?;
        let directory = owned.path().to_path_buf();
        let root = DirectoryCapability::open(&directory).map_err(WorkspaceError::io)?;
        root.validate_private().map_err(WorkspaceError::io)?;
        let lock_path = directory.join(LOCK_FILE);
        let authority =
            StorePublicationAuthority::acquire(&lock_path).map_err(|error| match error {
                PublicationAuthorityError::Busy => WorkspaceError::AlreadyOwned,
                PublicationAuthorityError::Io(error) => WorkspaceError::Store(error),
            })?;
        let epoch_path = directory.join(OWNER_STATE_FILE);
        let prior = read_owner_state(&root, &epoch_path)?;
        let epoch = prior.checked_add(1).ok_or(WorkspaceError::Bounds)?;
        let mut hasher = Hasher::new();
        hasher.update(b"backend.engine.owner-fence.v4\0");
        hasher.update(&epoch.to_be_bytes());
        hasher.update(&std::process::id().to_be_bytes());
        hasher.update(lock_path.to_string_lossy().as_bytes());
        let fence = *hasher.finalize().as_bytes();
        let state = encode_owner_state(epoch, fence);
        // Epoch and fence are one authenticated atomic file.  The lock is
        // held while this replacement and its parent-directory sync complete.
        // A failed write therefore leaves either the previous complete state
        // or the new complete state, never a pair of independently updated
        // fields.
        write_atomic_unfaulted(&root, &epoch_path, &state)?;
        Ok(Self {
            directory,
            authority,
            root,
            epoch,
            fence,
        })
    }

    /// Acquires the owner after the previous process has released its kernel
    /// lease.  A live owner cannot be guessed stale or renamed around.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn reclaim(directory: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        Self::acquire(directory)
    }

    /// Returns the durable owner epoch.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Returns the full-width fencing token.
    #[must_use]
    pub const fn fence(&self) -> [u8; 32] {
        self.fence
    }

    /// Retains this owner lock and fence for explicit legacy directory admission.
    /// Strict platform directory opens continue to reject insecure existing modes.
    #[must_use]
    pub fn directory_admission(&self) -> WorkspaceDirectoryAdmission {
        WorkspaceDirectoryAdmission {
            directory: self.directory.clone(),
            root: self.root.clone(),
            authority: self.authority.clone(),
            epoch: self.epoch,
            fence: self.fence,
        }
    }

    pub(crate) const fn publication_authority(&self) -> &StorePublicationAuthority {
        &self.authority
    }

    /// Rejects use after another owner has replaced this lock.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn assert_current(&self) -> Result<(), WorkspaceError> {
        assert_owner_identity(&self.directory, &self.root, self.epoch, self.fence)
    }
}

fn assert_owner_identity(
    directory: &Path,
    root: &DirectoryCapability,
    expected_epoch: u64,
    expected_fence: [u8; 32],
) -> Result<(), WorkspaceError> {
    root.verify_path(directory).map_err(WorkspaceError::io)?;
    root.validate_private().map_err(WorkspaceError::io)?;
    let state = match read_private_owner_state(root) {
        Ok(state) => state,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(WorkspaceError::Fenced);
        }
        Err(error) => return Err(WorkspaceError::io(error)),
    };
    let (epoch, fence) = decode_owner_state(&state)?;
    if epoch == expected_epoch && fence == expected_fence {
        Ok(())
    } else {
        Err(WorkspaceError::Fenced)
    }
}

/// Admission capability for exact application-state paths below a live owner.
///
/// The kernel lock is retained by this value. No directory is enumerated, no
/// ancestor outside the workspace is changed, and every encountered existing
/// component is opened without following links before its ownership is checked.
/// Legacy Unix modes can be repaired. Windows retains strict existing DACL
/// admission and private creation; this capability does not repair old DACLs.
#[derive(Clone, Debug)]
pub struct WorkspaceDirectoryAdmission {
    directory: PathBuf,
    root: DirectoryCapability,
    authority: StorePublicationAuthority,
    epoch: u64,
    fence: [u8; 32],
}

impl WorkspaceDirectoryAdmission {
    /// Admits one known application directory relative to this locked workspace.
    /// On Unix, existing current-user-owned components are made private through
    /// their held handles. Windows requires already-private existing DACLs.
    /// Foreign directories, links, and unsafe components fail.
    /// The work is bounded by 64 components, independent of cache cardinality.
    pub fn admit(&self, relative: impl AsRef<Path>) -> io::Result<DirectoryCapability> {
        let relative = relative.as_ref();
        let names = relative
            .components()
            .take(65)
            .map(|component| match component {
                std::path::Component::Normal(name) => name.to_str().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "state directory name is not UTF-8",
                    )
                }),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "state directory must be workspace-relative",
                )),
            })
            .collect::<io::Result<Vec<_>>>()?;
        if names.is_empty() || names.len() > 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "state directory component bound exceeded",
            ));
        }
        // Retaining authority keeps the kernel lease held across every chmod.
        let _authority = &self.authority;
        self.root.verify_path(&self.directory)?;
        self.root.validate_private()?;
        let (epoch, fence) = decode_owner_state(&read_private_owner_state(&self.root)?)
            .map_err(|error| io::Error::other(error.to_string()))?;
        if epoch != self.epoch || fence != self.fence {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "workspace owner was fenced",
            ));
        }
        let mut current = self.root.clone();
        let mut path = self.directory.clone();
        for name in names {
            path.push(name);
            current = (|| {
                let child = match current.open_dir(name) {
                    Ok(child) => child,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        match current.create_private_dir(name) {
                            Ok(child) => child,
                            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                                current.open_dir(name)?
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    Err(error) => return Err(error),
                };
                if child.validate_private().is_err() {
                    child.restrict_private()?;
                    child.validate_private()?;
                    child.sync_all()?;
                }
                Ok(child)
            })()
            .map_err(|error: io::Error| {
                io::Error::new(error.kind(), format!("{}: {error}", path.display()))
            })?;
        }
        self.root.verify_path(&self.directory)?;
        Ok(current)
    }
}

// Preserve direct owner acquisition on a missing directory suffix. Every new
// component is created privately below a pinned trusted parent; existing
// ancestors are only checked, never chmodded or followed through links.
fn open_workspace_directory(path: &Path, remaining: usize) -> io::Result<OwnedWorkspaceDirectory> {
    if remaining == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "workspace creation component bound exceeded",
        ));
    }
    match OwnedWorkspaceDirectory::open(path) {
        Ok(directory) => Ok(directory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "workspace has no trusted parent",
                    )
                })?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "workspace has no final name")
                })?;
            open_workspace_directory(parent, remaining - 1)?.child(name)
        }
        Err(error) => Err(error),
    }
}

fn read_private_owner_state(root: &DirectoryCapability) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    root.open_private_file(OWNER_STATE_FILE)?
        .take((OWNER_STATE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn read_owner_state(root: &DirectoryCapability, path: &Path) -> Result<u64, WorkspaceError> {
    let mut file = match root.open_file_read(OWNER_STATE_FILE) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(owner_state_io(path, error)),
    };
    // Old builds wrote 0664 under umask 0002. Read only an owned, single-link
    // regular file; its authenticated epoch is then replaced privately.
    validate_owned_file(&file).map_err(|error| owner_state_io(path, error))?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take((OWNER_STATE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| owner_state_io(path, error))?;
    decode_owner_state(&bytes).map(|(epoch, _)| epoch)
}

fn owner_state_io(path: &Path, error: io::Error) -> WorkspaceError {
    WorkspaceError::io(io::Error::new(
        error.kind(),
        format!("{}: {error}", path.display()),
    ))
}

fn validate_owned_file(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.nlink() != 1
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "owner state is not a current-user-owned single-link regular file",
            ));
        }
    }
    Ok(())
}

fn encode_owner_state(epoch: u64, fence: [u8; 32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(OWNER_STATE_BYTES);
    bytes.extend_from_slice(OWNER_STATE_MAGIC);
    bytes.extend_from_slice(&epoch.to_be_bytes());
    bytes.extend_from_slice(&fence);
    let checksum = digest_owner_state(&bytes);
    bytes.extend_from_slice(&checksum);
    bytes
}

fn decode_owner_state(bytes: &[u8]) -> Result<(u64, [u8; 32]), WorkspaceError> {
    if bytes.len() != OWNER_STATE_BYTES || !bytes.starts_with(OWNER_STATE_MAGIC) {
        return Err(WorkspaceError::Corrupt("owner state"));
    }
    let epoch_at = OWNER_STATE_MAGIC.len();
    let epoch_end = epoch_at.checked_add(8).ok_or(WorkspaceError::Bounds)?;
    let fence_end = epoch_end.checked_add(32).ok_or(WorkspaceError::Bounds)?;
    let epoch = u64::from_be_bytes(
        bytes
            .get(epoch_at..epoch_end)
            .ok_or(WorkspaceError::Corrupt("owner state"))?
            .try_into()
            .map_err(|_| WorkspaceError::Corrupt("owner state"))?,
    );
    let fence: [u8; 32] = bytes
        .get(epoch_end..fence_end)
        .ok_or(WorkspaceError::Corrupt("owner state"))?
        .try_into()
        .map_err(|_| WorkspaceError::Corrupt("owner state"))?;
    let checksum: [u8; 32] = bytes
        .get(fence_end..)
        .ok_or(WorkspaceError::Corrupt("owner state"))?
        .try_into()
        .map_err(|_| WorkspaceError::Corrupt("owner state"))?;
    if digest_owner_state(&bytes[..fence_end]) != checksum {
        return Err(WorkspaceError::Corrupt("owner state"));
    }
    Ok((epoch, fence))
}

fn digest_owner_state(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(b"backend.engine.owner-state.v1\0");
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

fn write_atomic_unfaulted(
    root: &DirectoryCapability,
    path: &Path,
    bytes: &[u8],
) -> Result<(), WorkspaceError> {
    let sequence = OWNER_STATE_TEMPORARY.fetch_add(1, Ordering::Relaxed);
    let name = format!(".OWNER.state.tmp.{}.{sequence}", std::process::id());
    let mut file = root
        .create_file_exclusive(&name)
        .map_err(|error| owner_state_io(path, error))?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    // Windows exclusive private files pin their name without DELETE sharing.
    // Close the writer before requesting the replacing rename's DELETE access.
    drop(file);
    let result = written.and_then(|()| root.rename(&name, OWNER_STATE_FILE, true));
    if result.is_err() {
        let _ = root.remove_file(&name);
    }
    result.map_err(|error| owner_state_io(path, error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    const TEST_MODE: &str = "LUNA_OWNER_LEASE_TEST_MODE";
    const TEST_DIRECTORY: &str = "LUNA_OWNER_LEASE_TEST_DIRECTORY";

    fn test_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "backend-engine-owner-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn child(mode: &str, directory: &Path) -> std::process::Child {
        Command::new(std::env::current_exe().expect("test executable"))
            .arg(format!("owner_lease_child_{mode}"))
            .arg("--nocapture")
            .env(TEST_MODE, mode)
            .env(TEST_DIRECTORY, directory)
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn owner lease child")
    }

    #[test]
    fn owner_lease_process_contention_and_crash_reacquire() {
        let directory = test_directory("process");
        let first = OwnerLease::acquire(&directory).expect("first owner");
        let first_epoch = first.epoch();

        let contender = child("contender", &directory);
        let output = contender
            .wait_with_output()
            .expect("wait for contention child");
        assert!(
            output.status.success(),
            "contention child failed: {output:?}"
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("BUSY"));
        drop(first);

        let mut holder = child("holder", &directory);
        let stdout = holder.stdout.take().expect("holder stdout");
        let mut reader = BufReader::new(stdout);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut ready = false;
        let mut line = String::new();
        while Instant::now() < deadline && !ready {
            line.clear();
            if reader.read_line(&mut line).expect("read holder output") == 0 {
                break;
            }
            ready = line.trim() == "READY";
        }
        assert!(ready, "holder did not acquire lease: {line:?}");
        holder.kill().expect("kill holder");
        let _ = holder.wait().expect("wait for killed holder");

        let reacquired = OwnerLease::reclaim(&directory).expect("reacquire after crash");
        assert_eq!(reacquired.epoch(), first_epoch + 2);
        drop(reacquired);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn owner_lease_rejects_stale_fence_and_legacy_epoch_file() {
        let directory = test_directory("fence");
        let lease = OwnerLease::acquire(&directory).expect("owner");
        let replacement = encode_owner_state(lease.epoch() + 1, [7; 32]);
        write_atomic_unfaulted(&lease.root, &directory.join(OWNER_STATE_FILE), &replacement)
            .expect("replace owner state");
        assert!(matches!(
            lease.assert_current(),
            Err(WorkspaceError::Fenced)
        ));
        drop(lease);

        fs::remove_file(directory.join(OWNER_STATE_FILE)).expect("remove canonical state");
        fs::write(directory.join("OWNER.epoch"), b"17").expect("write legacy state");
        let canonical = OwnerLease::acquire(&directory).expect("legacy file is ignored");
        assert_eq!(canonical.epoch(), 1);
        drop(canonical);
        let _ = fs::remove_dir_all(directory);
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::MetadataExt;
        fs::symlink_metadata(path).expect("metadata").mode() & 0o777
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("fixture mode");
    }

    #[cfg(unix)]
    #[test]
    fn owner_lease_recovers_legacy_modes_and_partial_repair_without_scanning_cache() {
        let directory = test_directory("legacy-modes");
        let first = OwnerLease::acquire(&directory).expect("initial owner");
        let epoch = first.epoch();
        drop(first);
        set_mode(&directory.join(OWNER_STATE_FILE), 0o664);
        set_mode(&directory.join(LOCK_FILE), 0o664);
        fs::create_dir_all(directory.join("objects/packs")).expect("legacy directories");
        set_mode(&directory.join("objects"), 0o775);
        set_mode(&directory.join("objects/packs"), 0o775);
        fs::write(directory.join("objects/packs/kept"), b"old index bytes").expect("kept object");
        let second = OwnerLease::acquire(&directory).expect("retry owns legacy state");
        assert_eq!(second.epoch(), epoch + 1);
        assert_eq!(mode(&directory.join(OWNER_STATE_FILE)), 0o600);
        assert_eq!(mode(&directory.join(LOCK_FILE)), 0o600);
        let admission = second.directory_admission();
        admission
            .admit("objects/packs")
            .expect("exact legacy directories repaired");
        assert_eq!(mode(&directory.join("objects")), 0o700);
        assert_eq!(mode(&directory.join("objects/packs")), 0o700);
        assert_eq!(
            fs::read(directory.join("objects/packs/kept")).expect("kept bytes"),
            b"old index bytes"
        );
        // OWNER.state is already private; this separately insecure child must
        // still recover. Unrelated cache size and permissions cannot affect it.
        fs::create_dir(directory.join("cache")).expect("unrelated cache");
        set_mode(&directory.join("cache"), 0o775);
        for number in 0..1024 {
            fs::write(directory.join("cache").join(number.to_string()), b"cached")
                .expect("cache entry");
        }
        set_mode(&directory.join("objects/packs"), 0o775);
        admission
            .admit("objects/packs")
            .expect("partial manual repair is admitted");
        assert_eq!(mode(&directory.join("objects/packs")), 0o700);
        assert_eq!(
            mode(&directory.join("cache")),
            0o775,
            "unrequested cache was never traversed or repaired"
        );
        assert!(matches!(
            OwnerLease::acquire(&directory),
            Err(WorkspaceError::AlreadyOwned)
        ));
        drop(second);
        assert!(
            matches!(
                OwnerLease::acquire(&directory),
                Err(WorkspaceError::AlreadyOwned)
            ),
            "admission retains the owner lease"
        );
        drop(admission);
        OwnerLease::reclaim(&directory)
            .expect("cold owner reopen")
            .assert_current()
            .expect("cold fence");
        let _ = fs::remove_dir_all(directory);
    }

    #[cfg(unix)]
    #[test]
    fn owner_directory_admission_refuses_links_foreign_owners_and_unsafe_paths() {
        use std::os::unix::fs::symlink;
        let directory = test_directory("unsafe-directory");
        let outside = test_directory("outside-directory");
        fs::create_dir(&outside).expect("outside directory");
        set_mode(&outside, 0o775);
        let owner = OwnerLease::acquire(&directory).expect("owner");
        let admission = owner.directory_admission();
        symlink(&outside, directory.join("linked")).expect("symbolic link");
        let error = admission.admit("linked/child").expect_err("link refused");
        assert!(
            error
                .to_string()
                .contains(&directory.join("linked").display().to_string()),
            "{error}"
        );
        assert_eq!(mode(&outside), 0o775);
        assert!(!outside.join("child").exists());
        assert!(OwnerLease::acquire(directory.join("linked/new/workspace")).is_err());
        assert!(
            !outside.join("new").exists(),
            "missing-parent creation cannot follow a symlink"
        );
        assert_eq!(mode(&outside), 0o775);
        for relative in [Path::new("../outside"), outside.as_path(), Path::new("/")] {
            assert!(admission.admit(relative).is_err());
        }
        let too_deep = std::iter::repeat_n("part", 65).collect::<PathBuf>();
        assert!(admission.admit(too_deep).is_err());
        assert!(!directory.join("part").exists());
        fs::write(directory.join("special"), b"regular file").expect("non-directory child");
        assert!(
            admission
                .admit("special")
                .expect_err("file refused")
                .to_string()
                .contains("special")
        );
        if rustix::process::geteuid().is_root() {
            let foreign = directory.join("foreign");
            fs::create_dir(&foreign).expect("foreign fixture");
            set_mode(&foreign, 0o775);
            rustix::fs::chown(&foreign, Some(rustix::fs::Uid::from_raw(1)), None)
                .expect("foreign owner");
            let error = admission
                .admit("foreign")
                .expect_err("foreign owner refused");
            assert!(
                error.to_string().contains(&foreign.display().to_string()),
                "{error}"
            );
            assert_eq!(mode(&foreign), 0o775);
        }
        drop(admission);
        drop(owner);
        let _ = fs::remove_dir_all(directory);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn owner_lease_refuses_sensitive_file_links_without_touching_the_target() {
        use std::os::unix::fs::symlink;
        for name in [OWNER_STATE_FILE, LOCK_FILE] {
            let directory = test_directory("sensitive-link");
            drop(OwnerLease::acquire(&directory).expect("initial owner"));
            let target = directory.with_extension("outside-state");
            fs::rename(directory.join(name), &target).expect("move sensitive file");
            set_mode(&target, 0o664);
            let bytes = fs::read(&target).expect("target bytes");
            symlink(&target, directory.join(name)).expect("sensitive symlink");
            let error = OwnerLease::acquire(&directory).expect_err("sensitive link refused");
            assert!(error.to_string().contains(name), "{error}");
            assert_eq!(mode(&target), 0o664);
            assert_eq!(fs::read(&target).expect("target unchanged"), bytes);
            let _ = fs::remove_dir_all(directory);
            let _ = fs::remove_file(target);
        }
    }

    #[cfg(unix)]
    #[test]
    fn owner_lease_private_creation_survives_umask_0002_on_retry_and_cold_open() {
        let directory = test_directory("umask-0002");
        let output = child("private_umask", &directory)
            .wait_with_output()
            .expect("umask child");
        assert!(output.status.success(), "umask child failed: {output:?}");
        assert_eq!(mode(&directory), 0o700);
        assert_eq!(mode(&directory.join(OWNER_STATE_FILE)), 0o600);
        assert_eq!(mode(&directory.join(LOCK_FILE)), 0o600);
        let _ = fs::remove_dir_all(directory);
    }

    #[cfg(unix)]
    #[test]
    fn owner_lease_child_private_umask() {
        if std::env::var(TEST_MODE).ok().as_deref() != Some("private_umask") {
            return;
        }
        rustix::process::umask(rustix::fs::Mode::from_bits_truncate(0o002));
        let directory = PathBuf::from(std::env::var_os(TEST_DIRECTORY).expect("test directory"));
        let nested = directory.join("missing/parents/workspace");
        {
            let owner = OwnerLease::acquire(&nested).expect("missing parents created privately");
            owner.assert_current().expect("nested owner fence");
            for path in [
                &directory,
                &directory.join("missing"),
                &directory.join("missing/parents"),
                &nested,
            ] {
                assert_eq!(mode(path), 0o700);
            }
        }
        for _ in 0..3 {
            let owner = OwnerLease::acquire(&directory).expect("owner under umask 0002");
            owner
                .directory_admission()
                .admit("compiler/journal")
                .expect("private compiler child");
            assert_eq!(mode(&directory.join(OWNER_STATE_FILE)), 0o600);
            assert_eq!(mode(&directory.join("compiler/journal")), 0o700);
        }
    }

    #[cfg(windows)]
    #[test]
    fn owner_lease_windows_private_atomic_replacement_allows_reacquire() {
        let directory = test_directory("windows-private-replace");
        for expected_epoch in 1..=3 {
            let owner = OwnerLease::acquire(&directory)
                .expect("private writer closed before replacing rename");
            assert_eq!(owner.epoch(), expected_epoch);
            owner
                .assert_current()
                .expect("complete private state selected");
        }
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn owner_lease_child_contender() {
        if std::env::var(TEST_MODE).ok().as_deref() != Some("contender") {
            return;
        }
        let directory = std::env::var_os(TEST_DIRECTORY).expect("owner test directory");
        match OwnerLease::acquire(directory) {
            Err(WorkspaceError::AlreadyOwned) => println!("BUSY"),
            other => panic!("expected kernel lock contention, got {other:?}"),
        }
    }

    #[test]
    fn owner_lease_child_holder() {
        if std::env::var(TEST_MODE).ok().as_deref() != Some("holder") {
            return;
        }
        let directory = std::env::var_os(TEST_DIRECTORY).expect("owner test directory");
        let _lease = OwnerLease::acquire(directory).expect("child owner");
        println!("READY");
        io::stdout().flush().expect("flush readiness");
        std::thread::sleep(Duration::from_mins(1));
    }
}
