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
use super::owner::{OwnerGate, OwnerState};
use super::reads::{Priority, ReadJob, ReadPool, ReadRequest};
use super::snapshot::{Keep, Kept, SnapRoot, SnapshotFile, kept_keys};
use crate::core::{ErrorValue, FaultCode, Resource, UnavailableReason};
use crate::model::AppSnapshot;
use crate::model::pages::{
    HealthModel, Landing, OrbitModel, PackageDossier, PackageRef, PageKey, PageStore, ReadFailure,
    SearchPage, SearchQuery, SourceView, Stamp, SymbolPage, SymbolRef,
};
use crate::navigation::Route;
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Task};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

/// How long the pages rest before the launch snapshot is saved (I2).
const SAVE_IDLE: Duration = Duration::from_millis(1_500);

/// Where the route's pages go, the root they are current at, and the pages.
type ToSave = (SnapshotFile, crate::core::VersionedRoot, Vec<(PageKey, Kept)>);

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
    /// What the hand holds changed.
    Hand,
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

/// Whether the owner behind this window's reads is answering (I1). While it
/// starts, reads are held here, never queued behind a socket that does not
/// exist yet, and never asked at a root nobody served; a failed owner is a
/// fault on every page, in its own words.
#[derive(Clone, Debug, Eq, PartialEq)]
enum OwnerPhase {
    /// Answering (the harness, tests, and every window once its owner is).
    Serving,
    /// Not answered yet: pages ask, and wait here.
    Starting,
    /// Could not start: every page says so.
    Failed(Arc<str>),
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
    /// The one visit-scoped notice (§15 ruling 1), for any route: an
    /// unresolved link, an unindexed hold, or a pinned card's failed
    /// lookup. A newer notice replaces an older one; it never survives a
    /// route change ([`super::graph_focus::Notice::active`]).
    notice: Option<super::graph_focus::Notice>,
    /// A tour asked for (T) and the ask's number: the graph flies it once
    /// it shows the world. Leaving the world drops it.
    tour: Option<(PackageRef, u64)>,
    tours: u64,
    /// The owner's phase, and the gate that restarts a failed one.
    owner: OwnerPhase,
    gate: Option<OwnerGate>,
    /// Pages asked for while the owner starts, fetched once it answers.
    held: BTreeSet<PageKey>,
    /// The root the launch snapshot's pages were read at, until the first
    /// served root settles them (confirmed as they are, or revalidated).
    seed_root: Option<SnapRoot>,
    /// Where the pages are saved for the next launch, and the pending save.
    snapshots: Option<SnapshotFile>,
    saving: Option<Task<()>>,
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

/// A declaration page is about to be read from the index: the world thread
/// starts on its anatomy now, so it is computed by the time the page lands
/// and the page's first frame already has it (`fixture_world::warm`).
fn warm_anatomy(key: &PageKey, cx: &mut App) {
    if let PageKey::Symbol(symbol) = key {
        super::fixture_world::warm_symbol(symbol, cx);
    }
}

/// Returns the page keys one route displays.
#[must_use]
pub fn route_keys(route: &Route) -> Vec<PageKey> {
    match route {
        Route::Orbit(crate::navigation::OrbitRoute::Browse(browse)) => vec![PageKey::Browse(browse.into())],
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
            tour: None,
            tours: 0,
            notice: None,
            owner: OwnerPhase::Serving,
            gate: None,
            held: BTreeSet::new(),
            seed_root: None,
            snapshots: None,
            saving: None,
        }
    }

    /// A display selection never becomes an address or a history entry.
    pub(crate) fn graph_focus(&self) -> Option<&super::graph_focus::GraphFocus> {
        self.graph_focus.as_ref().filter(|focus| focus.active(&self.snapshot))
    }

    /// The current notice, when its visit and root are still the one showing.
    pub(crate) fn notice(&self) -> Option<&super::graph_focus::Notice> {
        self.notice.as_ref().filter(|notice| notice.active(&self.snapshot))
    }

    /// Posts (or clears) the one visit-scoped notice on its own, for a
    /// caller with no graph focus of its own to admit alongside it (an
    /// anatomy link, Ask, an unindexed hold). A newer notice replaces an
    /// older one; setting `None` clears it early.
    pub(crate) fn set_notice(&mut self, notice: Option<super::graph_focus::Notice>, cx: &mut Context<Self>) {
        if self.notice != notice {
            self.notice = notice;
            self.emit(StoreEvent::Snapshot(Branch::GraphFocus), cx);
        }
    }

