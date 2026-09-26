//! Single-owner Unix listener loop.
//!
//! Each connection has one bounded request slot. The listener thread is the
//! only caller of the owner service.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use crate::service::{LocaldService, OwnerService};

use super::transport::{
    ConnectionContext, configure_stream, connection_worker, prepare_socket_path,
    set_private_socket_permissions,
};
use super::{
    FilesystemPeerPolicy, ListenerConfig, ListenerError, ListenerShutdown, PeerPolicy, RunReport,
    UnixListenerService, reap_finished_workers,
};

#[cfg(any(unix, windows))]
impl<O: OwnerService + 'static> UnixListenerService<O> {
    /// Binds a private Unix endpoint around one owner service.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint cannot be created or authorized.
    pub fn bind(service: LocaldService<O>, config: ListenerConfig) -> Result<Self, ListenerError> {
        Self::bind_with_peer_policy(service, config, Arc::new(FilesystemPeerPolicy))
    }

    /// Binds a private Unix endpoint with an explicit peer credential policy.
    /// The policy runs immediately after `accept`, before the connection is
    /// counted or handed to a worker thread.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint or peer policy cannot be established.
    pub fn bind_with_peer_policy(
        service: LocaldService<O>,
        config: ListenerConfig,
        peer_policy: Arc<dyn PeerPolicy>,
    ) -> Result<Self, ListenerError> {
        config.validate()?;
        let path = config.path.clone();
        // Sweeping runs on every start, before bind: a killed owner always
        // leaves its socket behind, and a live one must be reported as
        // `AlreadyRunning` rather than have its endpoint stolen.
        prepare_socket_path(&path)?;
        let listener = backend_engine::LocalListener::bind(&path)
            .map_err(|error| ListenerError::Io(error.kind()))?;
        set_private_socket_permissions(&path)?;
        listener
            .set_nonblocking(true)
            .map_err(|error| ListenerError::Io(error.kind()))?;
        let (inbound_sender, inbound) = mpsc::sync_channel(config.max_clients);
        let stop = Arc::new(AtomicBool::new(false));
        // The wire shutdown request is a listener lifecycle operation, never a
        // workspace mutation. Handing the capability to the service here —
        // rather than from one process entry point — keeps it reachable for
        // the embedded host as well as the headless daemon.
        let mut service = service;
        service.attach_lifecycle(ListenerShutdown {
            stop: Arc::clone(&stop),
        });
        Ok(Self {
            listener,
            service,
            path,
            stop,
            active: Arc::new(AtomicUsize::new(0)),
            inflight: Arc::new(AtomicUsize::new(0)),
            config,
            workers: Vec::new(),
            inbound,
            inbound_sender,
            peer_policy,
            streams: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            next_connection_id: AtomicUsize::new(1),
            report: RunReport::default(),
            telemetry: backend_engine::Telemetry::disabled(),
        })
    }

    /// Returns the private endpoint path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Requests clean shutdown. Existing clients finish their current bounded
    /// frame and then leave; no new connections are admitted.
    pub fn shutdown(&self) {
        self.shutdown_handle().request();
    }

    /// Returns a lifecycle-only capability suitable for another host thread.
    #[must_use]
    pub fn shutdown_handle(&self) -> ListenerShutdown {
        ListenerShutdown {
            stop: Arc::clone(&self.stop),
        }
    }

    /// Returns whether shutdown has been requested.
    #[must_use]
    pub fn is_shutdown(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Returns a report of work processed so far.
    #[must_use]
    pub const fn report(&self) -> RunReport {
        self.report
    }

    /// Enables bounded process telemetry for transport requests.
    #[must_use]
    pub fn with_telemetry(mut self, telemetry: backend_engine::Telemetry) -> Self {
        self.telemetry = telemetry;
        self
    }

    /// Returns an eventually consistent telemetry snapshot.
    #[must_use]
    pub fn telemetry_snapshot(&self) -> backend_engine::TelemetrySnapshot {
        self.telemetry.snapshot()
    }

    /// Returns a mutable reference to the owner service for integration hooks.
    #[must_use]
    pub const fn service_mut(&mut self) -> &mut LocaldService<O> {
        &mut self.service
    }

    /// Runs the accept/owner loop until shutdown or all channels close.
    ///
    /// # Errors
    ///
    /// Returns an error when accepting or servicing a connection fails.
    /// A detached daemon has no parent to reap it, so this loop also enforces
    /// [`ListenerConfig::idle_timeout`]: it stops itself once nothing has been
    /// connected and no owner work has progressed for that window.
    pub fn run(&mut self) -> Result<RunReport, ListenerError> {
        let mut last_progress = Instant::now();
        while !self.is_shutdown() {
            self.accept_available()?;
            if self.drain_owner_once() || self.active.load(Ordering::Acquire) != 0 {
                last_progress = Instant::now();
                continue;
            }
            if self.idle_window_elapsed(last_progress) {
                // Nothing has been connected and no owner work has progressed
                // for the whole window. A detached daemon has no parent to
                // reap it, so this branch is the only thing between one
                // abandoned surface and a socket per workspace that lives
                // until the machine restarts. Setting the stop flag rather
                // than only breaking means a host holding a
                // `ListenerShutdown` observes the retirement instead of
                // waiting on a loop that already ended.
                self.stop.store(true, Ordering::Release);
                break;
            }
            thread::sleep(self.config.poll_interval);
        }
        self.finish_workers();
        self.service.close();
        Ok(self.report)
    }

    /// Reports whether the configured idle window has passed with no client
    /// connected and no owner progress.
    fn idle_window_elapsed(&self, last_progress: Instant) -> bool {
        self.config
            .idle_timeout
            .is_some_and(|idle| last_progress.elapsed() >= idle)
    }

    /// Runs one nonblocking listener/owner iteration. This is useful for a
    /// host process that owns its own signal handling or event loop.
    ///
    /// # Errors
    ///
    /// Returns an error when accepting or servicing a connection fails.
    pub fn run_once(&mut self) -> Result<bool, ListenerError> {
        if self.is_shutdown() {
            return Ok(false);
        }
        self.accept_available()?;
        let handled = self.drain_owner_once();
        Ok(handled || self.active.load(Ordering::Acquire) != 0)
    }

    fn accept_available(&mut self) -> Result<(), ListenerError> {
        self.report.failures = self
            .report
            .failures
            .saturating_add(reap_finished_workers(&mut self.workers));
        loop {
            match self.listener.accept() {
                Ok((stream, _address)) => {
                    if self.peer_policy.authorize(&stream).is_err() {
                        self.report.failures = self.report.failures.saturating_add(1);
                        drop(stream);
                        continue;
                    }
                    if self.active.load(Ordering::Acquire) >= self.config.max_clients {
                        drop(stream);
                        self.report.failures = self.report.failures.saturating_add(1);
                        continue;
                    }
                    if configure_stream(&stream, self.config.io_timeout).is_err() {
                        // A probe can disconnect between `accept` and socket
                        // configuration. That connection is isolated input;
                        // it must not terminate the process-wide listener.
                        self.report.failures = self.report.failures.saturating_add(1);
                        drop(stream);
                        continue;
                    }
                    self.spawn_connection_worker(stream);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(ListenerError::Io(error.kind())),
            }
        }
        Ok(())
    }

    /// Hands one authorized stream to its own bounded worker thread.
    fn spawn_connection_worker(&mut self, stream: backend_engine::LocalStream) {
        let sender = self.inbound_sender.clone();
        let stop = Arc::clone(&self.stop);
        let active = Arc::clone(&self.active);
        let inflight = Arc::clone(&self.inflight);
        let streams = Arc::clone(&self.streams);
        let limits = self.config.limits;
        let timeout = self.config.io_timeout;
        let request_idle = self.config.request_idle_timeout;
        let owner_reply = self.config.owner_reply_timeout;
        let connection_id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut active_streams) = self.streams.lock()
            && let Ok(clone) = stream.try_clone()
        {
            active_streams.insert(connection_id, clone);
        }
        self.active.fetch_add(1, Ordering::AcqRel);
        self.report.connections = self.report.connections.saturating_add(1);
        let worker = thread::spawn(move || {
            connection_worker(
                stream,
                ConnectionContext {
                    sender,
                    stop,
                    active,
                    inflight,
                    streams,
                    connection_id,
                    limits,
                    timeout,
                    request_idle,
                    owner_reply,
                },
            );
        });
        self.workers.push(worker);
    }

    fn drain_owner_once(&mut self) -> bool {
        // Keep owner progress independent of whether clients are currently
        // producing requests.
        let owner_progress = self.service.owner_mut().serve_one();
        match self.inbound.try_recv() {
            Ok(inbound) => {
                let started = Instant::now();
                let result = self.service.handle_payload(&inbound.payload);
                let failed = result.is_err();
                self.telemetry.record_with(|| backend_engine::Observation {
                    family: backend_engine::MetricFamily::Transport,
                    outcome: if failed {
                        backend_engine::MetricOutcome::Failed
                    } else {
                        backend_engine::MetricOutcome::Completed
                    },
                    latency: started.elapsed(),
                    units: u64::try_from(inbound.payload.len()).unwrap_or(u64::MAX),
                });
                let _ = inbound.reply.send(result);
                self.report.frames = self.report.frames.saturating_add(1);
                if failed {
                    self.report.failures = self.report.failures.saturating_add(1);
                }
                true
            }
            Err(TryRecvError::Empty) => owner_progress,
            Err(TryRecvError::Disconnected) => {
                self.stop.store(true, Ordering::Release);
                false
            }
        }
    }

    fn finish_workers(&mut self) {
        self.shutdown();
        // A wire shutdown request sets the stop flag while its worker is still
        // waiting to write the acknowledgement.  Drain admitted requests
        // until those workers have handed their replies to the socket; only
        // then is it safe to close listener-owned clones.  The deadline keeps
        // an externally requested shutdown bounded when an owner is wedged.
        let deadline = Instant::now() + self.config.owner_reply_timeout;
        while self.inflight.load(Ordering::Acquire) != 0 && Instant::now() < deadline {
            let _ = self.drain_owner_once();
            thread::sleep(self.config.poll_interval);
        }
        // A worker may be parked in its bounded read deadline while holding a
        // long-lived subscription connection. Closing the listener-owned
        // clones wakes those readers immediately so shutdown can join every
        // worker deterministically instead of waiting for the idle timeout.
        if let Ok(streams) = self.streams.lock() {
            for stream in streams.values() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
        // Dropping the sender wakes readers that are waiting to submit their
        // final frame. The worker-owned clone is then released on exit.
        while let Some(worker) = self.workers.pop() {
            let _ = worker.join();
        }
    }
}
