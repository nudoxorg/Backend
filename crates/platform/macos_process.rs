//! Bounded Darwin process-session setup and process-group retirement.
//!
//! Darwin's `killpg` success only means that at least one eligible process was
//! signaled. This adapter therefore treats it as a signal attempt and confirms
//! the original PGID through `libproc` while the direct-child leader remains
//! waitable. The `libproc` interfaces and unique-identity flavor are private
//! Apple APIs; if their result is unavailable or does not match the expected
//! ABI, retirement fails closed. Identity consistency uses only flavor 18's
//! nonzero `p_uniqueid`; the flavor does not expose a PID generation/version
//! field. The guarantee covers live members of the
//! original PGID. Descendants that deliberately change process groups or
//! sessions escape that boundary.
//!
//! Apple's private `libproc` header declares `proc_listpgrppids` available from
//! macOS 10.7. The unique-identity flavor used below has no stable public
//! availability contract. If a target OS lacks the operation or returns a
//! result this adapter cannot validate, callers receive an error and must not
//! reuse the process session.
#![allow(
    unsafe_code,
    reason = "narrow Darwin process-info FFI and pre-exec setsid boundary"
)]

use rustix::{
    io::Errno,
    process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid},
};
use std::{
    io,
    mem::MaybeUninit,
    os::unix::process::CommandExt,
    process::Command,
    thread,
    time::{Duration, Instant},
};

const PROC_PIDT_BSDINFOWITHUNIQID: libc::c_int = 18;
const MAX_GROUP_MEMBERS: usize = 4096;
const RETIREMENT_TIMEOUT: Duration = Duration::from_millis(500);
const SZOMB: u32 = 5;

/// Failure to prove retirement of the original Darwin process group.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GroupRetirementError {
    /// The process group could not be signaled or its leader could not be observed.
    SignalOrLeader,
    /// Darwin's process-list or process-identity result was unavailable or malformed.
    ProcessInfo,
    /// The group remained live or changed during bounded verification.
    Deadline,
}

#[repr(C)]
struct ProcUniqIdentifierInfo {
    uuid: [u8; 16],
    unique_id: u64,
    parent_unique_id: u64,
    reserved_2: u64,
    reserved_3: u64,
    reserved_4: u64,
}

