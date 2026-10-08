//! Ephemeral Unix startup coalescing, independent of durable OWNER.lock.
//!
//! The lock inode is persistent. Only the small progress snapshot is replaced;
//! neither file claims durable ownership or semantic readiness.
use super::StartupPhase;
use backend_platform::{DirectoryCapability, FileIdentity};
use std::fs::{self, File};
use std::io::{self, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, FileExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

const GATE_ENV: &str = "BACKEND_LOCALD_STARTUP_GATE";
const GENERATION_ENV: &str = "BACKEND_LOCALD_STARTUP_GENERATION";
const MAGIC: &str = "nudox.local-startup.v1";
const MAX_RECORD: u64 = 256;
static NEXT: AtomicU64 = AtomicU64::new(0);

pub(crate) struct Admission {
    directory: DirectoryCapability,
    parent: PathBuf,
    name: String,
    file: File,
    endpoint: String,
    generation: Option<String>,
}

impl Admission {
    pub(crate) fn open(workspace: &Path, endpoint: &Path) -> io::Result<Self> {
        let parent = PathBuf::from(format!(
            "/tmp/nudox-local-startup-{}",
            rustix::process::geteuid().as_raw()
        ));
        match fs::DirBuilder::new().mode(0o700).create(&parent) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error),
        }
        let directory = DirectoryCapability::open(&parent)?;
        directory.validate_private()?;
        let name = format!("{}.lock", digest(workspace));
        let file = directory.open_private_file_read_write(&name, true)?;
        Ok(Self {
            directory,
            parent,
            name,
            file,
            endpoint: digest(endpoint),
            generation: None,
        })
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.parent
    }
    pub(crate) fn failure_name(&self) -> String {
        format!("{}.failure", self.name)
    }

    /// A free kernel lock supersedes any stale progress. No PID/age inference.
    pub(crate) fn try_lead(&mut self) -> io::Result<bool> {
        if self.generation.is_some() {
            return Ok(true);
        }
        match self.file.try_lock() {
            Ok(()) => (),
            Err(fs::TryLockError::WouldBlock) => {
                if let Ok((_, endpoint)) = header(&self.file)
                    && endpoint != self.endpoint
                {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "this workspace is starting at a different endpoint; use the same endpoint selection",
                    ));
                }
                return Ok(false);
            }
            Err(fs::TryLockError::Error(error)) => return Err(error),
        }
        self.verify_inode()?;
        let generation = format!(
            "{:x}-{:x}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let header = format!("{MAGIC}\n{generation}\n{}\n", self.endpoint);
        self.file.seek(SeekFrom::Start(0))?;
        self.file.set_len(0)?;
        self.file.write_all(header.as_bytes())?;
        self.generation = Some(generation);
        let _ = self.publish(StartupPhase::SelectingExecutable);
        Ok(true)
    }

    fn verify_inode(&self) -> io::Result<()> {
        self.directory.verify_path(&self.parent)?;
        let named = self
            .directory
            .open_private_file_read_write(&self.name, false)?;
        if FileIdentity::of_file(&named)? != FileIdentity::of_file(&self.file)? {
            return Err(io::Error::other("startup admission inode changed"));
        }
        Ok(())
    }

    pub(crate) fn configure(&self, command: &mut Command) -> io::Result<()> {
        self.verify_inode()?;
        command
            .env(GATE_ENV, self.parent.join(&self.name))
            .env(
                GENERATION_ENV,
                self.generation
                    .as_ref()
                    .ok_or_else(|| io::Error::other("startup admission is not held"))?,
            )
            .stdin(Stdio::from(self.file.try_clone()?));
        Ok(())
    }

    pub(crate) fn publish(&self, phase: StartupPhase) -> io::Result<()> {
        let generation = self
            .generation
            .as_deref()
            .ok_or_else(|| io::Error::other("startup admission is not held"))?;
        publish(
            &self.directory,
            &self.parent,
            &self.name,
            generation,
            &self.endpoint,
            phase,
        )
    }

    pub(crate) fn progress(&self) -> Option<(String, StartupPhase)> {
        self.verify_inode().ok()?;
        let before = header(&self.file).ok()?;
        if before.1 != self.endpoint {
            return None;
        }
        let bytes = bounded(
            &self
                .directory
                .open_private_file_read_write(&snapshot_name(&self.name), false)
                .ok()?,
        )
        .ok()?;
        let (generation, endpoint, phase) = parse(&bytes)?;
        let after = header(&self.file).ok()?;
        if before != after || generation != before.0 || endpoint != before.1 {
            return None;
        }
        Some((generation.to_owned(), phase))
    }
}

