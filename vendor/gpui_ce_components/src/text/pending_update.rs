//! A single pending publication and a single wake-up, independent of frame rate.
//! The reducer must preserve ordered deltas while replacing obsolete snapshots.

use crate::async_util::{Receiver, Sender, unbounded};
use futures::Stream;
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

pub(super) struct Publisher<T> {
    pending: Arc<Mutex<Option<T>>>,
    wake: Sender<()>,
    merge: fn(&mut T, T),
}

pub(super) struct Publications<T> {
    pending: Arc<Mutex<Option<T>>>,
    wake: Pin<Box<Receiver<()>>>,
}

pub(super) fn channel<T>(merge: fn(&mut T, T)) -> (Publisher<T>, Publications<T>) {
    let pending = Arc::new(Mutex::new(None));
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
        if let Some(current) = pending.as_mut() {
            (self.merge)(current, next);
            return Ok(());
        }
        *pending = Some(next);
        // Keep the lock through the signal so the consumer cannot take a
        // publication before its wake-up has been enqueued.
        if self.wake.try_send(()).is_err() {
            *pending = None;
            return Err(());
        }
        Ok(())
    }
}

impl<T> Publications<T> {
    #[cfg(test)]
    pub(super) fn try_recv(&self) -> Result<T, ()> {
        self.wake.as_ref().get_ref().try_recv().map_err(|_| ())?;
        self.pending.lock().unwrap().take().ok_or(())
    }
}

impl<T> Stream for Publications<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        match self.wake.as_mut().poll_next(cx) {
            Poll::Ready(Some(())) => Poll::Ready(self.pending.lock().unwrap().take()),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_consumer_does_not_retain_later_publications() {
        let (tx, rx) = channel(|current: &mut String, next| *current = next);
        drop(rx);
        assert!(tx.try_send("first".into()).is_err());
        assert!(tx.pending.lock().unwrap().is_none());
        assert!(tx.try_send("second".into()).is_err());
        assert!(tx.pending.lock().unwrap().is_none());
    }
}
