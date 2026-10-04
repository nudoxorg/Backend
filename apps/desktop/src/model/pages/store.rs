//! Keyed, LRU-bounded page resources.
//!
//! Each page family has its own map keyed by identity, so a package dossier
//! read can never land in the slot the Orbit catalog renders from. A slot
//! carries a generation: every fetch allocates a new one, and a result is
//! admitted only when it names the slot's current generation. That is the
//! latest-wins rule — a newer request for the same key supersedes the older
//! one even if the older result arrives later.
//!
//! The store is pure data: no GPUI, no threads. `runtime::store` owns one and
//! drives it from the UI thread.

use super::common::{PackageRef, SymbolRef};
use super::cargo_source::CargoSourcePage;
use super::health::HealthModel;
use super::key::{CargoSourceKey, PageKey, SearchQuery};
use super::orbit::OrbitModel;
use super::package::PackageDossier;
use super::search::SearchPage;
use super::source::SourceView;
use super::symbol::SymbolPage;
use crate::core::{Activity, ErrorValue, Resource, ResourceTerminal, UnavailableReason, VersionedRoot};
use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::sync::Arc;

/// One read-model value produced by the read pool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PageValue {
    /// A declaration page.
    Symbol(SymbolPage),
    /// A source view.
    Source(SourceView),
    /// A current owner-verified Cargo file, without semantic-index coverage.
    CargoSource(CargoSourcePage),
    /// A package dossier.
    Package(PackageDossier),
    /// The first page of a search.
    Search(SearchPage),
    /// A further page of a search, to append.
    SearchMore(SearchPage),
    /// The Orbit model.
    Orbit(OrbitModel),
    /// The health model.
    Health(HealthModel),
    /// A browsing page's value (your tree).
    Browse(crate::model::browse::BrowseValue),
}

/// One extractor per page family: the value if it is that family's, else
/// `None` (a result that lands in a slot of another family). Written as
/// `if let`, so no wildcard arm stands in for a family added later.
macro_rules! family_of {
    ($($name:ident: $variant:ident($page:ty)),+ $(,)?) => {
        impl PageValue {
            $(
                fn $name(self) -> Option<$page> {
                    if let Self::$variant(page) = self { Some(page) } else { None }
                }
            )+
        }
    };
}

family_of! {
    symbol: Symbol(SymbolPage),
    source: Source(SourceView),
    cargo_source: CargoSource(CargoSourcePage),
    package: Package(PackageDossier),
    orbit: Orbit(OrbitModel),
    health: Health(HealthModel),
    browse: Browse(crate::model::browse::BrowseValue),
}

impl PageValue {
    /// A search page, whether it is the first or a further one.
    fn search(self) -> Option<SearchPage> {
        if let Self::Search(page) | Self::SearchMore(page) = self { Some(page) } else { None }
    }
}

/// Why a read produced no value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadFailure {
    /// The producer does not provide this resource.
    Unavailable(UnavailableReason, Arc<str>),
    /// The read failed with a typed, bounded fault.
    Fault(ErrorValue),
    /// The request was cancelled or superseded before it produced a value.
    Cancelled,
}

/// One page the launch snapshot keeps: its key and its value are one value,
/// so a page can never be seeded into another family's slot (or saved under
/// another page's name).
#[derive(Clone, Debug, PartialEq)]
pub enum SeedEntry {
    /// A declaration page.
    Symbol(SymbolRef, Arc<SymbolPage>),
    /// A source view.
    Source(SymbolRef, Arc<SourceView>),
    /// A package dossier.
    Package(PackageRef, Arc<PackageDossier>),
    /// The Orbit model.
    Orbit(Arc<OrbitModel>),
}

impl SeedEntry {
    /// The page key this entry fills.
    #[must_use]
    pub fn key(&self) -> PageKey {
        match self {
            Self::Symbol(symbol, _) => PageKey::Symbol(symbol.clone()),
            Self::Source(symbol, _) => PageKey::Source(symbol.clone()),
            Self::Package(package, _) => PageKey::Package(package.clone()),
            Self::Orbit(_) => PageKey::Orbit,
        }
    }
}

/// Outcome of admitting one landing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Landing {
    /// The result named the slot's current generation and was applied.
    Applied,
    /// A newer request owns the slot (or the slot was evicted); dropped.
    Superseded,
    /// A quiet revalidation of a launch-snapshot value (W-Open I2) found
    /// the same value: it is now current at the new root and nothing a view
    /// draws changed, so no stamp moved and nothing needs to redraw.
    Unchanged,
}

/// Cheap identity of one slot's visible state, for equality-gated views: it
/// moves on every visible change, and only then.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Stamp(u64);

impl Stamp {
    const UNSEEN: Self = Self(0);

    const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// Which fetch owns a slot: every fetch allocates a new one, and a result is
/// admitted only when it names the slot's current one.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Generation(NonZeroU64);

impl Generation {
    const FIRST: Self = Self(NonZeroU64::MIN);

