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

#[cfg(test)]
pub(crate) mod cargo_context_tests;
#[cfg(test)]
mod cargo_tests;
mod dependencies;
mod keeper;
mod owner_link;
mod publication;
#[cfg(test)]
mod owner_read_tests;

pub(crate) use self::dependencies::{
    ContentAdmission, ContentFailure, RouteDependencies, RouteReadLease,
};
use self::keeper::SnapshotKeeper;
pub(crate) use self::owner_link::{OwnerAttachment, OwnerRetryAttachment};
use self::owner_link::{OwnerLink, OwnerPhase};
use super::actor::CancellationToken;
use super::owner::{OwnerFault, OwnerGate};
pub use super::reads::PoolLoad;
use super::reads::{Delivery, Evicted, Priority, ReadJob, ReadPool, ReadRequest};
use super::snapshot::{Keep, kept_keys};
use crate::core::{
    ErrorValue, FaultCode, Resource, ResourceAdmission, ResourceTerminal, UnavailableReason,
    admit_resource,
};
use crate::model::AppSnapshot;
use crate::model::pages::{
    Generation, HealthModel, Landing, OrbitModel, PackageDossier, PackageRef, PageKey, PageStore,
    ReadFailure, SearchPage, SearchQuery, SourceView, Stamp, SymbolPage, SymbolRef,
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
    /// Session-local per-visit reading intent; never refreshes data keys.
    Reading,
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
    pub fn new(
        store: &DataStore,
        keys: impl IntoIterator<Item = PageKey>,
        branches: &[Branch],
    ) -> Self {
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
    /// The one visit-scoped notice (§15 ruling 1), for any route: an
    /// unresolved link, an unindexed hold, or a pinned card's failed
    /// lookup. A newer notice replaces an older one; it never survives a
    /// route change ([`super::graph_focus::Notice::active`]).
    notice: Option<super::graph_focus::Notice>,
    /// A tour asked for (T) and the ask's number: the graph flies it once
    /// it shows the world. Leaving the world drops it.
    tour: Option<(PackageRef, u64)>,
    tours: u64,
    /// The owner behind the reads: its phase, and the pages held for it.
    owner: OwnerLink,
    /// The launch snapshot: seeded pages, and saving them for next time.
    keeper: SnapshotKeeper,
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
        Route::CargoSource(route) => return PackageRef::parse(route.package.as_str()).ok(),
        Route::Orbit(_) | Route::World => return None,
    };
    let pinned = PackageRef::parse(package.as_str()).ok()?;
    match at {
        Some(at) if at.is_valid() => pinned
            .at(at.as_str())
            .or_else(|| release_tree(&pinned, at.as_str())),
        Some(_) => None,
        None => Some(pinned),
    }
}

/// The tree of the release `at` of the registry package whose tree `pinned`
/// is: a registry root is read at another release by reading that release's
/// own tree (`…/toml-0.5.11` beside `…/toml-0.8.23`), which the library holds
/// once it is added. This is lexical only: the worker verifies existence and
/// index coverage. Route resolution must never touch disk on the UI lane.
fn release_tree(pinned: &PackageRef, at: &str) -> Option<PackageRef> {
    let (name, version) = pinned.registry_shape()?;
    // Only Cargo registry trees use this path layout. Other ecosystems keep
    // their own exact version spelling and are read through their purl.
    crate::model::release::Release::new(name, at).ok()?;
    if version == at {
        return Some(pinned.clone());
    }
    let current = std::path::Path::new(pinned.as_str());
    let parent = current.parent()?;
    let target = format!("{name}-{at}");
    let tree = if parent.file_name()? == current.file_name()? {
        parent.parent()?.join(&target).join(&target)
    } else {
        parent.join(&target)
    };
    PackageRef::parse(tree.to_str()?)
        .ok()
        .map(|target| target.with_release_origin(pinned))
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
    /// The declaration coordinate belongs to a different package than the route.
    CoordinateOutsidePackage,
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
    let pinned = PackageRef::parse(route.package.as_str()).map_err(|_| Unread::NotADeclaration)?;
    if !symbol
        .as_str()
        .strip_prefix(pinned.as_str())
        .is_some_and(|tail| tail.starts_with("::"))
    {
        return Err(Unread::CoordinateOutsidePackage);
    }
    let Some(at) = &route.at else {
        return Ok(symbol);
    };
    let viewed = route_package(&Route::Symbol(route.clone()))
        .ok_or_else(|| Unread::ReleaseNotHere(at.clone()))?;
    symbol
        .rebased(&pinned, &viewed)
        .ok_or(Unread::CoordinateOutsidePackage)
}

/// Returns the page keys one route displays.
#[must_use]
pub fn route_keys(route: &Route) -> Vec<PageKey> {
    RouteDependencies::new(route, None).into_keys()
}

/// Live admission of owner-checked Cargo bytes or path observations.
/// Retained values never become current merely because a slot still holds them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CargoReadAdmission {
    Current,
    Checking,
    Fault(ErrorValue),
    Unavailable(UnavailableReason),
}

fn is_cargo_source_resource(key: &PageKey) -> bool {
    matches!(
        key,
        PageKey::CargoSource(_)
            | PageKey::Browse(crate::model::browse::BrowseKey::CargoSourceInventory(_))
            | PageKey::Browse(crate::model::browse::BrowseKey::CargoReadme(_))
    )
}

