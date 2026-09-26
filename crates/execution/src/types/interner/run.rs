//! `WorkInterner` construction, coalescing, and lookup methods.
use super::*;

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
