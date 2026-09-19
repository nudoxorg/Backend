//! Count-and-byte bounded queues used at the daemon boundary.
//!
//! Queue admission is deliberately nonblocking. A caller that cannot reserve
//! both dimensions gets a typed error and cannot leave a half-admitted item in
//! the queue. [`FairQueues`] gives command, replication, completion, and
//! subscription traffic independent capacities while serving them round robin.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Condvar, Mutex};

/// Count and byte capacity of a queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueueBudget {
    /// Maximum queued item count.
    pub count: usize,
    /// Maximum queued encoded bytes.
    pub bytes: usize,
}

impl QueueBudget {
    /// Creates a bounded capacity.
    #[must_use]
    pub const fn new(count: usize, bytes: usize) -> Self {
        Self { count, bytes }
    }
}

/// Current count and byte usage.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueueUsage {
    /// Number of queued values.
    pub count: usize,
    /// Sum of queued value sizes.
    pub bytes: usize,
}

impl QueueUsage {
    /// Attempts to reserve one item.
    pub fn admit(&mut self, budget: QueueBudget, size: usize) -> bool {
        if self.count >= budget.count {
            return false;
        }
        let Some(bytes) = self.bytes.checked_add(size) else {
            return false;
        };
        let Some(count) = self.count.checked_add(1) else {
            return false;
        };
        if bytes > budget.bytes || count > budget.count {
            return false;
        }
        self.count = count;
        self.bytes = bytes;
        true
    }

    /// Releases one item and reports an accounting invariant violation.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn release(&mut self, size: usize) -> Result<(), QueueError> {
        if self.count == 0 || self.bytes < size {
            return Err(QueueError::Accounting);
        }
        self.count -= 1;
        self.bytes -= size;
        Ok(())
    }
}

/// A value that can report its encoded queue footprint.
pub trait QueueSized {
    /// Number of bytes charged to the queue.
    fn queue_bytes(&self) -> usize;

    /// Computes a checked encoded footprint. Implementations with multiple
    /// fields should override this so overflow is an admission error rather
    /// than a saturated accounting value.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn checked_queue_bytes(&self) -> Result<usize, QueueError> {
        Ok(self.queue_bytes())
    }
}

impl QueueSized for Vec<u8> {
    fn queue_bytes(&self) -> usize {
        self.len()
    }
}

impl QueueSized for Box<[u8]> {
    fn queue_bytes(&self) -> usize {
        self.len()
    }
}

impl QueueSized for String {
    fn queue_bytes(&self) -> usize {
        self.len()
    }
}

/// Failure to enqueue an item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueError {
    /// The sender has been closed.
    Closed,
    /// The count budget is exhausted.
    Count,
    /// The byte budget is exhausted.
    Bytes,
    /// Internal count/byte accounting would underflow or overflow.
    Accounting,
}

impl fmt::Display for QueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "queue admission failed: {self:?}")
    }
}

impl std::error::Error for QueueError {}

struct Entry<T> {
    value: T,
    bytes: usize,
}

struct State<T> {
    values: VecDeque<Entry<T>>,
    usage: QueueUsage,
    closed: bool,
}

/// A FIFO queue with atomic count-and-byte admission.
pub struct BoundedQueue<T> {
    budget: QueueBudget,
    state: Mutex<State<T>>,
    available: Condvar,
}

impl<T> fmt::Debug for BoundedQueue<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f.debug_struct("BoundedQueue")
            .field("budget", &self.budget)
            .field("usage", &state.usage)
            .field("closed", &state.closed)
            .field("available", &self.available)
            .finish()
    }
}

impl<T> BoundedQueue<T> {
    /// Creates an empty queue.
    #[must_use]
    pub fn new(budget: QueueBudget) -> Self {
        Self {
            budget,
            state: Mutex::new(State {
                values: VecDeque::new(),
                usage: QueueUsage::default(),
                closed: false,
            }),
            available: Condvar::new(),
        }
    }

    /// Returns the configured capacity.
    #[must_use]
    pub const fn budget(&self) -> QueueBudget {
        self.budget
    }

