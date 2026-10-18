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
#[derive(Debug, Eq, PartialEq)]
struct Identity {
    birth: u64,
    boot: String,
    executable: PathBuf,
    executable_stat: (u64, u64, u64, u32, u32, u64, i64, i64, i64, i64),
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
                        .is_some_and(|p| p.file_name().is_some_and(|name| name == "backend-locald"))
            })
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
        if !executable.is_absolute()
            || executable.file_name().is_none_or(|n| n != "backend-locald")
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
                executable_stat: (
                    meta.dev(),
                    meta.ino(),
                    meta.len(),
                    meta.mode(),
                    meta.uid(),
                    meta.nlink(),
                    meta.mtime(),
                    meta.mtime_nsec(),
                    meta.ctime(),
                    meta.ctime_nsec(),
                ),
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
        mut keep_waiting: impl FnMut() -> bool,
    ) -> io::Result<()> {
        let (pid, before) = self.owner_identity(
            stream,
            project,
            workspace,
            authority,
            deadline,
            &mut keep_waiting,
        )?;
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
            "nudox-upgrade-{}-{}",
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
                .spawn()
                .expect("isolated unrelated peer"),
        );
        let until = Instant::now() + Duration::from_secs(5);
        while !root.join("ready").exists() {
            assert!(Instant::now() < until, "finite child startup");
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut stream =
            UnixStream::connect(root.join("owner.sock")).expect("authenticated endpoint");
        let peer = AuthenticatedLocalPeer::authenticate(&stream, &root.join("owner.sock"))
            .expect("same-user private socket");
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
