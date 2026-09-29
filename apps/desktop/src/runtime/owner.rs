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
use std::future::Future;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::task::{Poll, Waker};

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
    /// Could not start, in the host's own words.
    Failed(Arc<str>),
}

/// The one place the owner's state lives; cheap to clone and `Send`.
#[derive(Clone)]
pub struct OwnerGate(Arc<Shared>);

impl std::fmt::Debug for OwnerGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
                OwnerState::Failed(message) => crate::runtime::trace::mark("owner.failed", message),
                OwnerState::Starting => crate::runtime::trace::mark("owner.starting", "gate"),
            }
            inner.state = state;
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
    pub fn wait(&self) -> Result<(), Arc<str>> {
        let mut inner = self.lock();
        loop {
            match &inner.state {
                OwnerState::Ready { .. } => return Ok(()),
                OwnerState::Failed(message) => return Err(Arc::clone(message)),
                OwnerState::Starting if inner.closed => {
                    return Err(Arc::from("the window closed before the index answered"));
                }
                OwnerState::Starting => {
                    inner = self
                        .0
                        .changed
                        .wait(inner)
                        .unwrap_or_else(PoisonError::into_inner);
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

/// Turns every published owner state into the window's data events, on the
/// UI thread, for as long as the app runs.
pub fn watch(gate: OwnerGate, root: Entity<super::UiRootEntity>, store: Entity<super::store::DataStore>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let mut seen = 0;
        loop {
            let (epoch, state) = gate.next(seen).await;
            seen = epoch;
            // The task ends with the app (a spawned task is dropped with it).
            cx.update(|cx| match state {
                OwnerState::Starting => store.update(cx, super::store::DataStore::owner_starting),
                OwnerState::Ready { key, mode } => {
                    root.update(cx, |root, cx| root.admit_owner(key, mode, cx));
                    store.update(cx, super::store::DataStore::owner_ready);
                }
                OwnerState::Failed(message) => {
                    store.update(cx, |store, cx| store.owner_failed(&message, cx));
                }
            });
        }
    })
    .detach();
}

#[cfg(test)]
#[allow(clippy::expect_used)]
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
        gate.publish(OwnerState::Failed(Arc::from("could not own the workspace")));
        assert_eq!(
            received.recv_timeout(Duration::from_secs(2)).expect("released"),
            Err(Arc::from("could not own the workspace"))
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
        gate.publish(OwnerState::Failed(Arc::from("no")));
        let Poll::Ready((epoch, state)) = first.as_mut().poll(&mut task) else {
            panic!("a publish resolves the watcher");
        };
        assert_eq!(state, OwnerState::Failed(Arc::from("no")));
        let mut second = Box::pin(gate.next(epoch));
        assert!(second.as_mut().poll(&mut task).is_pending(), "a seen state never resolves twice");
    }
}