    /// Attempts to enqueue without waiting for capacity.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn try_push(&self, value: T) -> Result<(), QueueError>
    where
        T: QueueSized,
    {
        let bytes = value.checked_queue_bytes()?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(QueueError::Closed);
        }
        let next_count = state
            .usage
            .count
            .checked_add(1)
            .ok_or(QueueError::Accounting)?;
        if next_count > self.budget.count {
            return Err(QueueError::Count);
        }
        let next_bytes = state
            .usage
            .bytes
            .checked_add(bytes)
            .ok_or(QueueError::Accounting)?;
        if next_bytes > self.budget.bytes {
            return Err(QueueError::Bytes);
        }
        state.values.push_back(Entry { value, bytes });
        state.usage.count = next_count;
        state.usage.bytes = next_bytes;
        self.available.notify_one();
        Ok(())
    }

    /// Removes the oldest value, waiting until a value is available or closed.
    pub fn pop(&self) -> Option<T> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(entry) = state.values.pop_front() {
                if state.usage.release(entry.bytes).is_err() {
                    // A violated invariant must fail closed.  Never leave a
                    // queue with wrapped accounting or panic in a daemon
                    // request path.
                    state.closed = true;
                    self.available.notify_all();
                    return None;
                }
                return Some(entry.value);
            }
            if state.closed {
                return None;
            }
            state = self
                .available
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Removes the oldest value without waiting.
    pub fn try_pop(&self) -> Option<T> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = state.values.pop_front()?;
        if state.usage.release(entry.bytes).is_err() {
            // Drop a corrupt entry instead of handing it to a caller with
            // accounting that can no longer be trusted.  The queue remains
            // closed until its owner reconstructs it.
            state.closed = true;
            self.available.notify_all();
            return None;
        }
        Some(entry.value)
    }

    /// Closes the queue and wakes waiting consumers.
    pub fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        self.available.notify_all();
    }

    /// Returns whether the queue has been closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closed
    }

    /// Returns current usage.
    #[must_use]
    pub fn usage(&self) -> QueueUsage {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .usage
    }
}

/// Four daemon traffic classes, each with an independent bounded budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueLane {
    /// Client commands and queries.
    Command,
    /// Replication requests and chunks.
    Replication,
    /// Worker/result completions.
    Completion,
    /// Subscription and cursor traffic.
    Subscription,
}

impl QueueLane {
    const fn index(self) -> usize {
        match self {
            Self::Command => 0,
            Self::Replication => 1,
            Self::Completion => 2,
            Self::Subscription => 3,
        }
    }
}

/// Round-robin queue set that prevents one traffic class starving another.
pub struct FairQueues<T> {
    queues: [BoundedQueue<T>; 4],
    cursor: Mutex<usize>,
}

impl<T> FairQueues<T> {
    /// Creates four queues with lane-specific capacities.
    #[must_use]
    pub fn new(budgets: [QueueBudget; 4]) -> Self {
        Self {
            queues: budgets.map(BoundedQueue::new),
            cursor: Mutex::new(0),
        }
    }

    /// Attempts to enqueue into a named lane.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn try_push(&self, lane: QueueLane, value: T) -> Result<(), QueueError>
    where
        T: QueueSized,
    {
        self.queues[lane.index()].try_push(value)
    }

    /// Takes one item from the next nonempty lane in round-robin order.
    pub fn try_pop(&self) -> Option<(QueueLane, T)> {
        let mut cursor = self
            .cursor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for offset in 0..4 {
            let index = (*cursor + offset) % 4;
            if let Some(value) = self.queues[index].try_pop() {
                *cursor = (index + 1) % 4;
                let lane = match index {
                    0 => QueueLane::Command,
                    1 => QueueLane::Replication,
                    2 => QueueLane::Completion,
                    _ => QueueLane::Subscription,
                };
                return Some((lane, value));
            }
        }
        None
    }

    /// Closes all lanes.
    pub fn close(&self) {
        for queue in &self.queues {
            queue.close();
        }
    }
}
