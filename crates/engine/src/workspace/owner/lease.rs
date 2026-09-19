//! Filesystem lease, epoch, and full-width fencing capability.

use super::WorkspaceError;
use backend_store::{PublicationAuthorityError, StorePublicationAuthority};
use blake3::Hasher;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const LOCK_FILE: &str = "OWNER.lock";
const OWNER_STATE_FILE: &str = "OWNER.state";
const OWNER_STATE_MAGIC: &[u8] = b"LUNA_OWNER_STATE_V1\0";
const OWNER_STATE_BYTES: usize = OWNER_STATE_MAGIC.len() + 8 + 32 + 32;

/// Filesystem owner lock with durable monotonic epoch and full-width fence.
pub struct OwnerLease {
    directory: PathBuf,
    authority: StorePublicationAuthority,
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

impl OwnerLease {
    /// Acquires the owner lock and durably advances the epoch.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn acquire(directory: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        let directory = directory.as_ref().to_owned();
        fs::create_dir_all(&directory).map_err(WorkspaceError::io)?;
        let lock_path = directory.join(LOCK_FILE);
        let authority =
            StorePublicationAuthority::acquire(&lock_path).map_err(|error| match error {
                PublicationAuthorityError::Busy => WorkspaceError::AlreadyOwned,
                PublicationAuthorityError::Io(error) => WorkspaceError::Store(error),
            })?;
        let epoch_path = directory.join(OWNER_STATE_FILE);
        let prior = read_owner_state(&epoch_path)?;
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
        write_atomic_unfaulted(&epoch_path, &state)?;
        Ok(Self {
            directory,
            authority,
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

    pub(crate) const fn publication_authority(&self) -> &StorePublicationAuthority {
        &self.authority
    }

    /// Rejects use after another owner has replaced this lock.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn assert_current(&self) -> Result<(), WorkspaceError> {
        let state = match fs::read(self.directory.join(OWNER_STATE_FILE)) {
            Ok(state) => state,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(WorkspaceError::Fenced);
            }
            Err(error) => return Err(WorkspaceError::io(error)),
        };
        let (epoch, fence) = decode_owner_state(&state)?;
        if epoch == self.epoch && fence == self.fence {
            Ok(())
        } else {
            Err(WorkspaceError::Fenced)
        }
    }
}

fn read_owner_state(path: &Path) -> Result<u64, WorkspaceError> {
    match fs::read(path) {
        Ok(bytes) if bytes.starts_with(OWNER_STATE_MAGIC) => {
            decode_owner_state(&bytes).map(|(epoch, _)| epoch)
        }
        Ok(_) => Err(WorkspaceError::Corrupt("owner state")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(WorkspaceError::io(error)),
    }
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

fn write_atomic_unfaulted(path: &Path, bytes: &[u8]) -> Result<(), WorkspaceError> {
    let parent = path.parent().ok_or(WorkspaceError::Bounds)?;
    fs::create_dir_all(parent).map_err(WorkspaceError::io)?;
    let tmp = parent.join(format!(
        ".{}.tmp.{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file"),
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&tmp)
        .map_err(WorkspaceError::io)?;
    file.write_all(bytes).map_err(WorkspaceError::io)?;
    file.sync_all().map_err(WorkspaceError::io)?;
    fs::rename(&tmp, path).map_err(WorkspaceError::io)?;
    super::sync_directory(parent)
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
        write_atomic_unfaulted(&directory.join(OWNER_STATE_FILE), &replacement)
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