    /// Admits a numeric generation only when it is nonzero.
    #[must_use]
    pub const fn new(number: u64) -> Option<Self> {
        match NonZeroU64::new(number) {
            Some(number) => Some(Self(number)),
            None => None,
        }
    }

    const fn next(self) -> Option<Self> {
        match self.0.get().checked_add(1) {
            Some(next) => Self::new(next),
            None => None,
        }
    }
}

/// This page store permanently consumed its final fetch generation.
/// Releasing pages or retrying cannot make an old identity reusable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenerationExhausted;

impl GenerationExhausted {
    /// The existing resource fault surface explains why no fetch can start.
    #[must_use]
    pub fn fault(self) -> ErrorValue {
        ErrorValue::new(
            crate::core::FaultCode::Protocol,
            "This window has exhausted its page read generations.",
        )
    }
}

/// A logical access time, for least-recently-used eviction.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
struct Tick(u64);

impl Tick {
    const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// How a running fetch treats what the slot shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Manner {
    /// The slot turned "working" when it began; its result replaces the value.
    Loud,
    /// A revalidation of a launch-snapshot value: nothing visible changed
    /// when it began, and an equal value changes nothing when it lands.
    Quiet,
}

/// Whether a fetch owns a slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Fetch {
    /// No fetch is running.
    Idle,
    /// This one is.
    Running { generation: Generation, manner: Manner },
}

/// Where a slot's value came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Provenance {
    /// Read from the owner.
    Live,
    /// From the launch snapshot, and no owner has confirmed it yet (W-Open
    /// I2): its next fetch is quiet.
    Seeded,
    /// Read from a previous owner attachment. Bytes may be disclosed, but
    /// only a successful new read can admit them for current actions.
    Revoked,
}

#[derive(Debug)]
struct Slot<T> {
    resource: Resource<T>,
    fetch: Fetch,
    /// Visible revision; moves on every visible change.
    revision: Stamp,
    /// Logical access time for LRU eviction.
    used: Tick,
    /// Root the running (or last) fetch was issued at.
    asked_at: Option<VersionedRoot>,
    provenance: Provenance,
}

impl<T> Slot<T> {
    const fn new(used: Tick) -> Self {
        Self {
            resource: Resource::not_yet(),
            fetch: Fetch::Idle,
            revision: Stamp::UNSEEN,
            used,
            asked_at: None,
            provenance: Provenance::Live,
        }
    }

    const fn running(&self) -> bool {
        matches!(self.fetch, Fetch::Running { .. })
    }

    /// Whether a fetch is needed to show this slot at `root`.
    fn wants_fetch(&self, root: VersionedRoot) -> bool {
        if self.running() {
            return false;
        }
        match self.asked_at {
            // Never asked.
            None => true,
            // Asked at an older authority: refresh, keeping the last good value.
            Some(asked) => !asked.same_authority(root),
        }
    }
}

#[derive(Debug)]
struct Slots<K: Ord, T> {
    map: BTreeMap<K, Slot<T>>,
    capacity: usize,
}

impl<K: Ord + Clone, T> Slots<K, T> {
    const fn new(capacity: usize) -> Self {
        Self {
            map: BTreeMap::new(),
            capacity,
        }
    }

    fn begin(
        &mut self,
        key: &K,
        root: VersionedRoot,
        force: bool,
        clock: Tick,
        generation: Option<Generation>,
    ) -> Result<Option<Generation>, GenerationExhausted> {
        // Current and already-running slots need no identity. A refused
        // fetch must not insert, evict, touch, or change any existing slot.
        if self
            .map
            .get(key)
            .is_some_and(|slot| !(force || slot.wants_fetch(root)))
        {
            if let Some(slot) = self.map.get_mut(key) {
                slot.used = clock;
            }
            return Ok(None);
        }
        let generation = generation.ok_or(GenerationExhausted)?;
        if !self.map.contains_key(key) {
            self.evict_for_insert();
            self.map.insert(key.clone(), Slot::new(clock));
        }
        let Some(slot) = self.map.get_mut(key) else {
            return Ok(None);
        };
        slot.used = clock;
        slot.asked_at = Some(root);
        // A snapshot value is revalidated quietly: it stays exactly as drawn
        // (no "working", no stamp) until a different value lands.
        let manner = if slot.provenance == Provenance::Seeded && !force { Manner::Quiet } else { Manner::Loud };
        slot.fetch = Fetch::Running { generation, manner };
        if manner == Manner::Loud {
            slot.resource = std::mem::replace(&mut slot.resource, Resource::not_yet()).working();
            slot.revision = slot.revision.next();
        }
        Ok(Some(generation))
    }

