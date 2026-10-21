//! Linux-only, affine retirement of an authenticated automatic locald.
//! No process discovery, numeric-PID signal, endpoint unlink, or forced kill.
use super::{AuthenticatedLocalPeer, LocalStream};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "existing owner process custody could not be proven; restart the previous NuDox owner manually",
    )
}
fn bounded(path: &Path, maximum: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(refused());
    }
    Ok(bytes)
}
fn stat_birth(bytes: &[u8], pid: u32) -> io::Result<u64> {
    let text = std::str::from_utf8(bytes).map_err(|_| refused())?;
    let (number, _) = text.split_once(" (").ok_or_else(refused)?;
    let (_, tail) = text.rsplit_once(") ").ok_or_else(refused)?;
    if number.parse::<u32>().ok() != Some(pid) {
        return Err(refused());
    }
    tail.split_whitespace()
        .nth(19)
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .ok_or_else(refused)
}
fn role(bytes: &[u8], project: &Path, workspace: &Path, endpoint: &Path, authority: &Path) -> bool {
    let Some(bytes) = bytes.strip_suffix(&[0]) else {
        return false;
    };
    let args = bytes.split(|b| *b == 0).collect::<Vec<_>>();
    args.len() == 14
        && args[1] == b"--runtime-startup-v1"
        && matches!(args[2], b"direct" | b"xdg-home" | b"xdg-data")
        && Path::new(std::ffi::OsStr::from_bytes(args[3])).is_absolute()
        && args[4] == project.as_os_str().as_bytes()
        && args[5] == workspace.as_os_str().as_bytes()
        && args[6] == endpoint.as_os_str().as_bytes()
        && args[7] == authority.as_os_str().as_bytes()
        && args[8] == b"--endpoint"
        && args[9] == endpoint.as_os_str().as_bytes()
        && args[10] == b"--workspace"
        && args[11] == workspace.as_os_str().as_bytes()
        && args[12] == b"--authority-secret-file"
        && args[13] == authority.as_os_str().as_bytes()
}
fn lock_claim(bytes: &[u8], pid: u32, inode: u64) -> bool {
    std::str::from_utf8(bytes).ok().is_some_and(|text| {
        text.lines().any(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            fields.len() == 9
                && fields[0] == "lock:"
                && fields[2..5] == ["FLOCK", "ADVISORY", "WRITE"]
                && fields[5].parse::<u32>().ok() == Some(pid)
                && fields[6]
                    .rsplit(':')
                    .next()
                    .and_then(|v| v.parse::<u64>().ok())
                    == Some(inode)
                && fields[7..] == ["0", "EOF"]
        })
    })
}
fn uid_claim(bytes: &[u8], expected: u32) -> bool {
    std::str::from_utf8(bytes).ok().is_some_and(|s| {
        s.lines()
            .find_map(|line| line.strip_prefix("Uid:"))
            .is_some_and(|line| {
                let ids = line.split_whitespace().collect::<Vec<_>>();
                ids.len() == 4 && ids.iter().all(|v| v.parse::<u32>().ok() == Some(expected))
            })
    })
}
fn catches_term(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).ok().is_some_and(|text| {
        text.lines()
            .find_map(|line| line.strip_prefix("SigCgt:"))
            .and_then(|value| u64::from_str_radix(value.trim(), 16).ok())
            .is_some_and(|mask| mask & (1 << 14) != 0)
    })
}
fn intent_seen(bytes: &[u8], pid: u32, birth: u64, boot: &str) -> io::Result<bool> {
    let text = std::str::from_utf8(bytes).map_err(|_| refused())?;
    if bytes.len() > 16384 || !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return Err(refused());
    }
    let mut found = false;
    for (i, line) in text.lines().enumerate() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if i >= 128 || fields.len() != 4 || fields[0] != "nudox.owner-term.v1" {
            return Err(refused());
        }
        let recorded_pid: u32 = fields[2].parse().map_err(|_| refused())?;
        let recorded_birth: u64 = fields[3].parse().map_err(|_| refused())?;
        if recorded_pid <= 1 || recorded_birth == 0 {
            return Err(refused());
        }
        found |= fields[1] == boot && (recorded_pid, recorded_birth) == (pid, birth);
    }
    Ok(found)
}
fn next_intent(bytes: &[u8], pid: u32, birth: u64, boot: &str) -> io::Result<Vec<u8>> {
    let next = format!("nudox.owner-term.v1 {boot} {pid} {birth}\n").into_bytes();
    if bytes.len().saturating_add(next.len()) > 16384
        || bytes.iter().filter(|&&byte| byte == b'\n').count() >= 128
    {
        return Err(refused());
    }
    Ok(next)
}
fn initializer_lease(parent: &Path) -> io::Result<File> {
    let directory = backend_platform::DirectoryCapability::open(parent)?;
    directory.validate_private()?;
    let file = directory.open_private_file_read_write("authority-initialize.lock", true)?;
    directory.sync_all()?;
    if backend_platform::file_identity::FileIdentity::of_file(&file)?
        != backend_platform::file_identity::FileIdentity::of_path_nofollow(
            &parent.join("authority-initialize.lock"),
        )?
    {
        return Err(refused());
    }
    Ok(file)
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ImageStat {
    dev: u64,
    ino: u64,
    len: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    nlink: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}
impl ImageStat {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mode: meta.mode(),
            uid: meta.uid(),
            gid: meta.gid(),
            nlink: meta.nlink(),
            mtime: meta.mtime(),
            mtime_nsec: meta.mtime_nsec(),
            ctime: meta.ctime(),
            ctime_nsec: meta.ctime_nsec(),
        }
    }
}
fn locald_image_name(path: &Path, nlink: u64) -> bool {
    let path = if nlink == 0 {
        let Some(bytes) = path.as_os_str().as_bytes().strip_suffix(b" (deleted)") else {
            return false;
        };
        Path::new(std::ffi::OsStr::from_bytes(bytes))
    } else {
        path
    };
    path.is_absolute()
        && path
            .file_name()
            .is_some_and(|name| name == "backend-locald")
}
fn executable_metadata(meta: &std::fs::Metadata, uid: u32) -> bool {
    meta.is_file()
        && [0, uid].contains(&meta.uid())
        && meta.mode() & 0o022 == 0
        && meta.mode() & 0o111 != 0
        && meta.mode() & 0o6000 == 0
}
fn image_mode_problem(path: &Path, meta: &std::fs::Metadata, uid: u32) -> Option<String> {
    if executable_metadata(meta, uid) {
        return None;
    }
    let mode = meta.mode() & 0o7777;
    let reason = if mode & 0o002 != 0 {
        "is world-writable"
    } else if mode & 0o020 != 0 {
        "is group-writable"
    } else if mode & 0o6000 != 0 {
        "has setuid or setgid bits"
    } else if mode & 0o111 == 0 {
        "has no executable permission"
    } else if ![0, uid].contains(&meta.uid()) {
        "is owned by another user"
    } else if !meta.is_file() {
        "is not a regular file"
    } else {
        "has insecure permissions"
    };
    Some(format!(
        "owner image {} {reason} (uid {}, gid {}, mode {mode:04o})",
        path.display(),
        meta.uid(),
        meta.gid(),
    ))
}

