//! Bounded reuse of live native authority sessions.
//!
//! A persistent helper is an expensive compilation resource.  This cache
//! keeps at most one live session per complete preparation key and serializes
//! only requests using that same session.  Different authority/version keys
//! can proceed independently.  A protocol, cancellation, or process failure
//! retires the slot before the next caller can accidentally reuse it.

use crate::{
    Cancellation, InputManifestId, NativeAuthorityRunner, NativeObservation, NativeRunnerError,
    PersistentNativeSession, PreparationKey,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::{thread, time::Duration};

/// Bounds for the live persistent session cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionCacheConfig {
    /// Maximum number of resident helper processes.
    pub max_sessions: usize,
}

impl SessionCacheConfig {
    /// Creates session cache bounds.
    ///
    /// # Errors
    ///
    /// Returns [`SessionCacheError::Capacity`] for a zero bound.
    pub const fn new(max_sessions: usize) -> Result<Self, SessionCacheError> {
        if max_sessions == 0 {
            return Err(SessionCacheError::Capacity);
        }
        Ok(Self { max_sessions })
    }
}

/// A live-session cache failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionCacheError {
    /// The cache has no configured or idle capacity.
    Capacity,
    /// The runner was not bound to the requested preparation key.
    KeyMismatch,
    /// The persistent helper could not be started or answered.
    Runner(NativeRunnerError),
    /// The bounded session generation counter overflowed.
    Overflow,
}

impl std::fmt::Display for SessionCacheError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Capacity => formatter.write_str("persistent session cache has no capacity"),
            Self::KeyMismatch => {
                formatter.write_str("persistent session key is not preparation-bound")
            }
            Self::Runner(error) => write!(formatter, "persistent session failed: {error}"),
            Self::Overflow => formatter.write_str("persistent session generation overflowed"),
        }
    }
}

impl std::error::Error for SessionCacheError {}

/// A point-in-time live-session cache snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionCacheStats {
    /// Number of resident sessions.
    pub resident: usize,
    /// Number of requests served by an already live process (a metric that
    /// saturates at the maximum counter value).
    pub hits: u64,
    /// Number of process startups (a metric that saturates at the maximum
    /// counter value).
    pub starts: u64,
    /// Number of retired sessions (a metric that saturates at the maximum
    /// counter value).
    pub evictions: u64,
    /// Number of sessions retired after an execution failure (a metric that
    /// saturates at the maximum counter value).
    pub failures: u64,
}

/// One manifest-bound request submitted to a persistent session cache.
#[derive(Clone, Copy)]
pub struct PersistentRequest<'a> {
    key: PreparationKey,
    runner: &'a NativeAuthorityRunner,
    manifest: InputManifestId,
    revision: u64,
    payload: &'a [u8],
    cancellation: &'a Cancellation,
}

impl<'a> PersistentRequest<'a> {
    /// Creates a request descriptor. The key is checked against the runner
    /// when the cache admits or reuses a process.
    #[must_use]
    pub const fn new(
        key: PreparationKey,
        runner: &'a NativeAuthorityRunner,
        manifest: InputManifestId,
        revision: u64,
        payload: &'a [u8],
        cancellation: &'a Cancellation,
    ) -> Self {
        Self {
            key,
            runner,
            manifest,
            revision,
            payload,
            cancellation,
        }
    }
}

struct SessionSlot {
    key: PreparationKey,
    session: Arc<Mutex<PersistentNativeSession>>,
    generation: u64,
    in_flight: usize,
    retiring: bool,
}

#[derive(Default)]
struct SessionState {
    slots: Vec<SessionSlot>,
    /// Process starts which have reserved a slot but have not published yet.
    /// This reservation is made under the small state lock; startup itself is
    /// deliberately performed after releasing it.
    starting: usize,
    generation: u64,
    hits: u64,
    starts: u64,
    evictions: u64,
    failures: u64,
}

struct StartupGate {
    map: Arc<Mutex<BTreeMap<PreparationKey, Arc<Mutex<()>>>>>,
    key: PreparationKey,
    lock: Arc<Mutex<()>>,
}

