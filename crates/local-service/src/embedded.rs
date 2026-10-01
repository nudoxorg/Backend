//! In-process lifecycle for hosts that own the local service directly.
//!
//! # Owning versus attaching
//!
//! Workspace ownership is a kernel lock on an inode; the endpoint is a
//! separate socket. [`EmbeddedLocalService::start`] takes the lock first and
//! binds afterwards, which is correct for a host that knows it is alone, but
//! it turns two ordinary situations into a hard failure: a surface that loses
//! a startup race, and a surface whose endpoint was swept out of `/tmp` while
//! the owner kept running. Both die on the lock instead of attaching to the
//! very process that holds it.
//!
//! [`start_or_attach`] inverts that order. It asks about the endpoint before
//! it asks for the lock, and it answers with a typed [`ServiceStart`] rather
//! than an error when somebody else is already the owner:
//!
//! 1. Connect to the endpoint. A reply is proof of a live owner — attach.
//! 2. Classify the endpoint file. A socket nobody answers is unlinked; a live
//!    one reports [`crate::ListenerError::AlreadyRunning`] — attach.
//! 3. Compose the owner and bind. On success this process is the owner.
//! 4. On a composition failure, wait a bounded window for an owner to publish
//!    the endpoint. Holding the workspace lock and binding the listener are
//!    not simultaneous, so a refusal at step 3 is not yet evidence that no
//!    owner exists.
//!
//! A [`ProcessError`] therefore now means what it says: either the failure had
//! nothing to do with contention, or an owner is provably alive and its
//! endpoint stayed unreachable for the whole window.

use crate::listener::{ListenerError, ListenerShutdown, RunReport, UnixListenerService};
use crate::process::{ProcessConfig, ProcessError};
use crate::service::LocaldService;
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How long [`start_or_attach`] waits for a contending owner to publish its
/// endpoint before reporting the composition failure it already has.
const ATTACH_WINDOW: Duration = Duration::from_secs(2);

/// How often that window is polled.
const ATTACH_POLL: Duration = Duration::from_millis(20);

/// A local owner running inside its presentation host.
///
/// The owner and listener stay on one dedicated thread. Callers receive only
/// the endpoint and a lifecycle capability, so GUI code cannot borrow or
/// mutate authoritative state directly. CLI and MCP therefore exercise the
/// identical authenticated protocol whether this owner lives in the desktop
/// process or in the headless launcher.
pub struct EmbeddedLocalService {
    endpoint: PathBuf,
    shutdown: ListenerShutdown,
    thread: Option<JoinHandle<Result<RunReport, ListenerError>>>,
    set_aside: Option<PathBuf>,
}

/// How a surface reached the local service.
///
/// This is the additive replacement for reading contention out of a
/// [`ProcessError`] string. A host that can work through either outcome
/// matches on it; a host that must own the workspace treats
/// [`ServiceStart::Attached`] as its own kind of refusal.
pub enum ServiceStart {
    /// This process composed the owner, holds the workspace lease, and bound
    /// the endpoint.
    Owned(EmbeddedLocalService),
    /// Another live process already owns the workspace. Its endpoint answered.
    Attached {
        /// Endpoint published by the live owner.
        endpoint: PathBuf,
    },
}

impl ServiceStart {
    /// Returns the endpoint every local surface shares, whichever way this
    /// process reached it.
    #[must_use]
    pub fn endpoint(&self) -> &Path {
        match self {
            Self::Owned(service) => service.endpoint(),
            Self::Attached { endpoint } => endpoint,
        }
    }

    /// Returns the owner when this process is it.
    #[must_use]
    pub const fn owned(&self) -> Option<&EmbeddedLocalService> {
        match self {
            Self::Owned(service) => Some(service),
            Self::Attached { .. } => None,
        }
    }
}

/// Binds the selected composition, or attaches to the owner that already
/// holds this workspace.
///
/// # Errors
///
/// Returns an error when composition, binding, or thread creation fails for a
/// reason other than contention. A busy workspace is reported only when an
/// owner is provably alive *and* its endpoint could not be reached for the
/// bounded attach window; every other contention outcome is
/// [`ServiceStart::Attached`].
pub fn start_or_attach(config: ProcessConfig) -> Result<ServiceStart, ProcessError> {
    let endpoint = config.listener.path.clone();
    if endpoint_is_live(&endpoint) {
        return Ok(ServiceStart::Attached { endpoint });
    }
    match sweep_endpoint(&endpoint) {
        Ok(()) => {}
        Err(ListenerError::AlreadyRunning) => return Ok(ServiceStart::Attached { endpoint }),
        Err(error) => return Err(ProcessError::Listener(error)),
    }
    match EmbeddedLocalService::start(config) {
        Ok(service) => Ok(ServiceStart::Owned(service)),
        Err(ProcessError::Listener(ListenerError::AlreadyRunning)) => {
            Ok(ServiceStart::Attached { endpoint })
        }
        Err(refusal) => wait_for_live_owner(endpoint, refusal),
    }
}

