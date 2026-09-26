//! The data plane's GPUI entity: the snapshot mirror, the keyed page store,
//! and the read pool, with typed change events for region views.
//!
//! Region views read from one `Entity<DataStore>` and subscribe to its
//! [`StoreEvent`]s. An event names exactly which snapshot branch or which
//! page key changed, and it is only emitted when that slice actually changed,
//! so a view re-renders only for its own slice:
//!
//! ```ignore
//! let key = PageKey::Symbol(symbol);
//! cx.subscribe(&store, move |view, store, event, cx| {
//!     if event.touches(&view.key) {
//!         let stamp = store.read(cx).stamp(&view.key);
//!         if view.seen != Some(stamp) {
//!             view.seen = Some(stamp);
//!             cx.notify();
//!         }
//!     }
//! })
//! .detach();
//! store.update(cx, |store, cx| store.ensure(key, cx));
//! ```
//!
//! Results arrive through a coalescing wake signal awaited by one
//! `cx.spawn` task: nothing polls, and an idle window requests no frame.

use super::actor::CancellationToken;
use super::reads::{Priority, ReadJob, ReadPool, ReadRequest};
use crate::core::{Resource, UnavailableReason};
use crate::model::AppSnapshot;
use crate::model::pages::{
    HealthModel, Landing, OrbitModel, PackageDossier, PackageRef, PageKey, PageStore, ReadFailure,
    SearchPage, SearchQuery, SourceView, Stamp, SymbolPage, SymbolRef,
};
use crate::navigation::Route;
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Task};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// One snapshot branch, as named by a change event.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Branch {
    /// The producer root advanced (page data from older roots is refreshed).
    Root,
    /// The content route changed.
    Route,
    /// The transient overlay changed.
    Overlay,
    /// Local project lifecycle rows changed.
    Workspace,
    /// Settings changed.
    Settings,
    /// Shelf rows changed.
    Shelf,
    /// The Orbit registry catalog changed.
    Catalog,
    /// The served project changed.
    Project,
    /// Local manifest facts changed.
    LocalPackage,
    /// Document tabs changed.
    Documents,
    /// The mounted graph's semantic selection changed, without a route change.
    GraphFocus,
}

/// A typed change notification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreEvent {
    /// One snapshot branch changed.
    Snapshot(Branch),
    /// One page resource changed (started, landed, failed, or cancelled).
    Resource(PageKey),
}

impl StoreEvent {
    /// Returns whether this event is about `key`.
    #[must_use]
    pub fn touches(&self, key: &PageKey) -> bool {
        matches!(self, Self::Resource(changed) if changed == key)
    }

    /// Returns whether this event is about `branch`.
    #[must_use]
    pub fn is_branch(&self, branch: Branch) -> bool {
        matches!(self, Self::Snapshot(changed) if *changed == branch)
    }
}

/// The equality gate a region view keeps for the slices it renders.
///
/// A view lists the page keys and snapshot branches it draws; on each store
/// event it asks [`Watch::changed`], which answers `true` only when one of
/// those slices' visible state actually moved (a new stamp for a key, or an
/// event for a watched branch), so the view calls `cx.notify()` only then.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Watch {
    keys: BTreeMap<PageKey, Stamp>,
    branches: BTreeSet<Branch>,
}

impl Watch {
    /// Watches `keys` and `branches`, starting from the store's current stamps.
    #[must_use]
    pub fn new(store: &DataStore, keys: impl IntoIterator<Item = PageKey>, branches: &[Branch]) -> Self {
        Self {
            keys: keys
                .into_iter()
                .map(|key| {
                    let stamp = store.stamp(&key);
                    (key, stamp)
                })
                .collect(),
            branches: branches.iter().copied().collect(),
        }
    }

    /// Replaces the watched keys (after the view navigates).
    pub fn retarget(&mut self, store: &DataStore, keys: impl IntoIterator<Item = PageKey>) {
        self.keys = keys
            .into_iter()
            .map(|key| {
                let stamp = store.stamp(&key);
                (key, stamp)
            })
            .collect();
    }

    /// Returns whether `event` changed a watched slice, recording its stamp.
    pub fn changed(&mut self, store: &DataStore, event: &StoreEvent) -> bool {
        match event {
            StoreEvent::Snapshot(branch) => self.branches.contains(branch),
            StoreEvent::Resource(key) => match self.keys.get_mut(key) {
                Some(seen) => {
                    let stamp = store.stamp(key);
                    let moved = *seen != stamp;
                    *seen = stamp;
                    moved
                }
                None => false,
            },
        }
    }

