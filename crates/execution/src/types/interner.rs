use super::{CompletionReceipt, OutputVersion, WorkKey};
use crate::{CancelHandle, Cancellation};
use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Mutex},
};

/// An interner admission role. The handle owns one live demand reference and
/// releases it on drop; a terminal leader completion removes the key even if
/// other followers still retain read-only handles.
#[must_use = "retain the interner handle until the work reaches a terminal outcome"]
pub struct Interned {
    interner: Arc<WorkInternerInner>,
    key: WorkKey,
    generation: u64,
    leader: bool,
    terminal: bool,
    cancellation: Cancellation,
    cancel_handle: CancelHandle,
}

impl fmt::Debug for Interned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Interned")
            .field("key", &self.key)
            .field("generation", &self.generation)
            .field("leader", &self.leader)
            .field("terminal", &self.terminal)
            .field("cancelled", &self.cancellation.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl Interned {
    /// Returns the coalesced work key.
    #[must_use]
    pub const fn key(&self) -> WorkKey {
        self.key
    }

    /// Returns whether this handle owns the one leader role.
    #[must_use]
    pub const fn is_leader(&self) -> bool {
        self.leader
    }

    /// Returns this demand's cancellation observation.
    #[must_use]
    pub fn cancellation(&self) -> Cancellation {
        self.cancellation.clone()
    }

    /// Cancels this follower demand without cancelling the leader attempt.
    pub fn cancel(&self) {
        self.cancel_handle.cancel();
    }

    /// Returns whether this demand has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Returns whether this handle observes a terminal outcome.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        if self.terminal {
            return true;
        }
        let state = self
            .interner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.entries.get(&self.key) {
            Some(entry) if entry.generation == self.generation => {
                entry.status != InternerStatus::Live
            }
            None | Some(_) => true,
        }
    }

    /// Returns the published output while this handle retains the terminal
    /// entry. Followers can read the same immutable output without rerunning
    /// the work.
    #[must_use]
    pub fn output(&self) -> Option<OutputVersion> {
        self.interner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .get(&self.key)
            .filter(|entry| entry.generation == self.generation)
            .and_then(|entry| entry.output)
    }

    /// Returns the retained canonical output after the leader publishes it.
    /// Followers receive the same immutable owner as the reusable lookup.
    #[must_use]
    pub fn reusable_output(&self) -> Option<crate::ReusableOutput> {
        self.interner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .get(&self.key)
            .filter(|entry| entry.generation == self.generation)
            .and_then(|entry| entry.reusable.clone())
    }

    /// Marks the leader's pure work complete with an admitted output and
    /// releases the live state.
    ///
    /// The returned receipt is immutable and can be shared by followers. A
    /// follower cannot complete shared work.
    /// # Errors
    ///
    /// Returns [`InternError::NotLeader`] for a follower, or
    /// [`InternError::AlreadyTerminal`] when the entry was completed or
    /// abandoned before this call. The admitted capability is consumed even
    /// when the entry has raced to a terminal state.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "completion consumes the output admission capability"
    )]
    pub fn complete(
        self,
        admission: crate::OutputAdmission,
    ) -> Result<CompletionReceipt, InternError> {
        self.prepare_complete(admission.output())?;
        Ok(self.complete_prepared(admission))
    }

    /// Prevalidates the leader transition before a scheduler publication
    /// changes attempt state. The leader owns the only handle that can make a
    /// live entry terminal, so a successful preflight makes the following
    /// [`Self::complete_prepared`] commit infallible under normal operation.
    pub(crate) fn prepare_complete(&self, output: OutputVersion) -> Result<(), InternError> {
        if !self.leader || self.terminal {
            return Err(InternError::NotLeader);
        }
        let state = self
            .interner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = state
            .entries
            .get(&self.key)
            .filter(|entry| entry.generation == self.generation)
            .ok_or(InternError::Unknown)?;
        if entry.status != InternerStatus::Live {
            return Err(InternError::AlreadyTerminal);
        }
        let _ = output;
        Ok(())
    }

    /// Commits a previously prevalidated leader completion. This method has
    /// no fallible return after the scheduler has accepted the attempt; a
    /// missing entry is treated as an invariant violation and still yields a
    /// typed terminal completion receipt so callers never observe an ordinary
    /// error after publication.
    pub(crate) fn complete_prepared(self, admission: crate::OutputAdmission) -> CompletionReceipt {
        self.complete_prepared_with_reusable(admission, None)
    }

    /// Commits a leader completion and publishes its retained output to
    /// bounded followers waiting on the same work key.
    pub(crate) fn complete_prepared_with_reusable(
        mut self,
        admission: crate::OutputAdmission,
        reusable: Option<crate::ReusableOutput>,
    ) -> CompletionReceipt {
        let output = admission.output();
        drop(admission);
        let mut state = self
            .interner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let remove = if let Some(entry) = state
            .entries
            .get_mut(&self.key)
            .filter(|entry| entry.generation == self.generation)
        {
            if entry.status == InternerStatus::Live {
                entry.status = InternerStatus::Completed;
                entry.output = Some(output);
                entry.reusable = reusable;
                self.terminal = true;
                entry.followers == 0
            } else {
                // The successful preflight and affine leader ownership make
                // this branch unreachable unless an invariant is corrupted.
                self.terminal = true;
                false
            }
        } else {
            self.terminal = true;
            false
        };
        if remove {
            state.entries.remove(&self.key);
        }
        CompletionReceipt {
            key: self.key,
            output,
        }
    }

    /// Terminates the leader without publishing an output.
    /// # Errors
    ///
    /// Returns [`InternError::NotLeader`] for a follower, or
    /// [`InternError::AlreadyTerminal`] when another terminal transition won.
    pub fn terminate(mut self) -> Result<(), InternError> {
        if !self.leader || self.terminal {
            return Err(InternError::NotLeader);
        }
        let mut state = self
            .interner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let remove = {
            let Some(entry) = state
                .entries
                .get_mut(&self.key)
                .filter(|entry| entry.generation == self.generation)
            else {
                return Err(InternError::Unknown);
            };
            if entry.status != InternerStatus::Live {
                return Err(InternError::AlreadyTerminal);
            }
            entry.status = InternerStatus::Abandoned;
            self.terminal = true;
            entry.followers == 0
        };
        if remove {
            state.entries.remove(&self.key);
        }
        Ok(())
    }
}