/// Waits a bounded window for a contending owner to publish its endpoint.
///
/// Composition can fail because another process holds the workspace lock and
/// has not bound its listener yet. That process is the authority, so the only
/// correct answer is to attach to it. The refusal is reported verbatim only
/// when the endpoint never becomes reachable.
fn wait_for_live_owner(
    endpoint: PathBuf,
    refusal: ProcessError,
) -> Result<ServiceStart, ProcessError> {
    let deadline = Instant::now() + ATTACH_WINDOW;
    while Instant::now() < deadline {
        if endpoint_is_live(&endpoint) {
            return Ok(ServiceStart::Attached { endpoint });
        }
        thread::sleep(ATTACH_POLL);
    }
    Err(refusal)
}

#[cfg(any(unix, windows))]
fn endpoint_is_live(endpoint: &Path) -> bool {
    backend_engine::LocalStream::connect(endpoint).is_ok()
}

#[cfg(not(any(unix, windows)))]
const fn endpoint_is_live(_endpoint: &Path) -> bool {
    false
}

#[cfg(any(unix, windows))]
fn sweep_endpoint(endpoint: &Path) -> Result<(), ListenerError> {
    crate::listener::sweep_endpoint(endpoint)
}

#[cfg(not(any(unix, windows)))]
const fn sweep_endpoint(_endpoint: &Path) -> Result<(), ListenerError> {
    Ok(())
}

impl EmbeddedLocalService {
    /// Binds and starts the selected compiled service composition.
    ///
    /// This takes the workspace lock before it binds the endpoint, so a host
    /// that may be racing another surface should call [`start_or_attach`]
    /// instead of treating the resulting error as fatal.
    ///
    /// # Errors
    /// Returns an error when composition, binding, or thread creation fails.
    pub fn start(config: ProcessConfig) -> Result<Self, ProcessError> {
        let owner = crate::builtin::compose_owner(&config)?;
        let service =
            LocaldService::new(owner, config.listener.limits).map_err(ProcessError::Protocol)?;
        let mut listener =
            UnixListenerService::bind(service, config.listener).map_err(ProcessError::Listener)?;
        let endpoint = listener.path().to_path_buf();
        let shutdown = listener.shutdown_handle();
        let thread = thread::Builder::new()
            .name("backend-local-owner".to_owned())
            .spawn(move || listener.run())
            .map_err(|error| ProcessError::Profile(format!("start embedded owner: {error}")))?;
        Ok(Self {
            endpoint,
            shutdown,
            thread: Some(thread),
            set_aside: None,
        })
    }

    /// Starts like [`Self::start`], for a host that owns its workspace and can
    /// index it again: a workspace another build of the product wrote
    /// ([`ProcessError::StateFromAnotherBuild`]) is set aside whole and a fresh
    /// one is started in its place.
    ///
    /// Nothing is deleted. Every entry of the workspace moves into a
    /// directory of it named `from-another-build` (`-2`, `-3`, ... when one
    /// exists), except the owner lock and its epoch and the authority
    /// credential, which are not index state. The set-aside state is a
    /// snapshot for the operator; nothing reads it back. The host must index
    /// its projects again, and can say where the old state went
    /// ([`Self::state_set_aside`]).
    ///
    /// The workspace is set aside only while this process holds its lock, so a
    /// workspace another process owns is never moved; that contention is the
    /// returned error, as for [`Self::start`].
    ///
    /// # Errors
    /// Returns the error of [`Self::start`], including for the fresh
    /// workspace, or the reason the old state could not be set aside.
    pub fn start_replacing_state_from_another_build(
        config: ProcessConfig,
    ) -> Result<Self, ProcessError> {
        match Self::start(config.clone()) {
            Err(ProcessError::StateFromAnotherBuild(_)) => {
                let set_aside = set_aside_workspace_state(&config)?;
                let mut started = Self::start(config)?;
                started.set_aside = Some(set_aside);
                Ok(started)
            }
            started => started,
        }
    }

