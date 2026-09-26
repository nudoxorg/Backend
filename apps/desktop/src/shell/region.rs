//! Region plumbing: the measured, cached embedding every region uses, the
//! state every region carries, and the handles regions act through.

use crate::model::AppSnapshot;
use crate::model::pages::PageKey;
use crate::navigation::Intent;
use crate::runtime::UiRootEntity;
use crate::runtime::store::{Branch, DataStore, StoreEvent, Watch};
use facet::{ActiveFacet as _, Measure};
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Element, ElementId, Entity, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Render, StyleRefinement, Subscription,
    WeakEntity, Window, px,
};
use std::sync::Arc;

/// What every region keeps: the width it was laid out at, its watch, and a
/// render counter the isolation tests read.
pub(crate) struct RegionCore {
    width: Pixels,
    renders: u64,
    watch: Watch,
    _events: Option<Subscription>,
}

impl RegionCore {
    /// A core watching `branches` and nothing else yet.
    pub(crate) fn new(store: &DataStore, branches: &[Branch]) -> Self {
        Self {
            width: px(0.0),
            renders: 0,
            watch: Watch::new(store, [], branches),
            _events: None,
        }
    }

    /// The measure for the width this region was laid out at, under the
    /// active facet (text scale, density, reveal).
    pub(crate) fn measure(&self, cx: &App) -> Measure {
        Measure::new(self.width, &cx.facet())
    }

    /// The measured width.
    pub(crate) const fn width(&self) -> Pixels {
        self.width
    }

    /// Counts one render. Call first thing in `render`.
    pub(crate) fn rendered(&mut self) {
        self.renders = self.renders.saturating_add(1);
    }

    /// How many times the region rendered.
    pub(crate) const fn renders(&self) -> u64 {
        self.renders
    }

    /// Replaces the watched page keys.
    pub(crate) fn watch_keys(&mut self, store: &DataStore, keys: impl IntoIterator<Item = PageKey>) {
        self.watch.retarget(store, keys);
    }
}

/// A region: an entity with a [`RegionCore`] that re-renders only when its
/// watch says its slice moved.
pub(crate) trait Region: Render + Sized + 'static {
    /// The region's core.
    fn core(&mut self) -> &mut RegionCore;

    /// The page keys this region draws for a snapshot. Called whenever the
    /// route or overlay changes; the default draws no page.
    fn keys(&self, _snapshot: &AppSnapshot) -> Vec<PageKey> {
        Vec::new()
    }

    /// How the region asks for its keys: a region the user reads (shelf,
    /// reader, pins) needs them now; a region that only decorates with them
    /// (the titlebar's bead marks) warms them at prefetch priority.
    fn urgency(&self) -> Urgency {
        Urgency::Now
    }

    /// Called for every store event before the watch decides; a region that
    /// keeps derived state (a focus index, an expanded set) resets it here.
    fn observe(&mut self, _event: &StoreEvent, _store: &DataStore) {}
}

/// How a region asks the store for its keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Urgency {
    /// `ensure`: the region is waiting on it.
    Now,
    /// `prefetch`: nice to have, never ahead of a page someone reads.
    Warm,
}

/// Asks the store for `keys` (idempotent: a current or running key is free).
fn request(store: &Entity<DataStore>, keys: Vec<PageKey>, urgency: Urgency, cx: &mut App) {
    if keys.is_empty() {
        return;
    }
    store.update(cx, |store, cx| {
        for key in keys {
            match urgency {
                Urgency::Now => {
                    store.ensure(key, cx);
                }
                Urgency::Warm => store.prefetch(key, cx),
            }
        }
    });
}

/// Subscribes a region to the store: on a route or overlay change it
/// re-targets its keys, and it notifies itself only when its watch says a
/// watched slice moved.
pub(crate) fn attach<R: Region>(region: &mut R, store: &Entity<DataStore>, cx: &mut Context<R>) {
    let snapshot = store.read(cx).snapshot();
    let keys = region.keys(&snapshot);
    region.core().watch_keys(store.read(cx), keys.clone());
    request(store, keys, region.urgency(), cx);
    let subscription = cx.subscribe(store, |region: &mut R, store, event: &StoreEvent, cx| {
        let moved = event.is_branch(Branch::Route) || event.is_branch(Branch::Overlay);
        let keys = if moved || event.is_branch(Branch::Root) {
            let keys = region.keys(&store.read(cx).snapshot());
            if moved {
                region.core().watch_keys(store.read(cx), keys.clone());
            }
            // A new place, or a new index root: ask for what this region
            // shows (a root advance refreshes it, keeping the last value).
            Some(keys)
        } else {
            None
        };
        region.observe(event, store.read(cx));
        let changed = region.core().watch.changed(store.read(cx), event);
        if let Some(keys) = keys {
            request(&store, keys, region.urgency(), cx);
        }
        if changed {
            cx.notify();
        }
    });
    region.core()._events = Some(subscription);
}

