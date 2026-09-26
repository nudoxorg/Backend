//! A coalescing, executor-neutral wake signal from worker threads to the UI.
//!
//! Worker threads (the engine actor lanes and the read pool) push results into
//! bounded mailboxes and then call [`WakeSender::wake`]. The UI side awaits
//! [`WakeReceiver::wait`] inside one `cx.spawn` task and, when it resolves,
//! drains every queued result before awaiting again. Many wakes between two
//! drains collapse into one pending bit, so a burst of results costs one UI
//! turn, and nothing is polled while no work lands: an idle window with a
//! long-running request schedules no frame at all.
//!
//! The signal carries no payload on purpose. The payload stays in the bounded
//! mailbox that already owns backpressure; this type only answers "is there
//! anything to drain?".

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};

#[derive(Debug, Default)]
struct WakeState {
    pending: bool,
    closed: bool,
    waker: Option<Waker>,
    /// Total wake calls, for tests that prove coalescing.
    signals: u64,
    /// Total resolved `next()` futures, for tests that prove coalescing.
    turns: u64,
}

#[derive(Debug, Default)]
struct Shared {
    state: Mutex<WakeState>,
}

impl Shared {
    fn with<R>(&self, f: impl FnOnce(&mut WakeState) -> R) -> R {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut state)
    }
}

/// Producer half: cheap to clone, callable from any thread.
#[derive(Clone, Debug)]
pub struct WakeSender {
    shared: Arc<Shared>,
}

/// Consumer half, owned by exactly one UI task.
#[derive(Debug)]
pub struct WakeReceiver {
    shared: Arc<Shared>,
}

/// Creates one connected wake pair.
#[must_use]
pub fn wake_channel() -> (WakeSender, WakeReceiver) {
    let shared = Arc::new(Shared::default());
    (
        WakeSender {
            shared: Arc::clone(&shared),
        },
        WakeReceiver { shared },
    )
}

impl WakeSender {
    /// Marks work pending and wakes the waiting task, if any.
    ///
    /// The waker is invoked outside the lock so an executor that polls
    /// synchronously inside `wake` cannot deadlock on this signal.
    pub fn wake(&self) {
        let waker = self.shared.with(|state| {
            state.signals = state.signals.saturating_add(1);
            if state.closed {
                return None;
            }
            state.pending = true;
            state.waker.take()
        });
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Closes the signal. A pending wake is still delivered once; after that
    /// the receiver resolves to `None` and its task ends.
    pub fn close(&self) {
        let waker = self.shared.with(|state| {
            state.closed = true;
            state.waker.take()
        });
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl WakeReceiver {
    /// Resolves when work is pending (`Some(())`) or the signal closed with
    /// nothing pending (`None`). Resolving clears the pending bit, so every
    /// wake that happened before the resolution is covered by one drain.
    pub fn wait(&mut self) -> Next<'_> {
        Next { receiver: self }
    }

    /// Takes the pending bit without waiting.
    pub fn try_take(&mut self) -> bool {
        self.shared.with(|state| {
            let pending = std::mem::take(&mut state.pending);
            if pending {
                state.turns = state.turns.saturating_add(1);
            }
            pending
        })
    }

    /// Returns `(wake calls, resolved turns)`; a test proves coalescing when
    /// turns are fewer than wake calls.
    #[must_use]
    pub fn counts(&self) -> (u64, u64) {
        self.shared.with(|state| (state.signals, state.turns))
    }

    /// Returns a sender connected to this receiver.
    #[must_use]
    pub fn sender(&self) -> WakeSender {
        WakeSender {
            shared: Arc::clone(&self.shared),
        }
    }
}

/// Future returned by [`WakeReceiver::wait`].
#[derive(Debug)]
pub struct Next<'a> {
    receiver: &'a mut WakeReceiver,
}

impl Future for Next<'_> {
    type Output = Option<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.receiver.shared.with(|state| {
            if std::mem::take(&mut state.pending) {
                state.turns = state.turns.saturating_add(1);
                return Poll::Ready(Some(()));
            }
            if state.closed {
                return Poll::Ready(None);
            }
            match &state.waker {
                Some(waker) if waker.will_wake(cx.waker()) => {}
                _ => state.waker = Some(cx.waker().clone()),
            }
            Poll::Pending
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;

    struct CountingWaker(AtomicUsize);

    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn poll_once(receiver: &mut WakeReceiver, waker: &Waker) -> Poll<Option<()>> {
        let mut context = Context::from_waker(waker);
        let mut future = receiver.wait();
        Pin::new(&mut future).poll(&mut context)
    }

    #[test]
    fn a_burst_of_wakes_is_one_turn_and_an_idle_signal_never_resolves() {
        let (sender, mut receiver) = wake_channel();
        let counter = Arc::new(CountingWaker(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&counter));

        assert_eq!(poll_once(&mut receiver, &waker), Poll::Pending);
        for _ in 0..50 {
            sender.wake();
        }
        // Only the first wake found a registered waker; the rest coalesced.
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
        assert_eq!(poll_once(&mut receiver, &waker), Poll::Ready(Some(())));
        // Nothing new happened: the signal stays pending forever, it does not spin.
        assert_eq!(poll_once(&mut receiver, &waker), Poll::Pending);
        assert_eq!(poll_once(&mut receiver, &waker), Poll::Pending);
        assert_eq!(receiver.counts(), (50, 1));
    }

    #[test]
    fn close_delivers_a_pending_wake_once_then_ends() {
        let (sender, mut receiver) = wake_channel();
        let waker = Waker::from(Arc::new(CountingWaker(AtomicUsize::new(0))));
        sender.wake();
        sender.close();
        assert_eq!(poll_once(&mut receiver, &waker), Poll::Ready(Some(())));
        assert_eq!(poll_once(&mut receiver, &waker), Poll::Ready(None));
    }

    #[test]
    fn a_wake_from_another_thread_reaches_the_registered_waker() {
        let (sender, mut receiver) = wake_channel();
        let counter = Arc::new(CountingWaker(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&counter));
        assert_eq!(poll_once(&mut receiver, &waker), Poll::Pending);
        std::thread::spawn(move || sender.wake())
            .join()
            .expect("waking thread");
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
        assert!(receiver.try_take());
        assert!(!receiver.try_take());
    }
}