    /// Returns the watched keys.
    pub fn keys(&self) -> impl Iterator<Item = &PageKey> {
        self.keys.keys()
    }
}

/// Counters for tests and the debug page.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StoreStats {
    /// Wake turns that drained at least zero results.
    pub turns: u64,
    /// Results admitted into their slot.
    pub landed: u64,
    /// Results dropped because a newer request owned the slot.
    pub superseded: u64,
    /// Jobs submitted to the pool.
    pub submitted: u64,
    /// Prefetch jobs submitted.
    pub prefetched: u64,
    /// Jobs cancelled by navigation or hover exit.
    pub cancelled: u64,
    /// Events emitted.
    pub events: u64,
}

/// The data plane entity.
pub struct DataStore {
    snapshot: Arc<AppSnapshot>,
    pages: PageStore,
    pool: Option<ReadPool>,
    /// Keys the current route displays; refreshed on root advance.
    focused: BTreeSet<PageKey>,
    /// Keys whose in-flight job is a prefetch.
    prefetching: BTreeSet<PageKey>,
    wake_task: Option<Task<()>>,
    stats: StoreStats,
    graph_focus: Option<super::graph_focus::GraphFocus>,
    graph_notice: Option<super::graph_focus::GraphNotice>,
}

impl std::fmt::Debug for DataStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataStore")
            .field("root", &self.snapshot.key())
            .field("focused", &self.focused)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<StoreEvent> for DataStore {}

/// The package a route reads, scoped to the release it views.
#[must_use]
pub fn route_package(route: &Route) -> Option<PackageRef> {
    let (package, at) = match route {
        Route::Package(route) => (&route.package, &route.at),
        Route::Symbol(route) => (&route.package, &route.at),
        Route::Orbit(_) | Route::World => return None,
    };
    let pinned = PackageRef::parse(package.as_str()).ok()?;
    Some(match at {
        Some(at) => pinned.at(at.as_str()).unwrap_or(pinned),
        None => pinned,
    })
}

/// The declaration a route reads, scoped to the release it views.
#[must_use]
pub fn route_symbol(route: &Route) -> Option<SymbolRef> {
    route_declaration(route).ok()
}

/// Why a route reads no declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Unread {
    /// The address does not spell a declaration.
    NotADeclaration,
    /// The route views a release its package cannot be read at: a
    /// workspace crate has only its working copy in the index.
    ReleaseNotHere(crate::navigation::ReleaseId),
}

/// [`route_symbol`], saying why when there is none.
///
/// # Errors
/// [`Unread`] when the route names no declaration this index can read.
pub fn route_declaration(route: &Route) -> Result<SymbolRef, Unread> {
    let Route::Symbol(route) = route else {
        return Err(Unread::NotADeclaration);
    };
    let symbol = SymbolRef::new(route.id.as_str()).map_err(|_| Unread::NotADeclaration)?;
    let Some(at) = &route.at else {
        return Ok(symbol);
    };
    let pinned = PackageRef::parse(route.package.as_str()).map_err(|_| Unread::NotADeclaration)?;
    let viewed = pinned.at(at.as_str()).ok_or_else(|| Unread::ReleaseNotHere(at.clone()))?;
    Ok(symbol.rebased(&pinned, &viewed).unwrap_or(symbol))
}

/// Returns the page keys one route displays.
#[must_use]
pub fn route_keys(route: &Route) -> Vec<PageKey> {
    match route {
        Route::Orbit(_) => vec![PageKey::Orbit, PageKey::Health],
        Route::World => vec![PageKey::Orbit],
        Route::Package(_) => route_package(route).map(PageKey::Package).into_iter().collect(),
        Route::Symbol(symbol) => route_symbol(route)
            .map(|id| match symbol.view {
                crate::navigation::View::Code => PageKey::Source(id),
                crate::navigation::View::Page | crate::navigation::View::Graph => PageKey::Symbol(id),
            })
            .into_iter()
            .collect(),
    }
}

impl DataStore {
    /// Creates a store around the current snapshot. `pool` is the read lane;
    /// without one, every page is reported unavailable instead of spinning.
    #[must_use]
    pub fn new(snapshot: Arc<AppSnapshot>, pool: Option<ReadPool>) -> Self {
        Self {
            snapshot,
            pages: PageStore::default(),
            pool,
            focused: BTreeSet::new(),
            prefetching: BTreeSet::new(),
            wake_task: None,
            stats: StoreStats::default(),
            graph_focus: None,
            graph_notice: None,
        }
    }