/// Holds the inherited startup admission until the detached owner exits.
/// It is not the durable store-owner lease and does not imply readiness.
pub(crate) struct StartupOwner {
    _gate: File,
    directory: DirectoryCapability,
    parent: PathBuf,
    name: String,
    generation: String,
    endpoint: String,
    failure_channel: Option<super::StartupAttempt>,
}

impl StartupOwner {
    /// Adopts only the launcher-admitted inode. The inherited stdin is replaced
    /// before any compiler child can inherit it; the retained duplicate is CLOEXEC.
    ///
    /// # Errors
    /// Rejects incomplete, replaced or mismatched admission records.
    pub(crate) fn from_environment() -> io::Result<Option<Self>> {
        let Some(path) = std::env::var_os(GATE_ENV).map(PathBuf::from) else {
            return Ok(None);
        };
        let generation = std::env::var(GENERATION_ENV).map_err(io::Error::other)?;
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("startup gate has no parent"))?
            .to_owned();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| io::Error::other("invalid startup gate name"))?
            .to_owned();
        let directory = DirectoryCapability::open(&parent)?;
        directory.validate_private()?;
        let named = directory.open_private_file_read_write(&name, false)?;
        let gate = File::from(rustix::io::fcntl_dupfd_cloexec(io::stdin(), 3)?);
        // Close the inheritable fd even when admission validation subsequently fails.
        rustix::stdio::dup2_stdin(File::open("/dev/null")?)?;
        if FileIdentity::of_file(&named)? != FileIdentity::of_file(&gate)? {
            return Err(io::Error::other(
                "startup admission does not match inherited descriptor",
            ));
        }
        let (selected, endpoint) = header(&gate)?;
        if generation != selected {
            return Err(io::Error::other("startup admission generation changed"));
        }
        gate.try_lock().map_err(io::Error::other)?;
        Ok(Some(Self {
            _gate: gate,
            directory,
            parent,
            name,
            generation,
            endpoint,
            failure_channel: super::StartupAttempt::adopt_configured().ok().flatten(),
        }))
    }

    pub(crate) fn admits_paths(&self, workspace: &Path, endpoint: &Path) -> bool {
        self.name == format!("{}.lock", digest(workspace)) && self.endpoint == digest(endpoint)
    }

    /// Publishes a bounded path-free phase without a durability barrier.
    /// Startup progress can disappear after a crash; OWNER.state cannot.
    ///
    /// # Errors
    /// Returns an error if the ephemeral admission or snapshot is replaced.
    pub(crate) fn phase(&self, phase: StartupPhase) -> io::Result<()> {
        let result = publish(
            &self.directory,
            &self.parent,
            &self.name,
            &self.generation,
            &self.endpoint,
            phase,
        );
        if phase == StartupPhase::Listening
            && let Some(channel) = &self.failure_channel
        {
            channel.remove_if_original();
        }
        result
    }
}