impl Drop for StartupGate {
    fn drop(&mut self) {
        // A cancelled waiter must not remove a lock still used by the leader.
        // The map owns one Arc and each active gate owns another.
        if Arc::strong_count(&self.lock) == 2 {
            lock(&self.map).remove(&self.key);
        }
    }
}

/// A bounded cache of live, key-bound persistent native sessions.
#[derive(Clone)]
pub struct PersistentSessionCache {
    config: SessionCacheConfig,
    state: Arc<Mutex<SessionState>>,
    startup: Arc<Mutex<BTreeMap<PreparationKey, Arc<Mutex<()>>>>>,
}

impl std::fmt::Debug for PersistentSessionCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PersistentSessionCache")
            .field("config", &self.config)
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

impl PersistentSessionCache {
    /// Creates an empty bounded live-session cache.
    #[must_use]
    pub fn new(config: SessionCacheConfig) -> Self {
        Self {
            config,
            state: Arc::new(Mutex::new(SessionState::default())),
            startup: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Sends one request through a reused or newly started persistent session.
    ///
    /// The runner's command must carry the exact session key.  A failed
    /// request removes that slot, ensuring cancellation and protocol faults
    /// force a cold start on the next call.
    ///
    /// # Errors
    ///
    /// Returns [`SessionCacheError`] when key admission, startup, or request
    /// handling fails.
    #[allow(
        clippy::too_many_lines,
        reason = "request owns the complete session admission and release transaction"
    )]
    pub fn request(
        &self,
        request: PersistentRequest<'_>,
    ) -> Result<NativeObservation, SessionCacheError> {
        if request.cancellation.is_cancelled() {
            return Err(SessionCacheError::Runner(NativeRunnerError::Process(
                crate::ProcessError::Cancelled,
            )));
        }
        if request.runner.command().session_key() != Some(request.key.session())
            || request.manifest != request.key.manifest()
        {
            return Err(SessionCacheError::KeyMismatch);
        }
        let startup_lock = {
            let mut startup = lock(&self.startup);
            Arc::clone(
                startup
                    .entry(request.key)
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        let startup_gate = StartupGate {
            map: Arc::clone(&self.startup),
            key: request.key,
            lock: startup_lock,
        };
        let _startup_guard = lock_cancellable(&startup_gate.lock, request.cancellation)?;
        let session = {
            let mut state = lock(&self.state);
            if let Some(index) = state.slots.iter().position(|slot| slot.key == request.key) {
                let session = Arc::clone(&state.slots[index].session);
                state.slots[index].in_flight = state.slots[index]
                    .in_flight
                    .checked_add(1)
                    .ok_or(SessionCacheError::Overflow)?;
                state.hits = state.hits.saturating_add(1);
                session
            } else {
                let mut occupied = state
                    .slots
                    .len()
                    .checked_add(state.starting)
                    .ok_or(SessionCacheError::Overflow)?;
                if occupied >= self.config.max_sessions {
                    evict_idle(&mut state, self.config.max_sessions.saturating_sub(1));
                    occupied = state
                        .slots
                        .len()
                        .checked_add(state.starting)
                        .ok_or(SessionCacheError::Overflow)?;
                }
                if occupied >= self.config.max_sessions {
                    return Err(SessionCacheError::Capacity);
                }
                state.starting = state
                    .starting
                    .checked_add(1)
                    .ok_or(SessionCacheError::Overflow)?;
                drop(state);
                let started = match request
                    .runner
                    .start_persistent_with_cancellation(request.cancellation)
                {
                    Ok(started) => started,
                    Err(error) => {
                        let mut state = lock(&self.state);
                        state.starting = state.starting.saturating_sub(1);
                        return Err(SessionCacheError::Runner(error));
                    }
                };
                let mut state = lock(&self.state);
                state.starting = state.starting.saturating_sub(1);
                let generation = state
                    .generation
                    .checked_add(1)
                    .ok_or(SessionCacheError::Overflow)?;
                state.generation = generation;
                state.starts = state.starts.saturating_add(1);
                let session = Arc::new(Mutex::new(started));
                state.slots.push(SessionSlot {
                    key: request.key,
                    session: Arc::clone(&session),
                    generation,
                    in_flight: 1,
                    retiring: false,
                });
                session
            }
        };
        let mut session_guard = match lock_session(&session, request.cancellation) {
            Ok(guard) => guard,
            Err(error) => {
                self.release(&request.key, &session, true);
                return Err(error);
            }
        };
        let result = session_guard.request_with_payload_with_cancellation(
            request.manifest,
            request.revision,
            request.payload,
            request.cancellation,
        );
        drop(session_guard);
        self.release(&request.key, &session, result.is_err());
        result.map_err(SessionCacheError::Runner)
    }

    fn release(
        &self,
        key: &PreparationKey,
        session: &Arc<Mutex<PersistentNativeSession>>,
        failed: bool,
    ) {
        let mut state = lock(&self.state);
        if let Some(index) = state
            .slots
            .iter()
            .position(|slot| slot.key == *key && Arc::ptr_eq(&slot.session, session))
        {
            let remove = {
                let slot = &mut state.slots[index];
                if slot.in_flight == 0 {
                    return;
                }
                slot.in_flight -= 1;
                slot.retiring || failed
            };
            if remove {
                state.slots.swap_remove(index);
                if failed {
                    state.failures = state.failures.saturating_add(1);
                }
                state.evictions = state.evictions.saturating_add(1);
            }
        }
        drop(state);
    }

    /// Retires one live session, terminating it when no request holds it.
    #[must_use]
    pub fn invalidate(&self, key: PreparationKey) -> bool {
        let mut state = lock(&self.state);
        let Some(index) = state.slots.iter().position(|slot| slot.key == key) else {
            return false;
        };
        if state.slots[index].in_flight == 0 {
            state.slots.swap_remove(index);
        } else {
            state.slots[index].retiring = true;
        }
        state.evictions = state.evictions.saturating_add(1);
        true
    }

    /// Retires all sessions for one authority digest.
    #[must_use]
    pub fn invalidate_authority(&self, authority: crate::SessionId) -> usize {
        self.invalidate_where(|key| key.authority() == authority)
    }

    /// Retires all sessions using one revoked toolchain version.
    #[must_use]
    pub fn invalidate_toolchain(&self, toolchain: crate::ToolchainId) -> usize {
        self.invalidate_where(|key| key.toolchain() == toolchain)
    }

    /// Returns cache accounting.
    #[must_use]
    pub fn stats(&self) -> SessionCacheStats {
        let state = lock(&self.state);
        SessionCacheStats {
            resident: state.slots.len(),
            hits: state.hits,
            starts: state.starts,
            evictions: state.evictions,
            failures: state.failures,
        }
    }

    fn invalidate_where(&self, predicate: impl Fn(PreparationKey) -> bool) -> usize {
        let mut state = lock(&self.state);
        let before = state.slots.len();
        for slot in &mut state.slots {
            if predicate(slot.key) {
                slot.retiring = true;
            }
        }
        state
            .slots
            .retain(|slot| !slot.retiring || slot.in_flight != 0);
        let removed = before - state.slots.len();
        state.evictions = state.evictions.saturating_add(removed as u64);
        removed
    }
}

fn evict_idle(state: &mut SessionState, target_len: usize) {
    while state.slots.len() > target_len {
        let Some(index) = state
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.in_flight == 0)
            .min_by_key(|(_, slot)| slot.generation)
            .map(|(index, _)| index)
        else {
            break;
        };
        state.slots.swap_remove(index);
        state.evictions = state.evictions.saturating_add(1);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn lock_session<'a>(
    mutex: &'a Mutex<PersistentNativeSession>,
    cancellation: &Cancellation,
) -> Result<MutexGuard<'a, PersistentNativeSession>, SessionCacheError> {
    loop {
        if cancellation.is_cancelled() {
            return Err(SessionCacheError::Runner(NativeRunnerError::Process(
                crate::ProcessError::Cancelled,
            )));
        }
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(2)),
        }
    }
}

fn lock_cancellable<'a, T>(
    mutex: &'a Mutex<T>,
    cancellation: &Cancellation,
) -> Result<MutexGuard<'a, T>, SessionCacheError> {
    loop {
        if cancellation.is_cancelled() {
            return Err(SessionCacheError::Runner(NativeRunnerError::Process(
                crate::ProcessError::Cancelled,
            )));
        }
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(2)),
        }
    }
}
