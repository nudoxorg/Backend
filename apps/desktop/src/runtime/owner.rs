//! The index owner as a data event, never a wait (W-Open I1, window first).
//!
//! `main` opens the window at once. The owner — embedded, or an attached one
//! that is already live — starts on its own thread (`host::owner`), and its
//! answer arrives through one [`OwnerGate`]:
//!
//! - worker threads (the read pool's sessions, the engine actor) block in
//!   [`OwnerGate::wait`] before their first connect. The UI thread never
//!   calls it;
//! - the UI observes the gate through [`watch`]. `Ready` becomes the
//!   owner's root (`Intent::OwnerReady`) and releases the reads the store
//!   held (`DataStore::owner_ready`). `Failed` becomes a fault on every page
//!   the window shows, in the owner's own words (`DataStore::owner_failed`).
//!   The page's "Try again" restarts the owner ([`OwnerGate::restart`]).

use crate::core::VersionedRoot;
use crate::model::ServiceMode;
use gpui::{App, Entity};
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

/// How long an owner may take to answer before the window says it did not.
/// An embedded start takes seconds; this bounds a host that hangs, so a page
/// never waits on it for ever and "Try again" has something to retry.
pub const PATIENCE: Duration = Duration::from_mins(1);

/// Why the owner is not answering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OwnerFault {
    /// The host said it could not start, in its own words.
    Host(Arc<str>),
    /// The owner's thread panicked, and said so.
    Panicked(Arc<str>),
    /// Nothing answered within the patience.
    Silent(Duration),
    /// The window closed before the owner answered.
    Closed,
}

impl fmt::Display for OwnerFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host(words) => formatter.write_str(words),
            Self::Panicked(what) => write!(formatter, "the index's thread panicked: {what}"),
            Self::Silent(waited) => write!(formatter, "the index did not answer within {} s", waited.as_secs()),
            Self::Closed => formatter.write_str("the window closed before the index answered"),
        }
    }
}

impl From<&str> for OwnerFault {
    fn from(words: &str) -> Self {
        Self::Host(Arc::from(words))
    }
}

impl From<String> for OwnerFault {
    fn from(words: String) -> Self {
        Self::Host(Arc::from(words))
    }
}

/// What the window knows about its owner.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::large_enum_variant, reason = "a handful are published per process")]
pub enum OwnerState {
    /// Starting, or attaching: nothing has answered yet.
    Starting,
    /// Answering: the root it answered at, and how this window reached it.
    Ready {
        /// The owner's root when it first answered.
        key: VersionedRoot,
        /// Embedded in this process, or attached to a live one.
        mode: ServiceMode,
    },
    /// Could not start, or stopped answering.
    Failed(OwnerFault),
}

/// The one place the owner's state lives; cheap to clone and `Send`.
#[derive(Clone)]
pub struct OwnerGate(Arc<Shared>);

impl fmt::Debug for OwnerGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OwnerGate").field(&self.state()).finish()
    }
}

struct Shared {
    inner: Mutex<Inner>,
    changed: Condvar,
}

struct Inner {
    state: OwnerState,
    /// Bumps on every publish; the UI watcher remembers the last it saw.
    epoch: u64,
    /// The UI watcher, parked until the next publish.
    waker: Option<Waker>,
    /// A restart was asked for (the page's "Try again" after a failure).
    restart: bool,
    /// When the state last changed: how long a start has been waited for.
    since: Instant,
    /// The app is quitting: the owner thread lets its host go.
    closed: bool,
}

impl OwnerGate {
    /// A gate whose owner has not answered yet.
    #[must_use]
    pub fn starting() -> Self {
        Self::with(OwnerState::Starting)
    }

    /// A gate whose owner already answered (tests; the harness's fixture
    /// owner is ready before its window mounts).
    #[must_use]
    pub fn ready(key: VersionedRoot, mode: ServiceMode) -> Self {
        Self::with(OwnerState::Ready { key, mode })
    }