    /// A refused foreground request has no generation to land. The caller
    /// first cancels its old fetch, then names that exact previous ownership.
    /// A newer fetch can never be cleared by this refusal.
    fn refuse_generation_exhaustion(
        &mut self,
        key: &K,
        root: VersionedRoot,
        expected: Option<Generation>,
        clock: Tick,
    ) -> bool {
        if self.inflight(key) != expected {
            return false;
        }
        if !self.map.contains_key(key) {
            self.evict_for_insert();
            self.map.insert(key.clone(), Slot::new(clock));
        }
        let Some(slot) = self.map.get_mut(key) else {
            return false;
        };
        let fault = GenerationExhausted.fault();
        let changed = slot.running()
            || slot.resource.activity() != Activity::Stopped
            || slot.resource.terminal() != &ResourceTerminal::Fault(fault.clone());
        slot.fetch = Fetch::Idle;
        slot.asked_at = Some(root);
        slot.used = clock;
        if !changed {
            return false;
        }
        // A partial is not a last-good complete value. Complete retained
        // bytes keep their original root; refusal never revalidates them.
        let previous = std::mem::replace(&mut slot.resource, Resource::not_yet());
        let previous = if previous.terminal() == &ResourceTerminal::Partial {
            Resource::not_yet()
        } else {
            previous
        };
        slot.resource = previous.mark_error(fault.code(), fault.message());
        slot.revision = slot.revision.next();
        true
    }

    /// Fills a slot from the launch snapshot, current at `root` (the
    /// unserved root at launch). A slot a fetch already owns is left alone.
    fn seed(&mut self, key: &K, value: T, root: VersionedRoot, clock: Tick) -> bool {
        if !self.map.contains_key(key) {
            self.evict_for_insert();
            self.map.insert(key.clone(), Slot::new(clock));
        }
        let Some(slot) = self.map.get_mut(key) else {
            return false;
        };
        if slot.running() || slot.resource.loaded_value().is_some() {
            return false;
        }
        slot.used = clock;
        slot.asked_at = Some(root);
        slot.resource = Resource::loaded_at(value, root);
        slot.provenance = Provenance::Seeded;
        slot.revision = slot.revision.next();
        true
    }

    /// The owner serves the root the snapshot was read at: the seeded value
    /// is current at `root` as it is. Nothing visible changes.
    fn confirm(&mut self, key: &K, root: VersionedRoot) -> bool {
        let Some(slot) = self.map.get_mut(key) else {
            return false;
        };
        if slot.provenance != Provenance::Seeded || slot.running() {
            return false;
        }
        slot.provenance = Provenance::Live;
        slot.asked_at = Some(root);
        slot.resource = std::mem::replace(&mut slot.resource, Resource::not_yet()).rebased(root);
        true
    }

    fn touch(&mut self, key: &K, clock: Tick) {
        if let Some(slot) = self.map.get_mut(key) {
            slot.used = clock;
        }
    }

    /// Evicts the least recently used idle slot while the map is full.
    fn evict_for_insert(&mut self) {
        while self.map.len() >= self.capacity.max(1) {
            let victim = self
                .map
                .iter()
                .filter(|(_, slot)| !slot.running())
                .min_by_key(|(_, slot)| slot.used)
                .map(|(key, _)| key.clone());
            match victim {
                Some(key) => {
                    self.map.remove(&key);
                }
                // Every slot is in flight: grow past the bound rather than
                // drop a request a view is waiting on.
                None => break,
            }
        }
    }

    fn land(
        &mut self,
        key: &K,
        generation: Generation,
        result: Result<T, ReadFailure>,
        merge: impl FnOnce(Option<&T>, T) -> T,
    ) -> Landing
    where
        T: PartialEq,
    {
        let Some(slot) = self.map.get_mut(key) else {
            return Landing::Superseded;
        };
        let Fetch::Running { generation: running, manner } = slot.fetch else {
            return Landing::Superseded;
        };
        if running != generation {
            return Landing::Superseded;
        }
        slot.fetch = Fetch::Idle;
        let root = slot.asked_at;
        if manner == Manner::Quiet {
            match (&result, root) {
                (Ok(value), Some(root)) if slot.resource.loaded_value() == Some(value) => {
                    slot.provenance = Provenance::Live;
                    slot.resource =
                        std::mem::replace(&mut slot.resource, Resource::not_yet()).rebased(root);
                    return Landing::Unchanged;
                }
                (Err(ReadFailure::Cancelled), _) => {
                    // Still the snapshot's value, still unconfirmed.
                    slot.asked_at = None;
                    return Landing::Unchanged;
                }
                (Ok(_), _) => slot.provenance = Provenance::Live,
                // A failed revalidation keeps the value it could not confirm.
                (Err(_), _) => {}
            }
        }
        let previous = std::mem::replace(&mut slot.resource, Resource::not_yet());
        // An interim page is useful while work continues, but a failed
        // primary read cannot present it as a last-good complete page.
        let previous = if matches!(previous.terminal(), ResourceTerminal::Partial)
            && matches!(&result, Err(ReadFailure::Fault(_) | ReadFailure::Unavailable(_, _)))
        {
            Resource::not_yet()
        } else {
            previous
        };
        slot.resource = match (result, root) {
            (Ok(value), Some(root)) => {
                slot.provenance = Provenance::Live;
                let value = merge(previous.loaded_value(), value);
                Resource::loaded_at(value, root)
            }
            (Ok(_), None) => previous.resting(),
            (Err(ReadFailure::Unavailable(reason, _detail)), _) => {
                previous.mark_unavailable(reason)
            }
            (Err(ReadFailure::Fault(error)), _) => {
                previous.mark_error(error.code(), error.message().to_owned())
            }
            (Err(ReadFailure::Cancelled), _) => {
                // A cancelled fetch leaves the slot as it was before the
                // fetch began; the next ensure asks again.
                slot.asked_at = None;
                if slot.provenance == Provenance::Revoked {
                    previous.waiting()
                } else {
                    previous.resting()
                }
            }
        };
        slot.revision = slot.revision.next();
        Landing::Applied
    }