impl Drop for Interned {
    fn drop(&mut self) {
        let mut state = self
            .interner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let follower_removed = if self.leader {
            if let Some(entry) = state
                .entries
                .get_mut(&self.key)
                .filter(|entry| entry.generation == self.generation)
                && entry.status == InternerStatus::Live
            {
                entry.status = InternerStatus::Abandoned;
            }
            false
        } else if let Some(entry) = state
            .entries
            .get_mut(&self.key)
            .filter(|entry| entry.generation == self.generation)
        {
            // Every follower is created through the shared relation and
            // therefore contributes exactly one global count.  Drop cannot
            // return an error, so guard the checked transition explicitly
            // instead of hiding underflow with saturation.
            if entry.followers != 0 {
                entry.followers -= 1;
                true
            } else {
                // The handle still owns one global follower demand even if
                // its per-generation row was already retired.
                true
            }
        } else {
            // A terminal row may already have been reclaimed. The handle can
            // no longer touch it, but still owns one global demand projection.
            true
        };
        if follower_removed && let Some(count) = state.follower_count.checked_sub(1) {
            state.follower_count = count;
        }
        let should_remove = state.entries.get(&self.key).is_some_and(|entry| {
            entry.generation == self.generation
                && entry.status != InternerStatus::Live
                && entry.followers == 0
        });
        if should_remove {
            state.entries.remove(&self.key);
        }
    }
}

