//! In-process lifecycle for hosts that own the local service directly.

use crate::listener::{ListenerShutdown, RunReport, UnixListenerService};
use crate::process::{ProcessConfig, ProcessError};
use crate::service::LocaldService;
use std::path::Path;
use std::thread::{self, JoinHandle};

/// A local owner running inside its presentation host.
///
/// The owner and listener stay on one dedicated thread. Callers receive only
/// the endpoint and a lifecycle capability, so GUI code cannot borrow or
/// mutate authoritative state directly. CLI and MCP therefore exercise the
/// identical authenticated protocol whether this owner lives in the desktop
/// process or in the headless launcher.
pub struct EmbeddedLocalService {
    endpoint: std::path::PathBuf,
    shutdown: ListenerShutdown,
    thread: Option<JoinHandle<Result<RunReport, crate::ListenerError>>>,
}

impl EmbeddedLocalService {
    /// Binds and starts the selected compiled service composition.
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