    /// A current-file capability cannot keep old bytes after its owner says
    /// the authority or file is gone, or its check was cancelled. Other
    /// families may retain a last-good value; this one must revoke it.
    fn land_strict(
        &mut self,
        key: &K,
        generation: Generation,
        result: Result<T, ReadFailure>,
    ) -> Landing
    where
        T: PartialEq,
    {
        if result.is_err() {
            if let Some(slot) = self.map.get_mut(key)
                && matches!(slot.fetch, Fetch::Running { generation: running, .. } if running == generation)
            {
                slot.resource = Resource::not_yet();
            }
        }
        self.land(key, generation, result, replace)
    }

    /// Publishes a useful partial value while the same generation keeps
    /// reading. An older or cancelled generation cannot paint over a newer
    /// route. Quiet snapshot revalidation keeps its already complete page.
    fn stage(&mut self, key: &K, generation: Generation, value: T) -> Landing
    where
        T: PartialEq,
    {
        let Some(slot) = self.map.get_mut(key) else { return Landing::Superseded };
        let Fetch::Running { generation: running, manner } = slot.fetch else { return Landing::Superseded };
        if running != generation { return Landing::Superseded; }
        if manner == Manner::Quiet || slot.resource.is_loaded() {
            return Landing::Unchanged;
        }
        let Some(root) = slot.asked_at else { return Landing::Superseded };
        if slot.resource.loaded_value() == Some(&value) && slot.resource.value_root() == Some(root) {
            return Landing::Unchanged;
        }
        slot.resource = Resource::partial_at(value, root);
        slot.revision = slot.revision.next();
        Landing::Applied
    }

    fn cancel(&mut self, key: &K) -> Option<Generation> {
        let slot = self.map.get_mut(key)?;
        let Fetch::Running { generation, manner } = slot.fetch else {
            return None;
        };
        slot.fetch = Fetch::Idle;
        slot.asked_at = None;
        if manner == Manner::Quiet {
            // Nothing visible began, so nothing visible ends.
            return Some(generation);
        }
        let previous = std::mem::replace(&mut slot.resource, Resource::not_yet());
        slot.resource = if slot.provenance == Provenance::Revoked {
            previous.waiting()
        } else {
            previous.resting()
        };
        slot.revision = slot.revision.next();
        Some(generation)
    }

    /// Invalidate read admission without deleting the immutable predecessor.
    /// Seeded launch bytes retain their separate first-owner confirmation path.
    fn revoke_owner_read(&mut self, key: &K) -> bool {
        let Some(slot) = self.map.get_mut(key) else {
            return false;
        };
        if slot.provenance == Provenance::Seeded {
            return false;
        }
        if slot.provenance == Provenance::Revoked
            && !slot.running()
            && slot.asked_at.is_none()
            && slot.resource.activity() == Activity::Waiting
        {
            return false;
        }
        slot.fetch = Fetch::Idle;
        slot.asked_at = None;
        slot.provenance = Provenance::Revoked;
        slot.resource = std::mem::replace(&mut slot.resource, Resource::not_yet()).waiting();
        slot.revision = slot.revision.next();
        true
    }

    fn owner_read_revoked(&self, key: &K) -> bool {
        self.map.get(key).is_some_and(|slot| slot.provenance == Provenance::Revoked)
    }

    fn get(&self, key: &K) -> Resource<T> {
        self.map
            .get(key)
            .map_or_else(Resource::not_yet, |slot| slot.resource.clone())
    }

    fn stamp(&self, key: &K) -> Stamp {
        self.map.get(key).map_or(Stamp::UNSEEN, |slot| slot.revision)
    }

    fn seeded(&self, key: &K) -> bool {
        self.map.get(key).is_some_and(|slot| slot.provenance == Provenance::Seeded)
    }

