use super::prepared_error::PreparationError;
use super::prepared_key::PreparationKey;
use crate::{InputManifestId, SessionId, ToolchainId};
use std::collections::{BTreeMap, HashMap};
use std::sync::{
    Arc, Condvar, Mutex, RwLock, RwLockReadGuard,
    atomic::{AtomicU64, Ordering},
};
/// Bounds for the immutable preparation cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparationCacheConfig {
    /// Maximum number of resident prepared request bodies.
    pub max_entries: usize,
    /// Maximum total bytes retained by all request bodies.
    pub max_bytes: usize,
}

impl PreparationCacheConfig {
    /// Creates cache bounds, rejecting zero-sized caches.
    ///
    /// # Errors
    ///
    /// Returns [`PreparationError::Capacity`] when either bound is zero.
    pub const fn new(max_entries: usize, max_bytes: usize) -> Result<Self, PreparationError> {
        if max_entries == 0 || max_bytes == 0 {
            return Err(PreparationError::Capacity);
        }
        Ok(Self {
            max_entries,
            max_bytes,
        })
    }
}

/// A point-in-time cache accounting snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PreparationCacheStats {
    /// Number of resident entries.
    pub resident: usize,
    /// Number of resident bytes.
    pub bytes: usize,
    /// Number of successful borrowed lookups (a metric that saturates at the
    /// maximum counter value).
    pub hits: u64,
    /// Number of misses that required canonical encoding (a metric that
    /// saturates at the maximum counter value).
    pub misses: u64,
    /// Number of entries evicted by the bounded policy (a metric that
    /// saturates at the maximum counter value).
    pub evictions: u64,
    /// Number of explicit invalidations (a metric that saturates at the
    /// maximum counter value).
    pub invalidations: u64,
    /// Monotonic preparation generation.
    pub generation: u64,
}

pub(super) struct PreparedEntry {
    pub(super) key: PreparationKey,
    pub(super) bytes: Arc<[u8]>,
    pub(super) generation: u64,
    pub(super) last_used: AtomicU64,
}

#[derive(Default)]
pub(super) struct CacheState {
    pub(super) entries: Vec<PreparedEntry>,
    /// Direct key to slot mapping. Entries use swap-remove, so every
    /// mutation updates at most two map entries and invalidation stays O(1).
    pub(super) index: HashMap<PreparationKey, usize>,
    pub(super) bytes: usize,
    pub(super) clock: u64,
    pub(super) generation: u64,
    pub(super) misses: u64,
    pub(super) evictions: u64,
    pub(super) invalidations: u64,
}

pub(super) struct PreparationFlight {
    pub(super) state: Mutex<Option<Result<(), PreparationError>>>,
    pub(super) wake: Condvar,
}

/// A bounded, generational cache of canonical native request bodies.
///
/// The cache owns only immutable byte arrays.  [`PreparedRequest`] holds a
/// read guard into the cache, so its byte slice cannot outlive the cache entry
/// or race an eviction.  Writers wait only while updating the small index;
/// encoding occurs before publication and therefore does not serialize other
/// borrowers.
#[derive(Clone)]
pub struct PreparationCache {
    pub(super) config: PreparationCacheConfig,
    pub(super) state: Arc<RwLock<CacheState>>,
    pub(super) flights: Arc<Mutex<BTreeMap<PreparationKey, Arc<PreparationFlight>>>>,
    pub(super) hits: Arc<AtomicU64>,
    pub(super) clock: Arc<AtomicU64>,
    pub(super) invalidation_epoch: Arc<AtomicU64>,
}

/// Explicit name for the cache at the compilation boundary.
pub type PreparedCompilationCache = PreparationCache;

/// A borrowed canonical request body retained by a cache read guard.
pub struct PreparedRequest<'a> {
    pub(super) guard: RwLockReadGuard<'a, CacheState>,
    pub(super) index: usize,
}

impl std::fmt::Debug for PreparedRequest<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedRequest")
            .field("key", &self.key())
            .field("len", &self.bytes().len())
            .field("generation", &self.generation())
            .finish()
    }
}

impl PreparedRequest<'_> {
    /// Returns the exact preparation identity.
    #[must_use]
    pub fn key(&self) -> PreparationKey {
        self.guard.entries[self.index].key
    }

    /// Borrows canonical request bytes without allocating.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.guard.entries[self.index].bytes.as_ref()
    }

    /// Returns the monotonic cache generation that published this request.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.guard.entries[self.index].generation
    }

    /// Returns the immutable byte owner for APIs that need to cross a thread
    /// boundary while retaining the same allocation.
    #[must_use]
    pub fn shared_bytes(&self) -> Arc<[u8]> {
        Arc::clone(&self.guard.entries[self.index].bytes)
    }
}

