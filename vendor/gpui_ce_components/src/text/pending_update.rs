//! A single pending publication and a single wake-up, independent of frame rate.
//! The reducer must preserve ordered deltas while replacing obsolete snapshots.

use crate::async_util::{Receiver, Sender, unbounded};
use futures::Stream;
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

struct Slot<T> {
    pending: Option<T>,
    closed: bool,
}

pub(super) struct Publisher<T> {
    pending: Arc<Mutex<Slot<T>>>,
    wake: Sender<()>,
    merge: fn(&mut T, T),
}

pub(super) struct Publications<T> {
    pending: Arc<Mutex<Slot<T>>>,
    wake: Pin<Box<Receiver<()>>>,
}

pub(super) fn channel<T>(merge: fn(&mut T, T)) -> (Publisher<T>, Publications<T>) {
    let pending = Arc::new(Mutex::new(Slot {
        pending: None,
        closed: false,
    }));
    let (tx, rx) = unbounded();
    (
        Publisher {
            pending: pending.clone(),
            wake: tx,
            merge,
        },
        Publications {
            pending,
            wake: Box::pin(rx),
        },
    )
}

impl<T> Publisher<T> {
    pub(super) fn try_send(&self, next: T) -> Result<(), ()> {
        let mut pending = self.pending.lock().unwrap();
        if pending.closed {
            return Err(());
        }
        if let Some(current) = pending.pending.as_mut() {
            (self.merge)(current, next);
            return Ok(());
        }
        pending.pending = Some(next);
        // Keep the lock through the signal so the consumer cannot take a
        // publication before its wake-up has been enqueued.
        if self.wake.try_send(()).is_err() {
            pending.closed = true;
            pending.pending = None;
            return Err(());
        }
        Ok(())
    }
}

impl<T> Publications<T> {
    #[cfg(test)]
    pub(super) fn try_recv(&self) -> Result<T, ()> {
        self.wake.as_ref().get_ref().try_recv().map_err(|_| ())?;
        self.pending.lock().unwrap().pending.take().ok_or(())
    }
}

impl<T> Stream for Publications<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        match self.wake.as_mut().poll_next(cx) {
            Poll::Ready(Some(())) => Poll::Ready(self.pending.lock().unwrap().pending.take()),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T> Drop for Publications<T> {
    fn drop(&mut self) {
        let mut slot = self.pending.lock().unwrap();
        slot.closed = true;
        slot.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_consumer_releases_pending_and_refuses_append_or_replace() {
        let (tx, rx) = channel(|current: &mut Arc<String>, next| *current = next);
        let queued = Arc::new("queued before cancellation".into());
        let retained = Arc::downgrade(&queued);
        tx.try_send(queued).unwrap();
        assert!(retained.upgrade().is_some());
        drop(rx);
        assert!(retained.upgrade().is_none());
        for label in ["append", "replace", "append again", "replace again"] {
            let next = Arc::new(label.into());
            let weak = Arc::downgrade(&next);
            assert!(tx.try_send(next).is_err());
            assert!(weak.upgrade().is_none());
        }
        let slot = tx.pending.lock().unwrap();
        assert!(slot.closed);
        assert!(slot.pending.is_none());
    }

    #[test]
    fn cancellation_racing_with_publication_releases_both_orders() {
        use std::sync::Barrier;
        for _ in 0..32 {
            let (tx, rx) = channel(|current: &mut Arc<String>, next| *current = next);
            tx.try_send(Arc::new("prior".into())).unwrap();
            let next = Arc::new("racing".into());
            let retained = Arc::downgrade(&next);
            let barrier = Arc::new(Barrier::new(2));
            std::thread::scope(|scope| {
                let producer_barrier = barrier.clone();
                let tx = &tx;
                let producer = scope.spawn(move || {
                    producer_barrier.wait();
                    let _ = tx.try_send(next);
                });
                barrier.wait();
                drop(rx);
                producer.join().unwrap();
            });
            assert!(retained.upgrade().is_none());
            assert!(tx.try_send(Arc::new("after".into())).is_err());
            assert!(tx.pending.lock().unwrap().pending.is_none());
        }
    }
}