    fn inflight(&self, key: &K) -> Option<Generation> {
        match self.map.get(key)?.fetch {
            Fetch::Running { generation, .. } => Some(generation),
            Fetch::Idle => None,
        }
    }
}

/// Bounds for each page family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Capacity {
    /// Declaration pages.
    pub symbols: usize,
    /// Source views.
    pub sources: usize,
    /// Owner-revalidated Cargo source files.
    pub cargo_sources: usize,
    /// Package dossiers.
    pub packages: usize,
    /// Search queries.
    pub searches: usize,
}

impl Default for Capacity {
    fn default() -> Self {
        Self {
            symbols: 64,
            sources: 32,
            cargo_sources: 16,
            packages: 16,
            searches: 16,
        }
    }
}

/// All keyed page resources for one window.
#[derive(Debug)]
pub struct PageStore {
    symbols: Slots<SymbolRef, SymbolPage>,
    sources: Slots<SymbolRef, SourceView>,
    cargo_sources: Slots<CargoSourceKey, CargoSourcePage>,
    packages: Slots<PackageRef, PackageDossier>,
    searches: Slots<SearchQuery, SearchPage>,
    orbit: Slots<(), OrbitModel>,
    health: Slots<(), HealthModel>,
    browse: Slots<crate::model::browse::BrowseKey, crate::model::browse::BrowseValue>,
    clock: Tick,
    next_generation: Option<Generation>,
}

impl Default for PageStore {
    fn default() -> Self {
        Self::new(Capacity::default())
    }
}

macro_rules! dispatch {
    ($store:expr, $key:expr, |$slots:ident, $k:ident| $body:expr) => {
        match $key {
            PageKey::Symbol(symbol) => {
                let $slots = &mut $store.symbols;
                let $k = symbol;
                $body
            }
            PageKey::Source(symbol) => {
                let $slots = &mut $store.sources;
                let $k = symbol;
                $body
            }
            PageKey::CargoSource(file) => {
                let $slots = &mut $store.cargo_sources;
                let $k = file;
                $body
            }
            PageKey::Package(package) => {
                let $slots = &mut $store.packages;
                let $k = package;
                $body
            }
            PageKey::Search(query) => {
                let $slots = &mut $store.searches;
                let $k = query;
                $body
            }
            PageKey::Orbit => {
                let $slots = &mut $store.orbit;
                let $k = &();
                $body
            }
            PageKey::Health => {
                let $slots = &mut $store.health;
                let $k = &();
                $body
            }
            PageKey::Browse(browse) => {
                let $slots = &mut $store.browse;
                let $k = browse;
                $body
            }
        }
    };
}

macro_rules! dispatch_ref {
    ($store:expr, $key:expr, |$slots:ident, $k:ident| $body:expr) => {
        match $key {
            PageKey::Symbol(symbol) => {
                let $slots = &$store.symbols;
                let $k = symbol;
                $body
            }
            PageKey::Source(symbol) => {
                let $slots = &$store.sources;
                let $k = symbol;
                $body
            }
            PageKey::CargoSource(file) => {
                let $slots = &$store.cargo_sources;
                let $k = file;
                $body
            }
            PageKey::Package(package) => {
                let $slots = &$store.packages;
                let $k = package;
                $body
            }
            PageKey::Search(query) => {
                let $slots = &$store.searches;
                let $k = query;
                $body
            }
            PageKey::Orbit => {
                let $slots = &$store.orbit;
                let $k = &();
                $body
            }
            PageKey::Health => {
                let $slots = &$store.health;
                let $k = &();
                $body
            }
            PageKey::Browse(browse) => {
                let $slots = &$store.browse;
                let $k = browse;
                $body
            }
        }
    };
}

impl PageStore {
    /// Creates an empty store with the given bounds.
    #[must_use]
    pub const fn new(capacity: Capacity) -> Self {
        Self {
            symbols: Slots::new(capacity.symbols),
            sources: Slots::new(capacity.sources),
            cargo_sources: Slots::new(capacity.cargo_sources),
            packages: Slots::new(capacity.packages),
            searches: Slots::new(capacity.searches),
            orbit: Slots::new(1),
            health: Slots::new(1),
            browse: Slots::new(4),
            clock: Tick(0),
            next_generation: Some(Generation::FIRST),
        }
    }

    /// Records an access and, when the slot needs data at `root` (never asked,
    /// or asked at an older root), starts a fetch: the slot turns working
    /// (keeping any last good value) and the new generation is returned.
    /// Returns `Ok(None)` when the slot is current or already in flight.
    ///
    /// # Errors
    /// Returns [`GenerationExhausted`] before changing a slot when a needed
    /// fetch has no new identity. Current slots still need no new identity.
    pub fn begin(
        &mut self,
        key: &PageKey,
        root: VersionedRoot,
    ) -> Result<Option<Generation>, GenerationExhausted> {
        self.begin_with(key, root, false)
    }

