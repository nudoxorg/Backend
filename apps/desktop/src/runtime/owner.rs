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
    /// An attached owner that had answered stopped answering a fresh probe.
    Lost(Arc<str>),
    /// The publication observer could not renew its producer lease.
    Observation(Arc<str>),
    /// Nothing answered within the patience.
    Silent(Duration),
    /// The window closed before the owner answered.
    Closed,
    /// The request was revoked while the owner was starting.
    Cancelled,
}

impl fmt::Display for OwnerFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host(words) => formatter.write_str(words),
            Self::Panicked(what) => write!(formatter, "the index's thread panicked: {what}"),
            Self::Lost(what) => write!(formatter, "the attached index stopped answering: {what}"),
            Self::Observation(what) => write!(
                formatter,
                "the index publication subscription is unavailable: {what}"
            ),
            Self::Silent(waited) => write!(formatter, "the index did not answer within {} s", waited.as_secs()),
            Self::Closed => formatter.write_str("the window closed before the index answered"),
            Self::Cancelled => formatter.write_str("the request was cancelled while the index was starting"),
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

/// Which publish a state was. The gate counts them, so a watcher can say
/// "after the last one I saw" and a patience can say "the start that began
/// at this one".
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Epoch(u64);

impl Epoch {
    const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
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

/// Terminal admission of a shared, complete worker root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PublicationAdmission {
    Admitted,
    Obsolete,
    Withdrawn,
    Invalid,
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
    epoch: Epoch,
    /// Stable attachment identity; root publications only bump `epoch`.
    attachment: Epoch,
    /// One latest complete admitted worker root, shared with the actor.
    publication: Option<(Arc<backend_library::ViewRoot>, backend_library::Cursor)>,
    observation_cancel: super::actor::CancellationToken,
    observation_suspended: bool,
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
                epoch: Epoch::default(),
                attachment: Epoch::default(),
                publication: None,
                observation_cancel: super::actor::CancellationToken::new(),
                observation_suspended: false,
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

    /// Generation of the attached owner currently answering. A page worker
    /// must retain this before its request so an old failure cannot fail a
    /// newly attached owner with the same root.
    #[must_use]
    pub(crate) fn attached_ready_epoch(&self) -> Option<Epoch> {
        let inner = self.lock();
        (!inner.observation_suspended && matches!(inner.state, OwnerState::Ready { mode: ServiceMode::Attached, .. })).then_some(inner.attachment)
    }

    /// Reports confirmed endpoint loss only for the attached generation that
    /// issued the failed read. This is atomic with the epoch/state check.
    pub(crate) fn attached_lost_at(&self, expected: Epoch, reason: Arc<str>) -> bool {
        let waker = {
            let mut inner = self.lock();
            if inner.closed || inner.attachment != expected
                || !matches!(inner.state, OwnerState::Ready { mode: ServiceMode::Attached, .. })
            {
                return false;
            }
            inner.state = OwnerState::Failed(OwnerFault::Lost(Arc::clone(&reason)));
            inner.since = Instant::now();
            inner.epoch = inner.epoch.next();
            inner.attachment = inner.epoch;
            inner.publication = None;
            inner.observation_cancel.cancel();
            inner.waker.take()
        };
        self.0.changed.notify_all();
        if let Some(waker) = waker { waker.wake(); }
        crate::runtime::trace::mark("owner.failed", OwnerFault::Lost(reason));
        true
    }

