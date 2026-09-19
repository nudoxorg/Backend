//! Bounded reusable mutable buffers.

use crate::errors::PoolError;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

struct PoolInner {
    available: Vec<Vec<u8>>,
    live: usize,
    max_buffers: usize,
    max_bytes: usize,
}

/// A bounded pool for mutable byte buffers.
///
/// A lease owns one live buffer.  Dropping the lease clears and returns its
/// allocation to the pool, so callers cannot accidentally retain mutable
/// scratch after releasing the capability.
#[derive(Clone)]
pub struct BufferPool {
    inner: Arc<Mutex<PoolInner>>,
}

/// A mutable buffer leased from a [`BufferPool`].
pub struct BufferLease {
    buffer: Option<Vec<u8>>,
    pool: Arc<Mutex<PoolInner>>,
}

/// A point-in-time pool usage snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PoolStats {
    /// Number of currently leased buffers.
    pub live: usize,
    /// Number of returned buffers available for reuse.
    pub available: usize,
}

impl std::fmt::Debug for BufferPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferPool")
            .field("stats", &self.stats())
            .finish()
    }
}

impl std::fmt::Debug for BufferLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferLease")
            .field("len", &self.len())
            .field("capacity", &self.capacity())
            .finish()
    }
}

impl BufferPool {
    /// Creates a pool with a maximum number of live buffers and bytes per
    /// buffer.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::BufferLimit`] when either bound is zero.
    pub fn new(max_buffers: usize, max_bytes: usize) -> Result<Self, PoolError> {
        if max_buffers == 0 || max_bytes == 0 {
            return Err(PoolError::BufferLimit);
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(PoolInner {
                available: Vec::new(),
                live: 0,
                max_buffers,
                max_bytes,
            })),
        })
    }

    /// Acquires one buffer with at least `capacity` bytes available.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::BufferLimit`] when `capacity` exceeds the pool
    /// bound, [`PoolError::Exhausted`] when all leases are in use, or another
    /// [`PoolError`] when allocation bookkeeping cannot advance.
    pub fn acquire(&self, capacity: usize) -> Result<BufferLease, PoolError> {
        let mut inner = lock(&self.inner);
        if capacity > inner.max_bytes {
            return Err(PoolError::BufferLimit);
        }
        if inner.live >= inner.max_buffers {
            return Err(PoolError::Exhausted);
        }
        let index = inner
            .available
            .iter()
            .position(|buffer| buffer.capacity() >= capacity);
        let mut buffer = match index {
            Some(position) => inner.available.swap_remove(position),
            None => Vec::new(),
        };
        buffer.clear();
        if buffer.capacity() < capacity {
            buffer
                .try_reserve(capacity - buffer.capacity())
                .map_err(|_| PoolError::Allocation)?;
        }
        inner.live = inner
            .live
            .checked_add(1)
            .ok_or(PoolError::CounterOverflow)?;
        drop(inner);
        Ok(BufferLease {
            buffer: Some(buffer),
            pool: Arc::clone(&self.inner),
        })
    }

    /// Returns current live and available buffer counts.
    #[must_use]
    pub fn stats(&self) -> PoolStats {
        let inner = lock(&self.inner);
        PoolStats {
            live: inner.live,
            available: inner.available.len(),
        }
    }

    /// Returns the configured maximum bytes per buffer.
    #[must_use]
    pub fn max_bytes(&self) -> usize {
        lock(&self.inner).max_bytes
    }

    /// Returns the configured maximum number of live buffers.
    #[must_use]
    pub fn max_buffers(&self) -> usize {
        lock(&self.inner).max_buffers
    }
}

impl BufferLease {
    /// Returns the currently initialized bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        match self.buffer.as_deref() {
            Some(buffer) => buffer,
            None => &[],
        }
    }

    /// Returns the currently initialized bytes as a mutable slice.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        match self.buffer.as_deref_mut() {
            Some(buffer) => buffer,
            None => &mut [],
        }
    }

    /// Returns the initialized length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buffer.as_ref().map_or(0, Vec::len)
    }

    /// Returns whether no bytes are initialized.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the allocation capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.buffer.as_ref().map_or(0, Vec::capacity)
    }

    /// Removes all initialized bytes while retaining the allocation.
    pub fn clear(&mut self) {
        if let Some(buffer) = &mut self.buffer {
            buffer.clear();
        }
    }

    /// Appends bytes within the pool's configured bound.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::BufferLimit`] when the append would exceed the
    /// configured capacity or [`PoolError::Allocation`] when memory cannot be
    /// reserved.
    pub fn extend_from_slice(&mut self, bytes: &[u8]) -> Result<(), PoolError> {
        let Some(buffer) = &mut self.buffer else {
            return Err(PoolError::BufferLimit);
        };
        let limit = lock(&self.pool).max_bytes;
        let new_len = buffer
            .len()
            .checked_add(bytes.len())
            .ok_or(PoolError::BufferLimit)?;
        if new_len > limit {
            return Err(PoolError::BufferLimit);
        }
        buffer
            .try_reserve(bytes.len())
            .map_err(|_| PoolError::Allocation)?;
        buffer.extend_from_slice(bytes);
        Ok(())
    }

    /// Consumes the lease and takes ownership of its buffer.
    ///
    /// The allocation is not returned to the pool after this method.  This is
    /// useful when immutable output ownership is being transferred to another
    /// layer.
    #[must_use]
    #[allow(
        clippy::manual_unwrap_or_default,
        reason = "the explicit match documents that an already-consumed lease yields an empty owner"
    )]
    pub fn into_vec(mut self) -> Vec<u8> {
        let buffer = match self.buffer.take() {
            Some(buffer) => buffer,
            None => Vec::new(),
        };
        let mut inner = lock(&self.pool);
        debug_assert!(inner.live > 0, "buffer lease live count underflow");
        if let Some(live) = inner.live.checked_sub(1) {
            inner.live = live;
        }
        buffer
    }
}

impl Drop for BufferLease {
    fn drop(&mut self) {
        let Some(mut buffer) = self.buffer.take() else {
            return;
        };
        buffer.clear();
        let mut inner = lock(&self.pool);
        debug_assert!(inner.live > 0, "buffer lease live count underflow");
        if let Some(live) = inner.live.checked_sub(1) {
            inner.live = live;
        }
        inner.available.push(buffer);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => PoisonError::into_inner(poisoned),
    }
}
