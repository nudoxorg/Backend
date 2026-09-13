use super::prepared_cache::{
    CacheState, PreparationCache, PreparationCacheConfig, PreparationFlight, PreparedEntry,
    PreparedRequest, evict, mutex_lock, write_lock,
};
use super::prepared_error::PreparationError;
use super::prepared_key::PreparationKey;
use crate::{NativeRequest, NativeRequestInput};
use std::collections::BTreeMap;
use std::sync::{
    Arc, Condvar, Mutex, RwLock,
    atomic::{AtomicU64, Ordering},
};
impl PreparationCache {
    /// Creates a bounded preparation cache.
    #[must_use]
    pub fn new(config: PreparationCacheConfig) -> Self {
        Self {
            config,
            state: Arc::new(RwLock::new(CacheState::default())),
            flights: Arc::new(Mutex::new(BTreeMap::new())),
            hits: Arc::new(AtomicU64::new(0)),
            clock: Arc::new(AtomicU64::new(0)),
            invalidation_epoch: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Returns the configured cache bounds.
    #[must_use]
    pub const fn config(&self) -> PreparationCacheConfig {
        self.config
    }

    /// Prepares a request, or borrows an already prepared request with the
    /// same complete version key.
    ///
    /// The supplied language and inputs must reproduce the digests in `key`;
    /// this prevents a caller from aliasing a prepared body under a different
    /// request while retaining zero-copy borrowing on a valid hit.
    ///
    /// # Errors
    ///
    /// Returns [`PreparationError`] when request encoding fails or the body
    /// exceeds the configured bound.
    pub fn prepare(
        &self,
        key: PreparationKey,
        language: &str,
        inputs: Vec<NativeRequestInput>,
    ) -> Result<PreparedRequest<'_>, PreparationError> {
        if !key.matches_request(language, &inputs) {
            return Err(PreparationError::KeyMismatch);
        }
        self.prepare_verified(&key, language, inputs)
    }

    /// Prepares a request using a lazy input builder. The builder is never
    /// called for a resident key, allowing warm callers to avoid constructing
    /// or copying input bodies entirely.
    ///
    /// # Errors
    ///
    /// Returns [`PreparationError`] when the language or built inputs do not
    /// match the key, or when request encoding exceeds the configured bound.
    pub fn prepare_with<F>(
        &self,
        key: PreparationKey,
        language: &str,
        build_inputs: F,
    ) -> Result<PreparedRequest<'_>, PreparationError>
    where
        F: FnOnce() -> Vec<NativeRequestInput>,
    {
        if let Some(request) = self.borrow(key) {
            return Ok(request);
        }
        if !key.language_matches(language) {
            return Err(PreparationError::KeyMismatch);
        }
        let (flight, leader) = {
            let mut flights = mutex_lock(&self.flights);
            if let Some(flight) = flights.get(&key) {
                (Arc::clone(flight), false)
            } else if let Some(request) = self.borrow_inner(&key, true) {
                // A preceding leader can publish and retire its flight after
                // our optimistic lookup but before we acquire `flights`.
                // Rechecking while holding the flight-table lock closes that
                // gap: no second leader can form for an already resident key.
                return Ok(request);
            } else {
                let flight = Arc::new(PreparationFlight {
                    state: Mutex::new(None),
                    wake: Condvar::new(),
                });
                flights.insert(key, Arc::clone(&flight));
                (flight, true)
            }
        };
        if !leader {
            let mut result = mutex_lock(&flight.state);
            while result.is_none() {
                result = match flight.wake.wait(result) {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
            }
            result.as_ref().ok_or(PreparationError::Overflow)?.clone()?;
            return self
                .borrow_inner(&key, false)
                .ok_or(PreparationError::Capacity);
        }
        let epoch = self.invalidation_epoch.load(Ordering::Acquire);
        let result = {
            let inputs = build_inputs();
            if key.matches_request(language, &inputs) {
                self.encode_and_publish(&key, language, inputs, epoch)
            } else {
                Err(PreparationError::KeyMismatch)
            }
        };
        let mut flight_result = mutex_lock(&flight.state);
        *flight_result = Some(result.clone());
        flight.wake.notify_all();
        drop(flight_result);
        mutex_lock(&self.flights).remove(&key);
        result?;
        self.borrow_inner(&key, false)
            .ok_or(PreparationError::Capacity)
    }

    pub(crate) fn prepare_verified(
        &self,
        key: &PreparationKey,
        language: &str,
        inputs: Vec<NativeRequestInput>,
    ) -> Result<PreparedRequest<'_>, PreparationError> {
        if let Some(request) = self.borrow(*key) {
            return Ok(request);
        }
        let (flight, leader) = {
            let mut flights = mutex_lock(&self.flights);
            if let Some(flight) = flights.get(key) {
                (Arc::clone(flight), false)
            } else if let Some(request) = self.borrow_inner(key, true) {
                // Pair the cache recheck with flight creation.  Without this
                // second observation, a caller that lost the publication race
                // could install a new flight after the original was retired
                // and redundantly encode the same immutable request.
                return Ok(request);
            } else {
                let flight = Arc::new(PreparationFlight {
                    state: Mutex::new(None),
                    wake: Condvar::new(),
                });
                flights.insert(*key, Arc::clone(&flight));
                (flight, true)
            }
        };
        if !leader {
            let mut result = mutex_lock(&flight.state);
            while result.is_none() {
                result = match flight.wake.wait(result) {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
            }
            let Some(result) = result.as_ref() else {
                return Err(PreparationError::Overflow);
            };
            result.clone()?;
            return self
                .borrow_inner(key, false)
                .ok_or(PreparationError::Capacity);
        }

        let epoch = self.invalidation_epoch.load(Ordering::Acquire);
        let result = self.encode_and_publish(key, language, inputs, epoch);
        let mut flight_result = mutex_lock(&flight.state);
        *flight_result = Some(result.clone());
        flight.wake.notify_all();
        drop(flight_result);
        mutex_lock(&self.flights).remove(key);
        result?;
        self.borrow_inner(key, false)
            .ok_or(PreparationError::Capacity)
    }

    fn encode_and_publish(
        &self,
        key: &PreparationKey,
        language: &str,
        inputs: Vec<NativeRequestInput>,
        epoch: u64,
    ) -> Result<(), PreparationError> {
        let bytes = NativeRequest::new(language, key.session(), inputs)?.encode()?;
        if bytes.len() > self.config.max_bytes {
            return Err(PreparationError::Capacity);
        }
        let bytes: Arc<[u8]> = bytes.into();
        let mut state = write_lock(&self.state);
        if self.invalidation_epoch.load(Ordering::Acquire) != epoch {
            return Err(PreparationError::Invalidated);
        }
        state.misses = state.misses.saturating_add(1);
        if let Some(&index) = state.index.get(key) {
            let tick = self.clock.fetch_add(1, Ordering::Relaxed).saturating_add(1);
            state.entries[index]
                .last_used
                .store(tick, Ordering::Relaxed);
        } else {
            let generation = state
                .generation
                .checked_add(1)
                .ok_or(PreparationError::Overflow)?;
            let last_used = self
                .clock
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |clock| {
                    clock.checked_add(1)
                })
                .map_err(|_| PreparationError::Overflow)?
                .saturating_add(1);
            let resident_bytes = state
                .bytes
                .checked_add(bytes.len())
                .ok_or(PreparationError::Overflow)?;
            state.generation = generation;
            state.clock = last_used;
            state.bytes = resident_bytes;
            state.entries.push(PreparedEntry {
                key: *key,
                bytes,
                generation,
                last_used: AtomicU64::new(last_used),
            });
            let index = state.entries.len() - 1;
            state.index.insert(*key, index);
            evict(&mut state, self.config);
        }
        Ok(())
    }
}