impl PreparationCache {
    /// Borrows a resident request without encoding or allocating.
    #[must_use]
    pub fn borrow(&self, key: PreparationKey) -> Option<PreparedRequest<'_>> {
        self.borrow_inner(&key, true)
    }

    pub(super) fn borrow_inner(
        &self,
        key: &PreparationKey,
        count_hit: bool,
    ) -> Option<PreparedRequest<'_>> {
        let state = read_lock(&self.state);
        let index = *state.index.get(key)?;
        if count_hit {
            self.hits.fetch_add(1, Ordering::Relaxed);
        }
        let tick = self.clock.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        state.entries[index]
            .last_used
            .store(tick, Ordering::Relaxed);
        Some(PreparedRequest {
            guard: state,
            index,
        })
    }

    /// Invalidates exactly one prepared version.
    #[must_use]
    pub fn invalidate(&self, key: PreparationKey) -> bool {
        bump_invalidation_epoch(&self.invalidation_epoch);
        let mut state = write_lock(&self.state);
        let Some(index) = state.index.remove(&key) else {
            return false;
        };
        let Some(bytes) = state.bytes.checked_sub(state.entries[index].bytes.len()) else {
            debug_assert!(false, "prepared cache byte accounting underflow");
            return false;
        };
        state.bytes = bytes;
        state.entries.swap_remove(index);
        if let Some(moved) = state.entries.get(index) {
            let moved_key = moved.key;
            state.index.insert(moved_key, index);
        }
        state.invalidations = state.invalidations.saturating_add(1);
        true
    }

    /// Invalidates every request issued by one authority digest.
    #[must_use]
    pub fn invalidate_authority(&self, authority: SessionId) -> usize {
        self.invalidate_where(|key| key.authority() == authority)
    }

    /// Invalidates every request using one revoked toolchain version.
    #[must_use]
    pub fn invalidate_toolchain(&self, toolchain: ToolchainId) -> usize {
        self.invalidate_where(|key| key.toolchain() == toolchain)
    }

    /// Invalidates every request for one source manifest version.
    #[must_use]
    pub fn invalidate_manifest(&self, manifest: InputManifestId) -> usize {
        self.invalidate_where(|key| key.manifest() == manifest)
    }

    /// Invalidates all prepared requests, advancing no generation.
    #[must_use]
    pub fn clear(&self) -> usize {
        bump_invalidation_epoch(&self.invalidation_epoch);
        let mut state = write_lock(&self.state);
        let removed = state.entries.len();
        if removed != 0 {
            state.entries.clear();
            state.index.clear();
            state.bytes = 0;
            state.invalidations = state.invalidations.saturating_add(removed as u64);
        }
        removed
    }

    /// Returns cache accounting without touching resident entries.
    #[must_use]
    pub fn stats(&self) -> PreparationCacheStats {
        let state = read_lock(&self.state);
        PreparationCacheStats {
            resident: state.entries.len(),
            bytes: state.bytes,
            hits: self.hits.load(Ordering::Relaxed),
            misses: state.misses,
            evictions: state.evictions,
            invalidations: state.invalidations,
            generation: state.generation,
        }
    }

    fn invalidate_where(&self, predicate: impl Fn(PreparationKey) -> bool) -> usize {
        bump_invalidation_epoch(&self.invalidation_epoch);
        let mut state = write_lock(&self.state);
        let before = state.entries.len();
        let removed_keys = state
            .entries
            .iter()
            .filter(|entry| predicate(entry.key))
            .map(|entry| entry.key)
            .collect::<Vec<_>>();
        if removed_keys.is_empty() {
            return 0;
        }
        let mut bytes = 0_usize;
        state.entries.retain(|entry| {
            if predicate(entry.key) {
                bytes = bytes.checked_add(entry.bytes.len()).unwrap_or_else(|| {
                    debug_assert!(false, "prepared cache byte accounting overflow");
                    usize::MAX
                });
                false
            } else {
                true
            }
        });
        let keys = state
            .entries
            .iter()
            .map(|entry| entry.key)
            .collect::<Vec<_>>();
        state.index.clear();
        for (index, key) in keys.into_iter().enumerate() {
            state.index.insert(key, index);
        }
        let removed = before - state.entries.len();
        let Some(remaining) = state.bytes.checked_sub(bytes) else {
            debug_assert!(false, "prepared cache byte accounting underflow");
            return removed;
        };
        state.bytes = remaining;
        state.invalidations = state.invalidations.saturating_add(removed as u64);
        removed
    }
}

pub(super) fn evict(state: &mut CacheState, config: PreparationCacheConfig) {
    while state.entries.len() > config.max_entries || state.bytes > config.max_bytes {
        let Some(index) = state
            .entries
            .iter()
            .enumerate()
            .min_by_key(|(_, entry)| (entry.generation, entry.last_used.load(Ordering::Relaxed)))
            .map(|(index, _)| index)
        else {
            break;
        };
        let Some(bytes) = state.bytes.checked_sub(state.entries[index].bytes.len()) else {
            debug_assert!(false, "prepared cache byte accounting underflow");
            break;
        };
        state.bytes = bytes;
        let evicted_key = state.entries[index].key;
        state.index.remove(&evicted_key);
        state.entries.swap_remove(index);
        if let Some(moved) = state.entries.get(index) {
            let moved_key = moved.key;
            state.index.insert(moved_key, index);
        }
        state.evictions = state.evictions.saturating_add(1);
    }
}

fn read_lock<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    match lock.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub(super) fn mutex_lock<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match lock.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn bump_invalidation_epoch(epoch: &AtomicU64) {
    let _ = epoch.fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
        value.checked_add(1)
    });
}

pub(super) fn write_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    match lock.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
