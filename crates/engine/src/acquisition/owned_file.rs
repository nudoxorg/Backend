//! Process ownership of private files in the content store.
//!
//! A temporary or destination-stage file belongs to the process that created
//! it until that process drops it. The scavenger must never remove a file whose
//! owner is alive, however old its timestamp, while it must be able to remove
//! the file an owner abandoned by crashing. Two operating-system models express
//! that:
//!
//! * Unix uses an advisory `flock`-style lock on the file. The lock constrains
//!   only other lock requests, so the owner's verifier can keep reading the file
//!   by path while it is held.
//! * Windows byte-range locks are mandatory: a lock held through one handle makes
//!   every read and write through any *other* handle fail with
//!   `ERROR_LOCK_VIOLATION`, which breaks exactly those path-based reads. The
//!   Windows owner therefore holds a handle whose share mode excludes
//!   `FILE_SHARE_DELETE`. Other processes may still read the file, but while the
//!   handle is open nobody can delete or rename it, so a scavenger's removal
//!   attempt fails with a sharing violation and the file is treated as live.
//!
//! The module exposes one operation per ownership transition: [`create_owned`]
//! to claim a new file, [`OwnedFile`] to keep the claim, and [`reclaim`] to
//! remove an abandoned one. Callers never see which model is in force.

use std::{
    fs::{self, File, OpenOptions},
    io,
    path::Path,
};

/// The claim a process holds on one private file; dropping it releases the claim.
///
/// Dropping must happen before the owner removes the file itself on Windows, so
/// that its own handle does not block the deletion.
#[derive(Debug)]
pub(super) struct OwnedFile {
    /// Only held for its drop: the lock (Unix) or the delete-denying handle (Windows).
    _claim: File,
}

/// What a scavenger learned by trying to remove one private file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Reclaimed {
    /// No process owned the file and this call removed it.
    Removed,
    /// The file was already gone.
    Absent,
    /// A live owner still holds the file; nothing was changed.
    Owned,
}

/// Creates `path` exclusively and claims it for the calling process.
///
/// Returns the read/write handle used for the file's contents and the claim.
/// Closing the content handle does not release the claim, so a verifier can
/// read the closed file by path while the owner still protects it from the
/// scavenger.
///
/// # Errors
///
/// Returns [`io::ErrorKind::AlreadyExists`] when the path is taken, and the
/// operating system's error when the file cannot be created or claimed. A file
/// created but not claimed is removed before the error is returned.
pub(super) fn create_owned(path: &Path) -> io::Result<(File, OwnedFile)> {
    claim_new(path)
}

#[cfg(unix)]
fn claim_new(path: &Path) -> io::Result<(File, OwnedFile)> {
    let file = OpenOptions::new()
        .write(true)
        .read(true)
        .create_new(true)
        .open(path)?;
    let claim = match backend_platform::durability::open_or_create_regular_file_nofollow(path) {
        Ok(claim) => claim,
        Err(error) => {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(error);
        }
    };
    if let Err(error) = claim.try_lock() {
        drop(claim);
        drop(file);
        let _ = fs::remove_file(path);
        return Err(match error {
            fs::TryLockError::WouldBlock => {
                io::Error::other("new private file lock was unexpectedly busy")
            }
            fs::TryLockError::Error(error) => error,
        });
    }
    Ok((file, OwnedFile { _claim: claim }))
}

/// `FILE_SHARE_READ` from `winnt.h`.
#[cfg(windows)]
const FILE_SHARE_READ: u32 = 0x0000_0001;
/// `FILE_SHARE_WRITE` from `winnt.h`.
#[cfg(windows)]
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
/// `ERROR_SHARING_VIOLATION`: another handle's share mode forbids this access.
#[cfg(windows)]
const ERROR_SHARING_VIOLATION: i32 = 32;

#[cfg(windows)]
fn claim_new(path: &Path) -> io::Result<(File, OwnedFile)> {
    use std::os::windows::fs::OpenOptionsExt as _;

    // Sharing read and write but never delete: the holder lets verifiers read
    // by path, while rename and delete by anyone else fail until it is closed.
    let holder = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(path)?;
    match holder.try_clone() {
        Ok(contents) => Ok((contents, OwnedFile { _claim: holder })),
        Err(error) => {
            drop(holder);
            let _ = fs::remove_file(path);
            Err(error)
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn claim_new(_path: &Path) -> io::Result<(File, OwnedFile)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no private-file ownership model exists for this platform",
    ))
}

/// Removes `path` unless a live process owns it.
///
/// # Errors
///
/// Returns the operating system's error for any failure other than the file
/// being absent or owned.
pub(super) fn reclaim(path: &Path) -> io::Result<Reclaimed> {
    reclaim_unowned(path)
}

#[cfg(unix)]
fn reclaim_unowned(path: &Path) -> io::Result<Reclaimed> {
    let claim = match backend_platform::durability::open_regular_file_readwrite_nofollow(path) {
        Ok(claim) => claim,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Reclaimed::Absent),
        Err(error) => return Err(error),
    };
    match claim.try_lock() {
        Ok(()) => match fs::remove_file(path) {
            Ok(()) => Ok(Reclaimed::Removed),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Reclaimed::Absent),
            Err(error) => Err(error),
        },
        Err(fs::TryLockError::WouldBlock) => Ok(Reclaimed::Owned),
        Err(fs::TryLockError::Error(error)) if error.kind() == io::ErrorKind::NotFound => {
            Ok(Reclaimed::Absent)
        }
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

