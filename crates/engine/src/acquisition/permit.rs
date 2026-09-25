use std::sync::{Arc, Condvar, Mutex};

/// Failure acquiring a bounded byte permit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PermitError {
    /// Requested bytes exceed the pool's total bound.
    TooLarge,
    /// An arithmetic bound was exceeded.
    Overflow,
}

/// A blocking byte budget used by archive admission.
#[derive(Debug)]
pub struct BytePermitPool {
    state: Mutex<u64>,
    wake: Condvar,
    capacity: u64,
}

impl BytePermitPool {
    /// Creates a pool with a fixed maximum of simultaneously admitted bytes.
    #[must_use]
    pub fn new(capacity: u64) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(capacity),
            wake: Condvar::new(),
            capacity,
        })
    }

    /// Acquires `bytes`, blocking until the budget is available.
    pub fn acquire(self: &Arc<Self>, bytes: u64) -> Result<BytePermit, PermitError> {
        if bytes > self.capacity {
            return Err(PermitError::TooLarge);
        }
        let mut available = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *available < bytes {
            available = self
                .wake
                .wait(available)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *available -= bytes;
        Ok(BytePermit {
            pool: Arc::clone(self),
            bytes,
        })
    }

    /// Returns available bytes.
    #[must_use]
    pub fn available(&self) -> u64 {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Affine byte budget reservation.
pub struct BytePermit {
    pool: Arc<BytePermitPool>,
    bytes: u64,
}

impl Drop for BytePermit {
    fn drop(&mut self) {
        let mut available = self
            .pool
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *available = available.saturating_add(self.bytes).min(self.pool.capacity);
        self.pool.wake.notify_all();
    }
}

/// Bounded count semaphore used for metadata/object effects.
#[derive(Debug)]
pub struct PermitPool {
    state: Mutex<usize>,
    wake: Condvar,
    capacity: usize,
}

impl PermitPool {
    /// Creates a count-bounded pool.
    #[must_use]
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(capacity),
            wake: Condvar::new(),
            capacity,
        })
    }
    /// Acquires one slot.
    pub fn acquire(self: &Arc<Self>) -> Permit {
        let mut available = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *available == 0 {
            available = self
                .wake
                .wait(available)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *available -= 1;
        Permit {
            pool: Arc::clone(self),
        }
    }
    /// Returns the number of currently available slots.
    #[must_use]
    pub fn available(&self) -> usize {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Affine count permit.
pub struct Permit {
    pool: Arc<PermitPool>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut available = self
            .pool
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *available = available.saturating_add(1).min(self.pool.capacity);
        self.pool.wake.notify_one();
    }
}