    /// A display selection never becomes an address or a history entry.
    pub(crate) fn graph_focus(&self) -> Option<&super::graph_focus::GraphFocus> {
        self.graph_focus.as_ref().filter(|focus| focus.active(&self.snapshot))
    }

    pub(crate) fn graph_notice(&self) -> Option<&super::graph_focus::GraphNotice> {
        self.graph_notice.as_ref().filter(|notice| notice.active(&self.snapshot))
    }

    /// Semantic equality is the notification boundary: camera and hover frames
    /// cannot invalidate the shell regions or schedule data-plane reads.
    pub(crate) fn admit_graph_focus(&mut self, focus: Option<super::graph_focus::GraphFocus>, notice: Option<super::graph_focus::GraphNotice>, cx: &mut Context<Self>) {
        if self.graph_focus != focus || self.graph_notice != notice {
            self.graph_focus = focus;
            self.graph_notice = notice;
            self.emit(StoreEvent::Snapshot(Branch::GraphFocus), cx);
        }
    }

    /// Creates the entity and starts its wake task.
    pub fn install(cx: &mut App, snapshot: Arc<AppSnapshot>, pool: Option<ReadPool>) -> Entity<Self> {
        cx.new(|cx| {
            let route = route_keys(snapshot.route());
            let mut store = Self::new(snapshot, pool);
            store.start(cx);
            // The first route is focused like every later one.
            store.focus(route, cx);
            store
        })
    }

    /// Spawns the one task that turns read-pool wakes into landings.
    fn start(&mut self, cx: &mut Context<Self>) {
        let Some(mut receiver) = self.pool.as_mut().and_then(ReadPool::take_wake) else {
            return;
        };
        self.wake_task = Some(cx.spawn(async move |this, cx| {
            while receiver.wait().await.is_some() {
                if this.update(cx, Self::drain).is_err() {
                    break;
                }
            }
        }));
    }