    /// Semantic equality is the notification boundary: camera and hover frames
    /// cannot invalidate the shell regions or schedule data-plane reads.
    /// Asks the graph to tour `package` (the route is already the world).
    pub(crate) fn ask_tour(&mut self, package: PackageRef, cx: &mut Context<Self>) {
        self.tours += 1;
        self.tour = Some((package, self.tours));
        cx.notify();
    }

    /// The tour asked for, with its number, while the route is the world.
    pub(crate) fn tour_ask(&self) -> Option<&(PackageRef, u64)> {
        self.tour.as_ref().filter(|_| matches!(self.snapshot.route(), Route::World))
    }

    pub(crate) fn admit_graph_focus(&mut self, focus: Option<super::graph_focus::GraphFocus>, notice: Option<super::graph_focus::Notice>, cx: &mut Context<Self>) {
        if self.graph_focus != focus || self.notice != notice {
            self.graph_focus = focus;
            self.notice = notice;
            self.emit(StoreEvent::Snapshot(Branch::GraphFocus), cx);
        }
    }

    /// Creates the entity and starts its wake task.
    pub fn install(cx: &mut App, snapshot: Arc<AppSnapshot>, pool: Option<ReadPool>) -> Entity<Self> {
        Self::install_with_owner(cx, snapshot, pool, None, None)
    }