    /// Starts a fetch even when the slot is current (retry, "load more").
    ///
    /// # Errors
    /// Returns [`GenerationExhausted`] without replacing a previous fetch
    /// when no new identity remains; the runtime handles that refusal.
    pub fn begin_forced(
        &mut self,
        key: &PageKey,
        root: VersionedRoot,
    ) -> Result<Option<Generation>, GenerationExhausted> {
        self.begin_with(key, root, true)
    }

    fn begin_with(
        &mut self,
        key: &PageKey,
        root: VersionedRoot,
        force: bool,
    ) -> Result<Option<Generation>, GenerationExhausted> {
        let clock = self.clock.next();
        let generation = self.next_generation;
        let started = dispatch!(self, key, |slots, k| slots
            .begin(k, root, force, clock, generation))?;
        self.clock = clock;
        if let Some(generation) = started {
            self.next_generation = generation.next();
        }
        Ok(started)
    }

    /// Applies a permanent admission fault after the runtime cancelled the
    /// captured previous fetch. No identity is minted for this terminal state.
    pub fn refuse_generation_exhaustion(
        &mut self,
        key: &PageKey,
        root: VersionedRoot,
        expected: Option<Generation>,
    ) -> bool {
        if self.next_generation.is_some() {
            return false;
        }
        let clock = self.clock.next();
        let changed = dispatch!(self, key, |slots, k| slots
            .refuse_generation_exhaustion(k, root, expected, clock));
        self.clock = clock;
        changed
    }

    /// Tests may advance, never rewind or reopen, the remaining mint state.
    #[cfg(test)]
    pub(crate) fn test_advance_generation_to(&mut self, next: Generation) {
        assert!(self.next_generation.is_some_and(|current| next >= current));
        self.next_generation = Some(next);
    }

    /// Fills `key`'s slot with a launch-snapshot value, current at `root`
    /// until an owner confirms it ([`Self::confirm`]) or revalidates it (its
    /// next fetch is quiet: [`Landing::Unchanged`] when the value is equal).
    /// Returns whether the slot took it (a family the snapshot does not
    /// keep, a slot in flight or already holding a value, does not).
    pub fn seed(&mut self, entry: SeedEntry, root: VersionedRoot) -> bool {
        self.clock = self.clock.next();
        let clock = self.clock;
        match entry {
            SeedEntry::Symbol(symbol, page) => self.symbols.seed(&symbol, Arc::unwrap_or_clone(page), root, clock),
            SeedEntry::Source(symbol, view) => self.sources.seed(&symbol, Arc::unwrap_or_clone(view), root, clock),
            SeedEntry::Package(package, dossier) => {
                self.packages.seed(&package, Arc::unwrap_or_clone(dossier), root, clock)
            }
            SeedEntry::Orbit(model) => self.orbit.seed(&(), Arc::unwrap_or_clone(model), root, clock),
        }
    }

    /// The owner serves the root the snapshot was read at: `key`'s seeded
    /// value is current at `root`, with no fetch and no visible change.
    pub fn confirm(&mut self, key: &PageKey, root: VersionedRoot) -> bool {
        dispatch!(self, key, |slots, k| slots.confirm(k, root))
    }

    /// Whether `key` shows a launch-snapshot value no owner has confirmed.
    #[must_use]
    pub fn is_seeded(&self, key: &PageKey) -> bool {
        dispatch_ref!(self, key, |slots, k| slots.seeded(k))
    }

    /// Revokes a previous attachment's read. Ordinary page values stay waiting
    /// predecessors; Cargo file capabilities and path listings drop their
    /// bytes. The next ensure rereads either family even at the same root.
    /// The caller must first cancel the read pool's running job for this key.
    pub fn revoke_owner_read(&mut self, key: &PageKey) -> bool {
        match key {
            PageKey::CargoSource(file) => {
                if self.activity(key) == Activity::NotYet && self.inflight(key).is_none() {
                    return false;
                }
                self.revoke_cargo_source(file);
                true
            }
            PageKey::Browse(crate::model::browse::BrowseKey::CargoSourceInventory(inventory)) => {
                if self.activity(key) == Activity::NotYet && self.inflight(key).is_none() {
                    return false;
                }
                self.revoke_cargo_source_inventory(inventory);
                true
            }
            PageKey::Symbol(_) | PageKey::Source(_) | PageKey::Package(_) | PageKey::Search(_)
                | PageKey::Orbit | PageKey::Health | PageKey::Browse(_) => {
                dispatch!(self, key, |slots, k| slots.revoke_owner_read(k))
            }
        }
    }

    /// A revoked predecessor is never saved as a current launch page.
    #[must_use]
    pub fn is_owner_read_revoked(&self, key: &PageKey) -> bool {
        dispatch_ref!(self, key, |slots, k| slots.owner_read_revoked(k))
    }

