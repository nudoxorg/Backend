//! Caller-owned cancellation; it never controls an owner preparation worker.

use crate::ClientError;
#[cfg(any(unix, windows))]
use crate::TransportInterrupt;
use std::fmt;

/// Why this exact request stopped waiting. Transport loss is a separate error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum RequestCancellationReason {
    /// The client explicitly cancelled the JSON-RPC request.
    Explicit = 1,
    /// The client abandoned its response or closed the transport.
    ClientDisconnected = 2,
    /// The transport or session is shutting down.
    Shutdown = 3,
}

impl fmt::Display for RequestCancellationReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Explicit => "request explicitly cancelled by client",
            Self::ClientDisconnected => "request client disconnected",
            Self::Shutdown => "request transport shut down",
        })
    }
}

#[cfg(any(unix, windows))]
mod native {
    use super::*;
    use std::sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicU8, Ordering},
    };

    const COMPLETED: u8 = 4;

    /// One request lifetime, including connection establishment and all probes.
    /// A retained socket is detached atomically at completion, before id reuse.
    #[derive(Clone)]
    pub struct RequestContext(Arc<State>);

    struct State {
        status: AtomicU8,
        interrupt: Mutex<Option<TransportInterrupt>>,
        changed: tokio::sync::Notify,
        started: std::time::Instant,
        read_deadline: OnceLock<ReadScope>,
        progress: Mutex<Option<std::sync::mpsc::SyncSender<crate::QueryPreparationProgress>>>,
    }

    struct ReadScope {
        initial: std::time::Instant,
        // Explicit scopes remain absolute. Default search scopes can adopt
        // one authenticated owner deadline and can subsequently only shorten it.
        policy: ReadPolicy,
    }
    enum ReadPolicy {
        Absolute,
        DefaultSearch(Mutex<Option<std::time::Instant>>),
    }
    impl ReadScope {
        fn deadline(&self) -> std::time::Instant {
            match &self.policy {
                ReadPolicy::Absolute => self.initial,
                ReadPolicy::DefaultSearch(owner) => owner
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .unwrap_or(self.initial),
            }
        }
    }

    impl Default for RequestContext {
        fn default() -> Self {
            Self::new()
        }
    }

    impl RequestContext {
        /// Creates an active request before any owner operation is admitted.
        #[must_use]
        pub fn new() -> Self {
            Self(Arc::new(State {
                status: AtomicU8::new(0),
                interrupt: Mutex::new(None),
                changed: tokio::sync::Notify::new(),
                started: std::time::Instant::now(),
                read_deadline: OnceLock::new(),
                progress: Mutex::new(None),
            }))
        }

        /// Cancels once, preserving the first reason and the exact bound lease.
        /// Calls after completion cannot interrupt a later request's socket.
        pub fn cancel(&self, reason: RequestCancellationReason) {
            let interrupt = self
                .0
                .interrupt
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if self
                .0
                .status
                .compare_exchange(0, reason as u8, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                if let Some(interrupt) = &*interrupt {
                    interrupt.interrupt();
                }
                self.0.changed.notify_waiters();
            }
        }

        /// Returns the original cancellation reason, independent of socket errors.
        #[must_use]
        pub fn cancellation_reason(&self) -> Option<RequestCancellationReason> {
            match self.0.status.load(Ordering::Acquire) {
                1 => Some(RequestCancellationReason::Explicit),
                2 => Some(RequestCancellationReason::ClientDisconnected),
                3 => Some(RequestCancellationReason::Shutdown),
                _ => None,
            }
        }

        /// Checks cancellation at a request admission or wait boundary.
        /// # Errors
        /// Returns the caller's original cancellation reason.
        pub fn check(&self) -> Result<(), ClientError> {
            if let Some(reason) = self.cancellation_reason() {
                return Err(ClientError::RequestCancelled(reason));
            }
            if self
                .0
                .read_deadline
                .get()
                .is_some_and(|scope| std::time::Instant::now() >= scope.deadline())
            {
                return Err(ClientError::RemoteDeadlineExceeded);
            }
            Ok(())
        }

        /// Starts the read-only deadline once, before owner connection/selection.
        /// Every replacement Session shares this same absolute request deadline.
        /// # Errors
        /// Returns the caller cancellation or exhausted original read budget.
        pub fn read_deadline(
            &self,
            budget: std::time::Duration,
        ) -> Result<std::time::Instant, ClientError> {
            let deadline = self
                .0
                .read_deadline
                .get_or_init(|| ReadScope {
                    initial: self.0.started + budget,
                    policy: ReadPolicy::Absolute,
                })
                .deadline();
            self.check()?;
            Ok(deadline)
        }

        /// Starts the default search's initial admission budget. Only an exact
        /// authenticated local deferred ACK can change its reply deadline once.
        /// A caller's previously installed explicit read_deadline remains absolute.
        /// # Errors
        /// Returns cancellation or exhaustion of the original admission budget.
        pub fn begin_default_search(&self, budget: std::time::Duration) -> Result<(), ClientError> {
            self.0.read_deadline.get_or_init(|| ReadScope {
                initial: self.0.started + budget,
                policy: ReadPolicy::DefaultSearch(Mutex::new(None)),
            });
            self.check()
        }

        // Called only after transport proof admission of the exact wrapped request.
        // The budget is rooted at the original exchange start, never at ACK receipt.
        pub(crate) fn admit_deferred_search(
            &self,
            started: std::time::Instant,
            budget: std::time::Duration,
        ) -> Result<Option<std::time::Instant>, ClientError> {
            self.check()?;
            let Some(scope) = self.0.read_deadline.get() else {
                return Ok(None);
            };
            let ReadPolicy::DefaultSearch(owner) = &scope.policy else {
                return Ok(None);
            };
            let mut admitted = owner.lock().unwrap_or_else(PoisonError::into_inner);
            let proposed = (started + budget.min(backend_replication::MAX_DEFERRED_COMMAND_WAIT))
                .min(self.0.started + backend_replication::MAX_DEFERRED_COMMAND_WAIT);
            let deadline = admitted.get_or_insert(proposed);
            // A later exchange can shorten the shared scope, never renew admission.
            *deadline = (*deadline).min(proposed);
            Ok(Some(*deadline))
        }

        /// Returns the existing read scope's remaining exchange budget.
        /// Mutations and other callers do not acquire a read deadline here.
        /// # Errors
        /// Returns cancellation or expiration of an already activated scope.
        pub fn remaining_read_budget(&self) -> Result<Option<std::time::Duration>, ClientError> {
            self.check()?;
            Ok(self.0.read_deadline.get().map(|scope| {
                scope
                    .deadline()
                    .saturating_duration_since(std::time::Instant::now())
            }))
        }

        /// Installs a bounded progress sink; sending never waits for stderr.
        pub fn set_preparation_observer(
            &self,
            sender: std::sync::mpsc::SyncSender<crate::QueryPreparationProgress>,
        ) {
            *self
                .0
                .progress
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(sender);
        }

        pub(crate) fn preparation_progress(&self, progress: crate::QueryPreparationProgress) {
            if let Some(sender) = &*self
                .0
                .progress
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
            {
                let _ = sender.try_send(progress);
            }
        }

        /// Binds the exact current transport, including a newly opened lease.
        /// # Errors
        /// A previously cancelled request closes this lease and fails admission.
        pub fn bind_transport(
            &self,
            handle: Option<TransportInterrupt>,
        ) -> Result<(), ClientError> {
            let mut interrupt = self
                .0
                .interrupt
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(reason) = self.cancellation_reason() {
                if let Some(handle) = handle {
                    handle.interrupt();
                }
                return Err(ClientError::RequestCancelled(reason));
            }
            self.check()?;
            if self.0.status.load(Ordering::Acquire) == COMPLETED {
                return Err(ClientError::Protocol(
                    "completed request cannot bind a transport".to_owned(),
                ));
            }
            *interrupt = handle;
            Ok(())
        }

        /// Retires this request before its JSON-RPC id may be registered again.
        pub fn finish(&self) {
            let mut interrupt = self
                .0
                .interrupt
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *interrupt = None;
            // Preserve cancellation for response classification after completion.
            let _ =
                self.0
                    .status
                    .compare_exchange(0, COMPLETED, Ordering::AcqRel, Ordering::Acquire);
        }

        /// Waits without polling while a remote dial or exchange is in flight.
        pub async fn cancelled(&self) -> RequestCancellationReason {
            loop {
                let notified = self.0.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if let Some(reason) = self.cancellation_reason() {
                    return reason;
                }
                notified.await;
            }
        }

        /// Returns whether two handles name the same lifetime, not merely the same id.
        #[must_use]
        pub fn same_request(&self, other: &Self) -> bool {
            Arc::ptr_eq(&self.0, &other.0)
        }
    }
}

