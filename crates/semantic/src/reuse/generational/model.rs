use super::super::InvalidationCounters;
use super::super::index::{RetainedReaders, SemanticReaderKey, SemanticRelationRoot};
use crate::DependencyManifestVersion;
use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};

/// A monotonically increasing non-zero semantic generation.
///
/// The value is allocated only by [`VersionedRetention`].  It is intentionally
/// separate from a content version: two executions can have the same
/// manifest while occupying different lifecycle generations.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticGeneration(NonZeroU64);

impl SemanticGeneration {
    /// The first generation allocated by an empty retention owner.
    #[must_use]
    pub const fn first() -> Self {
        Self(NonZeroU64::MIN)
    }

    /// Returns the numeric representation for diagnostics and durable
    /// lifecycle receipts.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    pub(super) fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// One immutable reader/manifest lifecycle root.
///
/// The constructor is private: callers can only obtain a root from an
/// admitted reservation or pin.  This keeps a random integer, reader key, or
/// manifest digest from being treated as a valid generation proof.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticGenerationRoot<K: SemanticReaderKey> {
    pub(super) reader: K,
    pub(super) generation: SemanticGeneration,
    pub(super) manifest: DependencyManifestVersion,
    pub(super) relation_root: SemanticRelationRoot<K>,
}

impl<K: SemanticReaderKey> SemanticGenerationRoot<K> {
    /// Returns the reader shard bound to this root.
    #[must_use]
    pub const fn reader(self) -> K {
        self.reader
    }

    /// Returns the lifecycle generation bound to this root.
    #[must_use]
    pub const fn generation(self) -> SemanticGeneration {
        self.generation
    }

    /// Returns the exact dependency manifest bound to this root.
    #[must_use]
    pub const fn manifest(self) -> DependencyManifestVersion {
        self.manifest
    }

    /// Returns the canonical dependency-relation root captured by this
    /// generation. Publication can use it to fence a root against a mutated
    /// reader relation even when the manifest version is unchanged.
    #[must_use]
    pub const fn relation_root(self) -> SemanticRelationRoot<K> {
        self.relation_root
    }
}

/// Explicit retention limits for live and retired semantic generations.
///
/// Retired generations are counted by generation, rather than by reader or
/// tombstone entry.  A single pinned generation can therefore retain many
/// leases without multiplying historical metadata.  Limits are checked before
/// an invalidation mutates the index, so a capacity rejection leaves the
/// previous state intact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionBudget {
    /// Maximum number of current reader generations.
    pub max_live_generations: usize,
    /// Maximum number of retired generations kept by active roots.
    pub max_retired_generations: usize,
    /// Maximum concurrent reservation leases for one generation.
    pub max_leases_per_generation: u32,
    /// Maximum result pins for one generation.
    pub max_pins_per_generation: u32,
}

impl Default for RetentionBudget {
    fn default() -> Self {
        Self {
            max_live_generations: 65_536,
            max_retired_generations: 65_536,
            max_leases_per_generation: 1_024,
            max_pins_per_generation: 1_024,
        }
    }
}

/// Measured state of a versioned semantic retention owner.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetentionStats {
    /// Current generations with a live reader index registration.
    pub live_generations: usize,
    /// Retired generations still held by a lease or pin root.
    pub retired_generations: usize,
    /// Explicit candidate leases currently held.
    pub lease_roots: u64,
    /// Explicit result pins currently held.
    pub pin_roots: u64,
    /// Reader observations retained by the reverse arrangements.
    pub registrations: usize,
}

/// Counters for the lifecycle paths that can cause work or retention.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetentionCounters {
    /// Successful reservations, including coalesced same-manifest work.
    pub reservations: u64,
    /// Reservations that became a current result pin.
    pub commits: u64,
    /// Commit attempts rejected because their generation had retired.
    pub stale_completions: u64,
    /// Explicit or invalidation-driven retirements.
    pub retirements: u64,
    /// Generations whose final root was released.
    pub reclaimed_generations: u64,
    /// Invalidation batches admitted by this owner.
    pub invalidation_batches: u64,
    /// Current generations moved behind a retirement root.
    pub invalidated_generations: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct GenerationEntry<K: SemanticReaderKey> {
    pub(super) root: SemanticGenerationRoot<K>,
    pub(super) leases: u32,
    pub(super) pins: u32,
}

impl<K: SemanticReaderKey> GenerationEntry<K> {
    pub(super) const fn has_roots(self) -> bool {
        self.leases != 0 || self.pins != 0
    }
}

pub(super) struct RetentionState<K: SemanticReaderKey> {
    pub(super) readers: RetainedReaders<K>,
    pub(super) current: BTreeMap<K, GenerationEntry<K>>,
    pub(super) retired: BTreeMap<SemanticGenerationRoot<K>, GenerationEntry<K>>,
    pub(super) last_generation: Option<SemanticGeneration>,
    pub(super) budget: RetentionBudget,
    pub(super) counters: RetentionCounters,
}

impl<K: SemanticReaderKey> Default for RetentionState<K> {
    fn default() -> Self {
        Self {
            readers: RetainedReaders::default(),
            current: BTreeMap::new(),
            retired: BTreeMap::new(),
            last_generation: None,
            budget: RetentionBudget::default(),
            counters: RetentionCounters::default(),
        }
    }
}

/// Versioned semantic reader retention.
///
/// This is the single mutation boundary for lifecycle state.  The underlying
/// [`RetainedReaders`] remains available through a read-only callback, while
/// registration, retirement, invalidation, and root release all pass through
/// this owner.  That prevents a caller from mutating the reverse index without
/// updating generation state.
#[derive(Clone)]
pub struct VersionedRetention<K: SemanticReaderKey> {
    pub(super) state: Arc<Mutex<RetentionState<K>>>,
}

/// Result of one generation invalidation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenerationInvalidation<K: SemanticReaderKey> {
    pub(super) readers: Vec<K>,
    pub(super) roots: Box<[SemanticGenerationRoot<K>]>,
    /// Reverse-index work measured while finding the affected readers.
    pub counters: InvalidationCounters,
}

/// A proof that a pinned generation was current at one observation point.
///
/// It carries no public constructor.  The owner must revalidate it when a
/// result is about to become visible because invalidation may race the first
/// observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CurrentGeneration<K: SemanticReaderKey> {
    pub(super) root: SemanticGenerationRoot<K>,
}