#[cfg(windows)]
fn reclaim_unowned(path: &Path) -> io::Result<Reclaimed> {
    match fs::remove_file(path) {
        Ok(()) => Ok(Reclaimed::Removed),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Reclaimed::Absent),
        Err(error) if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => Ok(Reclaimed::Owned),
        Err(error) => Err(error),
    }
}

#[cfg(not(any(unix, windows)))]
fn reclaim_unowned(_path: &Path) -> io::Result<Reclaimed> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no private-file ownership model exists for this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom, Write};

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "nudox-owned-file-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_nanos())
            ));
            fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }

        fn file(&self, name: &str) -> std::path::PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The failure the old two-handle design had on Windows: an owner that
    /// closed its content handle still has to let a verifier read the file by
    /// path, while holding its claim against the scavenger.
    #[test]
    fn a_closed_but_owned_file_is_readable_by_path_and_survives_reclaim() {
        let scratch = Scratch::new("readable");
        let path = scratch.file("artifact.part");
        let (mut contents, claim) = create_owned(&path).expect("claim a new file");
        contents.write_all(b"owned bytes").expect("write contents");
        contents.sync_all().expect("sync contents");
        drop(contents);

        let mut reader = backend_platform::durability::open_regular_file_nofollow(&path)
            .expect("a verifier reads the closed owned file by path");
        let mut observed = Vec::new();
        reader.read_to_end(&mut observed).expect("read by path");
        assert_eq!(observed, b"owned bytes");
        drop(reader);

        assert_eq!(reclaim(&path).expect("reclaim attempt"), Reclaimed::Owned);
        assert!(path.exists(), "a live owner's file must not be removed");

        drop(claim);
        assert_eq!(
            reclaim(&path).expect("reclaim abandoned"),
            Reclaimed::Removed
        );
        assert!(!path.exists(), "an abandoned file is removed");
    }

    #[test]
    fn the_content_handle_can_be_rewound_and_reread_while_owned() {
        let scratch = Scratch::new("reread");
        let path = scratch.file("artifact.stage");
        let (mut contents, _claim) = create_owned(&path).expect("claim a new file");
        contents.write_all(b"abc").expect("write");
        contents.seek(SeekFrom::Start(0)).expect("rewind");
        let mut observed = [0_u8; 3];
        contents.read_exact(&mut observed).expect("reread");
        assert_eq!(&observed, b"abc");
    }

    #[test]
    fn creation_is_exclusive() {
        let scratch = Scratch::new("exclusive");
        let path = scratch.file("artifact.part");
        let first = create_owned(&path).expect("first claim");
        let second = create_owned(&path).expect_err("a second claim on the same path");
        assert_eq!(second.kind(), io::ErrorKind::AlreadyExists);
        drop(first);
    }

    #[test]
    fn reclaiming_an_absent_file_reports_absence() {
        let scratch = Scratch::new("absent");
        assert_eq!(
            reclaim(&scratch.file("never-created.part")).expect("reclaim"),
            Reclaimed::Absent
        );
    }

    /// The owner of a file can always remove it after releasing its own claim,
    /// which is the order `Drop` for a private artifact relies on.
    #[test]
    fn an_owner_removes_its_file_after_releasing_the_claim() {
        let scratch = Scratch::new("self-remove");
        let path = scratch.file("artifact.part");
        let (contents, claim) = create_owned(&path).expect("claim");
        drop(contents);
        drop(claim);
        fs::remove_file(&path).expect("owner removes after release");
    }

    const CHILD_PATH: &str = "NUDOX_OWNED_FILE_CHILD_PATH";

    /// Child half of the hard-kill test: claim a file, announce it, and hold the
    /// claim until the parent kills this process. Without the environment
    /// variable it is an ordinary no-op test.
    #[test]
    fn owner_child_holds_a_claim_until_it_is_killed() {
        let Some(path) = std::env::var_os(CHILD_PATH) else {
            return;
        };
        let (_contents, _claim) = create_owned(Path::new(&path)).expect("child claims the file");
        println!("claimed");
        std::io::stdout().flush().expect("flush readiness");
        let mut input = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut input);
    }

    /// A crashed owner must not strand its file: the operating system closes the
    /// dead process's handles, so the scavenger sees an unowned file.
    #[test]
    fn a_hard_killed_owner_releases_its_claim() {
        use std::io::{BufRead as _, BufReader};
        use std::process::{Command, Stdio};

        let scratch = Scratch::new("hard-kill");
        let path = scratch.file("artifact.part");
        let mut child = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "acquisition::owned_file::tests::owner_child_holds_a_claim_until_it_is_killed",
                "--nocapture",
            ])
            .env(CHILD_PATH, &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn the owning child");
        let stdout = child.stdout.take().expect("child stdout");
        let claimed = BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .any(|line| line == "claimed");
        assert!(claimed, "the child never reported its claim");

        assert_eq!(
            reclaim(&path).expect("reclaim while the owner lives"),
            Reclaimed::Owned
        );
        assert!(path.exists(), "a live owner's file must not be removed");

        child.kill().expect("kill the owner");
        child.wait().expect("reap the owner");
        // Handle teardown of a terminated process is prompt but not instantaneous.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let outcome = loop {
            match reclaim(&path).expect("reclaim after the owner died") {
                Reclaimed::Owned if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                other => break other,
            }
        };
        assert_eq!(outcome, Reclaimed::Removed);
        assert!(!path.exists(), "the dead owner's file is reclaimed");
    }
}
