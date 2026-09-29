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
use super::health::HealthModel;
use super::key::{PageKey, SearchQuery};
use super::orbit::OrbitModel;
use super::package::PackageDossier;
use super::search::SearchPage;
use super::source::SourceView;
use super::symbol::SymbolPage;
use crate::core::{Activity, ErrorValue, Resource, UnavailableReason, VersionedRoot};
use std::collections::BTreeMap;
use std::sync::Arc;

/// One read-model value produced by the read pool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PageValue {
    /// A declaration page.
    Symbol(SymbolPage),
    /// A source view.
    Source(SourceView),
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

/// Cheap identity of one slot's visible state, for equality-gated views.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Stamp(u64);

#[derive(Debug)]
struct Slot<T> {
    resource: Resource<T>,
    /// Generation of the fetch that owns the slot; 0 before any fetch.
    generation: u64,
    /// Whether `generation` is still running.
    inflight: bool,
    /// Visible revision; bumps on every visible change.
    revision: u64,
    /// Logical access time for LRU eviction.
    used: u64,
    /// Root the running (or last) fetch was issued at.
    asked_at: Option<VersionedRoot>,
    /// The value came from the launch snapshot and no owner has confirmed
    /// it yet (W-Open I2): its next fetch is quiet.
    seeded: bool,
    /// The running fetch is a quiet revalidation: it changed nothing
    /// visible when it began, and an equal value changes nothing when it
    /// lands.
    quiet: bool,
}

impl<T> Slot<T> {
    const fn new(used: u64) -> Self {
        Self {
            resource: Resource::not_yet(),
            generation: 0,
            inflight: false,
            revision: 0,
            used,
            asked_at: None,
            seeded: false,
            quiet: false,
        }
    }