    fn with(state: OwnerState) -> Self {
        Self(Arc::new(Shared {
            inner: Mutex::new(Inner {
                state,
                epoch: 0,
                waker: None,
                restart: false,
                since: Instant::now(),
                closed: false,
            }),
            changed: Condvar::new(),
        }))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The owner's state now.
    #[must_use]
    pub fn state(&self) -> OwnerState {
        self.lock().state.clone()
    }

    /// Publishes a new state: wakes every waiting worker and the UI.
    pub fn publish(&self, state: OwnerState) {
        let waker = {
            let mut inner = self.lock();
            match &state {
                OwnerState::Ready { .. } => crate::runtime::trace::mark("owner.ready", "gate"),
                OwnerState::Failed(fault) => crate::runtime::trace::mark("owner.failed", fault),
                OwnerState::Starting => crate::runtime::trace::mark("owner.starting", "gate"),
            }
            inner.state = state;
            inner.since = Instant::now();
            inner.epoch = inner.epoch.wrapping_add(1);
            inner.waker.take()
        };
        self.0.changed.notify_all();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Blocks the calling **worker** thread until the owner answered.
    ///
    /// # Errors
    /// The owner's failure, in its own words; also when the app quits
    /// before it answered.
    pub fn wait(&self) -> Result<(), OwnerFault> {
        self.wait_for(PATIENCE)
    }

    /// [`Self::wait`] with its own patience: gives up with
    /// [`OwnerFault::Silent`] once the owner has been starting for `patience`.
    ///
    /// # Errors
    /// The owner's fault; the window closing; or the patience running out.
    pub fn wait_for(&self, patience: Duration) -> Result<(), OwnerFault> {
        let mut inner = self.lock();
        loop {
            match &inner.state {
                OwnerState::Ready { .. } => return Ok(()),
                OwnerState::Failed(fault) => return Err(fault.clone()),
                OwnerState::Starting if inner.closed => return Err(OwnerFault::Closed),
                OwnerState::Starting => {
                    let left = patience.saturating_sub(inner.since.elapsed());
                    if left.is_zero() {
                        return Err(OwnerFault::Silent(patience));
                    }
                    inner = self
                        .0
                        .changed
                        .wait_timeout(inner, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
            }
        }
    }

    /// Asks a failed owner to try again. Returns whether it was asked: a
    /// starting or answering owner is left alone.
    #[must_use]
    pub fn restart(&self) -> bool {
        {
            let mut inner = self.lock();
            if !matches!(inner.state, OwnerState::Failed(_)) {
                return false;
            }
            inner.restart = true;
        }
        self.publish(OwnerState::Starting);
        true
    }

    /// The owner thread, after a failure: blocks until a restart is asked
    /// for (`true`) or the app quits (`false`).
    pub(crate) fn await_restart(&self) -> bool {
        let mut inner = self.lock();
        loop {
            if inner.closed {
                return false;
            }
            if std::mem::take(&mut inner.restart) {
                return true;
            }
            inner = self
                .0
                .changed
                .wait(inner)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// The owner thread, once serving: blocks until the app quits.
    pub(crate) fn await_close(&self) {
        let mut inner = self.lock();
        while !inner.closed {
            inner = self
                .0
                .changed
                .wait(inner)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// The app is quitting: releases the owner thread and every waiter.
    pub fn close(&self) {
        self.lock().closed = true;
        self.0.changed.notify_all();
    }

    /// Resolves with the first state published after epoch `seen` (at once
    /// when one already was), and that state's epoch.
    pub fn next(&self, seen: u64) -> impl Future<Output = (u64, OwnerState)> + 'static {
        let gate = self.clone();
        std::future::poll_fn(move |task| {
            let mut inner = gate.lock();
            if inner.epoch > seen {
                Poll::Ready((inner.epoch, inner.state.clone()))
            } else {
                inner.waker = Some(task.waker().clone());
                Poll::Pending
            }
        })
    }
}

impl OwnerGate {
    /// The current state's epoch and whether it is a start still being
    /// waited for.
    fn starting_at(&self) -> Option<u64> {
        let inner = self.lock();
        matches!(inner.state, OwnerState::Starting).then_some(inner.epoch)
    }

    /// Publishes [`OwnerFault::Silent`] if the start that began at `epoch`
    /// is still the state (a later publish is another start, or an answer).
    fn give_up_on(&self, epoch: u64, patience: Duration) {
        if self.starting_at() == Some(epoch) {
            self.publish(OwnerState::Failed(OwnerFault::Silent(patience)));
        }
    }
}

/// After `patience`, a start nobody answered is a fault on the gate itself:
/// the store shows it and "Try again" restarts it.
fn watch_patience(gate: &OwnerGate, patience: Duration, cx: &mut App) {
    let Some(epoch) = gate.starting_at() else { return };
    let gate = gate.clone();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(patience).await;
        gate.give_up_on(epoch, patience);
    })
    .detach();
}

/// Turns every published owner state into the window's data events, on the
/// UI thread, for as long as the app runs.
pub fn watch(gate: OwnerGate, root: Entity<super::UiRootEntity>, store: Entity<super::store::DataStore>, cx: &mut App) {
    watch_patience(&gate, PATIENCE, cx);
    cx.spawn(async move |cx| {
        let mut seen = 0;
        loop {
            let (epoch, state) = gate.next(seen).await;
            seen = epoch;
            // The task ends with the app (a spawned task is dropped with it).
            cx.update(|cx| match state {
                OwnerState::Starting => {
                    // A restart is a new wait: its patience starts now.
                    watch_patience(&gate, PATIENCE, cx);
                    store.update(cx, super::store::DataStore::owner_starting);
                }
                OwnerState::Ready { key, mode } => {
                    root.update(cx, |root, cx| root.admit_owner(key, mode, cx));
                    store.update(cx, super::store::DataStore::owner_ready);
                }
                OwnerState::Failed(fault) => {
                    store.update(cx, |store, cx| store.owner_failed(&fault, cx));
                }
            });
        }
    })
    .detach();
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn a_worker_waits_for_the_owner_and_learns_its_answer() {
        let gate = OwnerGate::starting();
        let (sent, received) = mpsc::channel();
        let worker = gate.clone();
        let thread = std::thread::spawn(move || sent.send(worker.wait()).expect("send"));
        assert!(
            received.recv_timeout(Duration::from_millis(80)).is_err(),
            "a worker must not pass the gate while the owner is starting"
        );
        gate.publish(OwnerState::Failed("could not own the workspace".into()));
        assert_eq!(
            received.recv_timeout(Duration::from_secs(2)).expect("released"),
            Err(OwnerFault::from("could not own the workspace"))
        );
        thread.join().expect("join");
        assert!(gate.restart(), "a failed owner can be asked again");
        assert_eq!(gate.state(), OwnerState::Starting);
        assert!(gate.await_restart(), "the owner thread sees the restart");
        assert!(!gate.restart(), "a starting owner is left alone");
        gate.publish(OwnerState::Ready {
            key: VersionedRoot::unserved(),
            mode: ServiceMode::Attached,
        });
        assert_eq!(gate.wait(), Ok(()));
    }

    #[test]
    fn closing_releases_the_owner_thread_and_every_waiter() {
        let gate = OwnerGate::starting();
        let owner = gate.clone();
        let thread = std::thread::spawn(move || owner.await_close());
        gate.close();
        thread.join().expect("the owner thread lets go");
        assert!(gate.wait().is_err(), "nobody waits on an owner after quit");
        assert!(!gate.await_restart());
    }

    #[test]
    fn the_ui_future_resolves_on_each_new_state_only() {
        let gate = OwnerGate::starting();
        let waker = Waker::noop();
        let mut task = std::task::Context::from_waker(waker);
        let mut first = Box::pin(gate.next(0));
        assert!(first.as_mut().poll(&mut task).is_pending(), "nothing was published yet");
        gate.publish(OwnerState::Failed("no".into()));
        let Poll::Ready((epoch, state)) = first.as_mut().poll(&mut task) else {
            panic!("a publish resolves the watcher");
        };
        assert_eq!(state, OwnerState::Failed("no".into()));
        let mut second = Box::pin(gate.next(epoch));
        assert!(second.as_mut().poll(&mut task).is_pending(), "a seen state never resolves twice");
    }

    #[test]
    fn an_owner_that_never_answers_is_a_typed_fault_after_the_patience_not_a_wait_for_ever() {
        let gate = OwnerGate::starting();
        let started = Instant::now();
        assert_eq!(gate.wait_for(Duration::from_millis(60)), Err(OwnerFault::Silent(Duration::from_millis(60))));
        assert!(started.elapsed() >= Duration::from_millis(60), "it waited the patience, no less");
        assert_eq!(
            OwnerFault::Silent(Duration::from_mins(1)).to_string(),
            "the index did not answer within 60 s",
            "and the window says it in words"
        );
        // A start that answers inside the patience is not a fault.
        let answering = OwnerGate::starting();
        let owner = answering.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            owner.publish(OwnerState::Ready { key: VersionedRoot::unserved(), mode: ServiceMode::Attached });
        });
        assert_eq!(answering.wait_for(Duration::from_secs(5)), Ok(()));
        thread.join().expect("join");
    }

    #[test]
    fn giving_up_names_only_the_start_it_was_waiting_for() {
        let gate = OwnerGate::starting();
        let epoch = gate.starting_at().expect("starting");
        gate.publish(OwnerState::Ready { key: VersionedRoot::unserved(), mode: ServiceMode::Attached });
        gate.give_up_on(epoch, PATIENCE);
        assert!(matches!(gate.state(), OwnerState::Ready { .. }), "an owner that answered is never given up on");
        gate.publish(OwnerState::Failed("gone".into()));
        assert!(gate.restart());
        gate.give_up_on(epoch, PATIENCE);
        assert_eq!(gate.state(), OwnerState::Starting, "the old wait does not fail the new start");
        let now = gate.starting_at().expect("starting again");
        gate.give_up_on(now, PATIENCE);
        assert_eq!(gate.state(), OwnerState::Failed(OwnerFault::Silent(PATIENCE)));
    }
}