    /// Records an access without starting a fetch.
    pub fn touch(&mut self, key: &PageKey) {
        self.clock = self.clock.next();
        let clock = self.clock;
        dispatch!(self, key, |slots, k| slots.touch(k, clock));
    }

    /// Admits one result if it names the slot's current generation.
    pub fn land(
        &mut self,
        key: &PageKey,
        generation: Generation,
        result: Result<PageValue, ReadFailure>,
    ) -> Landing {
        match key {
            PageKey::Symbol(symbol) => self.symbols.land(symbol, generation, take(result, PageValue::symbol), replace),
            PageKey::Source(symbol) => self.sources.land(symbol, generation, take(result, PageValue::source), replace),
            PageKey::CargoSource(file) => self.cargo_sources.land_strict(file, generation, take(result, PageValue::cargo_source)),
            PageKey::Package(package) => self.packages.land(package, generation, take(result, PageValue::package), replace),
            PageKey::Search(query) => {
                let append = matches!(result, Ok(PageValue::SearchMore(_)));
                self.searches.land(query, generation, take(result, PageValue::search), move |previous, next| match previous {
                    Some(previous) if append => append_search(previous, next),
                    _ => next,
                })
            }
            PageKey::Orbit => self.orbit.land(&(), generation, take(result, PageValue::orbit), replace),
            PageKey::Health => self.health.land(&(), generation, take(result, PageValue::health), replace),
            PageKey::Browse(browse) => self.browse.land(browse, generation, take(result, PageValue::browse), replace),
        }
    }

    /// Publishes an intermediate result without ending its read generation.
    pub fn stage(&mut self, key: &PageKey, generation: Generation, value: PageValue) -> Landing {
        match key {
            PageKey::Symbol(symbol) => match value.symbol() { Some(value) => self.symbols.stage(symbol, generation, value), None => Landing::Superseded },
            PageKey::Package(package) => match value.package() { Some(value) => self.packages.stage(package, generation, value), None => Landing::Superseded },
            PageKey::Orbit => match value.orbit() { Some(value) => self.orbit.stage(&(), generation, value), None => Landing::Superseded },
            PageKey::Source(_) | PageKey::CargoSource(_) | PageKey::Search(_) | PageKey::Health | PageKey::Browse(_) => Landing::Superseded,
        }
    }

    /// Cancels the running fetch for `key`, returning its generation.
    pub fn cancel(&mut self, key: &PageKey) -> Option<Generation> {
        dispatch!(self, key, |slots, k| slots.cancel(k))
    }

    /// Returns the generation of the running fetch for `key`.
    #[must_use]
    pub fn inflight(&self, key: &PageKey) -> Option<Generation> {
        dispatch_ref!(self, key, |slots, k| slots.inflight(k))
    }

    /// Returns the visible-state stamp of one slot.
    #[must_use]
    pub fn stamp(&self, key: &PageKey) -> Stamp {
        dispatch_ref!(self, key, |slots, k| slots.stamp(k))
    }

    /// Returns whether a slot exists for `key` (asked at least once and not evicted).
    #[must_use]
    pub fn contains(&self, key: &PageKey) -> bool {
        match key {
            PageKey::Symbol(symbol) => self.symbols.map.contains_key(symbol),
            PageKey::Source(symbol) => self.sources.map.contains_key(symbol),
            PageKey::CargoSource(file) => self.cargo_sources.map.contains_key(file),
            PageKey::Package(package) => self.packages.map.contains_key(package),
            PageKey::Search(query) => self.searches.map.contains_key(query),
            PageKey::Orbit => self.orbit.map.contains_key(&()),
            PageKey::Health => self.health.map.contains_key(&()),
            PageKey::Browse(browse) => self.browse.map.contains_key(browse),
        }
    }

    /// Returns the declaration page resource.
    #[must_use]
    pub fn symbol(&self, symbol: &SymbolRef) -> Resource<SymbolPage> {
        self.symbols.get(symbol)
    }

    /// Returns the source view resource.
    #[must_use]
    pub fn source(&self, symbol: &SymbolRef) -> Resource<SourceView> {
        self.sources.get(symbol)
    }

    /// Returns an owner-revalidated Cargo source file resource.
    #[must_use]
    pub fn cargo_source(&self, file: &CargoSourceKey) -> Resource<CargoSourcePage> {
        self.cargo_sources.get(file)
    }

    /// Drops current-file bytes when their owner is left or restarted. The
    /// address and slot remain so the next focused read can be retried.
    pub fn revoke_cargo_source(&mut self, file: &CargoSourceKey) {
        if let Some(slot) = self.cargo_sources.map.get_mut(file) {
            slot.fetch = Fetch::Idle;
            slot.resource = Resource::not_yet();
            slot.asked_at = None;
            slot.revision = slot.revision.next();
        }
    }