    /// Whether a fetch is needed to show this slot at `root`.
    fn wants_fetch(&self, root: VersionedRoot) -> bool {
        if self.inflight {
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
        clock: u64,
        generation: u64,
    ) -> Option<u64> {
        if !self.map.contains_key(key) {
            self.evict_for_insert();
            self.map.insert(key.clone(), Slot::new(clock));
        }
        let slot = self.map.get_mut(key)?;
        slot.used = clock;
        if !(force || slot.wants_fetch(root)) {
            return None;
        }
        slot.generation = generation;
        slot.inflight = true;
        slot.asked_at = Some(root);
        // A snapshot value is revalidated quietly: it stays exactly as drawn
        // (no "working", no stamp) until a different value lands.
        slot.quiet = slot.seeded && !force;
        if !slot.quiet {
            slot.resource = std::mem::replace(&mut slot.resource, Resource::not_yet()).working();
            slot.revision = slot.revision.wrapping_add(1);
        }
        Some(generation)
    }

    /// Fills a slot from the launch snapshot, current at `root` (the
    /// unserved root at launch). A slot a fetch already owns is left alone.
    fn seed(&mut self, key: &K, value: T, root: VersionedRoot, clock: u64) -> bool {
        if !self.map.contains_key(key) {
            self.evict_for_insert();
            self.map.insert(key.clone(), Slot::new(clock));
        }
        let Some(slot) = self.map.get_mut(key) else {
            return false;
        };
        if slot.inflight || slot.resource.loaded_value().is_some() {
            return false;
        }
        slot.used = clock;
        slot.asked_at = Some(root);
        slot.resource = Resource::loaded_at(value, root);
        slot.seeded = true;
        slot.revision = slot.revision.wrapping_add(1);
        true
    }

    /// The owner serves the root the snapshot was read at: the seeded value
    /// is current at `root` as it is. Nothing visible changes.
    fn confirm(&mut self, key: &K, root: VersionedRoot) -> bool {
        let Some(slot) = self.map.get_mut(key) else {
            return false;
        };
        if !slot.seeded || slot.inflight {
            return false;
        }
        slot.seeded = false;
        slot.asked_at = Some(root);
        slot.resource = std::mem::replace(&mut slot.resource, Resource::not_yet()).rebased(root);
        true
    }

    fn touch(&mut self, key: &K, clock: u64) {
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
                .filter(|(_, slot)| !slot.inflight)
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
        generation: u64,
        result: Result<T, ReadFailure>,
        merge: impl FnOnce(Option<&T>, T) -> T,
    ) -> Landing
    where
        T: PartialEq,
    {
        let Some(slot) = self.map.get_mut(key) else {
            return Landing::Superseded;
        };
        if slot.generation != generation || !slot.inflight {
            return Landing::Superseded;
        }
        slot.inflight = false;
        let root = slot.asked_at;
        if std::mem::take(&mut slot.quiet) {
            match (&result, root) {
                (Ok(value), Some(root)) if slot.resource.loaded_value() == Some(value) => {
                    slot.seeded = false;
                    slot.resource =
                        std::mem::replace(&mut slot.resource, Resource::not_yet()).rebased(root);
                    return Landing::Unchanged;
                }
                (Err(ReadFailure::Cancelled), _) => {
                    // Still the snapshot's value, still unconfirmed.
                    slot.asked_at = None;
                    return Landing::Unchanged;
                }
                (Ok(_), _) => slot.seeded = false,
                // A failed revalidation keeps the value it could not confirm.
                (Err(_), _) => {}
            }
        }
        let previous = std::mem::replace(&mut slot.resource, Resource::not_yet());
        slot.resource = match (result, root) {
            (Ok(value), Some(root)) => {
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
                previous.resting()
            }
        };
        slot.revision = slot.revision.wrapping_add(1);
        Landing::Applied
    }

    fn cancel(&mut self, key: &K) -> Option<u64> {
        let slot = self.map.get_mut(key)?;
        if !slot.inflight {
            return None;
        }
        slot.inflight = false;
        slot.asked_at = None;
        if std::mem::take(&mut slot.quiet) {
            // Nothing visible began, so nothing visible ends.
            return Some(slot.generation);
        }
        slot.resource = std::mem::replace(&mut slot.resource, Resource::not_yet()).resting();
        slot.revision = slot.revision.wrapping_add(1);
        Some(slot.generation)
    }

    fn get(&self, key: &K) -> Resource<T> {
        self.map
            .get(key)
            .map_or_else(Resource::not_yet, |slot| slot.resource.clone())
    }

    fn stamp(&self, key: &K) -> Stamp {
        Stamp(self.map.get(key).map_or(0, |slot| slot.revision))
    }

    fn seeded(&self, key: &K) -> bool {
        self.map.get(key).is_some_and(|slot| slot.seeded)
    }

    fn inflight(&self, key: &K) -> Option<u64> {
        self.map
            .get(key)
            .filter(|slot| slot.inflight)
            .map(|slot| slot.generation)
    }
}

/// Bounds for each page family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Capacity {
    /// Declaration pages.
    pub symbols: usize,
    /// Source views.
    pub sources: usize,
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
    packages: Slots<PackageRef, PackageDossier>,
    searches: Slots<SearchQuery, SearchPage>,
    orbit: Slots<(), OrbitModel>,
    health: Slots<(), HealthModel>,
    browse: Slots<crate::model::browse::BrowseKey, crate::model::browse::BrowseValue>,
    clock: u64,
    next_generation: u64,
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
            packages: Slots::new(capacity.packages),
            searches: Slots::new(capacity.searches),
            orbit: Slots::new(1),
            health: Slots::new(1),
            browse: Slots::new(4),
            clock: 0,
            next_generation: 1,
        }
    }

    /// Records an access and, when the slot needs data at `root` (never asked,
    /// or asked at an older root), starts a fetch: the slot turns working
    /// (keeping any last good value) and the new generation is returned.
    /// Returns `None` when the slot is current or already in flight.
    pub fn begin(&mut self, key: &PageKey, root: VersionedRoot) -> Option<u64> {
        self.begin_with(key, root, false)
    }

    /// Starts a fetch even when the slot is current (retry, "load more").
    pub fn begin_forced(&mut self, key: &PageKey, root: VersionedRoot) -> Option<u64> {
        self.begin_with(key, root, true)
    }

    fn begin_with(&mut self, key: &PageKey, root: VersionedRoot, force: bool) -> Option<u64> {
        self.clock = self.clock.wrapping_add(1);
        let clock = self.clock;
        let generation = self.next_generation;
        let started = dispatch!(self, key, |slots, k| slots
            .begin(k, root, force, clock, generation));
        if started.is_some() {
            self.next_generation = self.next_generation.wrapping_add(1).max(1);
        }
        started
    }

    /// Fills `key`'s slot with a launch-snapshot value, current at `root`
    /// until an owner confirms it ([`Self::confirm`]) or revalidates it (its
    /// next fetch is quiet: [`Landing::Unchanged`] when the value is equal).
    /// Returns whether the slot took it (a family the snapshot does not
    /// keep, a slot in flight or already holding a value, does not).
    pub fn seed(&mut self, key: &PageKey, value: PageValue, root: VersionedRoot) -> bool {
        self.clock = self.clock.wrapping_add(1);
        let clock = self.clock;
        match (key, value) {
            (PageKey::Symbol(symbol), PageValue::Symbol(page)) => self.symbols.seed(symbol, page, root, clock),
            (PageKey::Source(symbol), PageValue::Source(view)) => self.sources.seed(symbol, view, root, clock),
            (PageKey::Package(package), PageValue::Package(dossier)) => {
                self.packages.seed(package, dossier, root, clock)
            }
            (PageKey::Orbit, PageValue::Orbit(model)) => self.orbit.seed(&(), model, root, clock),
            _ => false,
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

    /// Records an access without starting a fetch.
    pub fn touch(&mut self, key: &PageKey) {
        self.clock = self.clock.wrapping_add(1);
        let clock = self.clock;
        dispatch!(self, key, |slots, k| slots.touch(k, clock));
    }

    /// Admits one result if it names the slot's current generation.
    #[allow(clippy::too_many_lines)] // one arm per page family
    pub fn land(
        &mut self,
        key: &PageKey,
        generation: u64,
        result: Result<PageValue, ReadFailure>,
    ) -> Landing {
        match (key, result) {
            (PageKey::Symbol(symbol), result) => self.symbols.land(
                symbol,
                generation,
                take(result, |value| match value {
                    PageValue::Symbol(page) => Some(page),
                    _ => None,
                }),
                |_, next| next,
            ),
            (PageKey::Source(symbol), result) => self.sources.land(
                symbol,
                generation,
                take(result, |value| match value {
                    PageValue::Source(view) => Some(view),
                    _ => None,
                }),
                |_, next| next,
            ),
            (PageKey::Package(package), result) => self.packages.land(
                package,
                generation,
                take(result, |value| match value {
                    PageValue::Package(dossier) => Some(dossier),
                    _ => None,
                }),
                |_, next| next,
            ),
            (PageKey::Search(query), result) => {
                let append = matches!(result, Ok(PageValue::SearchMore(_)));
                self.searches.land(
                    query,
                    generation,
                    take(result, |value| match value {
                        PageValue::Search(page) | PageValue::SearchMore(page) => Some(page),
                        _ => None,
                    }),
                    move |previous, next| match previous {
                        Some(previous) if append => append_search(previous, next),
                        _ => next,
                    },
                )
            }
            (PageKey::Orbit, result) => self.orbit.land(
                &(),
                generation,
                take(result, |value| match value {
                    PageValue::Orbit(model) => Some(model),
                    _ => None,
                }),
                |_, next| next,
            ),
            (PageKey::Health, result) => self.health.land(
                &(),
                generation,
                take(result, |value| match value {
                    PageValue::Health(model) => Some(model),
                    _ => None,
                }),
                |_, next| next,
            ),
            (PageKey::Browse(browse), result) => self.browse.land(
                browse,
                generation,
                take(result, |value| match value {
                    PageValue::Browse(value) => Some(value),
                    _ => None,
                }),
                |_, next| next,
            ),
        }
    }

    /// Cancels the running fetch for `key`, returning its generation.
    pub fn cancel(&mut self, key: &PageKey) -> Option<u64> {
        dispatch!(self, key, |slots, k| slots.cancel(k))
    }

    /// Returns the generation of the running fetch for `key`.
    #[must_use]
    pub fn inflight(&self, key: &PageKey) -> Option<u64> {
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
        keys.extend(self.searches.map.keys().cloned().map(PageKey::Search));
        keys.extend(self.browse.map.keys().cloned().map(PageKey::Browse));
        keys
    }

    /// Returns the number of resident slots per family:
    /// (symbols, sources, packages, searches).
    #[must_use]
    pub fn resident(&self) -> (usize, usize, usize, usize) {
        (
            self.symbols.map.len(),
            self.sources.map.len(),
            self.packages.map.len(),
            self.searches.map.len(),
        )
    }

    /// Returns the activity of one slot.
    #[must_use]
    pub fn activity(&self, key: &PageKey) -> Activity {
        match key {
            PageKey::Symbol(symbol) => self.symbols.get(symbol).activity(),
            PageKey::Source(symbol) => self.sources.get(symbol).activity(),
            PageKey::Package(package) => self.packages.get(package).activity(),
            PageKey::Search(query) => self.searches.get(query).activity(),
            PageKey::Orbit => self.orbit.get(&()).activity(),
            PageKey::Health => self.health.get(&()).activity(),
            PageKey::Browse(browse) => self.browse.get(browse).activity(),
        }
    }
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