#[repr(C)]
struct ProcBsdInfoWithUniqId {
    bsd: libc::proc_bsdinfo,
    unique: ProcUniqIdentifierInfo,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ProcessIdentity {
    pid: i32,
    process_group: u32,
    unique_id: u64,
    status: u32,
}

enum Census {
    Stable(Vec<ProcessIdentity>),
    Changed,
}

/// Configures a newly spawned command as a fresh session leader.
///
/// The closure performs only `setsid` between fork and exec. A child created
/// by `Command` is not a process-group leader before this hook, so Darwin can
/// create the session without disturbing the parent application's session.
pub fn configure_process_session(command: &mut Command) {
    // SAFETY: The pre-exec closure calls only the async-signal-safe setsid
    // syscall through rustix and translates its errno into the required result.
    unsafe {
        command.pre_exec(|| {
            rustix::process::setsid()
                .map(|_| ())
                .map_err(io::Error::from)
        });
    }
}

/// Sends SIGKILL and confirms there are no live members in the original PGID.
///
/// The leader is observed with `waitid(WNOWAIT)` before either process census,
/// pinning its PID/PGID until the caller reaps it. A census requires two
/// matching process-list snapshots, with each PID's PGID and unique process
/// identity read atomically by Darwin's private flavor 18. Buffer saturation,
/// missing process records, ABI mismatch, or a live member is never treated as
/// success. The receipt is a bounded observation of the original PGID, not a
/// linearizable proof of continuous emptiness: the process-list and per-PID
/// identity queries are separate calls. A descendant can escape by
/// changing process groups or sessions, or race membership changes between
/// observations; strict whole-tree containment requires an outer OS sandbox.
/// The whole operation is bounded by `RETIREMENT_TIMEOUT`.
pub fn retire_process_group(pid: u32) -> Result<(), GroupRetirementError> {
    let raw_pid = i32::try_from(pid).map_err(|_| GroupRetirementError::SignalOrLeader)?;
    let leader = Pid::from_raw(raw_pid).ok_or(GroupRetirementError::SignalOrLeader)?;
    let deadline = Instant::now() + RETIREMENT_TIMEOUT;

    loop {
        if Instant::now() >= deadline {
            return Err(GroupRetirementError::Deadline);
        }
        if let Err(error) = kill_process_group(leader, Signal::KILL) {
            #[cfg(test)]
            eprintln!(
                "killpg {leader:?} returned {error:?} (raw errno {error}); observed group={:?}",
                rustix::process::getpgid(Some(leader))
            );
            if error != Errno::SRCH && error != Errno::PERM {
                return Err(GroupRetirementError::SignalOrLeader);
            }
        }
        // Success, ESRCH, and EPERM are only signal-attempt results. The
        // following identity census decides whether retirement holds.

        if !wait_for_leader_exit(leader, deadline)? {
            return Err(GroupRetirementError::Deadline);
        }

        match stable_group_census(leader, deadline)? {
            Census::Stable(members) if confirms_retired(&members, raw_pid) => return Ok(()),
            Census::Stable(_) | Census::Changed => {
                if Instant::now() >= deadline {
                    return Err(GroupRetirementError::Deadline);
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

fn checked_process_count(
    returned_count: libc::c_int,
    capacity_count: usize,
) -> Result<usize, GroupRetirementError> {
    let returned_count =
        usize::try_from(returned_count).map_err(|_| GroupRetirementError::ProcessInfo)?;
    if returned_count == 0 || returned_count >= capacity_count {
        return Err(GroupRetirementError::ProcessInfo);
    }
    Ok(returned_count)
}

fn process_info_was_changed(
    returned_size: libc::c_int,
    expected_size: libc::c_int,
    error_number: Option<i32>,
) -> Result<bool, GroupRetirementError> {
    if returned_size == expected_size {
        Ok(false)
    } else if returned_size == 0 && error_number == Some(libc::ESRCH) {
        Ok(true)
    } else {
        Err(GroupRetirementError::ProcessInfo)
    }
}

fn wait_for_leader_exit(leader: Pid, deadline: Instant) -> Result<bool, GroupRetirementError> {
    loop {
        let status = waitid(
            WaitId::Pid(leader),
            WaitIdOptions::NOHANG | WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
        )
        .map_err(|_| GroupRetirementError::SignalOrLeader)?;
        if status.is_some_and(|status| status.exited() || status.killed() || status.dumped()) {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn stable_group_census(leader: Pid, deadline: Instant) -> Result<Census, GroupRetirementError> {
    Ok(compare_group_snapshots(
        group_snapshot(leader, deadline)?,
        group_snapshot(leader, deadline)?,
    ))
}

fn compare_group_snapshots(
    first: Option<Vec<ProcessIdentity>>,
    second: Option<Vec<ProcessIdentity>>,
) -> Census {
    let (Some(first), Some(second)) = (first, second) else {
        return Census::Changed;
    };
    if first == second {
        Census::Stable(second)
    } else {
        Census::Changed
    }
}

fn process_identity(
    pid: libc::pid_t,
    expected_pgid: u32,
) -> Result<Option<ProcessIdentity>, GroupRetirementError> {
    let mut info = MaybeUninit::<ProcBsdInfoWithUniqId>::zeroed();
    let expected_size = libc::c_int::try_from(size_of::<ProcBsdInfoWithUniqId>())
        .map_err(|_| GroupRetirementError::ProcessInfo)?;
    // SAFETY: `__error` returns this thread's errno cell on Darwin.
    unsafe { *libc::__error() = 0 };
    // SAFETY: `info` is zero-initialized, correctly aligned storage of the
    // exact requested ABI size. Flavor 18 fills BSD state and process
    // unique identity in one kernel query; the result is read only when the
    // returned byte count exactly matches this struct.
    let returned = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDT_BSDINFOWITHUNIQID,
            1,
            info.as_mut_ptr().cast(),
            expected_size,
        )
    };
    if process_info_was_changed(
        returned,
        expected_size,
        io::Error::last_os_error().raw_os_error(),
    )? {
        // The PID exited or was reaped after the group-list snapshot.
        // Retry the bounded census instead of treating a normal race as a
        // permanent platform failure.
        return Ok(None);
    }
    // SAFETY: the exact struct size was returned for the initialized buffer
    // above, and the kernel ABI defines this flavor as this layout.
    let info = unsafe { info.assume_init() };
    if info.bsd.pbi_pid != pid as u32 || info.bsd.pbi_pgid != expected_pgid {
        return Ok(None);
    }
    if info.unique.unique_id == 0 {
        return Err(GroupRetirementError::ProcessInfo);
    }
    Ok(Some(ProcessIdentity {
        pid,
        process_group: info.bsd.pbi_pgid,
        unique_id: info.unique.unique_id,
        status: info.bsd.pbi_status,
    }))
}

fn group_snapshot(
    leader: Pid,
    deadline: Instant,
) -> Result<Option<Vec<ProcessIdentity>>, GroupRetirementError> {
    let capacity_bytes = MAX_GROUP_MEMBERS
        .checked_mul(size_of::<libc::pid_t>())
        .and_then(|size| libc::c_int::try_from(size).ok())
        .ok_or(GroupRetirementError::ProcessInfo)?;
    let mut pids = vec![0 as libc::pid_t; MAX_GROUP_MEMBERS];
    let returned_count =
        // SAFETY: `pids` is initialized writable storage for exactly the byte
        // length passed to libproc; the PGID is pinned by the unreaped leader.
        unsafe {
            libc::proc_listpgrppids(
                leader.as_raw_pid(),
                pids.as_mut_ptr().cast(),
                capacity_bytes,
            )
        };
    // `proc_listpgrppids` converts the kernel's copied byte count to a PID
    // count. Equality is ambiguous between exact fit and truncation, so a full
    // buffer fails closed.
    let count = checked_process_count(returned_count, MAX_GROUP_MEMBERS)?;
    pids.truncate(count);
    if Instant::now() >= deadline {
        return Err(GroupRetirementError::Deadline);
    }

    let mut members = Vec::with_capacity(pids.len());
    for pid in pids {
        if Instant::now() >= deadline {
            return Err(GroupRetirementError::Deadline);
        }
        if pid <= 0 {
            return Err(GroupRetirementError::ProcessInfo);
        }
        let Some(identity) = process_identity(pid, leader.as_raw_pid() as u32)? else {
            return Ok(None);
        };
        if pid == leader.as_raw_pid() && identity.status != SZOMB {
            return Err(GroupRetirementError::ProcessInfo);
        }
        members.push(identity);
    }
    members.sort_unstable();
    if members.windows(2).any(|pair| pair[0].pid == pair[1].pid) {
        return Err(GroupRetirementError::ProcessInfo);
    }
    if !members
        .iter()
        .any(|member| member.pid == leader.as_raw_pid())
    {
        return Err(GroupRetirementError::ProcessInfo);
    }
    Ok(Some(members))
}

fn confirms_retired(members: &[ProcessIdentity], leader_pid: i32) -> bool {
    members
        .iter()
        .any(|member| member.pid == leader_pid && member.status == SZOMB)
        && members.iter().all(|member| member.status == SZOMB)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FileIdentity;
    use std::{
        fs::{self, OpenOptions},
        io::{Read, Seek, SeekFrom},
        os::unix::{fs::OpenOptionsExt as _, process::ExitStatusExt as _},
        path::PathBuf,
        process::{Child, Stdio},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    struct OwnedPidMarker {
        file: fs::File,
        identity: FileIdentity,
        path: PathBuf,
    }

    impl OwnedPidMarker {
        fn create() -> io::Result<Self> {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            for attempt in 0..16_u8 {
                let path = std::env::temp_dir().join(format!(
                    "backend-platform-darwin-pgrp-child-{}-{nonce}-{attempt}.pid",
                    std::process::id()
                ));
                let file = match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)
                {
                    Ok(file) => file,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error),
                };
                let identity = FileIdentity::of_file(&file)?;
                let marker = Self {
                    file,
                    identity,
                    path,
                };
                marker.verify_named()?;
                return Ok(marker);
            }
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not reserve a unique process-group PID marker",
            ))
        }

        fn verify_named(&self) -> io::Result<()> {
            if FileIdentity::of_path_nofollow(&self.path)? != self.identity {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "process-group PID marker name changed identity",
                ));
            }
            Ok(())
        }

        fn read_complete_pid(&mut self) -> io::Result<Option<i32>> {
            self.verify_named()?;
            self.file.seek(SeekFrom::Start(0))?;
            let mut contents = [0_u8; 33];
            let read = self
                .file
                .by_ref()
                .take(contents.len() as u64)
                .read(&mut contents)?;
            if read > 32 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "child PID marker exceeds its 32-byte limit",
                ));
            }
            let contents = std::str::from_utf8(&contents[..read]).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "child PID marker is not UTF-8")
            })?;
            if !contents.ends_with('\n') {
                return Ok(None);
            }
            let pid = contents.trim().parse().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid child PID marker")
            })?;
            Ok(Some(pid))
        }
    }

    impl Drop for OwnedPidMarker {
        fn drop(&mut self) {
            if FileIdentity::of_path_nofollow(&self.path).ok() == Some(self.identity) {
                let _ = fs::remove_file(&self.path);
            }
        }
    }

    struct OwnedSessionLeader {
        child: Child,
        leader_reaped: bool,
    }

    impl OwnedSessionLeader {
        fn spawn(command: &mut Command) -> io::Result<Self> {
            configure_process_session(command);
            Ok(Self {
                child: command.spawn()?,
                leader_reaped: false,
            })
        }

        fn id(&self) -> u32 {
            self.child.id()
        }

        fn wait_after_retirement(&mut self) -> io::Result<std::process::ExitStatus> {
            let status = self.child.wait()?;
            self.leader_reaped = true;
            Ok(status)
        }
    }

    impl Drop for OwnedSessionLeader {
        fn drop(&mut self) {
            if self.leader_reaped {
                return;
            }
            // This guard owns the direct child and never waits it before this
            // branch. The leader therefore remains waitable and its PID cannot
            // have been reused when the fallback group signal is sent.
            let group_signal_succeeded = Pid::from_raw(self.child.id() as i32)
                .is_some_and(|group| kill_process_group(group, Signal::KILL).is_ok());
            if !group_signal_succeeded {
                let _ = self.child.kill();
            }
            if self.child.wait().is_ok() {
                self.leader_reaped = true;
            }
        }
    }

    fn member(pid: i32, unique_id: u64, status: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            process_group: 100,
            unique_id,
            status,
        }
    }

    #[test]
    fn private_identity_flavor_layout_uses_unique_id_without_a_pid_version() {
        assert_eq!(std::mem::offset_of!(ProcUniqIdentifierInfo, unique_id), 16);
        assert_eq!(
            std::mem::offset_of!(ProcUniqIdentifierInfo, parent_unique_id),
            24
        );
        assert_eq!(std::mem::offset_of!(ProcUniqIdentifierInfo, reserved_2), 32);
        assert_eq!(std::mem::offset_of!(ProcUniqIdentifierInfo, reserved_3), 40);
        assert_eq!(std::mem::offset_of!(ProcUniqIdentifierInfo, reserved_4), 48);
        assert_eq!(std::mem::size_of::<ProcUniqIdentifierInfo>(), 56);
        assert_eq!(
            std::mem::offset_of!(ProcBsdInfoWithUniqId, unique),
            std::mem::size_of::<libc::proc_bsdinfo>()
        );
    }

    #[test]
    fn killpg_success_with_a_live_member_does_not_confirm_retirement() {
        // XNU reports success after signaling at least one eligible member.
        // An owned target can be killed while a foreign target remains live.
        let members = [
            member(100, 1, SZOMB),
            member(101, 2, SZOMB),
            member(102, 3, 3),
        ];
        assert!(!confirms_retired(&members, 100));
    }

    #[test]
    fn zombie_only_group_can_retire_without_a_successful_killpg_result() {
        let members = [member(100, 1, SZOMB), member(101, 2, SZOMB)];
        assert!(confirms_retired(&members, 100));
    }

    #[test]
    fn changed_membership_or_disappearing_pid_never_becomes_a_retirement_receipt() {
        let zombies = vec![member(100, 1, SZOMB), member(101, 2, SZOMB)];
        let changed = vec![member(100, 1, SZOMB)];
        assert!(matches!(
            compare_group_snapshots(Some(zombies.clone()), Some(changed)),
            Census::Changed
        ));
        assert!(matches!(
            compare_group_snapshots(Some(zombies), None),
            Census::Changed
        ));
    }

    #[test]
    fn process_list_result_is_a_pid_count_and_saturation_fails_closed() {
        assert_eq!(checked_process_count(2, MAX_GROUP_MEMBERS), Ok(2));
        assert_eq!(
            checked_process_count(MAX_GROUP_MEMBERS as libc::c_int, MAX_GROUP_MEMBERS),
            Err(GroupRetirementError::ProcessInfo)
        );
        assert_eq!(
            checked_process_count(-1, MAX_GROUP_MEMBERS),
            Err(GroupRetirementError::ProcessInfo)
        );
    }

    #[test]
    fn vanished_pid_is_a_changed_snapshot_but_other_info_errors_fail_closed() {
        assert_eq!(
            process_info_was_changed(0, 100, Some(libc::ESRCH)),
            Ok(true)
        );
        assert_eq!(process_info_was_changed(100, 100, None), Ok(false));
        assert_eq!(
            process_info_was_changed(0, 100, Some(libc::EACCES)),
            Err(GroupRetirementError::ProcessInfo)
        );
        assert_eq!(
            process_info_was_changed(99, 100, None),
            Err(GroupRetirementError::ProcessInfo)
        );
    }

    #[test]
    fn darwin_session_group_retirement_observes_real_child_lifecycle() {
        let mut pid_file = OwnedPidMarker::create().expect("reserve unique descendant marker");
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "/bin/sleep 30 & echo $! > \"$1\"; exec /bin/sleep 30",
                "backend-platform-darwin-pgrp-test",
            ])
            .arg(&pid_file.path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut leader = OwnedSessionLeader::spawn(&mut command).expect("spawn session leader");

        let handshake_deadline = Instant::now() + Duration::from_secs(2);
        let descendant = loop {
            match pid_file
                .read_complete_pid()
                .expect("read the exact owned descendant marker")
            {
                Some(raw_pid) => {
                    break Pid::from_raw(raw_pid)
                        .expect("descendant marker contained an invalid PID");
                }
                None => {}
            }
            assert!(
                Instant::now() < handshake_deadline,
                "child PID handshake timed out"
            );
            thread::sleep(Duration::from_millis(1));
        };
        let live_descendant = process_identity(descendant.as_raw_pid(), leader.id())
            .expect("query descendant identity")
            .expect("descendant belongs to the session group");
        assert_ne!(
            live_descendant.status, SZOMB,
            "descendant must be live before retirement"
        );
        let live_leader = process_identity(leader.id() as libc::pid_t, leader.id())
            .expect("query session leader identity")
            .expect("leader belongs to its process group");
        assert_ne!(
            live_leader.status, SZOMB,
            "session leader must remain live and waitable before retirement"
        );

        retire_process_group(leader.id()).expect("retire process group");
        let status = leader
            .wait_after_retirement()
            .expect("reap pinned session leader");
        assert_eq!(
            status.signal(),
            Some(libc::SIGKILL),
            "the non-returning session leader must have received the group kill"
        );

        let reap_deadline = Instant::now() + Duration::from_secs(2);
        while process_identity(descendant.as_raw_pid(), leader.id())
            .expect("query descendant after retirement")
            .is_some_and(|identity| identity.status != SZOMB)
        {
            assert!(
                Instant::now() < reap_deadline,
                "group descendant remained alive"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn darwin_zombie_only_group_is_retired_while_leader_remains_waitable() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_session(&mut command);
        let mut child = command.spawn().expect("spawn session leader");
        let leader = Pid::from_raw(child.id() as i32).expect("valid child PID");
        assert!(
            wait_for_leader_exit(leader, Instant::now() + Duration::from_secs(2))
                .expect("observe leader exit")
        );

        let identity = process_identity(child.id() as libc::pid_t, child.id())
            .expect("query waitable zombie leader")
            .expect("leader remains in its process group");
        assert_eq!(identity.status, SZOMB);

        retire_process_group(child.id()).expect("retire zombie-only process group");
        assert!(child.wait().expect("reap waitable leader").success());
    }
}