    /// Returns where the workspace state this service replaced was moved, when
    /// it replaced one ([`Self::start_replacing_state_from_another_build`]).
    #[must_use]
    pub fn state_set_aside(&self) -> Option<&Path> {
        self.set_aside.as_deref()
    }

    /// Returns the authenticated endpoint shared with every local surface.
    #[must_use]
    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    /// Returns whether the owner loop is still running.
    ///
    /// An embedded listener can retire itself through its idle timeout, so a
    /// host that keeps one resident can observe that here without joining it.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
    }

    /// Requests shutdown and waits for the owner to close its durable state.
    ///
    /// # Errors
    /// Returns an error when the listener failed or its thread panicked.
    pub fn close(mut self) -> Result<RunReport, ProcessError> {
        self.finish()
    }

    fn finish(&mut self) -> Result<RunReport, ProcessError> {
        self.shutdown.request();
        let Some(thread) = self.thread.take() else {
            return Ok(RunReport::default());
        };
        thread
            .join()
            .map_err(|_| ProcessError::Profile("embedded owner thread panicked".to_owned()))?
            .map_err(ProcessError::Listener)
    }
}

/// Name of the directory, inside a workspace, that receives state from another
/// build.
const SET_ASIDE_DIRECTORY: &str = "from-another-build";

/// Moves every index entry of the workspace into a fresh [`SET_ASIDE_DIRECTORY`]
/// of it, under the workspace lock, and returns that directory.
///
/// The lock file and its epoch stay: the lock is what makes moving safe, and
/// they are not state anybody could read back. So does the authority
/// credential when it lives in the workspace: the fresh state is opened under
/// the same secret its clients already hold.
fn set_aside_workspace_state(config: &ProcessConfig) -> Result<PathBuf, ProcessError> {
    let refused = |what: String| {
        ProcessError::Profile(format!(
            "set aside workspace state from another build ({}): {what}",
            config.workspace.display()
        ))
    };
    let _lock =
        backend_store::StorePublicationAuthority::acquire(&config.workspace.join("OWNER.lock"))
            .map_err(|error| refused(error.to_string()))?;
    let destination = (1..1000)
        .map(|number| match number {
            1 => config.workspace.join(SET_ASIDE_DIRECTORY),
            number => config
                .workspace
                .join(format!("{SET_ASIDE_DIRECTORY}-{number}")),
        })
        .find(|candidate| !candidate.exists())
        .ok_or_else(|| refused("a thousand earlier sets exist".to_owned()))?;
    backend_platform::durable::ensure_private_directory(&destination)
        .map_err(|error| refused(format!("create {}: {error}", destination.display())))?;
    let entries = std::fs::read_dir(&config.workspace)
        .map_err(|error| refused(format!("read the workspace: {error}")))?;
    for entry in entries {
        let entry = entry.map_err(|error| refused(format!("read the workspace: {error}")))?;
        let path = entry.path();
        let name = entry.file_name();
        let kept = name == "OWNER.lock"
            || name == "OWNER.state"
            || name.to_string_lossy().starts_with(SET_ASIDE_DIRECTORY)
            || config.authority_secret.as_deref() == Some(path.as_path());
        if !kept {
            std::fs::rename(&path, destination.join(&name))
                .map_err(|error| refused(format!("move {}: {error}", path.display())))?;
        }
    }
    Ok(destination)
}

