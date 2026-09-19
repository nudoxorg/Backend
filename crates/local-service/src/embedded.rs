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
        })
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

impl Drop for EmbeddedLocalService {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