#[derive(Clone, Debug)]
struct InternerEntry {
    generation: u64,
    status: InternerStatus,
    followers: usize,
    output: Option<OutputVersion>,
    reusable: Option<crate::ReusableOutput>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InternerStatus {
    Live,
    Completed,
    Abandoned,
}

struct WorkInternerInner {
    state: Mutex<InternerState>,
}

struct InternerState {
    entries: BTreeMap<WorkKey, InternerEntry>,
    next_generation: u64,
    capacity: usize,
    follower_capacity: usize,
    /// Total follower demand across all work keys.  Keeping this projection
    /// in the same lock as entries makes capacity checks O(1) and gives the
    /// legacy [`crate::WaiterTable`] view the exact same accounting.
    follower_count: usize,
}

fn increment_follower(state: &mut InternerState, key: WorkKey) -> Result<(), InternError> {
    if state.follower_count >= state.follower_capacity {
        return Err(InternError::FollowersFull);
    }
    let next_total = state
        .follower_count
        .checked_add(1)
        .ok_or(InternError::Overflow)?;
    let next_followers = state
        .entries
        .get(&key)
        .and_then(|entry| entry.followers.checked_add(1))
        .ok_or(InternError::Overflow)?;
    if let Some(entry) = state.entries.get_mut(&key) {
        entry.followers = next_followers;
    } else {
        return Err(InternError::Full);
    }
    state.follower_count = next_total;
    Ok(())
}

fn allocate_generation(state: &mut InternerState) -> Result<u64, InternError> {
    let generation = state.next_generation;
    state.next_generation = generation.checked_add(1).ok_or(InternError::Overflow)?;
    Ok(generation)
}

/// Shared live-work table.
pub struct WorkInterner {
    inner: Arc<WorkInternerInner>,
}

impl fmt::Debug for WorkInterner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f.debug_struct("WorkInterner")
            .field("capacity", &state.capacity)
            .field("follower_capacity", &state.follower_capacity)
            .field("followers", &state.follower_count)
            .field("entries", &state.entries.len())
            .finish()
    }
}