#[cfg(any(unix, windows))]
pub use native::RequestContext;

#[cfg(all(test, unix))]
#[expect(
    clippy::expect_used,
    reason = "cancellation laws require real owned socket pairs"
)]
mod tests {
    use super::*;
    use crate::{Session, UnixCommandTransport};
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    #[test]
    fn cancellation_before_connect_and_during_exchange_has_exact_reason() {
        let before = RequestContext::new();
        before.cancel(RequestCancellationReason::Explicit);
        assert!(matches!(
            Session::connect_with_context("/not-an-endpoint", before),
            Err(ClientError::RequestCancelled(
                RequestCancellationReason::Explicit
            ))
        ));
        let (stream, mut peer) = backend_replication::LocalStream::pair().expect("owned socket");
        peer.set_read_timeout(Some(Duration::from_secs(2)))
            .expect("fixture peer bound");
        let context = RequestContext::new();
        let mut session = Session::from_transport(
            "/owned/request.sock",
            UnixCommandTransport::from_stream(stream),
        );
        session
            .set_request_context(Some(context.clone()))
            .expect("bind exact request socket");
        let (admitted, admission) = std::sync::mpsc::channel();
        let owner = std::thread::spawn(move || {
            let _ = backend_replication::read_frame(
                &mut peer,
                backend_replication::LocalControlLimits::default(),
            )
            .expect("owner received request");
            admitted.send(()).expect("report owner admission");
            let mut byte = [0];
            peer.read(&mut byte)
                .expect("client shutdown releases owner socket")
        });
        let waiting = std::thread::spawn(move || session.packages());
        admission
            .recv_timeout(Duration::from_secs(1))
            .expect("request actually in flight");
        let started = Instant::now();
        context.cancel(RequestCancellationReason::Explicit);
        assert!(matches!(
            waiting.join().expect("request completed"),
            Err(ClientError::RequestCancelled(
                RequestCancellationReason::Explicit
            ))
        ));
        assert_eq!(owner.join().expect("owner released"), 0);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn completion_retires_socket_and_late_bind_observes_prior_cancellation() {
        let (mut stream, mut peer) =
            backend_replication::LocalStream::pair().expect("completion socket");
        peer.set_read_timeout(Some(Duration::from_secs(2)))
            .expect("fixture peer bound");
        let completed = RequestContext::new();
        completed
            .bind_transport(Some(
                TransportInterrupt::new(&stream).expect("socket interrupt"),
            ))
            .expect("bind");
        completed.finish();
        completed.cancel(RequestCancellationReason::Explicit);
        stream
            .write_all(b"x")
            .expect("completion cannot shut down reused connection");
        let mut byte = [0];
        peer.read_exact(&mut byte).expect("connection remains live");
        assert_eq!(byte, [b'x']);
        let cancelled = RequestContext::new();
        cancelled.cancel(RequestCancellationReason::ClientDisconnected);
        assert!(matches!(
            cancelled.bind_transport(Some(
                TransportInterrupt::new(&stream).expect("new interrupt")
            )),
            Err(ClientError::RequestCancelled(
                RequestCancellationReason::ClientDisconnected
            ))
        ));
        assert_eq!(peer.read(&mut byte).expect("late socket closed"), 0);
    }

    #[test]
    fn one_read_deadline_survives_replacement_and_progress_backpressure() {
        let context = RequestContext::new();
        let first = context
            .read_deadline(Duration::from_millis(80))
            .expect("first deadline");
        assert_eq!(
            context
                .read_deadline(Duration::from_secs(30))
                .expect("replacement deadline"),
            first
        );
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        context.set_preparation_observer(sender);
        let progress = crate::QueryPreparationProgress {
            basis: backend_library::view_state_root(&[]).into(),
            state: backend_library::QueryPreparationState::Preparing,
            elapsed: Duration::ZERO,
            remaining: Duration::from_millis(80),
        };
        let started = Instant::now();
        for _ in 0..100 {
            context.preparation_progress(progress);
        }
        assert!(
            started.elapsed() < Duration::from_millis(40),
            "full progress channel never waits"
        );
    }

    #[test]
    fn default_search_admission_is_once_and_does_not_change_explicit_or_sibling_reads() {
        let scope = RequestContext::new();
        scope
            .begin_default_search(Duration::from_millis(60))
            .expect("initial default search");
        let started = Instant::now();
        let admitted = scope
            .admit_deferred_search(started, Duration::from_secs(1))
            .expect("exact admitted ACK")
            .expect("default scope");
        assert_eq!(
            scope
                .read_deadline(Duration::from_secs(30))
                .expect("shared request deadline"),
            admitted
        );
        assert_eq!(
            scope
                .admit_deferred_search(Instant::now(), Duration::from_secs(2))
                .expect("later exchange")
                .expect("same admission"),
            admitted
        );
        let shorter = scope
            .admit_deferred_search(started, Duration::from_millis(100))
            .expect("shorter exchange")
            .expect("bounded");
        assert!(shorter < admitted);
        let sibling = RequestContext::new();
        let absolute = sibling
            .read_deadline(Duration::from_secs(30))
            .expect("health/other read");
        assert!(
            sibling
                .admit_deferred_search(started, Duration::from_secs(60))
                .expect("absolute policy")
                .is_none()
        );
        assert_eq!(
            sibling
                .read_deadline(Duration::from_secs(60))
                .expect("unchanged sibling"),
            absolute
        );
        let explicit = RequestContext::new();
        let original = explicit
            .read_deadline(Duration::from_millis(60))
            .expect("caller budget");
        explicit
            .begin_default_search(Duration::from_secs(30))
            .expect("explicit first policy wins");
        assert!(
            explicit
                .admit_deferred_search(started, Duration::from_secs(1))
                .expect("no adoption")
                .is_none()
        );
        assert_eq!(
            explicit
                .read_deadline(Duration::from_secs(30))
                .expect("original absolute"),
            original
        );
        scope.cancel(RequestCancellationReason::Explicit);
        assert!(matches!(
            scope.check(),
            Err(ClientError::RequestCancelled(
                RequestCancellationReason::Explicit
            ))
        ));
    }

    #[tokio::test]
    async fn cancellation_wakes_remote_select_even_when_cancel_precedes_wait() {
        let context = RequestContext::new();
        context.cancel(RequestCancellationReason::Explicit);
        assert_eq!(
            context.cancelled().await,
            RequestCancellationReason::Explicit
        );
        let live = RequestContext::new();
        let waiter = live.clone();
        let task = tokio::spawn(async move { waiter.cancelled().await });
        tokio::task::yield_now().await;
        live.cancel(RequestCancellationReason::Shutdown);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .expect("woken without polling")
                .expect("join"),
            RequestCancellationReason::Shutdown
        );
    }
}