impl DataStore {
    /// Installs a completed Browse reply for shell event-boundary fixtures.
    #[cfg(test)]
    #[allow(clippy::expect_used, clippy::panic)]
    pub(crate) fn test_land_browse(
        &mut self,
        key: crate::model::browse::BrowseKey,
        value: crate::model::browse::BrowseValue,
        cx: &mut Context<Self>,
    ) {
        let key = PageKey::Browse(key);
        let before = self.pages.stamp(&key);
        let generation = self
            .pages
            .begin_forced(&key, self.snapshot.key())
            .expect("page generation admission")
            .expect("forced fixture reading");
        assert_eq!(
            self.pages.land(
                &key,
                generation,
                Ok(crate::model::pages::PageValue::Browse(value))
            ),
            Landing::Applied
        );
        self.emit_moved(key, before, cx);
    }

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
            owner: OwnerLink::serving(),
            keeper: SnapshotKeeper::default(),
        }
    }

    /// A display selection never becomes an address or a history entry.
    pub(crate) fn graph_focus(&self) -> Option<&super::graph_focus::GraphFocus> {
        self.graph_focus
            .as_ref()
            .filter(|focus| focus.active(&self.snapshot))
    }

    /// One visit-scoped answer shared by graph view controls and commands.
    pub(crate) fn graph_view_eligibility(&self) -> super::graph_focus::GraphViewEligibility {
        super::graph_focus::GraphViewEligibility::from_current(&self.snapshot, self.graph_focus())
    }

    /// The current notice, when its visit and root are still the one showing.
    pub(crate) fn notice(&self) -> Option<&super::graph_focus::Notice> {
        self.notice
            .as_ref()
            .filter(|notice| notice.active(&self.snapshot))
    }

    /// Earlier exact-destination display, separate from current resources.
    /// A late/other-route projection cannot attach to this mounted visit.
    pub(crate) fn retained_display(&self, route: &Route) -> Option<Arc<crate::runtime::snapshot::RetainedDisplay>> {
        (self.snapshot.route() == route).then(|| self.keeper.retained(route)).flatten()
    }

    /// Posts (or clears) the one visit-scoped notice on its own, for a
    /// caller with no graph focus of its own to admit alongside it (an
    /// anatomy link, Ask, an unindexed hold). A newer notice replaces an
    /// older one; setting `None` clears it early.
    pub(crate) fn set_notice(
        &mut self,
        notice: Option<super::graph_focus::Notice>,
        cx: &mut Context<Self>,
    ) {
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
        self.tour
            .as_ref()
            .filter(|_| matches!(self.snapshot.route(), Route::World))
    }

    pub(crate) fn admit_graph_focus(
        &mut self,
        focus: Option<super::graph_focus::GraphFocus>,
        notice: Option<super::graph_focus::Notice>,
        cx: &mut Context<Self>,
    ) {
        // The map publishes an empty focus when an overlay covers it. That
        // publication must not erase the failed owner's current recovery
        // notice, which belongs to the same route even while covered.
        let owner_notice = self.notice.as_ref().filter(|current| {
            current.retry.is_some()
                && current.visit == *self.snapshot.route()
                && current.root.same_authority(self.snapshot.key())
                && matches!(self.owner.phase(), OwnerPhase::Failed(_))
        }).cloned();
        let notice = owner_notice.or(notice);
        if self.graph_focus != focus || self.notice != notice {
            self.graph_focus = focus;
            self.notice = notice;
            self.emit(StoreEvent::Snapshot(Branch::GraphFocus), cx);
        }
    }

    /// Creates the entity and starts its wake task.
    pub fn install(
        cx: &mut App,
        snapshot: Arc<AppSnapshot>,
        pool: Option<ReadPool>,
    ) -> Entity<Self> {
        Self::install_with_owner(cx, snapshot, pool, None, None)
    }

    /// [`Self::install`] for a window whose owner may not have answered yet
    /// (`None`: it has, as in the harness and tests), painting the launch
    /// snapshot's pages until it does (`keep`, W-Open I2).
    pub(crate) fn install_with_owner(
        cx: &mut App,
        snapshot: Arc<AppSnapshot>,
        pool: Option<ReadPool>,
        gate: Option<OwnerGate>,
        keep: Option<Keep>,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let route = RouteDependencies::new(snapshot.route(), snapshot.overlay()).into_keys();
            let mut store = Self::new(snapshot, pool);
            if let Some(gate) = gate {
                store.owner = OwnerLink::behind(gate);
            }
            if let Some(keep) = keep {
                let root = store.snapshot.key();
                store.keeper.keep(&mut store.pages, root, keep);
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
                // A pending wake can resolve immediately. Yield the actual
                // foreground task so another UI task runs between batches.
                super::wake::yield_turn().await;
            }
        }));
    }

    /// Saves the launch snapshot now, on this thread (quit).
    ///
    /// # Errors
    /// The snapshot file's I/O error; nothing to save is `Ok(0)`.
    pub(crate) fn save_now(&self) -> std::io::Result<usize> {
        self.keeper
            .save_now(&self.pages, &self.snapshot, self.owner_serving())
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

    /// Whether the owner observed by this store still serves its admitted
    /// attachment. The gate can revoke it before the UI watcher runs.
    #[must_use]
    pub fn owner_serving(&self) -> bool {
        self.owner.is_current_serving()
    }

    /// Visible auxiliary readers also revalidate a superseded observation.
    /// A cached predecessor does not retain current action or save admission.
    pub(crate) fn observation_revoked(&self, key: &PageKey) -> bool {
        self.pages.is_owner_read_revoked(key)
    }

    /// What the read pool is doing now.
    #[must_use]
    pub fn pool_activity(&self) -> PoolLoad {
        self.pool
            .as_ref()
            .map_or_else(PoolLoad::default, |pool| pool.load().activity())
    }

    /// Legacy `(waiting, running)` view. Waiting includes both queued jobs
    /// and published outcomes not yet landed, so `(0, 0)` has the same idle
    /// meaning as [`PoolLoad::is_idle`]. Prefer [`Self::pool_activity`] when
    /// the individual lifecycle stages matter.
    #[must_use]
    pub fn pool_load(&self) -> (usize, usize) {
        let PoolLoad {
            queued,
            running,
            undelivered,
        } = self.pool_activity();
        (queued + undelivered, running)
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
        if old.session().reading != snapshot.session().reading {
            changed.push(Branch::Reading);
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
            // Old-root work must lose its generation before ensure asks for this root.
            let stale = self
                .pages
                .keys()
                .into_iter()
                .filter(|key| self.pages.inflight(key).is_some())
                .collect::<Vec<_>>();
            for key in stale {
                self.cancel_key(&key, cx);
            }
            let root = self.snapshot.key();
            self.keeper.settle(&mut self.pages, root);
        }
        if changed.contains(&Branch::Route) || changed.contains(&Branch::Overlay) {
            self.focus(
                RouteDependencies::new(snapshot.route(), snapshot.overlay()).into_keys(),
                cx,
            );
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
        match self.owner.phase() {
            OwnerPhase::Serving => {}
            OwnerPhase::Starting => {
                self.owner.hold(key.clone());
                return self.pages.stamp(&key);
            }
            OwnerPhase::Failed(fault) => {
                let fault = fault.clone();
                self.fail(&key, &fault, cx);
                return self.pages.stamp(&key);
            }
        }
        let before = self.pages.stamp(&key);
        if let Some(generation) = self.begin_read(&key, false, Priority::Normal, cx) {
            self.prefetching.remove(&key);
            self.submit(
                key.clone(),
                ReadRequest::for_key(&key),
                generation,
                Priority::Normal,
                None,
                cx,
            );
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
        let previous = self.focused.clone();
        let previous_file = previous
            .iter()
            .find(|key| matches!(key, PageKey::CargoSource(_)));
        let next_file = next
            .iter()
            .find(|key| matches!(key, PageKey::CargoSource(_)));
        let file_changed = next_file.is_some() && previous_file != next_file;
        let dropped = self.focused.difference(&next).cloned().collect::<Vec<_>>();
        self.owner.retain_held(|key| next.contains(key));
        self.focused = next;
        for key in dropped {
            if self.prefetching.contains(&key) {
                continue;
            }
            self.cancel_key(&key, cx);
        }
        for key in keys {
            if is_cargo_source_resource(&key)
                && (!previous.contains(&key)
                    || (file_changed
                        && matches!(
                            key,
                            PageKey::Browse(crate::model::browse::BrowseKey::CargoSourceInventory(
                                _
                            ))
                        )))
                && self.pages.contains(&key)
            {
                // A file page is a current source capability, not a durable
                // snapshot. Back/Forward after leaving must revalidate even
                // when the global index root has not changed.
                match &key {
                    PageKey::CargoSource(file) => self.pages.revoke_cargo_source(file),
                    PageKey::Browse(crate::model::browse::BrowseKey::CargoSourceInventory(
                        inventory,
                    )) => {
                        self.pages.revoke_cargo_source_inventory(inventory);
                    }
                    PageKey::Browse(crate::model::browse::BrowseKey::CargoReadme(_)) => {
                        self.pages.revoke_owner_read(&key);
                    }
                    _ => {}
                }
                self.retry(key, cx);
            } else {
                self.ensure(key, cx);
            }
        }
    }

    /// Hover-intent prefetch: the view calls this after the pointer has
    /// rested on a link for 120 ms. Low priority, cancellable, and a no-op
    /// when the page is current or already requested.
    pub fn prefetch(&mut self, key: PageKey, cx: &mut Context<Self>) {
        if !self.owner.is_serving()
            || (self.prefetching.len() >= 16 && !self.prefetching.contains(&key))
        {
            return;
        }
        self.keep_focused_resident();
        let before = self.pages.stamp(&key);
        if let Some(generation) = self.begin_read(&key, false, Priority::Prefetch, cx) {
            self.prefetching.insert(key.clone());
            self.stats.prefetched = self.stats.prefetched.saturating_add(1);
            self.submit(
                key.clone(),
                ReadRequest::for_key(&key),
                generation,
                Priority::Prefetch,
                None,
                cx,
            );
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

    /// Stop a query Ask no longer displays, unless the current page still
    /// owns that same read (for example, Ask opened over Find).
    pub fn cancel_unfocused_search(&mut self, query: &SearchQuery, cx: &mut Context<Self>) {
        let key = PageKey::Search(query.clone());
        if !self.focused.contains(&key) {
            self.cancel_key(&key, cx);
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
        match self.owner.phase() {
            OwnerPhase::Serving => {}
            // "Try again" on a page the owner could not serve asks the owner
            // to start again; the page is fetched once it answers.
            OwnerPhase::Failed(_) => {
                self.owner.retry(key);
                return;
            }
            OwnerPhase::Starting => {
                self.owner.hold(key);
                return;
            }
        }
        if let Some(generation) = self.begin_read(&key, true, Priority::Normal, cx) {
            self.prefetching.remove(&key);
            self.submit(
                key.clone(),
                ReadRequest::for_key(&key),
                generation,
                Priority::Normal,
                None,
                cx,
            );
            self.emit(StoreEvent::Resource(key), cx);
        }
    }

    /// Requests the next page of a loaded search. Returns whether a request
    /// was issued (false when there is no continuation).
    pub fn load_more(&mut self, query: &SearchQuery, cx: &mut Context<Self>) -> bool {
        if !self.owner.is_serving() {
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
        let Some(generation) = self.begin_read(&key, true, Priority::Normal, cx) else {
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

    /// One admission boundary for visible pages, hints, retries, continuations
    /// and owner faults. A failed mint never changes model fetch ownership.
    fn begin_read(
        &mut self,
        key: &PageKey,
        force: bool,
        priority: Priority,
        cx: &mut Context<Self>,
    ) -> Option<Generation> {
        let root = self.snapshot.key();
        let previous = self.pages.inflight(key);
        let admitted = if force {
            self.pages.begin_forced(key, root)
        } else {
            self.pages.begin(key, root)
        };
        match admitted {
            Ok(generation) => generation,
            Err(_) if priority == Priority::Prefetch => {
                // A hover hint did not start: preserve its previous bytes and
                // idle state, with no orphaned prefetch bookkeeping.
                self.prefetching.remove(key);
                None
            }
            Err(_) => {
                // Capture and fence the previous UI generation before any
                // cancellation callback can run. cancel captures old ReadIds
                // before callbacks, so a reentrant newer pool read survives.
                if self.pages.inflight(key) != previous {
                    return None;
                }
                if previous.is_some() {
                    if let Some(pool) = &self.pool {
                        let _ = pool.cancel(key);
                    }
                }
                if self.pages.inflight(key) != previous {
                    return None;
                }
                self.prefetching.remove(key);
                let before = self.pages.stamp(key);
                if self.pages.refuse_generation_exhaustion(key, root, previous) {
                    if previous.is_some() {
                        self.stats.cancelled = self.stats.cancelled.saturating_add(1);
                    }
                    self.emit_moved(key.clone(), before, cx);
                }
                None
            }
        }
    }

    fn submit(
        &mut self,
        key: PageKey,
        request: ReadRequest,
        generation: Generation,
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
        super::trace::mark("read.submit", format_args!("{key:?} {priority:?}"));
        let admitted = pool.submit(ReadJob {
            key: key.clone(),
            request,
            generation,
            priority,
            cancel: CancellationToken::new(),
            affinity,
        });
        match admitted {
            Ok(admitted) => {
                self.stats.submitted = self.stats.submitted.saturating_add(1);
                if let Some(evicted) = admitted.evicted {
                    self.cancel_evicted(evicted, cx);
                }
            }
            // A refused prefetch leaves its slot as it was: hover is a hint.
            Err(_) if priority == Priority::Prefetch => {
                if self.pages.inflight(&key) != Some(generation) {
                    return;
                }
                self.prefetching.remove(&key);
                let before = self.pages.stamp(&key);
                let _ = self.pages.cancel(&key);
                self.emit_moved(key, before, cx);
            }
            // A view waits on this read: say why it will not come.
            Err(refused) => {
                if self.pages.land(&key, generation, Err(refused.failure())) == Landing::Applied {
                    self.emit(StoreEvent::Resource(key), cx);
                }
            }
        }
    }

    /// A queued prefetch gave its admission to a read a view waits on. It
    /// never runs and posts nothing, so its slot is cancelled here, at once.
    fn cancel_evicted(&mut self, evicted: Evicted, cx: &mut Context<Self>) {
        if self.pages.inflight(&evicted.key) != Some(evicted.generation) {
            return;
        }
        self.prefetching.remove(&evicted.key);
        let before = self.pages.stamp(&evicted.key);
        if self.pages.cancel(&evicted.key).is_some() {
            self.stats.cancelled = self.stats.cancelled.saturating_add(1);
            self.emit_moved(evicted.key, before, cx);
        }
    }

    /// The owner answered, and its root is already admitted
    /// (`UiRootEntity::admit_owner`): every held page, and every page the
    /// route shows, is fetched now, at that root.
    pub(crate) fn owner_ready(&mut self, cx: &mut Context<Self>) {
        let answer = self.owner.prepare_answer();
        if answer.attachment_changed {
            // The watcher may see only the new Ready. Completed bytes from
            // the previous same-root attachment still need a fresh read.
            self.revoke_owner_reads(cx);
        }
        let mut keys = self.owner.answered(answer);
        keys.extend(self.focused.iter().cloned());
        for key in keys {
            self.ensure(key, cx);
        }
    }

    /// The owner could not start: every held page, and every page the route
    /// shows, lands as a fault carrying the owner's words.
    pub(crate) fn owner_failed(&mut self, fault: &OwnerFault, cx: &mut Context<Self>) {
        // A serving attachment can fail while reads are running. Revoke all
        // of their generations before faulting visible pages: a late reply
        // from the lost owner must not land after Retry, even at the same
        // producer root. Quiet snapshot reads keep their last painted value.
        let repeated =
            matches!(self.owner.phase(), OwnerPhase::Failed(previous) if previous == fault);
        self.revoke_owner_reads_for_failure(repeated.then_some(fault), cx);
        let mut keys = self.owner.failed(fault.clone());
        keys.extend(self.focused.iter().cloned());
        for key in keys {
            self.fail(&key, fault, cx);
        }
        // The foot says it wherever the person is, with "Try again" beside
        // it (R7). A page painted from the launch snapshot stays (it is the
        // page as it was left), and says it could not be brought up to date.
        let seeded = self.focused.iter().any(|key| self.pages.is_seeded(key));
        let message = if seeded {
            format!("The index could not start, so this is the page as you left it. {fault}")
        } else {
            format!("The index could not start. {fault}")
        };
        let notice = super::graph_focus::Notice {
            visit: self.snapshot.route().clone(),
            root: self.snapshot.key(),
            message: Arc::from(message),
            // "Try again" asks the page for itself again, which starts the
            // owner; with no page on the route, the Library's.
            retry: Some(
                self.focused
                    .iter()
                    .next()
                    .cloned()
                    .unwrap_or(PageKey::Orbit),
            ),
        };
        let unchanged_notice = self.notice.as_ref() == Some(&notice);
        self.set_notice(Some(notice), cx);
        // The gate still publishes every failure and invalidates the old
        // Retry attachment. Repaint capabilities even when the resource and
        // notice are identical; their stamps/events need not churn.
        if unchanged_notice {
            self.emit(StoreEvent::Snapshot(Branch::GraphFocus), cx);
        }
    }

    fn revoke_inflight(&mut self, cx: &mut Context<Self>) {
        let running = self
            .pages
            .keys()
            .into_iter()
            .filter(|key| self.pages.inflight(key).is_some())
            .collect::<Vec<_>>();
        for key in running {
            self.cancel_key(&key, cx);
        }
        self.prefetching.clear();
    }

    /// A replacement owner must renew each prior live read. Immutable values
    /// stay resident as nonactionable predecessors; only visible dependencies
    /// are requested again when the new owner answers.
    fn revoke_owner_reads(&mut self, cx: &mut Context<Self>) {
        self.revoke_owner_reads_for_failure(None, cx);
    }

    fn revoke_owner_reads_for_failure(
        &mut self,
        repeated: Option<&OwnerFault>,
        cx: &mut Context<Self>,
    ) {
        let settled_faults = repeated.map(|fault| {
            (
                owner_failure_value(fault),
                crate::model::pages::store::GenerationExhausted.fault(),
            )
        });
        for key in self.pages.keys() {
            if settled_faults.as_ref().is_some_and(|(owner, exhausted)| {
                self.pages.idle_fault_at(&key, self.snapshot.key(), owner)
                    || self
                        .pages
                        .idle_fault_at(&key, self.snapshot.key(), exhausted)
            }) {
                continue;
            }
            self.cancel_key(&key, cx);
            let before = self.pages.stamp(&key);
            if self.pages.revoke_owner_read(&key) {
                self.emit_moved(key, before, cx);
            }
        }
        self.prefetching.clear();
    }

    /// The owner is starting (again): pages asked from now on are held.
    pub(crate) fn owner_starting(&mut self, cx: &mut Context<Self>) {
        let changed = self.owner.starting();
        self.revoke_owner_reads(cx);
        if changed {
            cx.notify();
        }
    }

    /// Lands the owner's failure in `key`'s slot, once per root.
    fn fail(&mut self, key: &PageKey, fault: &OwnerFault, cx: &mut Context<Self>) {
        let Some(generation) = self.begin_read(key, false, Priority::Normal, cx) else {
            return;
        };
        let failure = ReadFailure::Fault(owner_failure_value(fault));
        if self.pages.land(key, generation, Err(failure)) == Landing::Applied {
            self.emit(StoreEvent::Resource(key.clone()), cx);
        }
    }

    /// Lands one bounded batch. Called by the wake task; public so tests
    /// and harnesses can drive it deterministically.
    pub fn drain(&mut self, cx: &mut Context<Self>) -> usize {
        if self.owner.attachment_changed() {
            self.revoke_inflight(cx);
        }
        self.stats.turns = self.stats.turns.saturating_add(1);
        let Some(pool) = &self.pool else {
            return 0;
        };
        let outcomes = pool.drain_for(&self.focused);
        let mut applied = 0;
        let mut save = false;
        for outcome in outcomes {
            // Ownership travels through admission into the slot, and drops
            // only after this closure lands or discards the value.
            outcome.land(|key, generation, delivery| match delivery {
                Delivery::Partial(value) => match self.pages.stage(&key, generation, value) {
                    Landing::Applied => {
                        applied += 1;
                        self.stats.landed = self.stats.landed.saturating_add(1);
                        self.emit(StoreEvent::Resource(key), cx);
                    }
                    Landing::Superseded => {
                        self.stats.superseded = self.stats.superseded.saturating_add(1)
                    }
                    Landing::Unchanged => {}
                },
                Delivery::Terminal(result) => {
                    if self.pages.inflight(&key) == Some(generation) {
                        self.prefetching.remove(&key);
                    }
                    match self.pages.land(&key, generation, result) {
                        Landing::Applied => {
                            applied += 1;
                            self.stats.landed = self.stats.landed.saturating_add(1);
                            save |= kept_keys(self.snapshot.route()).contains(&key);
                            self.emit(StoreEvent::Resource(key), cx);
                        }
                        Landing::Unchanged => {
                            super::trace::mark("read.same", format_args!("{key:?}"));
                            self.stats.landed = self.stats.landed.saturating_add(1);
                            save |= kept_keys(self.snapshot.route()).contains(&key);
                        }
                        Landing::Superseded => {
                            self.stats.superseded = self.stats.superseded.saturating_add(1)
                        }
                    }
                }
            });
        }
        if save {
            self.keeper.save_at_rest(cx);
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

    /// The serving owner this UI visit can capture for delayed actions.
    /// An absent lease means Starting, Failed, or a replacement the store
    /// has not yet admitted. The stable ungated test token is synthetic and
    /// does not establish acceptance by a real owner.
    pub(crate) fn current_owner_attachment(&self) -> Option<OwnerAttachment> {
        self.owner.current_attachment()
    }

    pub(crate) fn current_index_owner(&self) -> Option<super::actor::IndexMutationLease> {
        self.owner.current_mutation()
    }

    /// A captured visit cannot act through a later same-root attachment.
    pub(crate) fn admits_owner_attachment(&self, expected: &OwnerAttachment) -> bool {
        self.current_owner_attachment().as_ref() == Some(expected)
    }

    /// Whether the current failure has a live producer able to consume Retry.
    pub(crate) fn can_retry_current(&self) -> bool { self.owner.can_retry_current() }

    /// Capture this rendered failure; a delayed action must retain this token.
    pub(crate) fn current_owner_retry(&self) -> Option<OwnerRetryAttachment> {
        self.owner.current_retry_attachment()
    }

    /// Retry only the captured failure, never a subsequent attachment/failure.
    pub(crate) fn retry_owner_at(&mut self, expected: &OwnerRetryAttachment, key: PageKey, cx: &mut Context<Self>) -> bool {
        if !self.owner.retry_at(expected, key.clone()) { return false; }
        self.emit(StoreEvent::Resource(key), cx);
        true
    }

    /// The current serving attachment and producer authority must both admit
    /// a Cargo observation. UI observation counters are not producer authority.
    pub(crate) fn cargo_read_admission<T>(
        &self,
        key: &PageKey,
        resource: &Resource<T>,
    ) -> CargoReadAdmission {
        debug_assert!(is_cargo_source_resource(key));
        if !self.owner_serving() {
            return self
                .owner
                .current_fault()
                .map_or(CargoReadAdmission::Checking, |fault| {
                    CargoReadAdmission::Fault(ErrorValue::new(
                        FaultCode::Transport,
                        format!("The Cargo owner is unavailable. {fault}"),
                    ))
                });
        }
        if self.is_loading(key) {
            return CargoReadAdmission::Checking;
        }
        match admit_resource(resource, self.snapshot.key(), self.owner_serving()) {
            ResourceAdmission::Current(_) => CargoReadAdmission::Current,
            ResourceAdmission::Failed { terminal, .. } => match terminal {
                ResourceTerminal::Fault(error) => CargoReadAdmission::Fault(error.clone()),
                ResourceTerminal::Unavailable(reason) => {
                    CargoReadAdmission::Unavailable(reason.clone())
                }
                ResourceTerminal::Complete | ResourceTerminal::Partial => {
                    CargoReadAdmission::Checking
                }
            },
            ResourceAdmission::Retained { .. } | ResourceAdmission::Pending(_) => {
                CargoReadAdmission::Checking
            }
        }
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

    /// Returns a current Cargo source file read under the owner receipt.
    #[must_use]
    pub fn cargo_source(
        &self,
        file: &crate::model::pages::CargoSourceKey,
    ) -> Resource<crate::model::pages::CargoSourcePage> {
        self.pages.cargo_source(file)
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

fn owner_failure_value(fault: &OwnerFault) -> ErrorValue {
    ErrorValue::new(
        FaultCode::Transport,
        format!("The index could not start. {fault}"),
    )
}

#[cfg(test)]
mod owner_failure_tests;

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

    #[test]
    fn a_symbol_address_must_belong_to_its_pinned_package_before_release_rebasing() {
        let route = |id: &str| {
            Route::Symbol(crate::navigation::SymbolRoute {
                project: None,
                package: crate::core::PackageId::new("pkg:cargo/serde@1.0.0").expect("package"),
                id: crate::navigation::Coordinate::new(id).expect("coordinate"),
                at: Some(crate::navigation::ReleaseId::new("1.0.1").expect("release")),
                view: crate::navigation::View::Page,
                line: None,
                selected: None,
            })
        };
        let foreign = route("pkg:cargo/serde_core@1.0.0::src/lib.rs:1::Item");
        assert_eq!(
            route_declaration(&foreign),
            Err(Unread::CoordinateOutsidePackage)
        );
        assert!(route_symbol(&foreign).is_none());
        assert!(route_keys(&foreign).is_empty());

        let own = route("pkg:cargo/serde@1.0.0::src/lib.rs:1::Item");
        assert_eq!(
            route_declaration(&own).expect("own declaration").as_str(),
            "pkg:cargo/serde@1.0.1::src/lib.rs:1::Item"
        );
    }

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
            workspace: Arc::from([]),
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
            not_ready_capabilities: Arc::from([]),
        }
    }

    impl PageReader for FixtureReader {
        fn read(
            &mut self,
            request: &ReadRequest,
            context: &ReadContext<'_>,
        ) -> Result<PageValue, ReadFailure> {
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
                ReadRequest::Package(package) => {
                    let mut dossier = crate::shell::tests::dossier();
                    dossier.package = package.clone();
                    Ok(PageValue::Package(dossier))
                }
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
        rig_with_limits(cx, workers, crate::runtime::reads::ReadLimits::DEFAULT)
    }

    fn limits(reads: usize, prefetch: usize) -> crate::runtime::reads::ReadLimits {
        crate::runtime::reads::ReadLimits::new(
            std::num::NonZeroUsize::new(reads).expect("nonzero"),
            prefetch,
        )
    }

    fn rig_with_limits(
        cx: &mut TestAppContext,
        workers: usize,
        limits: crate::runtime::reads::ReadLimits,
    ) -> Rig {
        cx.executor().allow_parking();
        let gate: Gate = Arc::new((Mutex::new(BTreeSet::new()), Condvar::new()));
        let reader_gate = Arc::clone(&gate);
        let pool = ReadPool::start_with(workers, limits, move |_| FixtureReader {
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
            store.health().is_loaded()
                && store.orbit().activity() == Activity::Stopped
                && store.pool_activity().is_idle()
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
            crate::runtime::wait::until("the store reached the state the test waits for", || {
                cx.run_until_parked();
                self.store.read_with(cx, |store, _| done(store))
            });
        }
    }

    fn symbol(name: &str) -> SymbolRef {
        SymbolRef::new(name).expect("symbol")
    }

    fn exhaust_page_generations(store: &mut DataStore) {
        let last = Generation::new(u64::MAX).expect("final nonzero generation");
        store.pages.test_advance_generation_to(last);
        let root = store.snapshot.key();
        let generation = store
            .pages
            .begin_forced(&PageKey::Health, root)
            .expect("last mint admitted")
            .expect("last fetch");
        assert_eq!(generation, last);
        assert_eq!(
            store.pages.land(
                &PageKey::Health,
                generation,
                Ok(crate::model::pages::PageValue::Health(health(7)))
            ),
            Landing::Applied
        );
    }

    fn assert_exhaustion_fault<T>(resource: &Resource<T>) {
        assert_eq!(resource.activity(), Activity::Stopped);
        assert_eq!(
            resource.terminal(),
            &ResourceTerminal::Fault(crate::model::pages::store::GenerationExhausted.fault())
        );
    }

    #[gpui::test]
    fn exhausted_foreground_admission_is_discoverable_and_ensure_retry_are_stamp_stable(
        cx: &mut TestAppContext,
    ) {
        let rig = rig(cx, 1);
        let reference = symbol("fast-exhausted");
        let key = PageKey::Symbol(reference.clone());
        rig.store.update(cx, |store, cx| {
            exhaust_page_generations(store);
            store.ensure(key.clone(), cx);
            assert_exhaustion_fault(&store.symbol(&reference));
            assert_eq!(store.pages.inflight(&key), None);
            assert!(!store.prefetching.contains(&key));
            assert_eq!(store.stats.submitted, rig.base.submitted);
        });
        assert_eq!(rig.take_events(), [StoreEvent::Resource(key.clone())]);
        let stamp = rig.store.read_with(cx, |store, _| store.stamp(&key));
        rig.store.update(cx, |store, cx| {
            assert_eq!(store.ensure(key.clone(), cx), stamp);
            store.retry(key.clone(), cx);
            assert_eq!(store.stamp(&key), stamp);
            assert_exhaustion_fault(&store.symbol(&reference));
            assert_eq!(store.pages.inflight(&key), None);
            assert_eq!(store.stats.submitted, rig.base.submitted);
            assert!(store.pool_activity().is_idle());
        });
        assert!(
            rig.take_events().is_empty(),
            "permanent refusal cannot trigger a render/event loop"
        );
    }

    #[gpui::test]
    fn an_exhausted_prefetch_starts_nothing_and_a_visible_visit_faults_once(
        cx: &mut TestAppContext,
    ) {
        let rig = rig(cx, 1);
        let reference = symbol("fast-hint-exhausted");
        let key = PageKey::Symbol(reference.clone());
        rig.store.update(cx, |store, cx| {
            exhaust_page_generations(store);
            let stamp = store.stamp(&key);
            store.prefetch(key.clone(), cx);
            assert_eq!(store.stamp(&key), stamp);
            assert_eq!(store.pages.inflight(&key), None);
            assert!(!store.prefetching.contains(&key));
            assert_eq!(store.symbol(&reference).activity(), Activity::NotYet);
            assert_eq!(store.stats.prefetched, rig.base.prefetched);
            assert_eq!(store.stats.submitted, rig.base.submitted);
            let retained = symbol("retained-exhausted-hint");
            let retained_key = PageKey::Symbol(retained.clone());
            let old_root = crate::core::VersionedRoot::synthetic(
                backend_library::view_state_root(&[(
                    "old-hint".to_owned(),
                    "authority".to_owned(),
                )]),
                3,
            );
            assert!(store.pages.seed(
                crate::model::pages::SeedEntry::Symbol(retained.clone(), Arc::new(page(&retained))),
                old_root
            ));
            let before = store.symbol(&retained);
            let stamp = store.stamp(&retained_key);
            store.prefetch(retained_key.clone(), cx);
            assert_eq!(
                store.symbol(&retained),
                before,
                "failed hint mint preserves retained bytes and activity"
            );
            assert_eq!(store.symbol(&retained).value_root(), Some(old_root));
            assert_eq!(store.stamp(&retained_key), stamp);
            assert_eq!(store.pages.inflight(&retained_key), None);
            assert!(!store.prefetching.contains(&retained_key));
            assert_eq!(store.stats.prefetched, rig.base.prefetched);
            assert_eq!(store.stats.submitted, rig.base.submitted);
        });
        assert!(rig.take_events().is_empty());
        rig.store.update(cx, |store, cx| {
            store.ensure(key.clone(), cx);
        });
        assert_eq!(rig.take_events(), [StoreEvent::Resource(key.clone())]);
        rig.store.read_with(cx, |store, _| {
            assert_exhaustion_fault(&store.symbol(&reference));
            assert!(store.pool_activity().is_idle());
        });
    }

    #[gpui::test]
    fn an_exhausted_forced_request_cancels_the_previous_worker_before_its_terminal_can_land(
        cx: &mut TestAppContext,
    ) {
        struct Witness {
            began: std::sync::mpsc::Sender<CancellationToken>,
            interrupted: std::sync::mpsc::Sender<bool>,
            completed_success: std::sync::mpsc::Sender<Result<PageValue, ReadFailure>>,
            gate: Arc<(Mutex<bool>, Condvar)>,
        }
        impl PageReader for Witness {
            fn read(
                &mut self,
                request: &ReadRequest,
                context: &ReadContext<'_>,
            ) -> Result<crate::model::pages::PageValue, ReadFailure> {
                let ReadRequest::Symbol(reference) = request else {
                    return match request {
                        ReadRequest::Health => {
                            Ok(crate::model::pages::PageValue::Health(health(7)))
                        }
                        _ => Err(ReadFailure::Unavailable(
                            UnavailableReason::Unsupported,
                            Arc::from("fixture"),
                        )),
                    };
                };
                let gate = Arc::clone(&self.gate);
                let interrupt = context.cancel.on_cancel(move || {
                    let (lock, ready) = &*gate;
                    *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
                    ready.notify_all();
                });
                self.began
                    .send(context.cancel.clone())
                    .expect("actual reader token");
                let (lock, ready) = &*self.gate;
                let (released, timeout) = ready
                    .wait_timeout_while(
                        lock.lock().expect("wait predicate"),
                        crate::runtime::wait::HUNG,
                        |released| !*released,
                    )
                    .expect("bounded reader interruption wait");
                self.interrupted
                    .send(*released && !timeout.timed_out() && context.cancel.is_cancelled())
                    .expect("outside-callback interruption observation");
                drop((released, interrupt));
                // Deliberately return useful success after cancellation: the
                // real pool must convert it to its cancelled terminal.
                let success = Ok(PageValue::Symbol(page(reference)));
                self.completed_success
                    .send(success.clone())
                    .expect("constructed useful success witness");
                success
            }
        }
        cx.executor().allow_parking();
        let (began, entered) = std::sync::mpsc::channel();
        let (interrupted, observed) = std::sync::mpsc::channel();
        let (completed_success, completed) = std::sync::mpsc::channel();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let pool = ReadPool::start(1, move |_| Witness {
            began: began.clone(),
            interrupted: interrupted.clone(),
            completed_success: completed_success.clone(),
            gate: Arc::clone(&gate),
        })
        .expect("actual worker");
        let store = cx.update(|cx| DataStore::install(cx, snapshot(), Some(pool)));
        crate::runtime::wait::until("initial route is settled", || {
            cx.run_until_parked();
            store.read_with(cx, |store, _| {
                store.health().is_loaded() && store.pool_activity().is_idle()
            })
        });
        let reference = symbol("slow-final-generation");
        let key = PageKey::Symbol(reference.clone());
        let last = Generation::new(u64::MAX).expect("last");
        let base = store.read_with(cx, |store, _| store.stats());
        store.update(cx, |store, cx| {
            store.pages.test_advance_generation_to(last);
            store.ensure(key.clone(), cx);
        });
        let actual_token = entered
            .recv_timeout(crate::runtime::wait::HUNG)
            .expect("registered real worker");
        store.update(cx, |store, cx| {
            assert_eq!(store.pages.inflight(&key), Some(last));
            store.retry(key.clone(), cx);
            assert!(
                actual_token.is_cancelled(),
                "old fetch was cancelled before the fault became visible"
            );
            assert_exhaustion_fault(&store.symbol(&reference));
            assert_eq!(store.pages.inflight(&key), None);
            assert_eq!(store.stats.cancelled - base.cancelled, 1);
            assert_eq!(store.stats.submitted - base.submitted, 1);
            assert_eq!(
                store.pages.stage(
                    &key,
                    last,
                    crate::model::pages::PageValue::Symbol(page(&reference))
                ),
                Landing::Superseded
            );
            assert_eq!(
                store.pages.land(
                    &key,
                    last,
                    Ok(crate::model::pages::PageValue::Symbol(page(&reference)))
                ),
                Landing::Superseded
            );
        });
        assert_eq!(
            observed.recv_timeout(crate::runtime::wait::HUNG),
            Ok(true),
            "actual blocking reader was interrupted; a caught timeout panic cannot masquerade as cancellation"
        );
        assert_eq!(
            completed
                .recv_timeout(crate::runtime::wait::HUNG)
                .expect("reader constructed its useful success after cancellation"),
            Ok(PageValue::Symbol(page(&reference))),
            "a caught later mapping panic cannot satisfy the completion witness"
        );
        let fault_stamp = store.read_with(cx, |store, _| store.stamp(&key));
        crate::runtime::wait::until("late terminal is drained", || {
            cx.run_until_parked();
            store.read_with(cx, |store, _| store.pool_activity().is_idle())
        });
        store.read_with(cx, |store, _| {
            assert_exhaustion_fault(&store.symbol(&reference));
            assert!(store.symbol(&reference).loaded_value().is_none());
            assert_eq!(store.stamp(&key), fault_stamp);
            assert_eq!(
                store.pool.as_ref().expect("pool").load().held,
                crate::runtime::reads::Held::default()
            );
        });
    }

    #[gpui::test]
    fn exhausted_load_more_retains_the_page_without_issuing_a_continuation(
        cx: &mut TestAppContext,
    ) {
        let rig = rig(cx, 1);
        let query = SearchQuery::new("more-at-exhaustion", 12).expect("query");
        let key = PageKey::Search(query.clone());
        rig.store.update(cx, |store, cx| {
            let root = store.snapshot.key();
            let generation = store
                .pages
                .begin_forced(&key, root)
                .expect("admission")
                .expect("search generation");
            let value = SearchPage {
                query: Arc::clone(&query.text),
                rows: Arc::from([]),
                coverage: backend_present::CoverageLine::new(&[], Some(0)),
                next: Some(crate::model::pages::SearchContinuation {
                    cursor: backend_library::PageContinuation::from_cursor(
                        backend_library::Cursor::new(),
                    ),
                    worker: 0,
                }),
            };
            assert_eq!(
                store.pages.land(
                    &key,
                    generation,
                    Ok(crate::model::pages::PageValue::Search(value.clone()))
                ),
                Landing::Applied
            );
            exhaust_page_generations(store);
            assert!(!store.load_more(&query, cx));
            assert_exhaustion_fault(&store.search(&query));
            assert_eq!(store.search(&query).loaded_value(), Some(&value));
            assert_eq!(store.pages.inflight(&key), None);
            assert_eq!(store.stats.submitted, rig.base.submitted);
            let stamp = store.stamp(&key);
            assert!(!store.load_more(&query, cx));
            assert_eq!(store.stamp(&key), stamp);
        });
        assert_eq!(rig.take_events(), [StoreEvent::Resource(key)]);
    }

    #[gpui::test]
    fn owner_failure_at_generation_exhaustion_is_a_terminal_resource_with_retained_bytes(
        cx: &mut TestAppContext,
    ) {
        let rig = rig(cx, 1);
        rig.store.update(cx, |store, cx| {
            store.focus(vec![PageKey::Health], cx);
            exhaust_page_generations(store);
            let root = store.health().value_root();
            store.owner_failed(&OwnerFault::Lost("fixture owner vanished".into()), cx);
            assert_exhaustion_fault(&store.health());
            assert_eq!(
                store.health().loaded_value().map(|value| value.rows),
                Some(7)
            );
            assert_eq!(store.health().value_root(), root);
            assert_eq!(store.pages.inflight(&PageKey::Health), None);
            let stamp = store.stamp(&PageKey::Health);
            store.ensure(PageKey::Health, cx);
            assert_eq!(store.stamp(&PageKey::Health), stamp);
            assert_eq!(store.stats.submitted, rig.base.submitted);
            assert!(store.pool_activity().is_idle());
        });
    }

    /// Finish real worker reads while the foreground executor is withheld.
    /// All 24 terminals are in the outbox, with no worker or queued job left.
    fn finish_burst_without_landing(rig: &Rig, cx: &mut TestAppContext) -> Vec<PageKey> {
        let keys = (0..24)
            .map(|index| PageKey::Symbol(symbol(&format!("fast-burst-{index}"))))
            .collect::<Vec<_>>();
        rig.store
            .update(cx, |store, cx| store.focus(keys.clone(), cx));
        crate::runtime::wait::until("24 reads completed before any UI landing", || {
            rig.store.read_with(cx, |store, _| {
                store.pool_activity()
                    == PoolLoad {
                        queued: 0,
                        running: 0,
                        undelivered: 24,
                    }
            })
        });
        assert_eq!(
            rig.store.read_with(cx, |store, _| store.stats().landed),
            rig.base.landed
        );
        rig.take_events();
        keys
    }

    /// The native capture callers share this idle API: a finished backlog
    /// must keep them waiting after the first and second bounded landing.
    #[gpui::test]
    fn capture_readiness_waits_for_all_completed_read_landing_turns(cx: &mut TestAppContext) {
        let rig = rig(cx, 3);
        let keys = finish_burst_without_landing(&rig, cx);
        assert!(
            !rig.store
                .read_with(cx, |store, _| store.pool_activity().is_idle())
        );
        assert_eq!(
            rig.store.read_with(cx, |store, _| store.pool_load()),
            (24, 0)
        );
        for remaining in [16, 8, 0] {
            assert_eq!(
                rig.store.update(cx, DataStore::drain),
                8,
                "one bounded landing turn"
            );
            let load = rig.store.read_with(cx, |store, _| store.pool_activity());
            assert_eq!(
                load,
                PoolLoad {
                    queued: 0,
                    running: 0,
                    undelivered: remaining
                }
            );
            assert_eq!(
                load.is_idle(),
                remaining == 0,
                "capture readiness at {load:?}"
            );
            assert_eq!(
                rig.store.read_with(cx, |store, _| store.pool_load()),
                (remaining, 0)
            );
        }
        assert_eq!(
            rig.store
                .read_with(cx, |store, _| store.stats().landed - rig.base.landed),
            24
        );
        assert!(rig.store.read_with(cx, |store, _| {
            keys.iter().all(
                |key| matches!(key, PageKey::Symbol(symbol) if store.symbol(symbol).is_loaded()),
            )
        }));
    }

    #[gpui::test]
    fn a_completed_read_burst_lands_one_bounded_batch_per_executor_poll(cx: &mut TestAppContext) {
        let rig = rig(cx, 3);
        finish_burst_without_landing(&rig, cx);
        let landed =
            |cx: &mut TestAppContext| rig.store.read_with(cx, |store, _| store.stats().landed);
        let mut before = landed(cx);
        let mut turns = 0;
        while cx.executor().tick() {
            let now = landed(cx);
            assert!(
                now - before <= 8,
                "one executor poll landed {} outcomes",
                now - before
            );
            turns += usize::from(now > before);
            before = now;
        }
        assert_eq!(landed(cx) - rig.base.landed, 24);
        assert_eq!(turns, 3, "24 completed reads require three bounded turns");
        assert!(
            rig.store
                .read_with(cx, |store, _| store.pool_activity().is_idle())
        );
    }

    #[gpui::test]
    fn foreground_work_interleaves_all_completed_read_landing_turns(cx: &mut TestAppContext) {
        let rig = rig(cx, 3);
        finish_burst_without_landing(&rig, cx);
        let log = Rc::new(RefCell::new(String::new()));
        let landings = Rc::clone(&log);
        let _subscription = cx.update(|cx| {
            cx.subscribe(&rig.store, move |_, event: &StoreEvent, _| {
                if matches!(event, StoreEvent::Resource(_)) {
                    landings.borrow_mut().push('L');
                }
            })
        });
        let probe = Rc::clone(&log);
        let foreground = cx.update(|cx| {
            cx.spawn(async move |_| {
                for _ in 0..24 {
                    probe.borrow_mut().push('P');
                    super::super::wake::yield_turn().await;
                }
            })
        });
        cx.run_until_parked();
        drop(foreground);
        let log = log.borrow().clone();
        assert_eq!(log.matches('L').count(), 24, "{log}");
        let first = log.find('L').expect("first landing");
        let last = log.rfind('L').expect("last landing");
        assert!(
            log[first..last].contains('P'),
            "foreground work waited for the backlog: {log}"
        );
        assert!(
            log.split('P').all(|turn| turn.len() <= 8),
            "foreground work did not interleave all landing turns: {log}"
        );
        assert!(
            rig.store
                .read_with(cx, |store, _| store.pool_activity().is_idle())
        );
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
        let (submitted, landed) = rig.store.read_with(cx, |store, _| {
            (store.stats().submitted, store.stats().landed)
        });
        assert_eq!(
            (submitted - rig.base.submitted, landed - rig.base.landed),
            (1, 1),
            "one read for three ensures"
        );
        let page = rig
            .store
            .read_with(cx, |store, _| store.symbol(&symbol("fast-page")));
        assert_eq!(
            page.loaded_value()
                .map(|page| page.identity.name.to_string()),
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
        rig.store
            .update(cx, |store, cx| store.focus(vec![old.clone()], cx));
        rig.store
            .update(cx, |store, cx| store.focus(vec![new.clone()], cx));
        let (old_resource, cancelled) = rig.store.read_with(cx, |store, _| {
            (store.symbol(&symbol("slow-old")), store.stats().cancelled)
        });
        assert_eq!(cancelled - rig.base.cancelled, 1);
        assert_eq!(
            old_resource.activity(),
            Activity::NotYet,
            "the old page is back to not-yet"
        );
        rig.until(cx, |store| store.symbol(&symbol("fast-new")).is_loaded());
        // The cancelled read never lands, even when its gate opens later.
        rig.open("slow-old");
        // Wait for the cancelled read to finish (its worker leaves the pool),
        // not for a guessed 20 ms: only then is "it never lands" a claim.
        rig.until(cx, |store| store.pool_activity().is_idle());
        assert!(rig.store.read_with(cx, |store, _| {
            store.symbol(&symbol("slow-old")).loaded_value().is_none()
        }));
    }

    #[gpui::test]
    fn lost_owner_revokes_a_running_page_before_its_late_reply(cx: &mut TestAppContext) {
        let rig = rig(cx, 1);
        let symbol = symbol("slow-owner-lost");
        let key = PageKey::Symbol(symbol.clone());
        rig.store
            .update(cx, |store, cx| store.focus(vec![key.clone()], cx));
        rig.until(cx, |store| {
            store.pages.inflight(&key).is_some() && store.pool_activity().running > 0
        });
        rig.store.update(cx, |store, cx| {
            store.owner_failed(&OwnerFault::Lost("socket closed".into()), cx)
        });
        assert!(
            rig.store
                .read_with(cx, |store, _| store.pages.inflight(&key))
                .is_none()
        );
        rig.open("slow-owner-lost");
        rig.until(cx, |store| store.pool_activity().is_idle());
        assert!(
            rig.store.read_with(cx, |store, _| store
                .symbol(&symbol)
                .loaded_value()
                .is_none()),
            "the old owner cannot publish a page after its generation was revoked"
        );
    }

    #[gpui::test]
    fn a_new_root_supersedes_a_blocked_read_and_submits_exactly_one_current_read(
        cx: &mut TestAppContext,
    ) {
        let rig = rig(cx, 1);
        let key = PageKey::Symbol(symbol("slow-root"));
        rig.store
            .update(cx, |store, cx| store.focus(vec![key.clone()], cx));
        rig.until(cx, |store| store.pool_activity().running == 1);
        let old = rig
            .store
            .read_with(cx, |store, _| store.pages.inflight(&key).expect("R1 read"));

        let r2 = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("store".to_owned(), "new-root".to_owned())]),
            6,
        );
        rig.store.update(cx, |store, cx| {
            store.admit_snapshot(Arc::new(AppSnapshot::empty(r2)), cx)
        });
        let (current, submitted) = rig.store.read_with(cx, |store, _| {
            (store.pages.inflight(&key), store.stats().submitted)
        });
        assert_ne!(current, Some(old));
        assert!(current.is_some(), "R2 starts despite the old Running slot");
        assert_eq!(
            submitted - rig.base.submitted,
            2,
            "one R1 and exactly one R2 request"
        );
        rig.store.update(cx, |store, _| {
            let stale_stage =
                store
                    .pages
                    .stage(&key, old, PageValue::Symbol(page(&symbol("slow-root"))));
            assert_eq!(
                stale_stage,
                Landing::Superseded,
                "a queued R1 stage cannot overwrite R2"
            );
        });

        rig.open("slow-root");
        rig.until(cx, |store| {
            store.symbol(&symbol("slow-root")).is_loaded() && store.pool_activity().is_idle()
        });
        let (resource, landed) = rig.store.read_with(cx, |store, _| {
            (store.symbol(&symbol("slow-root")), store.stats().landed)
        });
        assert_eq!(resource.value_root(), Some(r2));
        assert_eq!(landed - rig.base.landed, 1, "only R2 can land");
    }

    #[gpui::test]
    fn a_hover_prefetch_is_cancellable_and_a_click_adopts_it(cx: &mut TestAppContext) {
        let rig = rig(cx, 1);
        let hovered = PageKey::Symbol(symbol("slow-hover"));
        rig.store
            .update(cx, |store, cx| store.prefetch(hovered.clone(), cx));
        assert!(
            rig.store
                .read_with(cx, |store, _| store.is_prefetching(&hovered))
        );
        rig.store
            .update(cx, |store, cx| store.cancel_prefetch(&hovered, cx));
        let resource = rig
            .store
            .read_with(cx, |store, _| store.symbol(&symbol("slow-hover")));
        assert_eq!(resource.activity(), Activity::NotYet);

        // Hover again, then click: the running prefetch is adopted, not repeated.
        let clicked = PageKey::Symbol(symbol("slow-click"));
        rig.store
            .update(cx, |store, cx| store.prefetch(clicked.clone(), cx));
        rig.store.update(cx, |store, cx| {
            store.ensure(clicked.clone(), cx);
        });
        assert!(
            !rig.store
                .read_with(cx, |store, _| store.is_prefetching(&clicked))
        );
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
        rig.store
            .update(cx, |store, cx| store.admit_snapshot(Arc::clone(&next), cx));
        assert_eq!(rig.take_events(), [StoreEvent::Snapshot(Branch::Settings)]);
        // The same snapshot again is not a change.
        rig.store
            .update(cx, |store, cx| store.admit_snapshot(next, cx));
        assert!(rig.take_events().is_empty());
    }

    #[gpui::test]
    fn completed_publication_rereads_open_failed_outline_without_navigation_or_root_change(
        cx: &mut TestAppContext,
    ) {
        let rig = rig(cx, 1);
        let package = PackageRef::parse("pkg:cargo/fail@0.5.1").expect("release");
        let key = PageKey::Package(package.clone());
        let before = rig.store.read_with(cx, |store, _| store.snapshot());
        rig.store.update(cx, |store, cx| {
            let mut old = crate::shell::tests::dossier();
            old.package = package.clone();
            old.outline = Known::unknown(GapReason::ReadFailed, "library record not found");
            assert!(store.pages.seed(crate::model::pages::SeedEntry::Package(package.clone(), Arc::new(old)), before.key()));
            store.focus(vec![key.clone()], cx);
        });
        assert!(rig.store.read_with(cx, |store, _| store.package(&package).loaded_value()
            .is_some_and(|dossier| matches!(dossier.outline, Known::Unknown(_)))));
        let submitted = rig.store.read_with(cx, |store, _| store.stats().submitted);
        rig.store.update(cx, |store, cx| {
            store.packages_published(&BTreeSet::from([package.clone()]), cx);
            assert!(!store.pages.is_seeded(&key));
            assert!(store.pages.is_owner_read_revoked(&key));
            assert_eq!(store.stats().submitted, submitted + 1);
            assert!(Arc::ptr_eq(&before, &store.snapshot()));
        });
        rig.until(cx, |store| store.package(&package).loaded_value()
            .is_some_and(|dossier| matches!(dossier.outline, Known::Known(_)))
            && store.pool_activity().is_idle());
        rig.store.update(cx, |store, cx| {
            let settled = store.stats().submitted;
            store.ensure(key.clone(), cx);
            assert_eq!(store.stats().submitted, settled, "the repaired page is idempotently current");
            assert_eq!(store.focused, BTreeSet::from([key]));
            assert!(Arc::ptr_eq(&before, &store.snapshot()), "no route/history/root change was synthesized");
        });
    }

    #[gpui::test]
    fn publication_defers_hidden_pages_and_preserves_unrelated_content(cx: &mut TestAppContext) {
        let rig = rig(cx, 1);
        let changed = PackageRef::parse("pkg:cargo/fail@0.5.1").expect("release");
        let other = PackageRef::parse("pkg:cargo/fail@0.5.0").expect("other release");
        rig.store.update(cx, |store, cx| {
            store.focus(Vec::new(), cx);
            for package in [&changed, &other] {
                let mut dossier = crate::shell::tests::dossier();
                dossier.package = package.clone();
                assert!(store.pages.seed(crate::model::pages::SeedEntry::Package(package.clone(), Arc::new(dossier)), store.snapshot.key()));
            }
            let other_stamp = store.stamp(&PageKey::Package(other.clone()));
            let submitted = store.stats().submitted;
            store.packages_published(&BTreeSet::from([changed.clone()]), cx);
            assert_eq!(store.stats().submitted, submitted, "hidden pages cost no eager read");
            assert_eq!(store.stamp(&PageKey::Package(other.clone())), other_stamp);
            assert!(store.pages.is_seeded(&PageKey::Package(other.clone())));
            assert!(store.pages.is_owner_read_revoked(&PageKey::Package(changed.clone())));
            store.focus(vec![PageKey::Package(changed.clone())], cx);
            assert_eq!(store.stats().submitted, submitted + 1, "Back/Forward revalidates on visitation");
        });
        rig.until(cx, |store| store.pool_activity().is_idle());
    }

    #[gpui::test]
    fn orbit_route_focus_reads_health_through_the_pool(cx: &mut TestAppContext) {
        let rig = rig(cx, 2);
        rig.store.update(cx, |store, cx| {
            store.focus(
                route_keys(&Route::Orbit(crate::navigation::OrbitRoute::Home)),
                cx,
            );
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

    fn held(rig: &Rig, cx: &mut TestAppContext) -> crate::runtime::reads::Held {
        rig.store
            .read_with(cx, |store, _| {
                store.pool.as_ref().map(|pool| pool.load().held)
            })
            .expect("the rig has a pool")
    }

    /// Schedule: the pages a view waits on hold every admission. A further
    /// read is refused with a typed busy fault instead of growing the pool,
    /// and the next ask succeeds once the held reads land.
    #[gpui::test]
    fn a_read_refused_at_capacity_shows_a_busy_fault_and_recovers(cx: &mut TestAppContext) {
        let rig = rig_with_limits(cx, 2, limits(2, 1));
        let held_keys = vec![
            PageKey::Symbol(symbol("slow-held-1")),
            PageKey::Symbol(symbol("slow-held-2")),
        ];
        rig.store.update(cx, |store, cx| store.focus(held_keys, cx));
        let refused = symbol("fast-refused");
        rig.store.update(cx, |store, cx| {
            store.ensure(PageKey::Symbol(refused.clone()), cx);
        });
        let busy = rig.store.read_with(cx, |store, _| store.symbol(&refused));
        assert_eq!(busy.activity(), Activity::Stopped);
        assert!(
            matches!(busy.terminal(), ResourceTerminal::Fault(error) if error.code() == FaultCode::Transport && error.message().contains("busy")),
            "{busy:?}"
        );
        assert_eq!(
            held(&rig, cx).total(),
            2,
            "the refused read took no admission"
        );
        rig.open("slow-held-1");
        rig.open("slow-held-2");
        rig.until(cx, |store| store.pool_activity().is_idle());
        rig.store.update(cx, |store, cx| {
            store.retry(PageKey::Symbol(refused.clone()), cx)
        });
        rig.until(cx, |store| store.symbol(&refused).is_loaded());
    }

    /// Schedule: every admission is held while a hover prefetch is still
    /// queued, and a view asks for another page. The prefetch gives up its
    /// admission and its slot is cancelled at once; it never runs.
    #[gpui::test]
    fn an_evicted_prefetch_is_cancelled_at_once_and_never_lands(cx: &mut TestAppContext) {
        let rig = rig_with_limits(cx, 1, limits(3, 1));
        let busy = PageKey::Symbol(symbol("slow-busy"));
        rig.store
            .update(cx, |store, cx| store.focus(vec![busy.clone()], cx));
        rig.until(cx, |store| store.pool_activity().running == 1);
        let hover = PageKey::Symbol(symbol("fast-hover"));
        let waiting = PageKey::Symbol(symbol("fast-waiting"));
        let evicting = PageKey::Symbol(symbol("fast-evicting"));
        rig.store.update(cx, |store, cx| {
            store.prefetch(hover.clone(), cx);
            store.ensure(waiting.clone(), cx);
        });
        assert!(
            rig.store
                .read_with(cx, |store, _| store.is_prefetching(&hover))
        );
        rig.take_events();
        rig.store.update(cx, |store, cx| {
            store.ensure(evicting.clone(), cx);
        });
        let (prefetching, hover_loading, evicting_loading) = rig.store.read_with(cx, |store, _| {
            (
                store.is_prefetching(&hover),
                store.is_loading(&hover),
                store.is_loading(&evicting),
            )
        });
        assert!(
            !prefetching && !hover_loading,
            "the evicted prefetch's slot was cancelled"
        );
        assert!(evicting_loading, "the view's read took the admission");
        assert!(
            rig.take_events()
                .contains(&StoreEvent::Resource(hover.clone()))
        );
        rig.open("slow-busy");
        rig.until(cx, |store| store.pool_activity().is_idle());
        rig.store.read_with(cx, |store, _| {
            assert!(store.symbol(&symbol("fast-evicting")).is_loaded());
            assert!(store.symbol(&symbol("fast-waiting")).is_loaded());
            assert!(
                store.symbol(&symbol("fast-hover")).loaded_value().is_none(),
                "the evicted prefetch never ran"
            );
        });
        assert_eq!(held(&rig, cx), crate::runtime::reads::Held::default());
    }

    /// Schedule: the owner is lost while a read runs, and its late reply
    /// arrives after the slot was revoked. It never lands, and its
    /// admission returns once the UI discards it.
    #[gpui::test]
    fn a_late_generation_never_lands_and_returns_its_admission(cx: &mut TestAppContext) {
        let rig = rig(cx, 1);
        let late = symbol("slow-late");
        let key = PageKey::Symbol(late.clone());
        rig.store
            .update(cx, |store, cx| store.focus(vec![key.clone()], cx));
        rig.until(cx, |store| store.pool_activity().running == 1);
        rig.store.update(cx, |store, cx| {
            store.owner_failed(&OwnerFault::Lost("socket closed".into()), cx)
        });
        assert_eq!(held(&rig, cx).total(), 1, "the revoked read still runs");
        rig.open("slow-late");
        rig.until(cx, |store| store.pool_activity().is_idle());
        assert!(
            rig.store
                .read_with(cx, |store, _| store.symbol(&late).loaded_value().is_none())
        );
        assert_eq!(held(&rig, cx), crate::runtime::reads::Held::default());
    }

    /// A delayed eviction receipt is generation-specific. It cannot cancel
    /// a newer prefetch, and a duplicate receipt emits no second event.
    #[gpui::test]
    fn stale_prefetch_eviction_cannot_revoke_a_newer_generation(cx: &mut TestAppContext) {
        let rig = rig(cx, 1);
        let key = PageKey::Symbol(symbol("eviction-generation"));
        let (old, current, before, cancelled) = rig.store.update(cx, |store, _| {
            let root = store.snapshot.key();
            let old = store
                .pages
                .begin_forced(&key, root)
                .expect("page generation admission")
                .expect("old prefetch slot");
            let current = store
                .pages
                .begin_forced(&key, root)
                .expect("page generation admission")
                .expect("new selected generation");
            store.prefetching.insert(key.clone());
            (old, current, store.pages.stamp(&key), store.stats.cancelled)
        });
        rig.take_events();
        rig.store.update(cx, |store, cx| {
            store.cancel_evicted(
                Evicted {
                    key: key.clone(),
                    generation: old,
                },
                cx,
            )
        });
        rig.store.read_with(cx, |store, _| {
            assert_eq!(store.pages.inflight(&key), Some(current));
            assert!(store.prefetching.contains(&key));
            assert_eq!(store.pages.stamp(&key), before);
            assert_eq!(store.stats.cancelled, cancelled);
        });
        assert!(
            rig.take_events().is_empty(),
            "old receipt changed no current page"
        );
        rig.store.update(cx, |store, cx| {
            store.cancel_evicted(
                Evicted {
                    key: key.clone(),
                    generation: current,
                },
                cx,
            )
        });
        assert_eq!(rig.take_events(), vec![StoreEvent::Resource(key.clone())]);
        rig.store.update(cx, |store, cx| {
            store.cancel_evicted(
                Evicted {
                    key: key.clone(),
                    generation: current,
                },
                cx,
            )
        });
        assert!(
            rig.take_events().is_empty(),
            "duplicate receipt cannot revoke twice"
        );
        rig.store.read_with(cx, |store, _| {
            assert_eq!(store.pages.inflight(&key), None);
            assert!(!store.prefetching.contains(&key));
            assert_eq!(store.stats.cancelled, cancelled + 1);
        });
    }

    #[test]
    fn a_pool_is_idle_only_when_no_read_is_queued_running_or_undelivered() {
        assert!(PoolLoad::default().is_idle(), "no jobs is idle");
        assert!(
            !PoolLoad {
                queued: 1,
                ..PoolLoad::default()
            }
            .is_idle(),
            "a queued job is work"
        );
        assert!(
            !PoolLoad {
                running: 1,
                ..PoolLoad::default()
            }
            .is_idle(),
            "a running job is work"
        );
        assert!(
            !PoolLoad {
                queued: 2,
                running: 3,
                undelivered: 4
            }
            .is_idle(),
            "both is work"
        );
        assert!(
            !PoolLoad {
                undelivered: 1,
                ..PoolLoad::default()
            }
            .is_idle(),
            "a finished result still needs landing"
        );
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
        store.update(cx, |store, cx| {
            store.admit_snapshot(Arc::clone(&unrelated), cx)
        });
        cx.run_until_parked();
        assert_eq!(notified(cx), baseline, "no re-render for other slices");

        // Its own key and its own branch do.
        store.update(cx, |store, cx| store.retry(mine.clone(), cx));
        cx.run_until_parked();
        let after_key = notified(cx);
        assert!(after_key > baseline, "its page changed");
        let mut settings = unrelated.settings().clone();
        settings.reduced_motion = !settings.reduced_motion;
        store.update(cx, |store, cx| {
            store.admit_snapshot(Arc::new(unrelated.with_settings(settings)), cx)
        });
        cx.run_until_parked();
        assert_eq!(notified(cx), after_key + 1, "its branch changed once");
    }
}
