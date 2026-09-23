//! Bounded coalescing mailbox used by the background engine actor.

use crate::model::snapshot::ObjectId;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

/// Requests that can replace an older request in the queue expose a stable
/// coalescing key.
pub trait Coalescible {
    /// Returns the key whose newest value supersedes older values.
    fn coalesce_key(&self) -> Option<CoalesceKey>;
}

/// Closed set of mailbox coalescing lanes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CoalesceKey {
    /// Only the newest root refresh matters.
    Root,
    /// Only the newest read for an object matters.
    Object(ObjectId),
    /// Only the newest request for one typed product surface matters.
    Surface(backend_library::CommandId),
    /// Only the newest compatibility ingest request for one local project
    /// matters while the shared ProjectIngest surface is being integrated.
    Index(backend_library::SemanticObject),
    /// Only the newest local package-facts read for one project matters.
    LocalPackage(backend_library::SemanticObject),
    /// Persistence writes can be collapsed into the newest state.
    Persistence,
}

#[derive(Debug)]
struct State<T> {
    queue: VecDeque<(Option<CoalesceKey>, T)>,
    closed: bool,
}

/// Result of a non-blocking bounded enqueue.
#[derive(Debug)]
pub enum PushResult<T> {
    /// The request was appended.
    Enqueued,
    /// An older request with the same key was replaced.
    Coalesced(T),
    /// The queue was full; the caller remains responsible for this request.
    Full(T),
    /// The worker has shut down.
    Closed(T),
}

/// A bounded mailbox that never blocks a UI caller.
#[derive(Debug)]
pub struct CoalescingMailbox<T> {
    capacity: usize,
    state: Arc<(Mutex<State<T>>, Condvar)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Eq, PartialEq)]
    struct Request(u8);

    #[test]
    fn coalescing_replaces_latest_root_without_growing_the_queue() {
        let mailbox = CoalescingMailbox::new(1);
        assert!(matches!(
            mailbox.try_push(Request(1), Some(CoalesceKey::Root)),
            PushResult::Enqueued
        ));
        assert!(matches!(
            mailbox.try_push(Request(2), Some(CoalesceKey::Root)),
            PushResult::Coalesced(Request(1))
        ));
        assert_eq!(mailbox.len(), 1);
        assert!(matches!(mailbox.recv(), Some(Request(2))));
    }

    #[test]
    fn a_full_non_coalesced_lane_reports_the_value_without_blocking() {
        let mailbox = CoalescingMailbox::new(1);
        assert!(matches!(
            mailbox.try_push(Request(1), None),
            PushResult::Enqueued
        ));
        assert!(matches!(
            mailbox.try_push(Request(2), None),
            PushResult::Full(Request(2))
        ));
    }

    #[test]
    fn background_producer_waits_at_the_bound_until_the_ui_drains() {
        let mailbox = CoalescingMailbox::new(1);
        assert!(matches!(
            mailbox.try_push(Request(1), None),
            PushResult::Enqueued
        ));
        let producer = mailbox.clone();
        let thread = std::thread::spawn(move || producer.push_wait(Request(2), None));
        assert!(matches!(mailbox.recv(), Some(Request(1))));
        assert!(thread.join().expect("producer thread"));
        assert!(matches!(mailbox.try_recv(), Some(Request(2))));
    }
}

impl<T> Clone for CoalescingMailbox<T> {
    fn clone(&self) -> Self {
        Self {
            capacity: self.capacity,
            state: Arc::clone(&self.state),
        }
    }
}

impl<T> CoalescingMailbox<T> {
    /// Creates a mailbox with a fixed number of non-coalesced entries.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Arc::new((
                Mutex::new(State {
                    queue: VecDeque::new(),
                    closed: false,
                }),
                Condvar::new(),
            )),
        }
    }

    /// Enqueues without waiting for the worker.
    pub fn try_push(&self, value: T, key: Option<CoalesceKey>) -> PushResult<T> {
        let (lock, wake) = &*self.state;
        let Ok(mut state) = lock.lock() else {
            return PushResult::Closed(value);
        };
        if state.closed {
            return PushResult::Closed(value);
        }
        if let Some(key) = key.as_ref() {
            if let Some(position) = state
                .queue
                .iter()
                .position(|(existing, _)| existing.as_ref() == Some(key))
            {
                let old = std::mem::replace(&mut state.queue[position].1, value);
                wake.notify_one();
                return PushResult::Coalesced(old);
            }
        }
        if state.queue.len() >= self.capacity {
            return PushResult::Full(value);
        }
        state.queue.push_back((key, value));
        wake.notify_one();
        PushResult::Enqueued
    }

    /// Enqueues from a background producer, waiting only while this bounded
    /// queue is full. The UI never calls this method; it is the backpressure
    /// edge that keeps result delivery bounded when a window is not polling.
    pub fn push_wait(&self, value: T, key: Option<CoalesceKey>) -> bool {
        let (lock, wake) = &*self.state;
        let Ok(mut state) = lock.lock() else {
            return false;
        };
        loop {
            if state.closed {
                return false;
            }
            if let Some(key) = key.as_ref() {
                if let Some(position) = state
                    .queue
                    .iter()
                    .position(|(existing, _)| existing.as_ref() == Some(key))
                {
                    let old = std::mem::replace(&mut state.queue[position].1, value);
                    drop(old);
                    wake.notify_one();
                    return true;
                }
            }
            if state.queue.len() < self.capacity {
                state.queue.push_back((key, value));
                wake.notify_one();
                return true;
            }
            state = match wake.wait(state) {
                Ok(state) => state,
                Err(_) => return false,
            };
        }
    }

    /// Blocks only the background worker until a value is available.
    pub fn recv(&self) -> Option<T> {
        let (lock, wake) = &*self.state;
        let Ok(mut state) = lock.lock() else {
            return None;
        };
        loop {
            if let Some((_, value)) = state.queue.pop_front() {
                wake.notify_all();
                return Some(value);
            }
            if state.closed {
                return None;
            }
            state = match wake.wait(state) {
                Ok(state) => state,
                Err(_) => return None,
            };
        }
    }

    /// Removes one queued item without waiting.
    #[must_use]
    pub fn try_recv(&self) -> Option<T> {
        let (lock, wake) = &*self.state;
        let Ok(mut state) = lock.lock() else {
            return None;
        };
        let value = state.queue.pop_front().map(|(_, value)| value);
        if value.is_some() {
            wake.notify_all();
        }
        value
    }

    /// Closes the queue and wakes the worker.
    pub fn close(&self) {
        let (lock, wake) = &*self.state;
        if let Ok(mut state) = lock.lock() {
            state.closed = true;
            wake.notify_all();
        }
    }

    /// Returns the current number of queued entries.
    #[must_use]
    pub fn len(&self) -> usize {
        let (lock, _) = &*self.state;
        lock.lock().map(|state| state.queue.len()).unwrap_or(0)
    }
}