    /// Publishes a new state: wakes every waiting worker and the UI.
    pub fn publish(&self, state: OwnerState) {
        let waker = {
            let mut inner = self.lock();
            if inner.closed {
                return;
            }
            match &state {
                OwnerState::Ready { .. } => crate::runtime::trace::mark("owner.ready", "gate"),
                OwnerState::Failed(fault) => crate::runtime::trace::mark("owner.failed", fault),
                OwnerState::Starting => crate::runtime::trace::mark("owner.starting", "gate"),
            }
            inner.observation_cancel.cancel();
            inner.observation_cancel = super::actor::CancellationToken::new();
            inner.publication = None;
            inner.observation_suspended = false;
            inner.state = state;
            inner.since = Instant::now();
            inner.epoch = inner.epoch.next();
            inner.attachment = inner.epoch;
            inner.waker.take()
        };
        self.0.changed.notify_all();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn event_is_current(&self, event: Epoch) -> bool {
        self.lock().epoch == event
    }

    /// Admits a UI wake only while that exact publication is still current.
    fn ready_at(&self, publication: Epoch, key: VersionedRoot) -> Option<Epoch> {
        let inner = self.lock();
        if inner.closed || inner.observation_suspended || inner.epoch != publication {
            return None;
        }
        matches!(inner.state, OwnerState::Ready { key: current, .. } if current.same_authority(key))
            .then_some(inner.attachment)
    }

    /// Current serving attachment, independent of publication wakes.
    pub(crate) fn ready_epoch(&self) -> Option<Epoch> {
        let inner = self.lock();
        (!inner.closed
            && !inner.observation_suspended
            && matches!(inner.state, OwnerState::Ready { .. }))
        .then_some(inner.attachment)
    }

    pub(crate) fn observation_scope(
        &self,
        expected: Epoch,
    ) -> Option<super::actor::CancellationToken> {
        let inner = self.lock();
        (!inner.closed
            && inner.attachment == expected
            && matches!(inner.state, OwnerState::Ready { .. }))
        .then(|| inner.observation_cancel.clone())
    }

    /// A complete root shared by the actor and observer. No UI serialization.
    pub(crate) fn publication(
        &self,
        expected: Epoch,
    ) -> Option<(Arc<backend_library::ViewRoot>, backend_library::Cursor)> {
        let inner = self.lock();
        if inner.closed
            || inner.attachment != expected
            || !matches!(inner.state, OwnerState::Ready { .. })
        {
            return None;
        }
        inner
            .publication
            .as_ref()
            .map(|(root, cursor)| (Arc::clone(root), *cursor))
    }

    /// Publishes only admitted producer state for the exact serving attachment.
    /// One slot coalesces updates; repeated authority keeps resources healthy.
    pub(crate) fn publish_view(
        &self,
        expected: Epoch,
        view: Arc<backend_library::ViewRoot>,
        cursor: backend_library::Cursor,
    ) -> PublicationAdmission {
        let waker = {
            let mut inner = self.lock();
            if inner.closed || inner.attachment != expected {
                return PublicationAdmission::Withdrawn;
            }
            if view.root() != cursor.root() || !view.is_coherent() || view.capability().is_none() {
                return PublicationAdmission::Invalid;
            }
            let OwnerState::Ready { key, mode } = inner.state else {
                return PublicationAdmission::Withdrawn;
            };
            let next =
                VersionedRoot::from_revision(key.producer_epoch(), cursor, key.observation());
            // Only the admitted same-stream lease may advance this attachment.
            if cursor.recipe() != key.revision().recipe()
                || cursor.branch() != key.revision().branch()
                || cursor.log() != key.revision().log()
                || cursor.schema() != key.revision().schema()
            {
                return PublicationAdmission::Invalid;
            }
            if cursor.sequence() < key.revision().sequence() {
                return PublicationAdmission::Obsolete;
            }
            if cursor.sequence() == key.revision().sequence() && cursor != key.revision() {
                return PublicationAdmission::Invalid;
            }
            let changed = !next.same_authority(key);
            inner.publication = Some((view, cursor));
            if !changed {
                self.0.changed.notify_all();
                return PublicationAdmission::Admitted;
            }
            inner.state = OwnerState::Ready { key: next, mode };
            if inner.observation_suspended {
                return PublicationAdmission::Admitted;
            }
            inner.epoch = inner.epoch.next();
            inner.waker.take()
        };
        self.0.changed.notify_all();
        if let Some(waker) = waker {
            waker.wake();
        }
        PublicationAdmission::Admitted
    }

    /// A rejected producer lease immediately withdraws readiness and rotates
    /// the attachment. Conservatively fence in-flight reads, preserving the exact
    /// admitted root and all healthy values already loaded at that authority.
    pub(crate) fn replace_observation(
        &self,
        expected: Epoch,
    ) -> Option<(Epoch, super::actor::CancellationToken)> {
        let (attachment, cancel, waker) = {
            let mut inner = self.lock();
            if inner.closed
                || inner.attachment != expected
                || !matches!(inner.state, OwnerState::Ready { .. })
            {
                return None;
            }
            inner.observation_cancel.cancel();
            inner.observation_cancel = super::actor::CancellationToken::new();
            inner.observation_suspended = true;
            inner.since = Instant::now();
            inner.epoch = inner.epoch.next();
            inner.attachment = inner.epoch;
            (
                inner.attachment,
                inner.observation_cancel.clone(),
                inner.waker.take(),
            )
        };
        self.0.changed.notify_all();
        if let Some(waker) = waker {
            waker.wake();
        }
        Some((attachment, cancel))
    }

    /// Fresh complete admission reopens the suspended serving attachment.
    pub(crate) fn complete_observation(&self, expected: Epoch) -> bool {
        let waker = {
            let mut inner = self.lock();
            if inner.closed
                || inner.attachment != expected
                || !inner.observation_suspended
                || inner.publication.is_none()
                || !matches!(inner.state, OwnerState::Ready { .. })
            {
                return false;
            }
            inner.observation_suspended = false;
            inner.epoch = inner.epoch.next();
            inner.waker.take()
        };
        self.0.changed.notify_all();
        if let Some(waker) = waker {
            waker.wake();
        }
        true
    }

    /// Worker-only timed wait, interruptible by closure or attachment change.
    pub(crate) fn observation_pause(&self, expected: Epoch, duration: Duration) -> bool {
        let inner = self.lock();
        if inner.closed || inner.attachment != expected || inner.observation_cancel.is_cancelled() {
            return false;
        }
        let (inner, _) = self
            .0
            .changed
            .wait_timeout(inner, duration)
            .unwrap_or_else(PoisonError::into_inner);
        !inner.closed && inner.attachment == expected && !inner.observation_cancel.is_cancelled()
    }

    pub(crate) fn observation_failed(&self, expected: Epoch, reason: Arc<str>) -> bool {
        // The publication worker is the only writer until this attachment is
        // withdrawn. Use one lock rather than a check followed by `publish`.
        let waker = {
            let mut inner = self.lock();
            if inner.closed
                || inner.attachment != expected
                || !matches!(inner.state, OwnerState::Ready { .. })
            {
                return false;
            }
            inner.state = OwnerState::Failed(OwnerFault::Observation(reason));
            inner.observation_cancel.cancel();
            inner.publication = None;
            inner.epoch = inner.epoch.next();
            inner.attachment = inner.epoch;
            inner.waker.take()
        };
        self.0.changed.notify_all();
        if let Some(waker) = waker {
            waker.wake();
        }
        true
    }

    /// Blocks the calling **worker** thread until the owner answered.
    ///
    /// # Errors
    /// The owner's failure, in its own words; also when the app quits
    /// before it answered.
    pub fn wait(&self) -> Result<(), OwnerFault> {
        self.wait_while(PATIENCE, None)
    }

    /// Waits on a worker for the owner, waking immediately when this exact
    /// request is cancelled. The callback locks the gate before signalling
    /// its condition variable, so cancellation cannot fall between the
    /// predicate check and the wait.
    pub(crate) fn wait_cancelled(&self, cancel: &super::actor::CancellationToken) -> Result<(), OwnerFault> {
        let weak = Arc::downgrade(&self.0);
        let _wake = cancel.on_cancel(move || {
            if let Some(shared) = weak.upgrade() {
                let _inner = shared.inner.lock().unwrap_or_else(PoisonError::into_inner);
                shared.changed.notify_all();
            }
        });
        self.wait_while(PATIENCE, Some(cancel))
    }

    /// [`Self::wait`] with its own patience: gives up with
    /// [`OwnerFault::Silent`] once the owner has been starting for `patience`.
    ///
    /// # Errors
    /// The owner's fault; the window closing; or the patience running out.
    pub fn wait_for(&self, patience: Duration) -> Result<(), OwnerFault> {
        self.wait_while(patience, None)
    }

    fn wait_while(&self, patience: Duration, cancel: Option<&super::actor::CancellationToken>) -> Result<(), OwnerFault> {
        let mut inner = self.lock();
        loop {
            if cancel.is_some_and(super::actor::CancellationToken::is_cancelled) {
                return Err(OwnerFault::Cancelled);
            }
            match &inner.state {
                OwnerState::Ready { .. } if !inner.observation_suspended => return Ok(()),
                OwnerState::Failed(fault) => return Err(fault.clone()),
                OwnerState::Starting if inner.closed => return Err(OwnerFault::Closed),
                OwnerState::Starting | OwnerState::Ready { .. } => {
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

    /// The owner thread, once serving: returns `true` when a failed attached
    /// owner was explicitly retried, or `false` when the app closes.
    pub(crate) fn await_close_or_restart(&self) -> bool {
        let mut inner = self.lock();
        loop {
            if inner.closed { return false; }
            if std::mem::take(&mut inner.restart) { return true; }
            inner = self
                .0
                .changed
                .wait(inner)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// The app is quitting: releases the owner thread and every waiter.
    pub fn close(&self) {
        let waker = {
            let mut inner = self.lock();
            inner.closed = true;
            inner.observation_cancel.cancel();
            inner.publication = None;
            inner.state = OwnerState::Failed(OwnerFault::Closed);
            inner.epoch = inner.epoch.next();
            inner.attachment = inner.epoch;
            inner.waker.take()
        };
        self.0.changed.notify_all();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Resolves with the first state published after epoch `seen` (at once
    /// when one already was), and that state's epoch.
    pub fn next(&self, seen: Epoch) -> impl Future<Output = (Epoch, OwnerState)> + 'static {
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
    fn starting_at(&self) -> Option<Epoch> {
        let inner = self.lock();
        matches!(inner.state, OwnerState::Starting).then_some(inner.epoch)
    }

    /// Publishes [`OwnerFault::Silent`] if the start that began at `epoch`
    /// is still the state (a later publish is another start, or an answer).
    fn give_up_on(&self, epoch: Epoch, patience: Duration) {
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
/// UI thread, for as long as the window's root and store live (D2). The task
/// holds them weakly: a window that is let go is not kept alive by its owner's
/// watch, and an app that is dropped leaks no handle.
pub(crate) fn watch(
    gate: OwnerGate,
    root: &Entity<super::UiRootEntity>,
    store: &Entity<super::store::DataStore>,
    cx: &mut App,
) {
    watch_patience(&gate, PATIENCE, cx);
    let (root, store) = (root.downgrade(), store.downgrade());
    cx.spawn(async move |cx| {
        let mut seen = Epoch::default();
        let mut serving = None;
        loop {
            let (epoch, state) = gate.next(seen).await;
            seen = epoch;
            let alive = cx.update(|cx| {
                let (Some(root), Some(store)) = (root.upgrade(), store.upgrade()) else {
                    return false;
                };
                if !gate.event_is_current(epoch) {
                    return true;
                }
                match state {
                    OwnerState::Starting => {
                        // A restart is a new wait: its patience starts now.
                        watch_patience(&gate, PATIENCE, cx);
                        store.update(cx, super::store::DataStore::owner_starting);
                    }
                    OwnerState::Ready { key, mode } => {
                        let Some(attachment) = gate.ready_at(epoch, key) else {
                            return true;
                        };
                        if serving.is_some() {
                            root.update(cx, |root, cx| root.renew_owner(key, mode, Some(attachment) != serving, cx));
                        } else {
                            root.update(cx, |root, cx| root.admit_owner(key, mode, cx));
                        }
                        if Some(attachment) != serving {
                            store.update(cx, super::store::DataStore::owner_ready);
                        }
                        serving = Some(attachment);
                    }
                    OwnerState::Failed(OwnerFault::Closed) => return false,
                    OwnerState::Failed(fault) => {
                        store.update(cx, |store, cx| store.owner_failed(&fault, cx));
                    }
                }
                true
            });
            if !alive {
                break;
            }
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
    fn cancelling_a_worker_wakes_the_owner_gate_without_waiting_for_patience() {
        let gate = OwnerGate::starting();
        let cancel = super::super::actor::CancellationToken::new();
        let worker_gate = gate.clone();
        let worker_cancel = cancel.clone();
        let (sent, received) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            sent.send(worker_gate.wait_cancelled(&worker_cancel)).expect("send wait result");
        });
        cancel.cancel();
        assert_eq!(received.recv_timeout(Duration::from_secs(1)).expect("cancellation woke worker"), Err(OwnerFault::Cancelled));
        thread.join().expect("join worker");
        assert_eq!(gate.state(), OwnerState::Starting, "one revoked request cannot fail the owner");
    }

    #[test]
    fn closing_releases_the_owner_thread_and_every_waiter() {
        let gate = OwnerGate::starting();
        let owner = gate.clone();
        let thread = std::thread::spawn(move || owner.await_close_or_restart());
        gate.close();
        assert!(!thread.join().expect("the owner thread lets go"));
        assert!(gate.wait().is_err(), "nobody waits on an owner after quit");
        assert!(!gate.await_restart());
    }

    #[test]
    fn the_ui_future_resolves_on_each_new_state_only() {
        let gate = OwnerGate::starting();
        let waker = Waker::noop();
        let mut task = std::task::Context::from_waker(waker);
        let mut first = Box::pin(gate.next(Epoch::default()));
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

#[cfg(test)]
#[allow(clippy::expect_used)]
mod publication_tests {
    use super::*;
    use backend_library::{
        AuthorityScopeClaim, Basis, CoverageCapability, Cursor, Frontier,
        ProducerObservationClaims, ProducerObservationVerifier, Row, RowId, ScopeRoot,
        UntrustedProducerObservation, ViewDelta, ViewRoot, admit_complete_scope,
        admit_producer_observation, object_version, symbol_key, view_key, view_state_root,
    };

    struct FixtureVerifier;
    impl ProducerObservationVerifier for FixtureVerifier {
        type Error = &'static str;
        fn verify(
            &self,
            observation: &UntrustedProducerObservation,
        ) -> Result<ProducerObservationClaims, Self::Error> {
            if observation.evidence() != b"publication-fixture" {
                return Err("wrong fixture evidence");
            }
            Ok(ProducerObservationClaims::new(
                observation.producer_identity(),
                observation.scope_root(),
                observation.context(),
                *blake3::hash(observation.evidence()).as_bytes(),
            ))
        }
    }
    fn view() -> Arc<ViewRoot> {
        let basis = Basis::new(view_state_root(&[]), object_version(b"publication-source"));
        let scope = ScopeRoot::from_bytes(basis.object.to_bytes());
        let observation = admit_producer_observation(
            UntrustedProducerObservation::new(
                [7; 32],
                scope,
                [9; 32],
                b"publication-fixture".to_vec(),
            ),
            &FixtureVerifier,
        )
        .expect("fixture observation");
        let coverage = CoverageCapability::from_authorized_with_evidence(
            admit_complete_scope(
                AuthorityScopeClaim::from_object_version(basis.object),
                observation,
            )
            .expect("fixture complete scope"),
            b"publication-fixture".to_vec(),
        )
        .expect("fixture coverage");
        Arc::new(
            ViewRoot::empty_checked(
                view_key(b"publication-view"),
                basis,
                Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
                coverage,
            )
            .expect("complete root"),
        )
    }
    fn attached(root: &ViewRoot) -> (OwnerGate, Epoch, Cursor) {
        let cursor = Cursor::for_view_root_at(root, 0);
        let gate = OwnerGate::ready(
            VersionedRoot::from_revision(1, cursor, 0),
            ServiceMode::Attached,
        );
        let attachment = gate.ready_epoch().expect("serving attachment");
        (gate, attachment, cursor)
    }

    #[test]
    fn external_publications_coalesce_without_reattaching_and_cannot_roll_back() {
        let first = view();
        let (gate, attachment, cursor) = attached(&first);
        assert_eq!(
            gate.publish_view(attachment, Arc::clone(&first), cursor),
            PublicationAdmission::Admitted
        );
        let initial_wake = gate.lock().epoch;
        let row = Row::new(
            RowId::Symbol(symbol_key("external")),
            first.basis(),
            "external publication",
        );
        let prepared = first
            .prepare(
                ViewDelta::Upsert { row },
                first.capability().expect("complete source"),
            )
            .expect("prepared external delta");
        let (second, _) = first.commit(prepared).expect("committed external delta");
        let second = Arc::new(second);
        let published = Cursor::for_view_root_at(&second, 1);
        assert_eq!(
            gate.publish_view(attachment, Arc::clone(&second), published),
            PublicationAdmission::Admitted
        );
        assert!(gate.lock().epoch > initial_wake);
        assert_eq!(gate.attached_ready_epoch(), Some(attachment));
        let (kept, kept_cursor) = gate.publication(attachment).expect("latest shared root");
        assert!(Arc::ptr_eq(&kept, &second));
        assert_eq!(kept_cursor, published);
        assert_eq!(
            gate.publish_view(attachment, first, cursor),
            PublicationAdmission::Obsolete
        );
        assert_eq!(
            gate.publication(attachment).expect("not rolled back").1,
            published
        );
        assert_eq!(
            gate.ready_at(initial_wake, VersionedRoot::from_revision(1, cursor, 0)),
            None,
            "superseded UI wake is fenced"
        );
    }

    #[test]
    fn same_authority_observation_does_not_wake_or_cancel_resources() {
        let root = view();
        let (gate, attachment, cursor) = attached(&root);
        let cancel = gate
            .observation_scope(attachment)
            .expect("observation scope");
        let wake = gate.lock().epoch;
        for _ in 0..16 {
            assert_eq!(
                gate.publish_view(attachment, Arc::clone(&root), cursor),
                PublicationAdmission::Admitted
            );
        }
        assert_eq!(gate.lock().epoch, wake);
        assert_eq!(gate.attached_ready_epoch(), Some(attachment));
        assert!(!cancel.is_cancelled());
        let key = VersionedRoot::from_revision(1, cursor, 0).observed_at(99);
        assert_eq!(
            gate.ready_at(wake, key),
            Some(attachment),
            "observation metadata is not authority"
        );
    }

    #[test]
    fn daemon_restart_fences_pending_work_even_when_the_root_is_identical() {
        let root = view();
        let (gate, old, cursor) = attached(&root);
        let pending = gate.observation_scope(old).expect("old scope");
        assert!(gate.observation_failed(old, "expired publication lease after reconnect".into()));
        assert!(pending.is_cancelled());
        assert!(gate.restart());
        gate.publish(OwnerState::Ready {
            key: VersionedRoot::from_revision(1, cursor, 0),
            mode: ServiceMode::Attached,
        });
        let new = gate.ready_epoch().expect("new attachment");
        assert_ne!(old, new);
        assert_eq!(
            gate.publish_view(old, Arc::clone(&root), cursor),
            PublicationAdmission::Withdrawn
        );
        assert!(!gate.observation_failed(old, "late failure".into()));
        assert!(matches!(gate.state(), OwnerState::Ready { .. }));
        assert_eq!(
            gate.publish_view(new, root, cursor),
            PublicationAdmission::Admitted
        );
    }

    #[test]
    fn replacement_socket_fences_pending_reads_but_keeps_healthy_authority() {
        let root = view();
        let (gate, old, cursor) = attached(&root);
        let pending = gate.observation_scope(old).expect("scope");
        assert_eq!(
            gate.publish_view(old, Arc::clone(&root), cursor),
            PublicationAdmission::Admitted
        );
        let (new, active) = gate
            .replace_observation(old)
            .expect("fresh admitted connection");
        assert_ne!(old, new);
        assert!(pending.is_cancelled());
        assert!(!active.is_cancelled());
        assert_eq!(gate.attached_ready_epoch(), None);
        assert!(gate.complete_observation(new));
        assert_eq!(gate.attached_ready_epoch(), Some(new));
        let (kept, current) = gate.publication(new).expect("same healthy root");
        assert!(Arc::ptr_eq(&kept, &root));
        assert_eq!(current, cursor);
        assert!(
            matches!(gate.state(), OwnerState::Ready { key, .. } if key.same_authority(VersionedRoot::from_revision(1, cursor, 0)))
        );
        assert_eq!(
            gate.publish_view(old, root, cursor),
            PublicationAdmission::Withdrawn
        );
    }

    #[test]
    fn teardown_releases_latest_root_and_cancels_worker_scope() {
        let root = view();
        let (gate, attachment, cursor) = attached(&root);
        let cancel = gate.observation_scope(attachment).expect("scope");
        assert_eq!(
            gate.publish_view(attachment, Arc::clone(&root), cursor),
            PublicationAdmission::Admitted
        );
        gate.close();
        assert!(cancel.is_cancelled());
        assert!(gate.publication(attachment).is_none());
        assert!(!gate.observation_pause(attachment, Duration::from_secs(10)));
        assert_eq!(
            gate.publish_view(attachment, root, cursor),
            PublicationAdmission::Withdrawn
        );
        gate.publish(OwnerState::Ready {
            key: VersionedRoot::from_revision(1, cursor, 0),
            mode: ServiceMode::Attached,
        });
        assert_eq!(
            gate.state(),
            OwnerState::Failed(OwnerFault::Closed),
            "late starter cannot revive a closed owner"
        );
    }
}
