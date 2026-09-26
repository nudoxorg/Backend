//! Output lookup admission and invalidation.

use super::*;

impl OutputLookup {
    /// Creates an empty reusable output index.
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(4096, 64 * 1024 * 1024)
    }

    /// Creates a reusable-output index with explicit count and byte bounds.
    ///
    /// A zero limit disables retention. The least recently used entry is
    /// evicted when the next publication would exceed either bound; the index
    /// therefore cannot become an unbounded hidden output store.
    #[must_use]
    pub fn with_limits(max_entries: u64, max_bytes: u64) -> Self {
        Self {
            state: Mutex::new(LookupState {
                entries: BTreeMap::new(),
                recency: BTreeSet::new(),
                next_generation: 0,
                retained_bytes: 0,
            }),
            max_entries: match usize::try_from(max_entries) {
                Ok(entries) => entries,
                Err(_) => usize::MAX,
            },
            max_bytes,
        }
    }

    /// Inserts a validated immutable output.
    pub fn insert(&self, output: ReusableOutput) -> Vec<WorkKey> {
        let mut evicted = Vec::new();
        let Some(output_bytes) = u64::try_from(output.canonical_bytes.capacity()).ok() else {
            return evicted;
        };
        if self.max_entries == 0 || output_bytes > self.max_bytes {
            return evicted;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !ensure_generation_capacity(&mut state) {
            return evicted;
        }

        // Plan every removal before mutating the relation. This keeps a
        // corrupted accounting projection from partially evicting entries;
        // all arithmetic is checked and an impossible state fails closed.
        let mut projected_entries = state.entries.len();
        let mut projected_bytes = state.retained_bytes;
        let replacing = state.entries.get(&output.key()).map(|previous| {
            (
                previous.generation,
                u64::try_from(previous.output.canonical_bytes.capacity()),
            )
        });
        if let Some((_, previous_bytes)) = replacing {
            let Ok(previous_bytes) = previous_bytes else {
                return evicted;
            };
            let Some(next_entries) = projected_entries.checked_sub(1) else {
                return evicted;
            };
            projected_entries = next_entries;
            let Some(next_bytes) = projected_bytes.checked_sub(previous_bytes) else {
                return evicted;
            };
            projected_bytes = next_bytes;
        }
        let Some(next_entries) = projected_entries.checked_add(1) else {
            return evicted;
        };
        projected_entries = next_entries;
        let mut planned = Vec::new();
        let mut planned_keys = HashSet::new();
        if let Some((generation, _)) = replacing {
            planned.push((output.key(), generation));
            planned_keys.insert(output.key());
        }
        while projected_entries > self.max_entries
            || projected_bytes
                .checked_add(output_bytes)
                .is_none_or(|bytes| bytes > self.max_bytes)
        {
            let Some(&(generation, key)) = state
                .recency
                .iter()
                .find(|(_, key)| *key != output.key() && !planned_keys.contains(key))
            else {
                return evicted;
            };
            let Some(previous) = state.entries.get(&key) else {
                return evicted;
            };
            if previous.generation != generation {
                return evicted;
            };
            let Ok(previous_bytes) = u64::try_from(previous.output.canonical_bytes.capacity())
            else {
                return evicted;
            };
            let Some(next_bytes) = projected_bytes.checked_sub(previous_bytes) else {
                return evicted;
            };
            projected_bytes = next_bytes;
            let Some(next_entries) = projected_entries.checked_sub(1) else {
                return evicted;
            };
            projected_entries = next_entries;
            planned.push((key, generation));
            planned_keys.insert(key);
            evicted.push(key);
        }
        let Some(final_bytes) = projected_bytes.checked_add(output_bytes) else {
            return Vec::new();
        };
        let Some(generation) = next_generation(&mut state) else {
            return Vec::new();
        };
        for (key, old_generation) in planned {
            state.entries.remove(&key);
            state.recency.remove(&(old_generation, key));
        }
        state.recency.insert((generation, output.key()));
        state
            .entries
            .insert(output.key(), LookupEntry { output, generation });
        state.retained_bytes = final_bytes;
        evicted
    }

    /// Returns whether a canonical payload is eligible for bounded retention.
    /// A full cache can still accept the payload because insertion evicts its
    /// least recently used entries under the same state lock.
    #[must_use]
    pub fn can_retain_len(&self, bytes: usize) -> bool {
        self.can_retain_capacity(bytes)
    }

    /// Returns whether an allocation of the supplied capacity can be
    /// retained under the bounded lookup policy.
    #[must_use]
    pub fn can_retain_capacity(&self, bytes: usize) -> bool {
        self.max_entries != 0
            && u64::try_from(bytes)
                .ok()
                .is_some_and(|bytes| bytes <= self.max_bytes)
    }

    /// Returns the retained output only when the caller presents the exact
    /// authority, revocation, and dependency-generation context emitted by
    /// the winning publication. A stable work key or authority version alone
    /// cannot authorize reuse.
    #[must_use]
    pub fn lookup(&self, key: WorkKey, context: &ReuseContext) -> Option<ReusableOutput> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !ensure_generation_capacity(&mut state) {
            return None;
        }
        let (old_generation, output) = {
            let entry = state.entries.get(&key)?;
            if entry.output.context != *context
                || entry.output.context.key() != key
                || entry.output.context.authority() != context.authority()
            {
                return None;
            }
            (entry.generation, entry.output.clone())
        };
        // Reserve the new recency generation before touching the old index.
        // If the counter cannot advance, the entry remains fully indexed and
        // the caller observes a miss rather than a partially mutated cache.
        let generation = next_generation(&mut state)?;
        state.recency.remove(&(old_generation, key));
        if let Some(entry) = state.entries.get_mut(&key) {
            entry.generation = generation;
        }
        state.recency.insert((generation, key));
        Some(output)
    }

    /// Looks up a result while applying the durable authority fence observed
    /// by the current scheduler incarnation. A persisted context from an
    /// older process or below the current revocation watermark is rejected
    /// before the entry's recency is touched.
    #[must_use]
    pub fn lookup_fenced(
        &self,
        key: WorkKey,
        context: &ReuseContext,
        incarnation: [u8; 32],
        minimum_revocation: u64,
    ) -> Option<ReusableOutput> {
        if !context.valid_for(incarnation, minimum_revocation) {
            return None;
        }
        self.lookup(key, context)
    }

    /// Invalidates every retained result below a durable revocation watermark
    /// or from an older process incarnation.
    pub fn invalidate_fence(&self, incarnation: [u8; 32], minimum_revocation: u64) -> Vec<WorkKey> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let keys = state
            .entries
            .iter()
            .filter(|(_, entry)| {
                !entry
                    .output
                    .context()
                    .valid_for(incarnation, minimum_revocation)
            })
            .map(|(key, _)| *key)
            .collect::<Vec<_>>();
        let removed_bytes = keys.iter().try_fold(0_u64, |total, key| {
            let entry = state.entries.get(key)?;
            let bytes = u64::try_from(entry.output.canonical_bytes.capacity()).ok()?;
            total.checked_add(bytes)
        });
        let Some(removed_bytes) = removed_bytes else {
            return Vec::new();
        };
        let Some(next_bytes) = state.retained_bytes.checked_sub(removed_bytes) else {
            return Vec::new();
        };
        for key in &keys {
            if let Some(entry) = state.entries.remove(key) {
                state.recency.remove(&(entry.generation, *key));
            }
        }
        state.retained_bytes = next_bytes;
        keys
    }

    /// Removes an output after authority revocation or failed revalidation.
    pub fn invalidate(&self, key: WorkKey) -> Option<ReusableOutput> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = state.entries.get(&key)?;
        let bytes = u64::try_from(entry.output.canonical_bytes.capacity()).ok()?;
        let next_bytes = state.retained_bytes.checked_sub(bytes)?;
        let generation = entry.generation;
        let removed = state.entries.remove(&key)?;
        state.recency.remove(&(generation, key));
        state.retained_bytes = next_bytes;
        Some(removed.output)
    }

    /// Number of retained entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .len()
    }

    /// Whether no reusable output is retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Number of bytes retained by reusable entries.
    #[must_use]
    pub fn retained_bytes(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retained_bytes
    }
}