impl Drop for EmbeddedLocalService {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use backend_runtime::WorkspacePaths;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A private scratch root, an initialised workspace in it, and the
    /// configuration that starts an owner on that workspace.
    fn scratch_workspace(tag: &str) -> (PathBuf, ProcessConfig) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        // `/tmp`: a socket path under a nix temp directory is too long.
        let root =
            PathBuf::from("/tmp").join(format!("nx-ls-{tag}-{}-{nonce}", std::process::id()));
        // The owner refuses a state directory whose parent anyone else can enter.
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&root)
                .expect("private scratch root");
        }
        #[cfg(not(unix))]
        std::fs::create_dir_all(&root).expect("scratch root");
        let project = root.join("project");
        std::fs::create_dir_all(&project).expect("project");
        let paths = WorkspacePaths::discover(
            Some(project),
            Some(root.join("data")),
            Some(root.with_extension("sock")),
        )
        .expect("paths");
        paths.initialize().expect("initialize the workspace");
        let config = ProcessConfig::parse([
            "--endpoint".to_owned(),
            paths.endpoint().to_string_lossy().into_owned(),
            "--workspace".to_owned(),
            paths.data().to_string_lossy().into_owned(),
            "--authority-secret-file".to_owned(),
            paths.authority_secret().to_string_lossy().into_owned(),
            "--profile".to_owned(),
            "builtin".to_owned(),
        ])
        .expect("configuration");
        (root, config)
    }

    /// A workspace an earlier build wrote, and the configuration that starts
    /// an owner on it.
    fn state_from_another_build(tag: &str) -> (PathBuf, ProcessConfig) {
        let (root, config) = scratch_workspace(tag);
        let secret = config.authority_secret.clone().expect("credential path");
        crate::builtin::write_state_from_another_build(&config.workspace, &secret)
            .expect("write the workspace an earlier build left");
        (root, config)
    }

    /// How another build's view journal differs from the one this build writes.
    #[derive(Clone, Copy)]
    enum Journal {
        /// Its snapshots carry a view of an older wire version: the
        /// `unsupported view DTO version` refusal.
        WireVersion,
        /// Its frames are of another journal format.
        FrameFormat,
    }

    /// A workspace this build wrote (an owner booted on it and closed, so its
    /// view journal is the one this build writes), with that journal then
    /// rewritten as another build would have left it.
    fn view_journal_from_another_build(tag: &str, how: Journal) -> (PathBuf, ProcessConfig) {
        let (root, config) = scratch_workspace(tag);
        EmbeddedLocalService::start(config.clone())
            .expect("this build boots on a fresh workspace")
            .close()
            .expect("and closes it");
        let path = config.workspace.join("view.journal");
        let old = std::fs::read(&path).expect("the view journal this build wrote");
        // A frame: magic (8), format (1), kind (1), length (8, big endian),
        // BLAKE3 of the payload (32), payload.
        let (mut new, mut at, mut rewritten) = (Vec::new(), 0, 0);
        while at < old.len() {
            let length = usize::try_from(u64::from_be_bytes(
                old[at + 10..at + 18].try_into().expect("length"),
            ))
            .expect("length fits");
            let payload = &old[at + 50..at + 50 + length];
            let mut header = old[at..at + 50].to_vec();
            let mut payload = payload.to_vec();
            match how {
                Journal::FrameFormat => {
                    header[8] = header[8].wrapping_add(1);
                    rewritten += 1;
                }
                Journal::WireVersion => {
                    let mut envelope: serde_json::Value =
                        serde_json::from_slice(&payload).expect("a JSON envelope");
                    if let Some(view) = envelope.get_mut("view").filter(|view| !view.is_null()) {
                        view["version"] = serde_json::json!(backend_library::DTO_VERSION - 1);
                        payload = serde_json::to_vec(&envelope).expect("envelope");
                        header[10..18].copy_from_slice(&(payload.len() as u64).to_be_bytes());
                        header[18..50].copy_from_slice(blake3::hash(&payload).as_bytes());
                        rewritten += 1;
                    }
                }
            }
            new.extend_from_slice(&header);
            new.extend_from_slice(&payload);
            at += 50 + length;
        }
        assert!(rewritten > 0, "the journal held no frame to rewrite");
        std::fs::write(&path, new).expect("write the rewritten journal");
        (root, config)
    }

    #[test]
    fn a_workspace_an_earlier_build_wrote_is_refused_as_state_from_another_build() {
        let (root, config) = state_from_another_build("typed");
        let workspace = config.workspace.clone();

        let refusal = EmbeddedLocalService::start(config)
            .err()
            .expect("a build that cannot read this layout must not start on it");

        // Before: `ProcessError::Profile("repair product view: source file is outside its
        // project's canonical frontier")`, indistinguishable from a corrupt workspace.
        let ProcessError::StateFromAnotherBuild(words) = &refusal else {
            panic!("refused as something else: {refusal}");
        };
        assert!(
            words.contains("1 indexed source files are keyed by the source-file key layout an earlier build wrote"),
            "the refusal says what was recognised: {words}"
        );
        assert!(
            workspace.join("workspace.journal").is_file()
                && !workspace.join(SET_ASIDE_DIRECTORY).exists(),
            "starting alone moves nothing: the operator's directory is as it was"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn replacing_state_from_another_build_keeps_the_old_state_whole_and_starts_a_fresh_owner() {
        let (root, config) = state_from_another_build("replace");
        let workspace = config.workspace.clone();
        let old_journal = std::fs::read(workspace.join("workspace.journal")).expect("old journal");
        let secret = std::fs::read(workspace.join("authority.secret")).expect("credential");

        let service = EmbeddedLocalService::start_replacing_state_from_another_build(config)
            .expect("the fresh owner starts where the old state was");

        let set_aside = service
            .state_set_aside()
            .expect("says where the old state went")
            .to_path_buf();
        assert_eq!(set_aside, workspace.join(SET_ASIDE_DIRECTORY));
        assert_eq!(
            std::fs::read(set_aside.join("workspace.journal")).expect("kept journal"),
            old_journal,
            "the old state is kept byte for byte"
        );
        assert_eq!(
            std::fs::read(workspace.join("authority.secret")).expect("credential"),
            secret,
            "the credential its clients hold is not replaced"
        );
        assert!(
            workspace.join("OWNER.lock").is_file(),
            "the lock file stays where it is"
        );
        assert_ne!(
            std::fs::read(workspace.join("workspace.journal")).expect("fresh journal"),
            old_journal,
            "the workspace the owner runs on is a fresh one"
        );
        assert!(service.is_running(), "the owner is serving");
        service.close().expect("clean shutdown");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_workspace_another_process_holds_the_lock_of_is_never_moved() {
        let (root, config) = state_from_another_build("held");
        let workspace = config.workspace.clone();
        let _held =
            backend_store::StorePublicationAuthority::acquire(&workspace.join("OWNER.lock"))
                .expect("this test is the other process");

        let refusal =
            set_aside_workspace_state(&config).expect_err("a held workspace is not set aside");

        assert!(
            refusal.to_string().contains("held by another process"),
            "the refusal says why: {refusal}"
        );
        assert!(
            workspace.join("workspace.journal").is_file()
                && !workspace.join(SET_ASIDE_DIRECTORY).exists(),
            "nothing moved"
        );
        drop(_held);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_view_journal_another_build_wrote_is_state_from_another_build_and_is_set_aside() {
        for (how, evidence) in [
            (Journal::WireVersion, "its view snapshot is wire version"),
            (Journal::FrameFormat, "its view journal is format"),
        ] {
            let (root, config) = view_journal_from_another_build("journal", how);
            let workspace = config.workspace.clone();
            let old_journal = std::fs::read(workspace.join("view.journal")).expect("old journal");

            let refusal = EmbeddedLocalService::start(config.clone())
                .err()
                .expect("a build that cannot read this journal must not start on it");

            // Before: `ProcessError::Profile("unsupported view DTO version")` (or
            // `view journal version mismatch`), which refused the directory whole.
            let ProcessError::StateFromAnotherBuild(words) = &refusal else {
                panic!("refused as something else: {refusal}");
            };
            assert!(
                words.contains(evidence),
                "the refusal says what was recognised: {words}"
            );

            let service = EmbeddedLocalService::start_replacing_state_from_another_build(config)
                .expect("the fresh owner starts where the old journal was");
            let set_aside = service
                .state_set_aside()
                .expect("says where the old state went");
            assert_eq!(
                std::fs::read(set_aside.join("view.journal")).expect("kept journal"),
                old_journal,
                "the old journal is kept byte for byte"
            );
            assert_ne!(
                std::fs::read(workspace.join("view.journal")).expect("fresh journal"),
                old_journal,
                "the owner runs on a fresh journal"
            );
            assert!(service.is_running());
            service.close().expect("clean shutdown");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn a_damaged_view_journal_is_not_state_from_another_build() {
        let (root, config) = scratch_workspace("damaged");
        EmbeddedLocalService::start(config.clone())
            .expect("this build boots on a fresh workspace")
            .close()
            .expect("and closes it");
        let path = config.workspace.join("view.journal");
        let mut bytes = std::fs::read(&path).expect("journal");
        // Flip a byte inside the first frame's payload: a checksum failure is damage.
        let inside = 60.min(bytes.len() - 1);
        bytes[inside] ^= 0xff;
        // Keep it an interior frame: a bad checksum on the last frame is a torn tail.
        bytes.extend_from_slice(&bytes.clone());
        std::fs::write(&path, bytes).expect("write");

        let refusal = EmbeddedLocalService::start(config)
            .err()
            .expect("a damaged journal is refused");

        assert!(
            matches!(refusal, ProcessError::Profile(_)),
            "damage stays a plain refusal, never a reason to set state aside: {refusal}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