impl WorkInterner {
    /// Creates an interner with bounded live entries and followers. Live
    /// entries are never evicted to make room for a new key.
    #[must_use]
    pub fn new(capacity: usize, followers: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(WorkInternerInner {
                state: Mutex::new(InternerState {
                    entries: BTreeMap::new(),
                    next_generation: 1,
                    capacity,
                    follower_capacity: followers,
                    follower_count: 0,
                }),
            }),
        })
    }

    /// Coalesces a request into an existing live entry or creates its leader.
    /// # Errors
    ///
    /// Returns [`InternError::Full`] when the live entry envelope is full,
    /// [`InternError::FollowersFull`] for a bounded follower overflow, or an
    /// arithmetic error while updating the entry.
    pub fn intern(&self, key: WorkKey) -> Result<Interned, InternError> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((status, generation)) = state
            .entries
            .get(&key)
            .map(|entry| (entry.status, entry.generation))
        {
            if status == InternerStatus::Live {
                increment_follower(&mut state, key)?;
                let (cancellation, cancel_handle) = Cancellation::new();
                return Ok(Interned {
                    interner: Arc::clone(&self.inner),
                    key,
                    generation,
                    leader: false,
                    terminal: false,
                    cancellation,
                    cancel_handle,
                });
            }
            if status == InternerStatus::Completed
                && state
                    .entries
                    .get(&key)
                    .is_some_and(|entry| entry.followers != 0)
            {
                increment_follower(&mut state, key)?;
                let (cancellation, cancel_handle) = Cancellation::new();
                return Ok(Interned {
                    interner: Arc::clone(&self.inner),
                    key,
                    generation,
                    leader: false,
                    terminal: true,
                    cancellation,
                    cancel_handle,
                });
            }
            if status == InternerStatus::Abandoned {
                // The generation belongs to the coalesced logical work, not
                // to one transient leader.  Transfer ownership in place so
                // followers of an abandoned owner remain attached to the
                // eventual result while the affine leader role still has one
                // winner. A row with no followers is removed by `Drop`, so a
                // later, unrelated admission receives a fresh generation.
                let entry = state.entries.get_mut(&key).ok_or(InternError::Unknown)?;
                entry.status = InternerStatus::Live;
                entry.output = None;
                entry.reusable = None;
                let (cancellation, cancel_handle) = Cancellation::new();
                return Ok(Interned {
                    interner: Arc::clone(&self.inner),
                    key,
                    generation,
                    leader: true,
                    terminal: false,
                    cancellation,
                    cancel_handle,
                });
            }
            state.entries.remove(&key);
        }
        if state.entries.len() >= state.capacity {
            return Err(InternError::Full);
        }
        let generation = allocate_generation(&mut state)?;
        state.entries.insert(
            key,
            InternerEntry {
                generation,
                status: InternerStatus::Live,
                followers: 0,
                output: None,
                reusable: None,
            },
        );
        let (cancellation, cancel_handle) = Cancellation::new();
        Ok(Interned {
            interner: Arc::clone(&self.inner),
            key,
            generation,
            leader: true,
            terminal: false,
            cancellation,
            cancel_handle,
        })
    }

    /// Returns a read-only follower for a known key.
    /// # Errors
    ///
    /// Returns [`crate::WaiterError::Unknown`] when no live or retained
    /// completed entry exists, or [`crate::WaiterError::Full`] when follower
    /// demand is at capacity. This method never creates a leader.
    pub fn register_waiter(&self, key: WorkKey) -> Result<Interned, crate::WaiterError> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some((status, generation)) = state
            .entries
            .get(&key)
            .map(|entry| (entry.status, entry.generation))
        else {
            return Err(crate::WaiterError::Unknown);
        };
        let terminal = match status {
            InternerStatus::Live => false,
            InternerStatus::Completed => true,
            InternerStatus::Abandoned => return Err(crate::WaiterError::Unknown),
        };
        increment_follower(&mut state, key).map_err(|error| match error {
            InternError::Overflow => crate::WaiterError::Overflow,
            InternError::FollowersFull | InternError::Full => crate::WaiterError::Full,
            InternError::NotLeader | InternError::Unknown | InternError::AlreadyTerminal => {
                crate::WaiterError::Unknown
            }
        })?;
        let (cancellation, cancel_handle) = Cancellation::new();
        Ok(Interned {
            interner: Arc::clone(&self.inner),
            key,
            generation,
            leader: false,
            terminal,
            cancellation,
            cancel_handle,
        })
    }

    /// Registers a follower through the shared demand relation.  This is
    /// used by the compatibility [`crate::WaiterTable`] wrapper, which may
    /// observe a key before a leader exists.  Such an orphan row is marked
    /// abandoned and is reclaimed by the same affine drop path as ordinary
    /// interner followers.
    pub(crate) fn register_external_waiter(
        &self,
        key: WorkKey,
    ) -> Result<Interned, crate::WaiterError> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.entries.contains_key(&key) {
            if state.entries.len() >= state.capacity {
                return Err(crate::WaiterError::Full);
            }
            let generation = allocate_generation(&mut state).map_err(|error| match error {
                InternError::Overflow => crate::WaiterError::Overflow,
                _ => crate::WaiterError::Full,
            })?;
            state.entries.insert(
                key,
                InternerEntry {
                    generation,
                    status: InternerStatus::Abandoned,
                    followers: 0,
                    output: None,
                    reusable: None,
                },
            );
        }
        let (status, generation) = state
            .entries
            .get(&key)
            .map(|entry| (entry.status, entry.generation))
            .ok_or(crate::WaiterError::Unknown)?;
        increment_follower(&mut state, key).map_err(|error| match error {
            InternError::Overflow => crate::WaiterError::Overflow,
            InternError::FollowersFull | InternError::Full => crate::WaiterError::Full,
            InternError::NotLeader | InternError::Unknown | InternError::AlreadyTerminal => {
                crate::WaiterError::Unknown
            }
        })?;
        let (cancellation, cancel_handle) = Cancellation::new();
        Ok(Interned {
            interner: Arc::clone(&self.inner),
            key,
            generation,
            leader: false,
            terminal: !matches!(status, InternerStatus::Live),
            cancellation,
            cancel_handle,
        })
    }

    /// Returns total follower demand in O(1) from the shared relation
    /// projection.
    #[must_use]
    pub(crate) fn follower_len(&self) -> usize {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .follower_count
    }

    /// Returns a completed output while its terminal entry remains retained.
    #[must_use]
    pub fn output(&self, key: WorkKey) -> Option<OutputVersion> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .get(&key)
            .and_then(|entry| entry.output)
    }

    /// Number of live or follower-retained entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .len()
    }

    /// Whether the table currently has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Failure while coalescing or terminally completing work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InternError {
    /// The live-entry envelope is full.
    Full,
    /// The follower envelope is full.
    FollowersFull,
    /// An arithmetic operation needed to update a reservation overflowed.
    Overflow,
    /// A requested terminal operation was not performed by the leader.
    NotLeader,
    /// The key has no retained interner entry.
    Unknown,
    /// The entry has already reached a terminal state.
    AlreadyTerminal,
}

impl fmt::Display for InternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "work interner error: {self:?}")
    }
}

impl std::error::Error for InternError {}
