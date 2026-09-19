//! Cancellation-safe leases for retained readers.

use super::index::{RetainedReaders, SemanticReaderKey, SemanticWorkKey};
use crate::{ScopedRead, ScopedReadObservation, SemanticError};
use std::fmt;
use std::sync::{Arc, Mutex};

/// A thread-safe retained-reader store whose leases remove state on drop.
#[derive(Clone, Debug)]
pub struct RetainedReaderStore<K: SemanticReaderKey = SemanticWorkKey> {
    inner: Arc<Mutex<RetainedReaders<K>>>,
}

impl<K: SemanticReaderKey> Default for RetainedReaderStore<K> {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RetainedReaders::default())),
        }
    }
}

impl<K: SemanticReaderKey> RetainedReaderStore<K> {
    /// Creates an empty retained-reader store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Retains one reader observation and returns its cancellation lease.
    ///
    /// # Errors
    ///
    /// Returns the registration admission error when the observation cannot
    /// be retained.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the lease owns its observation so the returned lease can retain it"
    )]
    pub fn lease(
        &self,
        reader: K,
        observation: ScopedReadObservation,
    ) -> Result<ReaderLease<K>, SemanticError> {
        self.with_mut(|index| index.register(reader, observation.clone()))?;
        Ok(ReaderLease {
            store: self.clone(),
            reader,
            read: observation.read().clone(),
            active: true,
        })
    }

    /// Runs an immutable operation against the retained reverse index.
    pub fn with<R>(&self, operation: impl FnOnce(&RetainedReaders<K>) -> R) -> R {
        let index = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operation(&index)
    }

    /// Runs a checked mutable operation against the retained reverse index.
    ///
    /// # Errors
    ///
    /// Returns the error produced by `operation`.
    pub fn with_mut<R>(
        &self,
        operation: impl FnOnce(&mut RetainedReaders<K>) -> Result<R, SemanticError>,
    ) -> Result<R, SemanticError> {
        let mut index = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operation(&mut index)
    }
}

/// RAII lease for one retained reader observation.
#[must_use = "retain the lease while the reader remains interested"]
pub struct ReaderLease<K: SemanticReaderKey = SemanticWorkKey> {
    store: RetainedReaderStore<K>,
    reader: K,
    read: ScopedRead,
    active: bool,
}

impl<K: SemanticReaderKey> fmt::Debug for ReaderLease<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReaderLease")
            .field("reader", &self.reader)
            .field("read", &self.read)
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

impl<K: SemanticReaderKey> ReaderLease<K> {
    /// Returns the reader shard retaining this lease.
    #[must_use]
    pub const fn reader(&self) -> K {
        self.reader
    }

    /// Returns the exact selector retaining this lease.
    #[must_use]
    pub const fn read(&self) -> &ScopedRead {
        &self.read
    }

    /// Returns whether the registration is still retained.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Releases the retained registration immediately.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "consuming the lease makes release idempotent with Drop"
    )]
    pub fn release(mut self) -> bool {
        self.release_inner()
    }

    /// Cancels this reader's interest and releases its retained registration.
    ///
    /// Cancellation is deliberately affine: consuming the lease makes a
    /// second cancellation or drop harmless and prevents a late worker from
    /// removing a newer registration for the same reader.
    pub fn cancel(self) -> bool {
        self.release()
    }

    fn release_inner(&mut self) -> bool {
        if !self.active {
            return false;
        }
        self.active = false;
        self.store
            .with_mut(|index| Ok(index.unregister(self.reader, &self.read)))
            .unwrap_or(false)
    }
}

impl<K: SemanticReaderKey> Drop for ReaderLease<K> {
    fn drop(&mut self) {
        let _ = self.release_inner();
    }
}

/// Compatibility alias for callers that name the reverse arrangement an
/// index.
pub type ReaderIndex<K = SemanticWorkKey> = RetainedReaders<K>;