    /// Returns the current snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Arc<AppSnapshot> {
        Arc::clone(&self.snapshot)
    }

    /// Returns counters.
    #[must_use]
    pub const fn stats(&self) -> StoreStats {
        self.stats
    }

    /// Returns the keys the current route displays.
    #[must_use]
    pub const fn focused(&self) -> &BTreeSet<PageKey> {
        &self.focused
    }

    /// Returns the read pool's (queued, running) job counts.
    #[must_use]
    pub fn pool_load(&self) -> (usize, usize) {
        self.pool
            .as_ref()
            .map_or((0, 0), |pool| (pool.queued(), pool.running()))
    }

    fn emit(&mut self, event: StoreEvent, cx: &mut Context<Self>) {
        self.stats.events = self.stats.events.saturating_add(1);
        cx.emit(event);
    }

    /// Admits a new snapshot from the root entity, emitting one event per
    /// branch that actually changed. A root advance refreshes the focused
    /// pages; a route change refocuses.
    pub fn admit_snapshot(&mut self, snapshot: Arc<AppSnapshot>, cx: &mut Context<Self>) {
        if Arc::ptr_eq(&self.snapshot, &snapshot) {
            return;
        }
        let old = std::mem::replace(&mut self.snapshot, snapshot);
        let snapshot = Arc::clone(&self.snapshot);
        let mut changed = Vec::new();
        if !old.key().same_authority(snapshot.key()) {
            changed.push(Branch::Root);
        }
        if old.route() != snapshot.route() {
            changed.push(Branch::Route);
        }
        if old.overlay() != snapshot.overlay() {
            changed.push(Branch::Overlay);
        }
        if !old.shares_workspace_with(&snapshot) && old.workspace() != snapshot.workspace() {
            changed.push(Branch::Workspace);
        }
        if !old.shares_settings_with(&snapshot) && old.settings() != snapshot.settings() {
            changed.push(Branch::Settings);
        }
        if !old.shares_shelf_with(&snapshot) && old.shelf() != snapshot.shelf() {
            changed.push(Branch::Shelf);
        }
        if old.catalog() != snapshot.catalog() {
            changed.push(Branch::Catalog);
        }
        if old.project() != snapshot.project() {
            changed.push(Branch::Project);
        }
        if old.local_package() != snapshot.local_package() {
            changed.push(Branch::LocalPackage);
        }
        if old.documents() != snapshot.documents() {
            changed.push(Branch::Documents);
        }
        for branch in &changed {
            self.emit(StoreEvent::Snapshot(*branch), cx);
        }
        if changed.contains(&Branch::Route) {
            self.focus(route_keys(snapshot.route()), cx);
        } else if changed.contains(&Branch::Root) {
            let focused = self.focused.iter().cloned().collect::<Vec<_>>();
            for key in focused {
                self.ensure(key, cx);
            }
        }
    }

    /// Ensures one page is loaded at the current root. Idempotent: a page
    /// that is current or in flight costs nothing, so views may call this
    /// from render. A queued prefetch for the key is promoted.
    pub fn ensure(&mut self, key: PageKey, cx: &mut Context<Self>) -> Stamp {
        self.keep_focused_resident();
        let root = self.snapshot.key();
        if let Some(generation) = self.pages.begin(&key, root) {
            self.prefetching.remove(&key);
            self.submit(key.clone(), ReadRequest::for_key(&key), generation, Priority::Normal, None, cx);
            let stamp = self.pages.stamp(&key);
            self.emit(StoreEvent::Resource(key), cx);
            return stamp;
        }
        if self.prefetching.remove(&key) {
            // A view now waits on a prefetch: a queued one jumps the queue,
            // a running one simply keeps running.
            if let Some(pool) = &self.pool {
                let _ = pool.promote(&key);
            }
        }
        self.pages.stamp(&key)
    }

    /// Replaces the focused key set: jobs for keys the route no longer shows
    /// are cancelled (queued ones never run; running ones stop at their next
    /// round trip and are dropped on landing), and the new keys are ensured.
    pub fn focus(&mut self, keys: Vec<PageKey>, cx: &mut Context<Self>) {
        let next = keys.iter().cloned().collect::<BTreeSet<_>>();
        let dropped = self
            .focused
            .difference(&next)
            .cloned()
            .collect::<Vec<_>>();
        self.focused = next;
        for key in dropped {
            if self.prefetching.contains(&key) {
                continue;
            }
            self.cancel_key(&key, cx);
        }
        for key in keys {
            self.ensure(key, cx);
        }
    }

    /// Hover-intent prefetch: the view calls this after the pointer has
    /// rested on a link for 120 ms. Low priority, cancellable, and a no-op
    /// when the page is current or already requested.
    pub fn prefetch(&mut self, key: PageKey, cx: &mut Context<Self>) {
        self.keep_focused_resident();
        let root = self.snapshot.key();
        if let Some(generation) = self.pages.begin(&key, root) {
            self.prefetching.insert(key.clone());
            self.stats.prefetched = self.stats.prefetched.saturating_add(1);
            self.submit(key.clone(), ReadRequest::for_key(&key), generation, Priority::Prefetch, None, cx);
            self.emit(StoreEvent::Resource(key), cx);
        }
    }

    /// Keeps the pages the route shows most recently used, so a burst of
    /// prefetches can never evict the page on screen.
    fn keep_focused_resident(&mut self) {
        for key in &self.focused {
            self.pages.touch(key);
        }
    }

    /// Cancels a prefetch that has not been promoted by a view.
    pub fn cancel_prefetch(&mut self, key: &PageKey, cx: &mut Context<Self>) {
        if self.prefetching.remove(key) {
            self.cancel_key(key, cx);
        }
    }

    fn cancel_key(&mut self, key: &PageKey, cx: &mut Context<Self>) {
        if self.pages.inflight(key).is_none() {
            return;
        }
        if let Some(pool) = &self.pool {
            let _ = pool.cancel(key);
        }
        if self.pages.cancel(key).is_some() {
            self.stats.cancelled = self.stats.cancelled.saturating_add(1);
            self.emit(StoreEvent::Resource(key.clone()), cx);
        }
    }

    /// Fetches a page again even when it is current (retry after a fault).
    pub fn retry(&mut self, key: PageKey, cx: &mut Context<Self>) {
        let root = self.snapshot.key();
        if let Some(generation) = self.pages.begin_forced(&key, root) {
            self.prefetching.remove(&key);
            self.submit(key.clone(), ReadRequest::for_key(&key), generation, Priority::Normal, None, cx);
            self.emit(StoreEvent::Resource(key), cx);
        }
    }

    /// Requests the next page of a loaded search. Returns whether a request
    /// was issued (false when there is no continuation).
    pub fn load_more(&mut self, query: &SearchQuery, cx: &mut Context<Self>) -> bool {
        let Some(next) = self
            .pages
            .search(query)
            .loaded_value()
            .and_then(|page| page.next)
        else {
            return false;
        };
        let key = PageKey::Search(query.clone());
        let root = self.snapshot.key();
        let Some(generation) = self.pages.begin_forced(&key, root) else {
            return false;
        };
        self.submit(
            key.clone(),
            ReadRequest::SearchMore {
                query: query.clone(),
                continuation: next,
            },
            generation,
            Priority::Normal,
            Some(next.worker),
            cx,
        );
        self.emit(StoreEvent::Resource(key), cx);
        true
    }

    fn submit(
        &mut self,
        key: PageKey,
        request: ReadRequest,
        generation: u64,
        priority: Priority,
        affinity: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let Some(pool) = &self.pool else {
            // No read lane: say so once instead of leaving the page working.
            let landing = self.pages.land(
                &key,
                generation,
                Err(ReadFailure::Unavailable(
                    UnavailableReason::Unsupported,
                    Arc::from("this window has no read lane"),
                )),
            );
            if landing == Landing::Applied {
                self.emit(StoreEvent::Resource(key), cx);
            }
            return;
        };
        self.stats.submitted = self.stats.submitted.saturating_add(1);
        pool.submit(ReadJob {
            key,
            request,
            generation,
            priority,
            cancel: CancellationToken::new(),
            affinity,
        });
    }

    /// Lands every finished read. Called by the wake task; public so tests
    /// and harnesses can drive it deterministically.
    pub fn drain(&mut self, cx: &mut Context<Self>) -> usize {
        self.stats.turns = self.stats.turns.saturating_add(1);
        let Some(pool) = &self.pool else {
            return 0;
        };
        let outcomes = pool.drain();
        let mut applied = 0;
        for outcome in outcomes {
            if self.pages.inflight(&outcome.key) == Some(outcome.generation) {
                self.prefetching.remove(&outcome.key);
            }
            match self.pages.land(&outcome.key, outcome.generation, outcome.result) {
                Landing::Applied => {
                    applied += 1;
                    self.stats.landed = self.stats.landed.saturating_add(1);
                    self.emit(StoreEvent::Resource(outcome.key), cx);
                }
                Landing::Superseded => {
                    self.stats.superseded = self.stats.superseded.saturating_add(1);
                }
            }
        }
        applied
    }

    /// Returns the visible-state stamp of one page slot.
    #[must_use]
    pub fn stamp(&self, key: &PageKey) -> Stamp {
        self.pages.stamp(key)
    }

    /// Returns whether a fetch for `key` is running or queued.
    #[must_use]
    pub fn is_loading(&self, key: &PageKey) -> bool {
        self.pages.inflight(key).is_some()
    }

    /// Returns whether the in-flight fetch for `key` is a prefetch.
    #[must_use]
    pub fn is_prefetching(&self, key: &PageKey) -> bool {
        self.prefetching.contains(key)
    }

    /// Returns the declaration page.
    #[must_use]
    pub fn symbol(&self, symbol: &SymbolRef) -> Resource<SymbolPage> {
        self.pages.symbol(symbol)
    }

    /// Returns the source view.
    #[must_use]
    pub fn source(&self, symbol: &SymbolRef) -> Resource<SourceView> {
        self.pages.source(symbol)
    }

    /// Returns the package dossier.
    #[must_use]
    pub fn package(&self, package: &PackageRef) -> Resource<PackageDossier> {
        self.pages.package(package)
    }

    /// Returns accumulated search results.
    #[must_use]
    pub fn search(&self, query: &SearchQuery) -> Resource<SearchPage> {
        self.pages.search(query)
    }

    /// Returns the Orbit model.
    #[must_use]
    pub fn orbit(&self) -> Resource<OrbitModel> {
        self.pages.orbit()
    }

    /// Returns the health model.
    #[must_use]
    pub fn health(&self) -> Resource<HealthModel> {
        self.pages.health()
    }

    /// Returns the underlying keyed store (read-only).
    #[must_use]
    pub const fn pages(&self) -> &PageStore {
        &self.pages
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::core::{Activity, VersionedRoot};
    use crate::model::pages::{
        DeclRef, Gap, GapReason, HealthModel, IngestModel, Known, PageValue, Rose, SourceSite,
    };
    use crate::runtime::reads::{PageReader, ReadContext};
    use gpui::{Subscription, TestAppContext};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::{Condvar, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    type Gate = Arc<(Mutex<BTreeSet<String>>, Condvar)>;

    /// Answers every page at once, except coordinates starting with `slow`,
    /// which wait for the test to open their gate (or for cancellation).
    struct FixtureReader {
        gate: Gate,
    }

    fn unknown() -> Gap {
        Gap::new(GapReason::NotServed, "fixture")
    }

    fn page(symbol: &SymbolRef) -> SymbolPage {
        let identity = DeclRef::from_label(symbol.as_str(), None, None, None).expect("decl");
        SymbolPage {
            identity,
            package: Known::Unknown(unknown()),
            signature: Known::Unknown(unknown()),
            docs: Arc::from([]),
            site: SourceSite {
                location: Known::Unknown(unknown()),
                excerpt: Known::Unknown(unknown()),
            },
            members: Known::Unknown(unknown()),
            rose: Rose {
                up: Known::Unknown(unknown()),
                down: Known::Unknown(unknown()),
                left: Known::Unknown(unknown()),
                right: Known::Unknown(unknown()),
                implemented_by: Known::Unknown(unknown()),
            },
            references: Known::Unknown(unknown()),
            outline: Known::Unknown(unknown()),
        }
    }

    fn health(rows: u64) -> HealthModel {
        HealthModel {
            lanes: backend_present::CoverageLine::new(&[], Some(rows)),
            rows,
            ingest: IngestModel {
                files_discovered: 1,
                files_indexed: 1,
                files_unavailable: 0,
                declarations: rows,
                languages: Arc::from([]),
                faults: Arc::from([]),
            },
            ready_capabilities: Arc::from([]),
            missing_capabilities: Arc::from([]),
        }
    }

    impl PageReader for FixtureReader {
        fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
            match request {
                ReadRequest::Symbol(symbol) => {
                    let (lock, opened) = &*self.gate;
                    let mut open = lock.lock().unwrap_or_else(PoisonError::into_inner);
                    let deadline = Instant::now() + Duration::from_secs(10);
                    while symbol.as_str().starts_with("slow") && !open.contains(symbol.as_str()) {
                        if context.cancel.is_cancelled() {
                            return Err(ReadFailure::Cancelled);
                        }
                        assert!(Instant::now() < deadline, "gate never opened");
                        open = opened
                            .wait_timeout(open, Duration::from_millis(5))
                            .unwrap_or_else(PoisonError::into_inner)
                            .0;
                    }
                    Ok(PageValue::Symbol(page(symbol)))
                }
                ReadRequest::Health => Ok(PageValue::Health(health(7))),
                _ => Err(ReadFailure::Unavailable(
                    UnavailableReason::Unsupported,
                    Arc::from("fixture reader"),
                )),
            }
        }
    }

    struct Rig {
        store: Entity<DataStore>,
        events: Rc<RefCell<Vec<StoreEvent>>>,
        gate: Gate,
        /// Counters after the initial Orbit route settled.
        base: StoreStats,
        _subscription: Subscription,
    }

    fn snapshot() -> Arc<AppSnapshot> {
        Arc::new(AppSnapshot::empty(VersionedRoot::synthetic(
            backend_library::view_state_root(&[("store".to_owned(), "tests".to_owned())]),
            5,
        )))
    }

    fn rig(cx: &mut TestAppContext, workers: usize) -> Rig {
        cx.executor().allow_parking();
        let gate: Gate = Arc::new((Mutex::new(BTreeSet::new()), Condvar::new()));
        let reader_gate = Arc::clone(&gate);
        let pool = ReadPool::start(workers, move |_| FixtureReader {
            gate: Arc::clone(&reader_gate),
        })
        .expect("pool");
        let store = cx.update(|cx| DataStore::install(cx, snapshot(), Some(pool)));
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&events);
        let subscription = cx.update(|cx| {
            cx.subscribe(&store, move |_, event: &StoreEvent, _| {
                sink.borrow_mut().push(event.clone());
            })
        });
        let mut rig = Rig {
            store,
            events,
            gate,
            base: StoreStats::default(),
            _subscription: subscription,
        };
        // The store focuses its first route (Orbit) at install; let those
        // reads settle so each test starts from an idle store.
        rig.until(cx, |store| {
            store.health().is_loaded() && store.orbit().activity() == Activity::Stopped
        });
        rig.take_events();
        rig.base = rig.store.read_with(cx, |store, _| store.stats());
        rig
    }

    impl Rig {
        fn open(&self, name: &str) {
            let (lock, opened) = &*self.gate;
            lock.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(name.to_owned());
            opened.notify_all();
        }

        fn take_events(&self) -> Vec<StoreEvent> {
            std::mem::take(&mut *self.events.borrow_mut())
        }

        /// Runs the executor until `done` holds, letting worker threads land.
        fn until(&self, cx: &mut TestAppContext, done: impl Fn(&DataStore) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                cx.run_until_parked();
                if self.store.read_with(cx, |store, _| done(store)) {
                    return;
                }
                assert!(Instant::now() < deadline, "condition never held");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn symbol(name: &str) -> SymbolRef {
        SymbolRef::new(name).expect("symbol")
    }

    #[gpui::test]
    fn ensure_is_idempotent_and_a_landing_wakes_the_store_with_one_event(cx: &mut TestAppContext) {
        let rig = rig(cx, 2);
        let key = PageKey::Symbol(symbol("fast-page"));
        rig.store.update(cx, |store, cx| {
            store.ensure(key.clone(), cx);
            // Views call ensure from render: repeats cost nothing.
            store.ensure(key.clone(), cx);
            store.ensure(key.clone(), cx);
        });
        assert_eq!(rig.take_events(), [StoreEvent::Resource(key.clone())]);
        rig.until(cx, |store| store.symbol(&symbol("fast-page")).is_loaded());
        assert_eq!(rig.take_events(), [StoreEvent::Resource(key.clone())]);
        let (submitted, landed) = rig
            .store
            .read_with(cx, |store, _| (store.stats().submitted, store.stats().landed));
        assert_eq!(
            (submitted - rig.base.submitted, landed - rig.base.landed),
            (1, 1),
            "one read for three ensures"
        );
        let page = rig.store.read_with(cx, |store, _| store.symbol(&symbol("fast-page")));
        assert_eq!(
            page.loaded_value().map(|page| page.identity.name.to_string()),
            Some("fast-page".to_owned())
        );
        // A current page is not fetched again.
        rig.store.update(cx, |store, cx| {
            store.ensure(key.clone(), cx);
        });
        assert!(rig.take_events().is_empty());
    }

    #[gpui::test]
    fn navigating_away_cancels_the_old_route_reads(cx: &mut TestAppContext) {
        let rig = rig(cx, 1);
        let old = PageKey::Symbol(symbol("slow-old"));
        let new = PageKey::Symbol(symbol("fast-new"));
        rig.store.update(cx, |store, cx| store.focus(vec![old.clone()], cx));
        rig.store.update(cx, |store, cx| store.focus(vec![new.clone()], cx));
        let (old_resource, cancelled) = rig.store.read_with(cx, |store, _| {
            (store.symbol(&symbol("slow-old")), store.stats().cancelled)
        });
        assert_eq!(cancelled - rig.base.cancelled, 1);
        assert_eq!(old_resource.activity(), Activity::NotYet, "the old page is back to not-yet");
        rig.until(cx, |store| store.symbol(&symbol("fast-new")).is_loaded());
        // The cancelled read never lands, even when its gate opens later.
        rig.open("slow-old");
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(20));
        cx.run_until_parked();
        assert!(
            rig.store
                .read_with(cx, |store, _| store.symbol(&symbol("slow-old")).loaded_value().is_none())
        );
    }

    #[gpui::test]
    fn a_hover_prefetch_is_cancellable_and_a_click_adopts_it(cx: &mut TestAppContext) {
        let rig = rig(cx, 1);
        let hovered = PageKey::Symbol(symbol("slow-hover"));
        rig.store.update(cx, |store, cx| store.prefetch(hovered.clone(), cx));
        assert!(rig.store.read_with(cx, |store, _| store.is_prefetching(&hovered)));
        rig.store.update(cx, |store, cx| store.cancel_prefetch(&hovered, cx));
        let resource = rig.store.read_with(cx, |store, _| store.symbol(&symbol("slow-hover")));
        assert_eq!(resource.activity(), Activity::NotYet);

        // Hover again, then click: the running prefetch is adopted, not repeated.
        let clicked = PageKey::Symbol(symbol("slow-click"));
        rig.store.update(cx, |store, cx| store.prefetch(clicked.clone(), cx));
        rig.store.update(cx, |store, cx| {
            store.ensure(clicked.clone(), cx);
        });
        assert!(!rig.store.read_with(cx, |store, _| store.is_prefetching(&clicked)));
        rig.open("slow-click");
        rig.until(cx, |store| store.symbol(&symbol("slow-click")).is_loaded());
        let stats = rig.store.read_with(cx, |store, _| store.stats());
        assert_eq!(stats.prefetched - rig.base.prefetched, 2);
        assert_eq!(
            stats.submitted - rig.base.submitted,
            2,
            "the click did not resubmit the prefetch"
        );
    }

    #[gpui::test]
    fn snapshot_admission_emits_only_the_branches_that_changed(cx: &mut TestAppContext) {
        let rig = rig(cx, 1);
        let current = rig.store.read_with(cx, |store, _| store.snapshot());
        let mut settings = current.settings().clone();
        settings.reduced_motion = !settings.reduced_motion;
        let next = Arc::new(current.with_settings(settings));
        rig.store.update(cx, |store, cx| store.admit_snapshot(Arc::clone(&next), cx));
        assert_eq!(rig.take_events(), [StoreEvent::Snapshot(Branch::Settings)]);
        // The same snapshot again is not a change.
        rig.store.update(cx, |store, cx| store.admit_snapshot(next, cx));
        assert!(rig.take_events().is_empty());
    }

    #[gpui::test]
    fn orbit_route_focus_reads_health_through_the_pool(cx: &mut TestAppContext) {
        let rig = rig(cx, 2);
        rig.store.update(cx, |store, cx| {
            store.focus(route_keys(&Route::Orbit(crate::navigation::OrbitRoute::Home)), cx);
        });
        rig.until(cx, |store| store.health().is_loaded());
        let (health, orbit) = rig
            .store
            .read_with(cx, |store, _| (store.health(), store.orbit()));
        assert_eq!(health.loaded_value().map(|model| model.rows), Some(7));
        // The fixture reader cannot answer Orbit: that is an unavailable page,
        // not a spinner.
        rig.until(cx, |store| store.orbit().activity() == Activity::Stopped);
        assert!(matches!(
            orbit.terminal(),
            crate::core::ResourceTerminal::Complete | crate::core::ResourceTerminal::Unavailable(_)
        ));
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod watch_tests {
    //! Region views re-render only for their own slice.

    use super::*;
    use crate::core::VersionedRoot;
    use gpui::{IntoElement, Render, TestAppContext, Window, div};

    /// A region view that renders one page key and the settings branch.
    struct Region {
        watch: Watch,
        notified: usize,
        _events: gpui::Subscription,
    }

    impl Render for Region {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[gpui::test]
    fn a_region_view_is_notified_only_for_its_own_slice(cx: &mut TestAppContext) {
        let snapshot = Arc::new(AppSnapshot::empty(VersionedRoot::synthetic(
            backend_library::view_state_root(&[("watch".to_owned(), "tests".to_owned())]),
            2,
        )));
        // No read lane: every fetch lands "unavailable" at once, which is
        // still a visible change the store announces.
        let store = cx.update(|cx| DataStore::install(cx, Arc::clone(&snapshot), None));
        let mine = PageKey::Health;
        let other = PageKey::Search(SearchQuery::new("other", 10).expect("query"));
        let region = cx.update(|cx| {
            let watched = store.clone();
            let key = mine.clone();
            cx.new(|cx| {
                let watch = Watch::new(watched.read(cx), [key], &[Branch::Settings]);
                let events = cx.subscribe(&watched, |region: &mut Region, store, event, cx| {
                    if region.watch.changed(store.read(cx), event) {
                        region.notified += 1;
                        cx.notify();
                    }
                });
                Region {
                    watch,
                    notified: 0,
                    _events: events,
                }
            })
        });
        let notified = |cx: &mut TestAppContext| region.read_with(cx, |region, _| region.notified);
        let baseline = notified(cx);

        // Another page's traffic and an unwatched branch never reach it.
        store.update(cx, |store, cx| {
            store.ensure(other.clone(), cx);
            store.retry(other.clone(), cx);
        });
        let mut workspace = snapshot.workspace().clone();
        workspace.path_error = Some(Arc::from("unrelated"));
        let unrelated = Arc::new(snapshot.with_workspace(workspace));
        store.update(cx, |store, cx| store.admit_snapshot(Arc::clone(&unrelated), cx));
        cx.run_until_parked();
        assert_eq!(notified(cx), baseline, "no re-render for other slices");

        // Its own key and its own branch do.
        store.update(cx, |store, cx| store.retry(mine.clone(), cx));
        cx.run_until_parked();
        let after_key = notified(cx);
        assert!(after_key > baseline, "its page changed");
        let mut settings = unrelated.settings().clone();
        settings.reduced_motion = !settings.reduced_motion;
        store.update(cx, |store, cx| store.admit_snapshot(Arc::new(unrelated.with_settings(settings)), cx));
        cx.run_until_parked();
        assert_eq!(notified(cx), after_key + 1, "its branch changed once");
    }
}
