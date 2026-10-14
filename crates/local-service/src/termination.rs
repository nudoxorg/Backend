//! Graceful retirement of the detached owner process on SIGTERM and SIGINT.
//!
//! A detached owner has no supervisor, and the default action of these
//! signals ends the process without running any destructor: the endpoint
//! socket stays on disk and nothing closes the listener until the kernel
//! finishes tearing the process down. During that window a dial is still
//! accepted by a listener that will never answer, which is exactly the
//! failure a client sees when it races a fresh command against a restart.
//!
//! The handler turns the signal into the listener's own stop request. The run
//! loop then stops accepting, drains admitted frames, closes the workspace
//! owner (releasing its lease), and unlinks the endpoint when the listener
//! drops, so the process exits `0` with nothing left behind. `kill -9` cannot
//! be handled and is covered on the client side by owner-absence detection.
//!
//! A shutdown that does not finish is never allowed to outlast the signal: a
//! second signal, or [`TERMINATION_GRACE`] after the first, unlinks the
//! endpoint and exits immediately, which is what the default action did.

#[cfg(unix)]
pub(crate) use unix::{TerminationGuard, install};

/// How long a requested shutdown may take before the process exits anyway.
#[cfg(unix)]
pub(crate) const TERMINATION_GRACE: std::time::Duration = std::time::Duration::from_secs(15);

#[cfg(unix)]
mod unix {
    use crate::listener::ListenerShutdown;
    use std::io;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    use tokio::signal::unix::{SignalKind, signal};

    /// Marks the orderly shutdown as finished when dropped, which disarms the
    /// forced exit. The process normally ends right after, so this only
    /// matters to a host that keeps running (a test binary).
    #[must_use = "dropping the guard immediately disarms the forced exit"]
    pub(crate) struct TerminationGuard(Arc<AtomicBool>);

    impl Drop for TerminationGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    /// Routes SIGTERM and SIGINT to `shutdown`, forcing exit if it stalls.
    ///
    /// The handlers are installed before this returns, so a signal that
    /// arrives afterwards can never take the default action.
    ///
    /// # Errors
    /// Returns the I/O error when the signal thread or its handlers cannot be
    /// created; the caller keeps running with the default actions.
    pub(crate) fn install(
        shutdown: ListenerShutdown,
        endpoint: PathBuf,
        grace: Duration,
    ) -> io::Result<TerminationGuard> {
        let finished = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&finished);
        let (ready_sender, ready) = std::sync::mpsc::sync_channel::<io::Result<()>>(1);
        std::thread::Builder::new()
            .name("locald-termination".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready_sender.send(Err(error));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let (mut terminate, mut interrupt) = match (
                        signal(SignalKind::terminate()),
                        signal(SignalKind::interrupt()),
                    ) {
                        (Ok(terminate), Ok(interrupt)) => (terminate, interrupt),
                        (Err(error), _) | (_, Err(error)) => {
                            let _ = ready_sender.send(Err(error));
                            return;
                        }
                    };
                    let _ = ready_sender.send(Ok(()));
                    let first = tokio::select! {
                        _ = terminate.recv() => SignalKind::terminate(),
                        _ = interrupt.recv() => SignalKind::interrupt(),
                    };
                    shutdown.request();
                    let deadline = Instant::now() + grace;
                    let forced = tokio::select! {
                        _ = terminate.recv() => true,
                        _ = interrupt.recv() => true,
                        () = async {
                            while Instant::now() < deadline && !observed.load(Ordering::Acquire) {
                                tokio::time::sleep(Duration::from_millis(25)).await;
                            }
                        } => !observed.load(Ordering::Acquire),
                    };
                    if !forced {
                        return;
                    }
                    // The orderly path did not finish in time. Leave nothing
                    // behind that a client could mistake for a live owner.
                    let _ = std::fs::remove_file(&endpoint);
                    std::process::exit(128 + first.as_raw_value());
                });
            })?;
        ready
            .recv()
            .unwrap_or_else(|_| Err(io::Error::other("termination handler thread exited")))?;
        Ok(TerminationGuard(finished))
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::listener::{ListenerConfig, UnixListenerService};
    use crate::protocol::{EngineRequest, EngineStatus, FrameLimits, ProtocolError};
    use crate::service::{LocaldService, OwnerService};

    struct Idle;
    impl OwnerService for Idle {
        fn command(&mut self, _: &[u8]) -> Result<Vec<u8>, ProtocolError> {
            Err(ProtocolError::Closed)
        }
        fn engine(&mut self, _: u64, _: EngineRequest) -> Result<EngineStatus, ProtocolError> {
            Err(ProtocolError::Closed)
        }
        fn serve_one(&mut self) -> bool {
            false
        }
        fn close(&mut self) {}
    }

    /// The signal must reach the listener's own stop request, and the run loop
    /// must then unlink the endpoint, exactly as an idle retirement does.
    #[test]
    fn sigterm_stops_the_run_loop_and_unlinks_the_endpoint() {
        let path = std::path::PathBuf::from(format!(
            "/tmp/backend-termination-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let mut config = ListenerConfig::new(&path);
        config.idle_timeout = None;
        let service = LocaldService::new(Idle, FrameLimits::default()).expect("service");
        let mut listener = UnixListenerService::bind(service, config).expect("bind");
        let guard = install(
            listener.shutdown_handle(),
            path.clone(),
            std::time::Duration::from_secs(60),
        )
        .expect("install handlers");
        assert!(path.exists(), "the endpoint is bound while serving");
        // Another test thread would also receive a process-directed signal,
        // so raise it on this process only after the handler is registered.
        rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::TERM)
            .expect("raise SIGTERM");
        let report = listener.run().expect("run loop ends on its stop request");
        assert_eq!(report.frames, 0);
        assert!(listener.is_shutdown());
        drop(listener);
        drop(guard);
        assert!(!path.exists(), "a retired owner leaves no endpoint behind");
    }
}
