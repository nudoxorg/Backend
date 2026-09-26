//! Reusable output lookup guarded by semantic key and authority.

use crate::{AuthorityVersion, OutputVersion, ResultReceipt, WorkKey};
use backend_version::Relation;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};

mod run;

/// Engine-owned authority and dependency freshness required to reuse one
/// retained result. The value is emitted by typed receipt publication; callers
/// cannot construct it from a raw epoch or revocation number.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReuseContext {
    key: WorkKey,
    authority: AuthorityVersion,
    authority_epoch: u64,
    revocation_version: u64,
    dependency_generation: u64,
    incarnation: [u8; 32],
}

impl ReuseContext {
    pub(crate) fn from_receipt<R: Relation>(
        receipt: &ResultReceipt<R>,
        dependency_generation: NonZeroU64,
    ) -> Self {
        let evidence = receipt.authority_evidence();
        Self {
            key: receipt.key(),
            authority: evidence.authority(),
            authority_epoch: evidence.authority_epoch(),
            revocation_version: evidence.revocation_version(),
            dependency_generation: dependency_generation.get(),
            incarnation: evidence.incarnation(),
        }
    }

    /// Returns the exact work key covered by this context.
    #[must_use]
    pub const fn key(self) -> WorkKey {
        self.key
    }

    /// Returns the authority version covered by this context.
    #[must_use]
    pub const fn authority(self) -> AuthorityVersion {
        self.authority
    }

    /// Returns the admitted authority epoch.
    #[must_use]
    pub const fn authority_epoch(self) -> u64 {
        self.authority_epoch
    }

    /// Returns the admitted revocation observation.
    #[must_use]
    pub const fn revocation_version(self) -> u64 {
        self.revocation_version
    }

    /// Returns the semantic registration generation.
    #[must_use]
    pub const fn dependency_generation(self) -> u64 {
        self.dependency_generation
    }

    /// Returns the durable process incarnation that admitted the result.
    #[must_use]
    pub const fn incarnation(self) -> [u8; 32] {
        self.incarnation
    }

    /// Returns whether this context is admitted by the current durable
    /// incarnation and revocation watermark.
    #[must_use]
    pub fn valid_for(self, incarnation: [u8; 32], minimum_revocation: u64) -> bool {
        self.incarnation == incarnation && self.revocation_version >= minimum_revocation
    }

    #[cfg(test)]
    pub(crate) const fn test_new(
        key: WorkKey,
        authority: AuthorityVersion,
        authority_epoch: u64,
        revocation_version: u64,
        dependency_generation: u64,
    ) -> Self {
        Self {
            key,
            authority,
            authority_epoch,
            revocation_version,
            dependency_generation,
            incarnation: [0; 32],
        }
    }
}

/// One cached result that has passed validation for an exact semantic key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReusableOutput {
    /// Exact authority and dependency freshness binding.
    context: ReuseContext,
    /// Immutable output version.
    output: OutputVersion,
    /// Canonical bytes retained by the validated publication capability.
    canonical_bytes: Arc<Vec<u8>>,
}

impl ReusableOutput {
    pub(crate) const fn from_parts(
        context: ReuseContext,
        output: OutputVersion,
        canonical_bytes: Arc<Vec<u8>>,
    ) -> Self {
        Self {
            context,
            output,
            canonical_bytes,
        }
    }

    /// Returns the exact semantic work key.
    #[must_use]
    pub const fn key(&self) -> WorkKey {
        self.context.key
    }

    /// Returns the validated immutable output.
    #[must_use]
    pub const fn output(&self) -> OutputVersion {
        self.output
    }

    /// Returns the authority version used for validation.
    #[must_use]
    pub const fn authority(&self) -> AuthorityVersion {
        self.context.authority
    }

    /// Returns the authority and dependency freshness binding.
    #[must_use]
    pub const fn context(&self) -> ReuseContext {
        self.context
    }

    /// Returns the canonical bytes retained for local retrieval.
    #[must_use]
    pub fn canonical_bytes(&self) -> &[u8] {
        self.canonical_bytes.as_slice()
    }

    /// Returns the shared byte owner for scheduler and store integrations.
    #[must_use]
    pub fn canonical_bytes_arc(&self) -> Arc<Vec<u8>> {
        Arc::clone(&self.canonical_bytes)
    }

    /// Returns the allocation capacity retained by this output owner.
    /// Capacity, rather than only logical length, is charged to the bounded
    /// lookup because the owner keeps that full allocation alive.
    #[must_use]
    pub fn canonical_capacity(&self) -> usize {
        self.canonical_bytes.capacity()
    }
}

/// Thread-safe lookup for reusable validated outputs.
pub struct OutputLookup {
    state: Mutex<LookupState>,
    max_entries: usize,
    max_bytes: u64,
}

struct LookupState {
    entries: BTreeMap<WorkKey, LookupEntry>,
    /// `(generation, key)` pairs ordered from least to most recently used.
    recency: BTreeSet<(u64, WorkKey)>,
    next_generation: u64,
    retained_bytes: u64,
}

struct LookupEntry {
    output: ReusableOutput,
    generation: u64,
}

impl std::fmt::Debug for OutputLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputLookup")
            .field("entries", &self.len())
            .field("max_entries", &self.max_entries)
            .field("max_bytes", &self.max_bytes)
            .field("retained_bytes", &self.retained_bytes())
            .finish_non_exhaustive()
    }
}

fn ensure_generation_capacity(state: &mut LookupState) -> bool {
    if state.next_generation != u64::MAX {
        return true;
    }
    let keys = state.entries.keys().copied().collect::<Vec<_>>();
    let Ok(count) = u64::try_from(keys.len()) else {
        return false;
    };
    if count == u64::MAX {
        return false;
    }
    let mut recency = BTreeSet::new();
    for (index, key) in keys.into_iter().enumerate() {
        let Some(next_index) = index.checked_add(1) else {
            return false;
        };
        let Ok(generation) = u64::try_from(next_index) else {
            return false;
        };
        let Some(entry) = state.entries.get_mut(&key) else {
            return false;
        };
        entry.generation = generation;
        recency.insert((generation, key));
    }
    state.recency = recency;
    state.next_generation = count;
    true
}

fn next_generation(state: &mut LookupState) -> Option<u64> {
    let next = state.next_generation.checked_add(1)?;
    state.next_generation = next;
    Some(next)
}

impl Default for OutputLookup {
    fn default() -> Self {
        Self::new()
    }
}