fn digest(path: &Path) -> String {
    blake3::hash(path.as_os_str().as_encoded_bytes())
        .to_hex()
        .to_string()
}
fn snapshot_name(name: &str) -> String {
    format!("{name}.progress")
}
fn bounded(file: &File) -> io::Result<Vec<u8>> {
    // pread keeps parent/child reads independent despite their shared open-file
    // description. Progress never seeks the inherited admission descriptor.
    let mut bytes = vec![0; MAX_RECORD as usize + 1];
    let mut used = 0;
    loop {
        let count = file.read_at(&mut bytes[used..], used as u64)?;
        used += count;
        if count == 0 || used == bytes.len() {
            break;
        }
    }
    if used as u64 > MAX_RECORD {
        return Err(io::Error::other("startup record exceeds bound"));
    }
    bytes.truncate(used);
    Ok(bytes)
}
fn header(file: &File) -> io::Result<(String, String)> {
    let bytes = bounded(file)?;
    let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
    if !text.ends_with('\n') {
        return Err(io::Error::other("incomplete startup header"));
    }
    let mut lines = text.lines();
    if lines.next() != Some(MAGIC) {
        return Err(io::Error::other("unknown startup record"));
    }
    let generation = lines
        .next()
        .filter(|s| !s.is_empty() && s.len() <= 64)
        .ok_or_else(|| io::Error::other("invalid startup generation"))?;
    let endpoint = lines
        .next()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| io::Error::other("invalid startup endpoint identity"))?;
    if lines.next().is_some() {
        return Err(io::Error::other("unknown startup header suffix"));
    }
    Ok((generation.to_owned(), endpoint.to_owned()))
}
fn parse(bytes: &[u8]) -> Option<(&str, &str, StartupPhase)> {
    let text = std::str::from_utf8(bytes).ok()?;
    if !text.ends_with('\n') {
        return None;
    }
    let mut lines = text.lines();
    if lines.next()? != MAGIC {
        return None;
    }
    let generation = lines.next()?;
    let endpoint = lines.next()?;
    let phase = StartupPhase::from_code(lines.next()?)?;
    if lines.next().is_some() {
        return None;
    }
    Some((generation, endpoint, phase))
}
fn publish(
    directory: &DirectoryCapability,
    parent: &Path,
    name: &str,
    generation: &str,
    endpoint: &str,
    phase: StartupPhase,
) -> io::Result<()> {
    directory.verify_path(parent)?;
    let gate = directory.open_private_file_read_write(name, false)?;
    if header(&gate)? != (generation.to_owned(), endpoint.to_owned()) {
        return Err(io::Error::other("startup generation was superseded"));
    }
    let snapshot = snapshot_name(name);
    if let Ok(file) = directory.open_private_file_read_write(&snapshot, false)
        && let Ok(bytes) = bounded(&file)
        && let Some((old_generation, _, old_phase)) = parse(&bytes)
        && old_generation == generation
        && phase < old_phase
    {
        return Ok(());
    }
    let temporary = format!(
        "{snapshot}.{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let mut file = directory.create_file_exclusive(&temporary)?;
    let result = (|| {
        write!(
            file,
            "{MAGIC}\n{generation}\n{endpoint}\n{}\n",
            phase.code()
        )?;
        directory.verify_path(parent)?;
        // Ephemeral snapshot only: do not call the durable atomic writer here.
        fs::rename(parent.join(&temporary), parent.join(&snapshot))
    })();
    if result.is_err() {
        let _ = fs::remove_file(parent.join(&temporary));
    }
    result
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn fixture(tag: &str) -> PathBuf {
        super::super::super::tests::test_directory(tag)
    }
    fn wait_for(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "fixture rendezvous timed out: {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn cleanup(admission: &Admission) {
        // Only this test's unique workspace, after all descriptors/children retire.
        let _ = admission
            .directory
            .remove_file(&snapshot_name(&admission.name));
        let _ = admission.directory.remove_file(&admission.name);
    }

    #[test]
    fn launcher_timeout_preserves_late_owner_failure_until_next_generation() {
        use std::os::unix::fs::PermissionsExt as _;
        const ROLE: &str = "BACKEND_STARTUP_TIMEOUT_TEST_ROLE";
        const ROOT: &str = "BACKEND_STARTUP_TIMEOUT_TEST_ROOT";
        const CAUSE: &str = "late fixture storage recovery failed";
        if std::env::var_os(ROLE).is_some() {
            let root = PathBuf::from(std::env::var_os(ROOT).expect("fixture root"));
            assert!(crate::adopt_startup_admission().expect("inherited owner admission"));
            crate::report_startup_phase(StartupPhase::OpeningOwner);
            fs::write(root.join("owner.pid"), std::process::id().to_string())
                .expect("own child PID");
            wait_for(&root.join("report-after-timeout"));
            // Open after the caller returned, matching the real process error
            // reporter rather than retaining a descriptor from before timeout.
            super::super::StartupFailureReporter::from_environment()
                .expect("late private receipt open")
                .expect("configured failure receipt")
                .report(&CAUSE)
                .expect("publish original cause after caller deadline");
            fs::write(root.join("reported"), b"reported").expect("late cause rendezvous");
            wait_for(&root.join("owner-exit"));
            return;
        }
        let root = PathBuf::from(format!(
            "/tmp/ns-timeout-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .expect("private fixture");
        let quote = |p: &Path| format!("'{}'", p.to_string_lossy().replace('\'', "'\"'\"'"));
        let executable = root.join("fake-locald");
        fs::write(&executable, format!("#!/bin/sh\n{ROLE}=owner {ROOT}={} exec {} --exact startup::admission::tests::launcher_timeout_preserves_late_owner_failure_until_next_generation --nocapture\n", quote(&root), quote(&std::env::current_exe().expect("test executable")))).expect("fake owner launcher");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("fixture executable");
        let paths = crate::WorkspacePaths::discover(
            Some(root.clone()),
            Some(root.join("data")),
            Some(root.join("endpoint")),
        )
        .expect("paths");
        let error = crate::ensure_locald_unix(
            &paths,
            |_| {},
            Instant::now(),
            Duration::from_secs(1),
            || Ok(executable.clone()),
        )
        .expect_err("blocked child cannot establish readiness");
        assert!(matches!(
            error,
            crate::RuntimeError::StartupTimeout { spawned: true, .. }
        ));
        assert!(!error.to_string().contains("owner still starting"));
        wait_for(&root.join("owner.pid"));
        let pid = fs::read_to_string(root.join("owner.pid"))
            .expect("own PID")
            .parse::<i32>()
            .expect("numeric PID");
        struct OwnedChild(Option<rustix::process::Pid>);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                if let Some(pid) = self.0.take() {
                    // Only the unreaped child created by this fixture. Its PID
                    // cannot be reused until this parent performs the wait.
                    let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
                    let _ =
                        rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::empty());
                }
            }
        }
        let mut owned = OwnedChild(rustix::process::Pid::from_raw(pid));
        let mut follower = Admission::open(paths.data(), paths.endpoint()).expect("follower");
        assert!(
            !follower
                .try_lead()
                .expect("child retains admission after caller timeout")
        );
        let receipt_path = follower.directory().join(follower.failure_name());
        // Keep the old inode open through replacement so identity reuse cannot
        // turn this next-generation check into a filesystem timing assumption.
        let retained = File::open(&receipt_path).expect("retained pending receipt");
        let before = FileIdentity::of_file(&retained).expect("pending receipt identity");
        fs::write(root.join("report-after-timeout"), b"report").expect("allow late failure");
        wait_for(&root.join("reported"));
        let bytes = fs::read(&receipt_path).expect("late terminal receipt");
        assert_eq!(
            super::super::terminal_cause(&bytes)
                .expect("valid bounded cause")
                .as_deref(),
            Some(CAUSE)
        );
        assert_eq!(
            FileIdentity::of_file(&File::open(&receipt_path).expect("same receipt"))
                .expect("terminal receipt identity"),
            before
        );
        fs::write(root.join("owner-exit"), b"exit").expect("retire own child");
        rustix::process::waitpid(owned.0, rustix::process::WaitOptions::empty())
            .expect("kernel wait detached owned child");
        owned.0 = None;
        assert!(
            follower
                .try_lead()
                .expect("next kernel-admitted generation")
        );
        let next = super::super::StartupAttempt::prepare_reserved(
            follower.directory(),
            follower.failure_name(),
        )
        .expect("validated stale receipt cleanup");
        assert_ne!(
            FileIdentity::of_file(&next.file).expect("fresh generation receipt"),
            before
        );
        assert_eq!(
            fs::read(&receipt_path).expect("fresh pending record"),
            super::super::MAGIC.as_bytes()
        );
        drop(next);
        assert!(
            !receipt_path.exists(),
            "normal next-generation cleanup stays bounded"
        );
        cleanup(&follower);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn admission_coalesces_independent_processes_and_survives_launcher_drop() {
        const ROLE: &str = "BACKEND_STARTUP_ADMISSION_TEST_ROLE";
        const ROOT: &str = "BACKEND_STARTUP_ADMISSION_TEST_ROOT";
        if let Ok(role) = std::env::var(ROLE) {
            let root = PathBuf::from(std::env::var_os(ROOT).expect("fixture root"));
            if role == "descendant" {
                fs::write(root.join("descendant-ready"), b"ready").expect("descendant ready");
                wait_for(&root.join("descendant-exit"));
                return;
            }
            let owner = StartupOwner::from_environment()
                .expect("adopt inherited admission")
                .expect("configured admission");
            if role == "launcher-holder" {
                fs::write(root.join("launcher-ready"), b"ready").expect("launcher holder ready");
                wait_for(&root.join("launcher-exit"));
                return;
            }
            owner
                .phase(StartupPhase::PreparingWorkspace)
                .expect("phase before durable work");
            fs::write(root.join("owner-ready"), b"ready").expect("owner ready");
            wait_for(&root.join("advance"));
            owner
                .phase(StartupPhase::OpeningOwner)
                .expect("owner admission phase");
            // A late/duplicate earlier phase must never regress the snapshot.
            owner
                .phase(StartupPhase::PreparingWorkspace)
                .expect("ignore regression");
            fs::write(root.join("owner-advanced"), b"advanced").expect("owner advanced");
            let mut descendant = Command::new(std::env::current_exe().expect("test exe"))
                .args(["--exact", "startup::admission::tests::admission_coalesces_independent_processes_and_survives_launcher_drop"])
                .env(ROLE, "descendant").env(ROOT, &root)
                .stdout(Stdio::null()).stderr(Stdio::null())
                .spawn().expect("spawn descendant with default inherited stdin");
            wait_for(&root.join("descendant-ready"));
            wait_for(&root.join("owner-exit"));
            let _listener = std::os::unix::net::UnixListener::bind(root.join("endpoint"))
                .expect("actual listener before receipt cleanup");
            owner
                .phase(StartupPhase::Listening)
                .expect("bound listener cleanup");
            drop(owner);
            fs::write(root.join("owner-released"), b"released").expect("admission released");
            descendant.wait().expect("reap descendant");
            return;
        }
        let root = PathBuf::from(format!(
            "/tmp/ns-admission-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("fixture root");
        let mut leader = Admission::open(&root, &root.join("endpoint")).expect("open gate");
        assert!(leader.try_lead().expect("lead"));
        let mut command = Command::new(std::env::current_exe().expect("test exe"));
        command.args(["--exact", "startup::admission::tests::admission_coalesces_independent_processes_and_survives_launcher_drop"])
            .env(ROLE, "owner").env(ROOT, &root).stdout(Stdio::null()).stderr(Stdio::null());
        let receipt = super::super::StartupAttempt::prepare_reserved(
            leader.directory(),
            leader.failure_name(),
        )
        .expect("bounded reserved receipt");
        let receipt_path = receipt.path.clone();
        receipt.configure(&mut command);
        leader.configure(&mut command).expect("inherit gate");
        let mut owner = command.spawn().expect("spawn owner");
        // Command also owns a duplicate of its configured stdin; retiring the
        // launcher means dropping both that copy and the leader descriptor.
        drop(command);
        wait_for(&root.join("owner-ready"));
        use std::os::unix::process::CommandExt as _;
        let mut launcher_command = Command::new(std::env::current_exe().expect("test exe"));
        launcher_command.args(["--exact", "startup::admission::tests::admission_coalesces_independent_processes_and_survives_launcher_drop"])
            .env(ROLE, "launcher-holder").env(ROOT, &root).process_group(0).stdout(Stdio::null()).stderr(Stdio::null());
        receipt.configure(&mut launcher_command);
        leader
            .configure(&mut launcher_command)
            .expect("inherited launcher-holder descriptor");
        let mut launcher = launcher_command.spawn().expect("launcher holder");
        drop(launcher_command);
        wait_for(&root.join("launcher-ready"));
        let launcher_pid = rustix::process::Pid::from_raw(launcher.id().cast_signed())
            .expect("owned launcher PID");
        assert_eq!(
            rustix::process::getpgid(Some(launcher_pid)).expect("owned launcher group"),
            launcher_pid
        );
        rustix::process::kill_process_group(launcher_pid, rustix::process::Signal::TERM)
            .expect("cancel owned launcher group");
        launcher
            .wait()
            .expect("kernel wait canceled launcher holder");
        let mut follower = Admission::open(&root, &root.join("endpoint")).expect("follower");
        assert!(!follower.try_lead().expect("independent contention"));
        let (generation, phase) = follower.progress().expect("shared progress");
        assert_eq!(phase, StartupPhase::PreparingWorkspace);
        drop(leader);
        assert!(
            !follower
                .try_lead()
                .expect("child survives launcher cancellation")
        );
        fs::write(root.join("advance"), b"advance").expect("advance owner");
        wait_for(&root.join("owner-advanced"));
        assert_eq!(
            follower.progress(),
            Some((generation.clone(), StartupPhase::OpeningOwner))
        );
        fs::write(root.join("owner-exit"), b"exit").expect("retire owner admission");
        wait_for(&root.join("owner-released"));
        assert!(
            !receipt_path.exists(),
            "successful detached owner cleans its canceled launch receipt"
        );
        drop(receipt);
        assert!(
            follower
                .try_lead()
                .expect("compiler descendant cannot retain admission")
        );
        assert_ne!(follower.progress().expect("new generation").0, generation);
        fs::write(root.join("descendant-exit"), b"exit").expect("retire descendant");
        assert!(owner.wait().expect("kernel wait owner").success());
        cleanup(&follower);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn unknown_torn_oversize_and_stale_snapshots_never_prove_progress() {
        let root = fixture("startup-snapshot-fencing");
        let mut leader = Admission::open(&root, &root.join("endpoint")).expect("gate");
        assert!(leader.try_lead().expect("lead"));
        let snapshot = leader.parent.join(snapshot_name(&leader.name));
        for payload in [b"".as_slice(), b"nudox.local-startup.v2\n", &[b'x'; 257]] {
            fs::write(&snapshot, payload).expect("corrupt snapshot");
            assert!(leader.progress().is_none());
        }
        leader
            .publish(StartupPhase::BindingListener)
            .expect("publish bounded progress");
        assert_eq!(
            leader.progress().expect("phase").1,
            StartupPhase::BindingListener
        );
        let stale = fs::read(&snapshot).expect("old generation");
        let mut next = Admission::open(&root, &root.join("endpoint")).expect("next");
        drop(leader);
        assert!(next.try_lead().expect("dead owner reacquire"));
        fs::write(&snapshot, stale).expect("stale snapshot");
        assert!(next.progress().is_none());
        cleanup(&next);
    }

    #[test]
    fn progress_replacement_does_not_replace_the_canonical_lock_inode() {
        let root = fixture("startup-lock-identity");
        let mut admission = Admission::open(&root, &root.join("endpoint")).expect("gate");
        assert!(admission.try_lead().expect("lead"));
        let identity = FileIdentity::of_file(&admission.file).expect("lock identity");
        for phase in [
            StartupPhase::PreparingWorkspace,
            StartupPhase::SelectingCompilers,
            StartupPhase::RecoveringSemanticState,
            StartupPhase::PublishingView,
            StartupPhase::BindingListener,
            StartupPhase::Listening,
        ] {
            admission.publish(phase).expect("bounded ephemeral phase");
            assert_eq!(
                FileIdentity::of_path_nofollow(&admission.parent.join(&admission.name))
                    .expect("same lock inode"),
                identity
            );
            assert_eq!(admission.progress().expect("snapshot").1, phase);
        }
        // Listening is deliberately just a phase; there is no Ready variant.
        cleanup(&admission);
    }
}