/// Held, named default ELF observation for one bounded owner-image handoff.
/// This is not authority to signal a peer; the complete automatic-owner proof is separate.
#[derive(Debug)]
pub struct DefaultLocaldImage {
    selected_path: PathBuf,
    path: PathBuf,
    file: File,
    identity: ImageStat,
}
impl DefaultLocaldImage {
    /// Rechecks the held and named complete image identities without image-sized reads.
    #[must_use]
    pub fn is_current(&self) -> bool {
        std::fs::canonicalize(&self.selected_path).ok().as_ref() == Some(&self.path)
            && self
                .file
                .metadata()
                .ok()
                .zip(std::fs::symlink_metadata(&self.path).ok())
                .is_some_and(|(held, named)| {
                    named.is_file()
                        && ImageStat::of(&held) == self.identity
                        && ImageStat::of(&named) == self.identity
                })
    }

    /// Compares the authenticated kernel peer's actual ELF with this held default image.
    /// Only bounded metadata and the ELF magic are read; equality grants no retirement right.
    ///
    /// # Errors
    /// Refuses changed candidates, peer credentials, image names, modes or racing observations.
    pub fn matches_peer(
        &self,
        peer: &AuthenticatedLocalPeer,
        stream: &LocalStream,
    ) -> io::Result<bool> {
        if !self.is_current()
            || AuthenticatedLocalPeer::authenticate(stream, &peer.endpoint)
                .as_ref()
                .ok()
                != Some(peer)
        {
            return Err(refused());
        }
        let credentials = rustix::net::sockopt::socket_peercred(stream.as_fd())?;
        if credentials.uid.as_raw() != peer.effective_uid {
            return Err(refused());
        }
        let proc = PathBuf::from(format!("/proc/{}", credentials.pid.as_raw_nonzero()));
        let link = std::fs::read_link(proc.join("exe"))?;
        let mut file = File::open(proc.join("exe"))?;
        let meta = file.metadata()?;
        let before = ImageStat::of(&meta);
        let mut magic = [0; 4];
        file.read_exact(&mut magic)?;
        if !executable_metadata(&meta, peer.effective_uid)
            || !locald_image_name(&link, meta.nlink())
            || magic != *b"\x7fELF"
            || before != ImageStat::of(&file.metadata()?)
            || link != std::fs::read_link(proc.join("exe"))?
            || credentials.pid != rustix::net::sockopt::socket_peercred(stream.as_fd())?.pid
            || credentials.uid != rustix::net::sockopt::socket_peercred(stream.as_fd())?.uid
            || !self.is_current()
        {
            return Err(refused());
        }
        Ok(before == self.identity)
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Identity {
    birth: u64,
    boot: String,
    executable: PathBuf,
    executable_stat: ImageStat,
    arguments: Vec<u8>,
    lock: (u64, u64),
    lock_fd: std::ffi::OsString,
}
impl AuthenticatedLocalPeer {
    /// Whether the kernel peer is running an executable named backend-locald.
    /// This is only a probe selector, never authority to send a signal.
    pub fn is_locald_process(&self, stream: &LocalStream) -> bool {
        rustix::net::sockopt::socket_peercred(stream.as_fd())
            .ok()
            .is_some_and(|c| {
                c.uid.as_raw() == self.effective_uid
                    && std::fs::read_link(format!("/proc/{}/exe", c.pid.as_raw_nonzero()))
                        .ok()
                        .zip(
                            std::fs::metadata(format!("/proc/{}/exe", c.pid.as_raw_nonzero())).ok(),
                        )
                        .is_some_and(|(path, meta)| locald_image_name(&path, meta.nlink()))
            })
    }
    /// Holds the explicitly selected default sibling's stable regular ELF identity.
    /// No PATH search, process launch or full-image hash occurs.
    ///
    /// # Errors
    /// Refuses nonabsolute, writable, setid, missing, changed or non-ELF candidates.
    pub fn observe_default_locald_image(&self, path: &Path) -> io::Result<DefaultLocaldImage> {
        if !path.is_absolute() {
            return Err(refused());
        }
        let selected_path = path.to_path_buf();
        let path = std::fs::canonicalize(path)?;
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
            .open(&path)?;
        let meta = file.metadata()?;
        let mut magic = [0; 4];
        file.read_exact(&mut magic)?;
        if let Some(problem) = image_mode_problem(&path, &meta, self.effective_uid) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, problem));
        }
        if !executable_metadata(&meta, self.effective_uid)
            || meta.nlink() == 0
            || !locald_image_name(&path, meta.nlink())
            || magic != *b"\x7fELF"
        {
            return Err(refused());
        }
        let observation = DefaultLocaldImage {
            selected_path,
            path,
            file,
            identity: ImageStat::of(&meta),
        };
        if !observation.is_current() {
            return Err(refused());
        }
        Ok(observation)
    }

    fn owner_identity(
        &self,
        stream: &LocalStream,
        project: &Path,
        workspace: &Path,
        authority: &Path,
        deadline: Instant,
        keep_waiting: &mut impl FnMut() -> bool,
    ) -> io::Result<(rustix::process::Pid, Identity)> {
        if Instant::now() >= deadline || !keep_waiting() {
            return Err(refused());
        }
        if Self::authenticate(stream, &self.endpoint).as_ref().ok() != Some(self) {
            return Err(refused());
        }
        let credentials = rustix::net::sockopt::socket_peercred(stream.as_fd())?;
        let pid = credentials.pid;
        if credentials.uid.as_raw() != self.effective_uid
            || pid == rustix::process::getpid()
            || pid.as_raw_nonzero().get() <= 1
        {
            return Err(refused());
        }
        let proc = PathBuf::from(format!("/proc/{}", pid.as_raw_nonzero()));
        let status = bounded(&proc.join("status"), 65536)?;
        if !uid_claim(&status, self.effective_uid) || !catches_term(&status) {
            return Err(refused());
        }
        let boot = String::from_utf8(bounded(Path::new("/proc/sys/kernel/random/boot_id"), 128)?)
            .map_err(|_| refused())?
            .trim()
            .to_owned();
        if boot.len() != 36 || !boot.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return Err(refused());
        }
        let birth = stat_birth(
            &bounded(&proc.join("stat"), 16384)?,
            pid.as_raw_nonzero().get().cast_unsigned(),
        )?;
        let executable = std::fs::read_link(proc.join("exe"))?;
        let mut image = File::open(proc.join("exe"))?;
        let meta = image.metadata()?;
        let mut magic = [0; 4];
        image.read_exact(&mut magic)?;
        if !locald_image_name(&executable, meta.nlink())
            || !meta.is_file()
            || ![0, self.effective_uid].contains(&meta.uid())
            || meta.mode() & 0o022 != 0
            || meta.mode() & 0o6111 == 0
            || meta.mode() & 0o6000 != 0
            || magic != *b"\x7fELF"
            || std::fs::read_link(proc.join("cwd"))? != project
        {
            return Err(refused());
        }
        let arguments = bounded(&proc.join("cmdline"), 16384)?;
        if !role(&arguments, project, workspace, &self.endpoint, authority) {
            return Err(refused());
        }
        let lock_path = workspace.join("OWNER.lock");
        let lock_file = OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
            .open(&lock_path)?;
        let lock = lock_file.metadata()?;
        if !lock.is_file()
            || lock.uid() != self.effective_uid
            || lock.nlink() != 1
            || lock.mode() & 0o777 != 0o600
        {
            return Err(refused());
        }
        let mut lock_fd = None;
        for (i, entry) in std::fs::read_dir(proc.join("fdinfo"))?.enumerate() {
            if i >= 4096 || Instant::now() >= deadline || !keep_waiting() {
                return Err(refused());
            }
            let entry = entry?;
            let name = entry.file_name();
            if name.to_str().and_then(|v| v.parse::<u32>().ok()).is_none() {
                return Err(refused());
            }
            let fd_meta = std::fs::metadata(proc.join("fd").join(&name))?;
            if (fd_meta.dev(), fd_meta.ino()) == (lock.dev(), lock.ino())
                && lock_claim(
                    &bounded(&entry.path(), 16384)?,
                    pid.as_raw_nonzero().get().cast_unsigned(),
                    lock.ino(),
                )
            {
                if lock_fd.replace(name).is_some() {
                    return Err(refused());
                }
            }
        }
        let lock_fd = lock_fd.ok_or_else(refused)?;
        let lock_after = std::fs::symlink_metadata(lock_path)?;
        if (lock_after.dev(), lock_after.ino()) != (lock.dev(), lock.ino()) {
            return Err(refused());
        }
        Ok((
            pid,
            Identity {
                birth,
                boot,
                executable,
                executable_stat: ImageStat::of(&meta),
                arguments,
                lock: (lock.dev(), lock.ino()),
                lock_fd,
            },
        ))
    }
    /// Sends one SIGTERM to an exact pidfd only after repeated socket, UID,
    /// birth, executable, automatic-role and kernel workspace-lock admission.
    /// Waits for that handle to exit; never escalates, unlinks, or starts an owner.
    pub fn retire_automatic_owner(
        &self,
        stream: &LocalStream,
        project: &Path,
        workspace: &Path,
        authority: &Path,
        deadline: Instant,
        keep_waiting: impl FnMut() -> bool,
    ) -> io::Result<()> {
        self.retire_owner(
            stream,
            (project, workspace, authority),
            None,
            deadline,
            keep_waiting,
        )
    }

    /// Retires only an automatic peer still different from the held intended image.
    /// The extra witness is checked immediately before intent and TERM; a racing
    /// process which already execed the intended image must not be retired.
    ///
    /// # Errors
    /// Preserves every original custody refusal and rejects changed/matching images.
    pub fn retire_automatic_owner_for_image(
        &self,
        stream: &LocalStream,
        paths: (&Path, &Path, &Path),
        image: &DefaultLocaldImage,
        deadline: Instant,
        keep_waiting: impl FnMut() -> bool,
    ) -> io::Result<()> {
        self.retire_owner(stream, paths, Some(image), deadline, keep_waiting)
    }

    fn retire_owner(
        &self,
        stream: &LocalStream,
        paths: (&Path, &Path, &Path),
        image: Option<&DefaultLocaldImage>,
        deadline: Instant,
        mut keep_waiting: impl FnMut() -> bool,
    ) -> io::Result<()> {
        let (project, workspace, authority) = paths;
        let image_still_differs = || match image {
            None => Ok(()),
            Some(image) if image.matches_peer(self, stream)? => Err(refused()),
            Some(_) => Ok(()),
        };
        let (pid, before) = self.owner_identity(
            stream,
            project,
            workspace,
            authority,
            deadline,
            &mut keep_waiting,
        )?;
        image_still_differs()?;
        let handle = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())?;
        // This existing private initializer lease also serializes handoff
        // callers. Its bounded intent is durable BEFORE TERM, so cancellation,
        // a crash or another caller can never send a second signal to this birth.
        // Unknown/nonempty legacy content is never replaced or truncated.
        let parent = authority.parent().ok_or_else(refused)?;
        let mut initializer = initializer_lease(parent)?;
        let owns = match initializer.try_lock() {
            Ok(()) => true,
            Err(std::fs::TryLockError::WouldBlock) => false,
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        };
        if owns {
            let (again, after) = self.owner_identity(
                stream,
                project,
                workspace,
                authority,
                deadline,
                &mut keep_waiting,
            )?;
            if pid != again || before != after || !keep_waiting() || Instant::now() >= deadline {
                return Err(refused());
            }
            let mut bytes = Vec::new();
            (&mut initializer).take(16385).read_to_end(&mut bytes)?;
            if !intent_seen(
                &bytes,
                pid.as_raw_nonzero().get().cast_unsigned(),
                before.birth,
                &before.boot,
            )? {
                image_still_differs()?;
                let intent = next_intent(
                    &bytes,
                    pid.as_raw_nonzero().get().cast_unsigned(),
                    before.birth,
                    &before.boot,
                )?;
                initializer.seek(std::io::SeekFrom::End(0))?;
                initializer.write_all(&intent)?;
                initializer.flush()?;
                initializer.sync_all()?;
                if backend_platform::file_identity::FileIdentity::of_file(&initializer)?
                    != backend_platform::file_identity::FileIdentity::of_path_nofollow(
                        &parent.join("authority-initialize.lock"),
                    )?
                {
                    return Err(refused());
                }
                let (again, final_identity) = self.owner_identity(
                    stream,
                    project,
                    workspace,
                    authority,
                    deadline,
                    &mut keep_waiting,
                )?;
                if pid != again
                    || before != final_identity
                    || !keep_waiting()
                    || Instant::now() >= deadline
                {
                    return Err(refused());
                }
                image_still_differs()?;
                rustix::process::pidfd_send_signal(&handle, rustix::process::Signal::TERM)?;
            }
        }
        loop {
            let mut fds = [rustix::event::PollFd::new(
                &handle,
                rustix::event::PollFlags::IN,
            )];
            rustix::event::poll(
                &mut fds,
                Some(&rustix::event::Timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                }),
            )?;
            if fds[0].revents().contains(rustix::event::PollFlags::IN) {
                return Ok(());
            }
            if !keep_waiting() || Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "previous NuDox owner is still shutting down; retry the original command, which was not submitted",
                ));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn group_write_refusal_names_selected_image_and_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        let path = std::env::temp_dir().join(format!(
            "nudox-owner-image-mode-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("test clock is after the Unix epoch")
                .as_nanos()
        ));
        std::fs::write(&path, b"fixture").expect("write image fixture");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o775))
            .expect("set group-writable image mode");
        let meta = std::fs::metadata(&path).expect("stat image fixture");
        let detail = image_mode_problem(&path, &meta, meta.uid()).expect("775 is refused");
        assert!(detail.contains(&path.display().to_string()));
        assert!(detail.contains("group-writable"));
        assert!(detail.contains(&format!("uid {}, gid {}", meta.uid(), meta.gid())));
        assert!(detail.contains("mode 0775"));
        std::fs::remove_file(path).expect("remove image fixture");
    }

    #[test]
    fn deleted_locald_name_requires_kernel_zero_links_and_exact_original_basename() {
        assert!(locald_image_name(Path::new("/install/backend-locald"), 1));
        assert!(locald_image_name(
            Path::new("/old/backend-locald (deleted)"),
            0
        ));
        assert!(!locald_image_name(
            Path::new("/old/backend-locald (deleted)"),
            1
        ));
        assert!(!locald_image_name(Path::new("/old/foreign (deleted)"), 0));
        assert!(!locald_image_name(Path::new("/old/backend-locald"), 0));
        assert!(!locald_image_name(Path::new("backend-locald (deleted)"), 0));
    }

    struct QualifiedOwner {
        root: PathBuf,
        workspace: PathBuf,
        cli: PathBuf,
        qualified_locald: PathBuf,
        child: std::process::Child,
    }
    impl QualifiedOwner {
        fn start(copy_owner: bool) -> Self {
            use std::os::unix::fs::PermissionsExt as _;
            let cli = std::fs::canonicalize(
                std::env::var_os("NUDOX_TEST_QUALIFIED_CLI")
                    .expect("Root-qualified same-source CLI required"),
            )
            .expect("qualified CLI");
            let qualified_locald = std::fs::canonicalize(
                std::env::var_os("NUDOX_TEST_QUALIFIED_LOCALD")
                    .expect("Root-qualified same-source locald required"),
            )
            .expect("qualified locald");
            assert_eq!(
                cli.file_name().expect("qualified CLI has a basename"),
                "backend-cli"
            );
            assert_eq!(
                qualified_locald
                    .file_name()
                    .expect("qualified locald has a basename"),
                "backend-locald"
            );
            assert_eq!(
                cli.parent(),
                qualified_locald.parent(),
                "actual matched immutable cohort"
            );
            let root = std::env::temp_dir().join(format!(
                "owner-img-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("test clock is after the Unix epoch")
                    .as_nanos()
            ));
            std::fs::create_dir(&root).expect("create isolated owner fixture root");
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .expect("make owner fixture root private");
            let root = root.canonicalize().expect("resolve owner fixture root");
            std::fs::create_dir(root.join("p")).expect("create fixture project");
            std::fs::create_dir(root.join("home")).expect("create fixture home");
            // Exercise the production direct-startup contract: an automatic
            // workspace is the per-project default beneath this private root.
            let project = root.join("p").canonicalize().expect("canonical fixture project");
            let mut key = blake3::Hasher::new();
            key.update(b"backend.runtime.project-state.v1\0");
            key.update(project.as_os_str().as_encoded_bytes());
            let workspace = root
                .join("Nudox")
                .join("projects")
                .join(key.finalize().to_hex().as_str());
            let startup_stdout = std::fs::File::create(root.join("startup.stdout.raw"))
                .expect("capture startup stdout");
            let startup_stderr = std::fs::File::create(root.join("startup.stderr.raw"))
                .expect("capture startup stderr");
            let executable = if copy_owner {
                std::fs::create_dir(root.join("old")).expect("create old-image fixture directory");
                let copy = root.join("old/backend-locald");
                std::fs::copy(&qualified_locald, &copy)
                    .expect("copy qualified locald for old-image fixture");
                std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o500))
                    .expect("make old-image fixture executable");
                copy
            } else {
                qualified_locald.clone()
            };
            let child = std::process::Command::new(executable)
                .arg("--runtime-startup-v1")
                .arg("direct")
                .arg(&root)
                .arg(root.join("p"))
                .arg(&workspace)
                .arg(root.join("s"))
                .arg(workspace.join("authority.secret"))
                .arg("--endpoint")
                .arg(root.join("s"))
                .arg("--workspace")
                .arg(&workspace)
                .arg("--authority-secret-file")
                .arg(workspace.join("authority.secret"))
                .current_dir(root.join("p"))
                .env("HOME", root.join("home"))
                .stdin(std::process::Stdio::null())
                .stdout(startup_stdout)
                .stderr(startup_stderr)
                .spawn()
                .expect("actual product automatic owner");
            let mut fixture = Self {
                root,
                workspace,
                cli,
                qualified_locald,
                child,
            };
            let until = Instant::now() + Duration::from_secs(15);
            loop {
                if fixture.connection().is_ok() {
                    break;
                }
                if let Some(status) = fixture
                    .child
                    .try_wait()
                    .expect("poll actual owner during startup")
                {
                    panic!(
                        "actual owner startup failed ({status}); {}",
                        fixture.startup_diagnostic()
                    );
                }
                assert!(Instant::now() < until, "bounded actual owner startup");
                std::thread::sleep(Duration::from_millis(10));
            }
            fixture
        }
        fn startup_diagnostic(&self) -> String {
            use std::io::Read as _;
            let mut diagnostic = String::new();
            for name in ["startup.stdout.raw", "startup.stderr.raw"] {
                let mut bytes = Vec::new();
                if let Ok(file) = std::fs::File::open(self.root.join(name)) {
                    let _ = file.take(16 * 1024).read_to_end(&mut bytes);
                }
                use std::fmt::Write as _;
                let _ = write!(diagnostic, "{name}: {}; ", String::from_utf8_lossy(&bytes));
            }
            diagnostic
        }
        fn connection(
            &self,
        ) -> io::Result<(std::os::unix::net::UnixStream, AuthenticatedLocalPeer)> {
            let stream = std::os::unix::net::UnixStream::connect(self.root.join("s"))?;
            let peer = AuthenticatedLocalPeer::authenticate(&stream, &self.root.join("s"))
                .map_err(|_| refused())?;
            Ok((stream, peer))
        }
        fn pid(&self) -> u32 {
            let (stream, _) = self.connection().expect("connect to actual owner");
            rustix::net::sockopt::socket_peercred(stream.as_fd())
                .expect("read actual owner peer credentials")
                .pid
                .as_raw_nonzero()
                .get()
                .cast_unsigned()
        }
        fn command(&self, pinned: bool, passive: bool) -> std::process::Output {
            let mut command = std::process::Command::new(&self.cli);
            command
                .arg("--project")
                .arg(self.root.join("p"))
                .arg("--workspace")
                .arg(&self.workspace)
                .arg("--endpoint")
                .arg(self.root.join("s"))
                .args(["--format", "json"]);
            if passive {
                command.arg("--passive");
            }
            command
                .arg("health")
                .current_dir(self.root.join("p"))
                .env("HOME", self.root.join("home"))
                .env_remove("BACKEND_LOCALD_BIN")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            if pinned {
                command.env("BACKEND_LOCALD_BIN", &self.qualified_locald);
            }
            let mut child = command.spawn().expect("spawn actual CLI health command");
            let until = Instant::now() + Duration::from_secs(20);
            while child
                .try_wait()
                .expect("poll actual CLI health command")
                .is_none()
            {
                if Instant::now() >= until {
                    child.kill().expect("stop timed-out actual CLI command");
                    let _ = child.wait();
                    panic!("bounded actual CLI command");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let output = child.wait_with_output().expect("collect actual CLI output");
            assert!(
                output.stdout.len() + output.stderr.len() <= 65536,
                "bounded real health output"
            );
            output
        }
        fn retire(&self, deadline: Instant, keep: impl FnMut() -> bool) -> io::Result<()> {
            let (stream, peer) = self.connection()?;
            peer.retire_automatic_owner(
                &stream,
                &self.root.join("p"),
                &self.workspace,
                &self.workspace.join("authority.secret"),
                deadline,
                keep,
            )
        }
    }
    impl Drop for QualifiedOwner {
        fn drop(&mut self) {
            // This fixture owns the exact automatic workspace. Retire only a
            // peer proven by the same production pidfd/role/lock/handler path.
            let retired = self
                .retire(Instant::now() + Duration::from_secs(5), || true)
                .is_ok();
            if self.child.try_wait().ok().flatten().is_none() {
                let _ = self.child.kill();
            }
            let _ = self.child.wait();
            if retired {
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }
    }

    #[test]
    #[ignore = "requires Root-qualified matched CLI/locald in NUDOX_TEST_QUALIFIED_CLI and NUDOX_TEST_QUALIFIED_LOCALD; run explicitly after ordinary qualification"]
    fn qualified_same_dto_default_image_converges_once_and_preserves_pins() {
        let same = QualifiedOwner::start(false);
        let same_pid = same.pid();
        assert!(same.command(false, false).status.success());
        assert_eq!(same.pid(), same_pid);
        drop(same);
        let mut changed = QualifiedOwner::start(true);
        let old_pid = changed.pid();
        let (stream, peer) = changed
            .connection()
            .expect("connect to changed-image owner");
        let intended = peer
            .observe_default_locald_image(&changed.qualified_locald)
            .expect("inspect intended replacement image");
        assert!(
            !intended
                .matches_peer(&peer, &stream)
                .expect("compare intended image with authenticated peer")
        );
        assert!(changed.command(true, false).status.success());
        assert_eq!(changed.pid(), old_pid, "explicit helper pin never retires");
        let passive = changed.command(false, true);
        assert!(!passive.status.success());
        assert_eq!(changed.pid(), old_pid);
        assert!(String::from_utf8_lossy(&passive.stdout).contains("owner build update"));
        assert!(
            changed
                .retire(Instant::now() - Duration::from_secs(1), || true)
                .is_err()
        );
        assert!(
            changed
                .retire(Instant::now() + Duration::from_secs(2), || false)
                .is_err()
        );
        assert_eq!(
            changed.pid(),
            old_pid,
            "expired/cancelled admission cannot signal"
        );
        let lock = changed.workspace.join("OWNER.lock");
        let saved = changed.root.join("old-owner-lock");
        std::fs::rename(&lock, &saved).expect("remove exact owner lock for refusal control");
        let lock_result = changed.retire(Instant::now() + Duration::from_secs(2), || true);
        std::fs::rename(&saved, &lock).expect("restore exact owner lock after refusal control");
        assert!(lock_result.is_err());
        assert_eq!(changed.pid(), old_pid, "missing exact lock cannot signal");
        assert!(
            changed.command(false, false).status.success(),
            "actual same-DTO image update"
        );
        assert_ne!(
            changed.pid(),
            old_pid,
            "default image changed before command admission"
        );
        assert!(
            changed
                .child
                .wait()
                .expect("wait for gracefully retired owner")
                .success(),
            "real old owner graceful terminal"
        );
        let intent = std::fs::read(changed.workspace.join("authority-initialize.lock"))
            .expect("read durable owner retirement intent");
        assert_eq!(
            intent
                .split(|&b| b == b'\n')
                .filter(|line| !line.is_empty())
                .count(),
            1,
            "one durable TERM intent"
        );
        let new_pid = changed.pid();
        assert!(changed.command(false, false).status.success());
        assert_eq!(changed.pid(), new_pid);
        assert_eq!(
            std::fs::read(changed.workspace.join("authority-initialize.lock"))
                .expect("read durable owner retirement intent"),
            intent
        );
    }

    #[test]
    #[ignore = "requires Root-qualified matched CLI/locald in NUDOX_TEST_QUALIFIED_CLI and NUDOX_TEST_QUALIFIED_LOCALD; run explicitly after ordinary qualification"]
    fn qualified_deleted_default_image_requires_zero_links_and_refuses_candidate_races() {
        let mut old = QualifiedOwner::start(true);
        let old_pid = old.pid();
        let (stream, peer) = old.connection().expect("connect to deleted-image owner");
        let candidate = old.root.join("backend-locald");
        std::fs::copy(&old.qualified_locald, &candidate)
            .expect("copy candidate default locald image");
        let observed = peer
            .observe_default_locald_image(&candidate)
            .expect("inspect candidate default image");
        assert!(observed.is_current());
        std::fs::rename(&candidate, old.root.join("replaced-locald"))
            .expect("replace candidate to exercise image race");
        std::fs::copy(&old.qualified_locald, &candidate)
            .expect("copy candidate default locald image");
        assert!(!observed.is_current());
        assert!(
            old.retire(Instant::now() + Duration::from_secs(2), || observed
                .is_current())
                .is_err()
        );
        assert_eq!(old.pid(), old_pid, "changed named candidate cannot signal");
        std::fs::remove_file(old.root.join("old/backend-locald"))
            .expect("unlink old executable to test deleted image identity");
        let proc = PathBuf::from(format!("/proc/{old_pid}/exe"));
        let image = File::open(&proc).expect("open live process executable link");
        assert_eq!(
            image
                .metadata()
                .expect("read process executable metadata")
                .nlink(),
            0,
            "actual unlink, not basename spoof"
        );
        assert!(
            std::fs::read_link(&proc)
                .expect("read deleted process executable link")
                .as_os_str()
                .as_bytes()
                .ends_with(b" (deleted)")
        );
        let intended = peer
            .observe_default_locald_image(&old.qualified_locald)
            .expect("inspect qualified default locald image");
        assert!(
            !intended
                .matches_peer(&peer, &stream)
                .expect("compare intended image with authenticated peer")
        );
        assert!(peer.is_locald_process(&stream));
        assert!(old.command(false, false).status.success());
        assert_ne!(old.pid(), old_pid);
        assert!(
            old.child
                .wait()
                .expect("wait for gracefully retired owner")
                .success()
        );
        assert_eq!(
            image
                .metadata()
                .expect("read process executable metadata")
                .nlink(),
            0
        );
    }

    #[test]
    fn authenticated_unrelated_live_child_is_not_signalled_or_unlinked() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        use std::os::unix::net::{UnixListener, UnixStream};
        const CHILD: &str = "NUDOX_OWNER_UPDATE_UNRELATED_CHILD";
        if let Some(root) = std::env::var_os(CHILD) {
            let root = PathBuf::from(root);
            let listener =
                UnixListener::bind(root.join("owner.sock")).expect("owned child endpoint");
            std::fs::set_permissions(
                root.join("owner.sock"),
                std::fs::Permissions::from_mode(0o600),
            )
            .expect("private socket");
            std::fs::write(root.join("ready"), b"ready").expect("readiness");
            let (mut stream, _) = listener.accept().expect("owned parent connects");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("finite fixture");
            let mut release = [0];
            stream.read_exact(&mut release).expect("parent release");
            assert_eq!(release, [1]);
            return;
        }
        struct OwnedChild(std::process::Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let root = std::env::temp_dir().join(format!(
            "nu-{:x}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir(&root).expect("exclusive fixture");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .expect("private fixture");
        let name = "local_peer::owner_upgrade::tests::authenticated_unrelated_live_child_is_not_signalled_or_unlinked";
        let mut child = OwnedChild(
            std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args(["--exact", name, "--nocapture"])
                .env(CHILD, &root)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("isolated unrelated peer"),
        );
        let until = Instant::now() + Duration::from_secs(5);
        while !root.join("ready").exists() {
            if let Some(status) = child.0.try_wait().expect("exact fixture child status") {
                let mut diagnostic = Vec::new();
                if let Some(stderr) = child.0.stderr.take() {
                    stderr
                        .take(65537)
                        .read_to_end(&mut diagnostic)
                        .expect("bounded fixture child diagnostic");
                }
                assert!(diagnostic.len() <= 65536, "bounded fixture child stderr");
                panic!(
                    "unrelated fixture child exited before readiness ({status}): {}",
                    String::from_utf8_lossy(&diagnostic)
                );
            }
            assert!(Instant::now() < until, "finite child startup");
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut stream =
            UnixStream::connect(root.join("owner.sock")).expect("authenticated endpoint");
        let peer = AuthenticatedLocalPeer::authenticate(&stream, &root.join("owner.sock"))
            .expect("same-user private socket");
        assert!(
            !catches_term(
                &bounded(
                    &PathBuf::from(format!("/proc/{}/status", child.0.id())),
                    65536
                )
                .expect("actual unrelated child status")
            ),
            "unrelated child has no TERM handler"
        );
        let refusal =
            peer.retire_automatic_owner(&stream, &root, &root, &root.join("secret"), until, || {
                true
            });
        let alive = child
            .0
            .try_wait()
            .expect("exact child observation")
            .is_none();
        let endpoint_preserved = root.join("owner.sock").exists();
        stream
            .write_all(&[1])
            .expect("release owned fixture without signal");
        while child.0.try_wait().expect("bounded wait").is_none() {
            assert!(Instant::now() < until, "finite child completion");
            std::thread::sleep(Duration::from_millis(10));
        }
        let status = child.0.wait().expect("kernel-wait exact child");
        drop(child);
        std::fs::remove_dir_all(root).expect("remove fixture only");
        assert!(refusal.is_err());
        assert!(alive);
        assert!(endpoint_preserved);
        assert!(status.success());
    }
    #[test]
    fn role_requires_the_exact_automatic_owner_paths_and_closed_grammar() {
        let args = b"/old/backend-locald\0--runtime-startup-v1\0xdg-home\0/home/u\0/project\0/workspace\0/socket\0/secret\0--endpoint\0/socket\0--workspace\0/workspace\0--authority-secret-file\0/secret\0";
        assert!(role(
            args,
            Path::new("/project"),
            Path::new("/workspace"),
            Path::new("/socket"),
            Path::new("/secret")
        ));
        for old in [b"/foreign".as_slice(), b"/workspace/other".as_slice()] {
            assert!(!role(
                args,
                Path::new("/project"),
                Path::new(std::ffi::OsStr::from_bytes(old)),
                Path::new("/socket"),
                Path::new("/secret")
            ));
        }
        assert!(!role(
            &[args.as_slice(), b"extra\0"].concat(),
            Path::new("/project"),
            Path::new("/workspace"),
            Path::new("/socket"),
            Path::new("/secret")
        ));
    }
    #[test]
    fn kernel_lock_claim_requires_write_flock_exact_pid_and_inode() {
        let claim = b"pos:\t0\nlock:\t1: FLOCK ADVISORY WRITE 123 08:02:456 0 EOF\n";
        assert!(lock_claim(claim, 123, 456));
        assert!(!lock_claim(claim, 124, 456));
        assert!(!lock_claim(claim, 123, 457));
        assert!(!lock_claim(
            b"lock: 1: POSIX ADVISORY WRITE 123 08:02:456 0 EOF",
            123,
            456
        ));
    }
    #[test]
    fn existing_valid_authority_without_initializer_gets_one_private_serializing_inode() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!(
            "nudox-owner-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir(&root).expect("exclusive fixture");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .expect("private parent");
        std::fs::write(root.join("authority.secret"), [7; 32])
            .expect("existing valid-size authority");
        std::fs::set_permissions(
            root.join("authority.secret"),
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("private authority");
        assert!(!root.join("authority-initialize.lock").exists());
        let mut first = initializer_lease(&root).expect("safe no-clobber initializer creation");
        first.try_lock().expect("first updater");
        let second = initializer_lease(&root).expect("same stable initializer");
        assert_eq!(
            first.metadata().expect("metadata").ino(),
            second.metadata().expect("metadata").ino()
        );
        assert!(matches!(
            second.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        first
            .write_all(b"nudox.owner-term.v1 00000000-0000-0000-0000-000000000001 123 987\n")
            .expect("intent");
        first.sync_all().expect("durable intent");
        drop(first);
        second.try_lock().expect("later updater");
        let bytes =
            bounded(&root.join("authority-initialize.lock"), 16384).expect("existing intent");
        assert!(
            intent_seen(&bytes, 123, 987, "00000000-0000-0000-0000-000000000001")
                .expect("same birth never signals twice")
        );
        assert_eq!(
            std::fs::read(root.join("authority.secret")).expect("unchanged authority"),
            [7; 32]
        );
        assert_eq!(
            second.metadata().expect("private metadata").mode() & 0o777,
            0o600
        );
        drop(second);
        std::fs::remove_dir_all(root).expect("remove own fixture only");
    }
    #[test]
    fn durable_term_intent_fences_exact_boot_and_birth_and_refuses_unknown_partial_content() {
        let boot = "00000000-0000-0000-0000-000000000001";
        let intent = b"nudox.owner-term.v1 00000000-0000-0000-0000-000000000001 123 987\n";
        assert!(intent_seen(intent, 123, 987, boot).expect("existing exact intent"));
        assert!(!intent_seen(intent, 123, 988, boot).expect("different birth"));
        assert!(
            !intent_seen(intent, 123, 987, "00000000-0000-0000-0000-000000000002")
                .expect("different boot")
        );
        assert!(!intent_seen(b"", 123, 987, boot).expect("empty initializer"));
        assert!(intent_seen(b"unrelated credentials or config\n", 123, 987, boot).is_err());
        assert!(intent_seen(b"nudox.owner-term.v1 123 987", 123, 987, boot).is_err());
        assert_eq!(
            next_intent(b"", 123, 987, boot).expect("first intent"),
            intent
        );
        let full = intent.repeat(128);
        assert!(intent_seen(&full, 123, 987, boot).expect("existing bounded intent"));
        assert!(next_intent(&full, 123, 988, boot).is_err());
        assert!(next_intent(&vec![b' '; 16384], 123, 988, boot).is_err());
    }
    #[test]
    fn graceful_term_requires_an_installed_handler() {
        assert!(catches_term(b"SigCgt:\t0000000000004000\n"));
        assert!(!catches_term(b"SigCgt:\t0000000000000000\n"));
        assert!(!catches_term(b"SigIgn:\t0000000000004000\n"));
    }
    #[test]
    fn live_uid_cannot_differ_from_the_socket_credentials() {
        assert!(uid_claim(
            b"Name: locald\nUid:\t1000 1000 1000 1000\n",
            1000
        ));
        assert!(!uid_claim(b"Uid: 0 1000 0 1000\n", 1000));
        assert!(!uid_claim(b"Uid: 1000 1000 1000\n", 1000));
    }
    #[test]
    fn birth_parsing_preserves_identity_with_parentheses_in_process_name() {
        let stat = b"123 (locald (old)) S 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 987 0";
        assert_eq!(stat_birth(stat, 123).expect("starttime"), 987);
        assert!(stat_birth(stat, 124).is_err());
        assert!(stat_birth(b"123 (old) S 0", 123).is_err());
    }
}