    /// [`Self::install`] for a window whose owner may not have answered yet
    /// (`None`: it has, as in the harness and tests), painting the launch
    /// snapshot's pages until it does (`keep`, W-Open I2).
    pub fn install_with_owner(
        cx: &mut App,
        snapshot: Arc<AppSnapshot>,
        pool: Option<ReadPool>,
        gate: Option<OwnerGate>,
        keep: Option<Keep>,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let route = route_keys(snapshot.route());
            let mut store = Self::new(snapshot, pool);
            if let Some(gate) = gate {
                store.owner = match gate.state() {
                    OwnerState::Starting => OwnerPhase::Starting,
                    OwnerState::Ready { .. } => OwnerPhase::Serving,
                    OwnerState::Failed(message) => OwnerPhase::Failed(message),
                };
                store.gate = Some(gate);
            }
            if let Some(keep) = keep {
                store.keep(keep);
            }
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

    /// Seeds the launch snapshot's pages at the current (unserved) root and
    /// remembers where to save them.
    fn keep(&mut self, keep: Keep) {
        let root = self.snapshot.key();
        if let Some(seed) = keep.seed {
            let mut seeded = 0_usize;
            for (key, value) in seed.pages {
                let name = format!("{key:?}");
                if self.pages.seed(&key, value, root) {
                    seeded += 1;
                    super::trace::mark("snapshot.seed", name);
                }
            }
            if seeded > 0 {
                self.seed_root = Some(seed.root);
            }
        }
        self.snapshots = Some(keep.file);
    }

    /// The first served root settles the seeded pages: at the root they
    /// were read at they are current as they are; otherwise their next
    /// fetch revalidates them quietly.
    fn settle_seed(&mut self) {
        let root = self.snapshot.key();
        if root.is_unserved() {
            return;
        }
        let Some(seed) = self.seed_root.take() else {
            return;
        };
        if seed.serves(root) {
            let confirmed = self
                .pages
                .keys()
                .into_iter()
                .filter(|key| self.pages.confirm(key, root))
                .count();
            super::trace::mark("snapshot.confirm", format_args!("{confirmed} pages at the served root"));
        } else {
            super::trace::mark("snapshot.revalidate", "the owner serves a newer root");
        }
    }

    /// The route's pages as they are now, when all are current at a served
    /// root: what the next launch paints first.
    fn to_save(&self) -> Option<ToSave> {
        fn at<T>(resource: &Resource<T>, root: crate::core::VersionedRoot) -> Option<Arc<T>> {
            (resource.is_loaded() && resource.value_root().is_some_and(|at| at.same_authority(root)))
                .then(|| resource.loaded_arc().cloned())
                .flatten()
        }
        let file = self.snapshots.clone()?;
        let root = self.snapshot.key();
        if root.is_unserved() {
            return None;
        }
        let current = |key: &PageKey| -> Option<Kept> {
            if self.pages.is_seeded(key) {
                return None;
            }
            match key {
                PageKey::Symbol(symbol) => at(&self.pages.symbol(symbol), root).map(Kept::Symbol),
                PageKey::Source(symbol) => at(&self.pages.source(symbol), root).map(Kept::Source),
                PageKey::Package(package) => at(&self.pages.package(package), root).map(Kept::Package),
                PageKey::Orbit => at(&self.pages.orbit(), root).map(Kept::Orbit),
                _ => None,
            }
        };
        let pages = kept_keys(self.snapshot.route())
            .into_iter()
            .filter_map(|key| current(&key).map(|kept| (key, kept)))
            .collect::<Vec<_>>();
        (!pages.is_empty()).then_some((file, root, pages))
    }

    /// Saves the launch snapshot now, on this thread (quit).
    ///
    /// # Errors
    /// The snapshot file's I/O error; nothing to save is `Ok(0)`.
    pub fn save_now(&self) -> std::io::Result<usize> {
        let Some((file, root, pages)) = self.to_save() else {
            return Ok(0);
        };
        let saving = std::time::Instant::now();
        let written = file.write(root, &pages)?;
        super::trace::span("snapshot.write", saving, format_args!("{} pages, {written} bytes, on quit", pages.len()));
        Ok(written)
    }

    /// Saves the launch snapshot once the pages have rested (a newer landing
    /// restarts the wait), encoding and writing off the UI thread.
    fn save_at_rest(&mut self, cx: &mut Context<Self>) {
        if self.snapshots.is_none() {
            return;
        }
        self.saving = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_IDLE).await;
            let Ok(Some((file, root, pages))) = this.update(cx, |store, _| store.to_save()) else {
                return;
            };
            cx.background_spawn(async move {
                let saving = std::time::Instant::now();
                match file.write(root, &pages) {
                    Ok(written) => super::trace::span(
                        "snapshot.write",
                        saving,
                        format_args!("{} pages, {written} bytes, at rest", pages.len()),
                    ),
                    Err(error) => eprintln!("backend-desktop: save {}: {error}", file.path().display()),
                }
            })
            .await;
        }));
    }

    /// Emits `key`'s change only when its visible state moved.
    fn emit_moved(&mut self, key: PageKey, before: Stamp, cx: &mut Context<Self>) {
        if self.pages.stamp(&key) != before {
            self.emit(StoreEvent::Resource(key), cx);
        }
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
            if !matches!(snapshot.route(), Route::World) {
                self.tour = None;
            }
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
        if old.session().hand != snapshot.session().hand {
            changed.push(Branch::Hand);
        }
        for branch in &changed {
            self.emit(StoreEvent::Snapshot(*branch), cx);
        }
        if changed.contains(&Branch::Root) {
            self.settle_seed();
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
        match &self.owner {
            OwnerPhase::Serving => {}
            OwnerPhase::Starting => {
                self.held.insert(key.clone());
                return self.pages.stamp(&key);
            }
            OwnerPhase::Failed(message) => {
                let message = Arc::clone(message);
                self.fail(&key, &message, cx);
                return self.pages.stamp(&key);
            }
        }
        let root = self.snapshot.key();
        let before = self.pages.stamp(&key);
        if let Some(generation) = self.pages.begin(&key, root) {
            self.prefetching.remove(&key);
            warm_anatomy(&key, cx);
            self.submit(key.clone(), ReadRequest::for_key(&key), generation, Priority::Normal, None, cx);
            let stamp = self.pages.stamp(&key);
            self.emit_moved(key, before, cx);
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
        self.held.retain(|key| next.contains(key));
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
        if self.owner != OwnerPhase::Serving {
            return;
        }
        self.keep_focused_resident();
        let root = self.snapshot.key();
        let before = self.pages.stamp(&key);
        if let Some(generation) = self.pages.begin(&key, root) {
            self.prefetching.insert(key.clone());
            self.stats.prefetched = self.stats.prefetched.saturating_add(1);
            warm_anatomy(&key, cx);
            self.submit(key.clone(), ReadRequest::for_key(&key), generation, Priority::Prefetch, None, cx);
            self.emit_moved(key, before, cx);
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
        let before = self.pages.stamp(key);
        if self.pages.cancel(key).is_some() {
            self.stats.cancelled = self.stats.cancelled.saturating_add(1);
            self.emit_moved(key.clone(), before, cx);
        }
    }

    /// Fetches a page again even when it is current (retry after a fault).
    pub fn retry(&mut self, key: PageKey, cx: &mut Context<Self>) {
        match &self.owner {
            OwnerPhase::Serving => {}
            // "Try again" on a page the owner could not serve asks the owner
            // to start again; the page is fetched once it answers.
            OwnerPhase::Failed(_) => {
                if let Some(gate) = &self.gate {
                    // A starting owner is left alone; either way the page waits.
                    let _ = gate.restart();
                }
                self.owner = OwnerPhase::Starting;
                self.held.insert(key);
                return;
            }
            OwnerPhase::Starting => {
                self.held.insert(key);
                return;
            }
        }
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
        if self.owner != OwnerPhase::Serving {
            return false;
        }
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
        super::trace::mark("read.submit", format_args!("{key:?} {priority:?}"));
        pool.submit(ReadJob {
            key,
            request,
            generation,
            priority,
            cancel: CancellationToken::new(),
            affinity,
        });
    }

    /// The owner answered, and its root is already admitted
    /// (`UiRootEntity::admit_owner`): every held page, and every page the
    /// route shows, is fetched now, at that root.
    pub fn owner_ready(&mut self, cx: &mut Context<Self>) {
        self.owner = OwnerPhase::Serving;
        let mut keys = std::mem::take(&mut self.held);
        keys.extend(self.focused.iter().cloned());
        for key in keys {
            self.ensure(key, cx);
        }
    }

    /// The owner could not start: every held page, and every page the route
    /// shows, lands as a fault carrying the owner's words.
    pub fn owner_failed(&mut self, message: &Arc<str>, cx: &mut Context<Self>) {
        self.owner = OwnerPhase::Failed(Arc::clone(message));
        let mut keys = std::mem::take(&mut self.held);
        keys.extend(self.focused.iter().cloned());
        for key in keys {
            self.fail(&key, message, cx);
        }
        // A page painted from the launch snapshot stays (it is the page as
        // it was left), and says it could not be brought up to date.
        if self.focused.iter().any(|key| self.pages.is_seeded(key)) {
            let notice = super::graph_focus::Notice {
                visit: self.snapshot.route().clone(),
                root: self.snapshot.key(),
                message: Arc::from(format!(
                    "The index could not start, so this is the page as you left it. {message}"
                )),
            };
            self.set_notice(Some(notice), cx);
        }
    }

    /// The owner is starting (again): pages asked from now on are held.
    pub fn owner_starting(&mut self, cx: &mut Context<Self>) {
        if self.owner != OwnerPhase::Serving {
            self.owner = OwnerPhase::Starting;
            cx.notify();
        }
    }

    /// Lands the owner's failure in `key`'s slot, once per root.
    fn fail(&mut self, key: &PageKey, message: &Arc<str>, cx: &mut Context<Self>) {
        let root = self.snapshot.key();
        let Some(generation) = self.pages.begin(key, root) else {
            return;
        };
        let failure = ReadFailure::Fault(ErrorValue::new(
            FaultCode::Transport,
            format!("The index could not start. {message}"),
        ));
        if self.pages.land(key, generation, Err(failure)) == Landing::Applied {
            self.emit(StoreEvent::Resource(key.clone()), cx);
        }
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
        let mut save = false;
        for outcome in outcomes {
            if self.pages.inflight(&outcome.key) == Some(outcome.generation) {
                self.prefetching.remove(&outcome.key);
            }
            match self.pages.land(&outcome.key, outcome.generation, outcome.result) {
                Landing::Applied => {
                    applied += 1;
                    super::trace::mark("read.land", format_args!("{:?}", outcome.key));
                    self.stats.landed = self.stats.landed.saturating_add(1);
                    save |= kept_keys(self.snapshot.route()).contains(&outcome.key);
                    self.emit(StoreEvent::Resource(outcome.key), cx);
                }
                Landing::Unchanged => {
                    // The launch snapshot's value, confirmed at the new root:
                    // nothing to draw.
                    super::trace::mark("read.same", format_args!("{:?}", outcome.key));
                    self.stats.landed = self.stats.landed.saturating_add(1);
                    save |= kept_keys(self.snapshot.route()).contains(&outcome.key);
                }
                Landing::Superseded => {
                    self.stats.superseded = self.stats.superseded.saturating_add(1);
                }
            }
        }
        if save {
            self.save_at_rest(cx);
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
            sections: crate::model::pages::DocSections::default(),
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