/// Embeds `entity` as a cached view laid out at `style`, and hands it the
/// width it was laid out at before it renders.
///
/// A cached view renders during prepaint, when its bounds are known; this
/// element records those bounds into the region's core first, so the region
/// builds its [`Measure`] from the width it actually gets in the same frame.
/// A width change is a bounds change, which busts the view's cache, so a
/// region never renders at a stale width. Anything else reuses the cached
/// subtree until the region notifies itself.
pub(crate) fn measured<R: Region>(entity: &Entity<R>, style: StyleRefinement) -> Measured<R> {
    Measured {
        entity: entity.clone(),
        style,
    }
}

/// See [`measured`].
pub(crate) struct Measured<R: Region> {
    entity: Entity<R>,
    style: StyleRefinement,
}

impl<R: Region> IntoElement for Measured<R> {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl<R: Region> Element for Measured<R> {
    /// The cached view, on the product path.
    type RequestLayoutState = Option<AnyElement>;
    /// The region rendered at its bounds, while the probe records.
    type PrepaintState = Option<AnyElement>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Option<AnyElement>) {
        // While the probe records (the harness), every region renders every
        // frame so its texts and targets are in every frame's ledger; the
        // product path reuses a region's frame until its slice moves. Both
        // render in prepaint, after the width below is recorded, so no frame
        // is ever laid out for the width before.
        if facet::probe::enabled(cx) {
            let mut style = gpui::Style::default();
            gpui::Refineable::refine(&mut style, &self.style);
            (window.request_layout(style, None, cx), None)
        } else {
            let mut inner = self.entity.clone().cached(self.style.clone()).into_any_element();
            (inner.request_layout(window, cx), Some(inner))
        }
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        inner: &mut Option<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.entity.update(cx, |region, _| region.core().width = bounds.size.width);
        if let Some(inner) = inner {
            inner.prepaint(window, cx);
            return None;
        }
        let mut region = self.entity.clone().into_any_element();
        region.layout_as_root(bounds.size.into(), window, cx);
        region.prepaint_at(bounds.origin, window, cx);
        Some(region)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        inner: &mut Option<AnyElement>,
        probing: &mut Option<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(element) = inner.as_mut().or(probing.as_mut()) {
            element.paint(window, cx);
        }
    }
}

/// The handles a region acts through: the state owner for intents, the store
/// for data, and the shell for focus and transient layers.
#[derive(Clone)]
pub(crate) struct Links {
    pub(crate) root: WeakEntity<UiRootEntity>,
    pub(crate) store: Entity<DataStore>,
    pub(crate) shell: WeakEntity<super::Shell>,
}

impl Links {
    /// Queues one typed intent on the state owner.
    pub(crate) fn dispatch(&self, intent: Intent, cx: &mut App) {
        if let Some(root) = self.root.upgrade() {
            root.update(cx, |root, cx| root.queue(intent, cx));
        }
    }

    /// The current snapshot.
    pub(crate) fn snapshot(&self, cx: &App) -> Arc<AppSnapshot> {
        self.store.read(cx).snapshot()
    }

    /// Runs `f` on the shell, when it is still alive.
    pub(crate) fn shell(&self, cx: &mut App, f: impl FnOnce(&mut super::Shell, &mut Context<super::Shell>)) {
        if let Some(shell) = self.shell.upgrade() {
            shell.update(cx, f);
        }
    }

    /// Hover intent reached a link: warm its page through the read pool.
    pub(crate) fn prefetch(&self, key: PageKey, cx: &mut App) {
        self.store.update(cx, |store, cx| store.prefetch(key, cx));
    }

    /// The pointer left a link before it was followed: drop its prefetch.
    pub(crate) fn cancel_prefetch(&self, key: &PageKey, cx: &mut App) {
        self.store.update(cx, |store, cx| store.cancel_prefetch(key, cx));
    }

    /// Asks for a page again after a fault.
    pub(crate) fn retry(&self, key: PageKey, cx: &mut App) {
        self.store.update(cx, |store, cx| store.retry(key, cx));
    }
}

/// Creates a region entity: builds it with its core, then attaches it to the
/// store.
pub(crate) fn new_region<R: Region>(
    links: &Links,
    cx: &mut App,
    build: impl FnOnce(&DataStore) -> R,
) -> Entity<R> {
    let store = links.store.clone();
    cx.new(|cx| {
        let mut region = build(store.read(cx));
        attach(&mut region, &store, cx);
        region
    })
}