    /// A new owner attachment must revalidate every retained file receipt.
    pub fn revoke_all_cargo_sources(&mut self) {
        for slot in self.cargo_sources.map.values_mut() {
            slot.fetch = Fetch::Idle;
            slot.resource = Resource::not_yet();
            slot.asked_at = None;
            slot.revision = slot.revision.next();
        }
    }

    /// A path inventory is an observation of the current owner, not a saved
    /// file capability. Recheck it when its source route is revisited.
    pub fn revoke_cargo_source_inventory(&mut self, key: &crate::model::browse::CargoSourceInventoryKey) {
        let browse = crate::model::browse::BrowseKey::CargoSourceInventory(key.clone());
        if let Some(slot) = self.browse.map.get_mut(&browse) {
            slot.fetch = Fetch::Idle;
            slot.resource = Resource::not_yet();
            slot.asked_at = None;
            slot.revision = slot.revision.next();
        }
    }

    /// An owner restart invalidates all retained file-address listings.
    pub fn revoke_all_cargo_source_inventories(&mut self) {
        for (key, slot) in &mut self.browse.map {
            if matches!(key, crate::model::browse::BrowseKey::CargoSourceInventory(_)) {
                slot.fetch = Fetch::Idle;
                slot.resource = Resource::not_yet();
                slot.asked_at = None;
                slot.revision = slot.revision.next();
            }
        }
    }

    /// Returns the package dossier resource.
    #[must_use]
    pub fn package(&self, package: &PackageRef) -> Resource<PackageDossier> {
        self.packages.get(package)
    }

    /// Returns the search results resource.
    #[must_use]
    pub fn search(&self, query: &SearchQuery) -> Resource<SearchPage> {
        self.searches.get(query)
    }

    /// Returns the Orbit resource.
    #[must_use]
    pub fn orbit(&self) -> Resource<OrbitModel> {
        self.orbit.get(&())
    }

    /// Returns the health resource.
    #[must_use]
    pub fn health(&self) -> Resource<HealthModel> {
        self.health.get(&())
    }

    /// Returns a browsing page's resource.
    #[must_use]
    pub fn browse(&self, key: &crate::model::browse::BrowseKey) -> Resource<crate::model::browse::BrowseValue> {
        self.browse.get(key)
    }

    /// Returns every resident key, orbit and health first, then by family.
    #[must_use]
    pub fn keys(&self) -> Vec<PageKey> {
        let mut keys = Vec::new();
        if self.orbit.map.contains_key(&()) {
            keys.push(PageKey::Orbit);
        }
        if self.health.map.contains_key(&()) {
            keys.push(PageKey::Health);
        }
        keys.extend(self.packages.map.keys().cloned().map(PageKey::Package));
        keys.extend(self.symbols.map.keys().cloned().map(PageKey::Symbol));
        keys.extend(self.sources.map.keys().cloned().map(PageKey::Source));
        keys.extend(self.cargo_sources.map.keys().cloned().map(PageKey::CargoSource));
        keys.extend(self.searches.map.keys().cloned().map(PageKey::Search));
        keys.extend(self.browse.map.keys().cloned().map(PageKey::Browse));
        keys
    }

    /// Returns the activity of one slot.
    #[must_use]
    pub fn activity(&self, key: &PageKey) -> Activity {
        match key {
            PageKey::Symbol(symbol) => self.symbols.get(symbol).activity(),
            PageKey::Source(symbol) => self.sources.get(symbol).activity(),
            PageKey::CargoSource(file) => self.cargo_sources.get(file).activity(),
            PageKey::Package(package) => self.packages.get(package).activity(),
            PageKey::Search(query) => self.searches.get(query).activity(),
            PageKey::Orbit => self.orbit.get(&()).activity(),
            PageKey::Health => self.health.get(&()).activity(),
            PageKey::Browse(browse) => self.browse.get(browse).activity(),
        }
    }
}

/// A landing replaces what was there.
fn replace<T>(_previous: Option<&T>, next: T) -> T {
    next
}

fn take<T>(
    result: Result<PageValue, ReadFailure>,
    pick: impl FnOnce(PageValue) -> Option<T>,
) -> Result<T, ReadFailure> {
    match result {
        Ok(value) => pick(value).ok_or_else(|| {
            ReadFailure::Fault(ErrorValue::new(
                crate::core::FaultCode::Protocol,
                "a read result landed in a slot of another page family",
            ))
        }),
        Err(failure) => Err(failure),
    }
}

fn append_search(previous: &SearchPage, next: SearchPage) -> SearchPage {
    let offset = previous.rows.len();
    let rows = previous
        .rows
        .iter()
        .cloned()
        .chain(next.rows.iter().cloned().map(|mut row| {
            row.rank += offset;
            row
        }))
        .collect::<Vec<_>>();
    SearchPage {
        query: next.query,
        rows: rows.into(),
        coverage: previous.coverage.across_pages(next.coverage),
        next: next.next,
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
