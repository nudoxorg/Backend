//! The world graph view: the region the shell mounts for the world route and
//! for `Route::Symbol { view: Graph }` (gui-plan §8.2, app.js).
//!
//! One custom element paints the map ([`draw`](super::draw),
//! [`prism`](super::prism)); plain divs carry the quiet chrome over it — the
//! find field (`/`, ⌘K), the focus card, the "where" line. Pointer: hover
//! lights a symbol's neighbourhood, click focuses (the camera flies there
//! and the prism gathers), drag pans with inertia, the wheel and the pinch
//! zoom about the pointer, a trackpad scroll pans. Keys: ↑↓←→ walk the prism
//! (or pan), ↵ goes to the walked row or opens the page, Esc releases the
//! prism then backs out, `+`/`-` zoom, `0` shows the world.

use super::camera::{FocusKind, FocusRoute, Landing, Rig, Travel, View};
use super::draw::{self, Look, Stats, Strategy, tone};
use super::layout::{self, Box2};
use super::interaction::{Reach, TourRoad};
use super::navigation::{Navigation, Exploration, Event, Effect};
use super::discovery::{Chain, Discovery, Search, RoadStop};
use crate::semantics::tour::{self, Tour};
use super::model::{NodeId, World};
use super::prism::{self, Prism, PrismFrame};
use super::scene::{Scene, Terr};
use crate::controls::{field, kbd};
use crate::overlay::float;
use crate::icons::{self, Icon, KindSize};
use crate::measure::{Measure, Set};
use crate::motion::{self, Camera, Motion};

/// The prism's gather track.
const PRISM_KEY: &str = "graph-prism";
/// The hover highlight's fade track.
const HOVER_KEY: &str = "graph-hover";
/// The finite edge activity envelope, independent from its wrapped phase.
const FLOW_KEY: &str = "graph-flow";
/// Finite ambient ink transition when entering or leaving a reading focus.
const READING_KEY: &str = "graph-reading-ink";
use crate::paint::{Bevel, Chamfer, Plate, cut};
use crate::theme::ActiveFacet;
use crate::tokens::ty;
use gpui::{
    AnyElement, App, AppContext, Bounds, Context, CursorStyle, DispatchPhase, Element, ElementId,
    Entity, FocusHandle, Focusable, GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId,
    InteractiveElement, IntoElement, KeyDownEvent, LayoutId, MouseButton, MouseDownEvent,
    MouseExitEvent, MouseMoveEvent, MouseUpEvent, ParentElement, PinchEvent, Pixels, Render, ScrollDelta,
    ScrollWheelEvent, ScrollHandle, SharedString, Style, Styled, StyledText, Subscription,
    Task, TextRun, UnderlineStyle, Window, StatefulInteractiveElement, div, point, px, relative,
};
use gpui::prelude::FluentBuilder as _;
use gpui_component::input::{InputEvent, InputState};
use std::rc::Rc;
use std::cell::RefCell;
use std::sync::Arc;
use std::time::Instant;

/// How the camera starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Start {
    /// The whole world.
    World,
    /// A box framed with padding (app.js `frame=`: 1.2).
    Frame(Box2, f64),
    /// An exact camera (app.js `cam=`).
    Cam(Camera),
    /// A symbol at reading scale with its prism gathered (`focus=`).
    Focus(NodeId),
}

/// Opens a symbol's page (↵, double-click): the shell routes it.
pub type OpenPage = Rc<dyn Fn(NodeId, &mut Window, &mut App)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadingIntent { Focus(NodeId), Reach(NodeId), Tour(NodeId), Chain(u64) }

#[derive(Clone, Copy, Debug)]
enum PeekOrigin {
    Glyph(NodeId),
    Proxy { owner: NodeId, key: prism::SlotKey },
}

struct RetiringHover {
    packet: Option<Arc<super::scene::Neighbourhood>>,
    alpha: f32,
    flow_alpha: f32,
}

struct Drag {
    x: f32,
    y: f32,
    from: Camera,
    moved: f32,
    hist: Vec<(Instant, f32, f32)>,
}

/// Bounded collections retained by the graph between frames.
#[derive(Clone, Copy, Debug, Default)]
pub struct Retained {
    /// Active or settled animation tracks.
    pub motion_tracks: usize,
    /// Recently visited symbols.
    pub trail: usize,
    /// Prepared package tours.
    pub tours: usize,
    /// Rows in the current relation model.
    pub prism_rows: usize,
    /// Cached semantic searches and seeded producer runs.
    pub search_cache: usize,
    /// Positive symbol envelopes awaiting their finite terminal fade.
    pub fading_symbols: usize,
    /// Relations in retained immutable departure packets.
    pub fading_relations: usize,
}

/// Actual native interaction state sampled by the deterministic gallery.
#[cfg(feature = "gallery")]
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct Inspection {
    pub find_open: bool,
    pub query: String,
    pub discovery_ready: bool,
    pub discovery_prepare_ms: Option<f64>,
    pub preparing_tour: Option<(u32, usize)>,
    pub unavailable_tour: Option<u32>,
    pub find_scroll: (f32, f32),
    pub find_selection: (usize, usize),
    pub pending_accept: bool,
    pub searching: bool,
    pub rows: Vec<NodeId>,
    pub chains: usize,
    pub held_chain: Option<Vec<NodeId>>,
    pub hover: Option<NodeId>,
    pub hover_strength: f32,
    pub fading_hover: Option<(NodeId, f32)>,
    pub fading_hovers: Vec<(NodeId, f32)>,
    pub hover_slot: Option<usize>,
    pub prism_selected: Option<usize>,
    pub prism: Option<(NodeId, f32)>,
    pub frame: Option<PrismFrame>,
    pub rail_viewport: Option<Bounds<Pixels>>,
    pub rail_offset: (f32, f32),
    pub rail_visible: Vec<prism::SlotKey>,
    pub viewport: Option<View>,
    pub card_bounds: Option<Bounds<Pixels>>,
    pub pointer: Option<(f32, f32)>,
    pub moving: bool,
}

/// The world graph region.
pub struct GraphView {
    world: Arc<World>,
    scene: Option<Arc<Scene>>,
    _loading: Option<Task<()>>,
    start: Start,
    pending_enter: Option<NodeId>,
    rig: Option<Rig>,
    view: Option<View>,
    hover: Option<NodeId>,
    hover_slot: Option<usize>,
    hover_a: f32,
    flow_a: f32,
    reading_a: f32,
    /// The layout modes the view is in (the focus card beside the map or under it).
    modes: crate::fluid::Modes,
    hover_terr: Option<Terr>,
    prism: Option<Prism>,
    frame: Option<PrismFrame>,
    rail_plan: Option<prism::RailPlan>,
    rail_geometry: Option<prism::RailGeometry>,
    rail_scroll: ScrollHandle,
    rail_text_scale: Option<f32>,
    card_bounds: Option<Bounds<Pixels>>,
    reading_frame: Option<(ReadingIntent, View, Option<Bounds<Pixels>>)>,
    chrome_bounds: std::collections::BTreeMap<&'static str, Bounds<Pixels>>,
    state: Navigation,
    reach_started: Option<Instant>,
    _search_task: Option<Task<()>>,
    searching: bool,
    trail: Vec<NodeId>,
    tours: std::collections::HashMap<u32, Tour>,
    drag: Option<Drag>,
    peek: Option<(ElementId, NodeId)>,
    peek_origins: std::collections::HashMap<ElementId, PeekOrigin>,
    prepared_peek: Option<super::peek::Prepared>,
    /// Only sampled live symbol tracks retire; immutable packets are acquired
    /// during prepaint, never on the pointer input path. One entry per symbol
    /// bounds both tracks and topology by this immutable world.
    retiring_hovers: std::collections::BTreeMap<NodeId, RetiringHover>,
    hover_packet: Option<Arc<super::scene::Neighbourhood>>,
    hover_sampled: bool,
    terminal_hovers: std::collections::BTreeSet<NodeId>,
    motion: Motion,
    /// The camera moved this frame (no new peek rests until it settles).
    moving: bool,
    /// Stable symbol identity: reacquisition reverses the existing envelope.
    hover_key: usize,
    /// The pointer's last position over the map.
    pointer: Option<(f32, f32)>,
    focus_handle: FocusHandle,
    find: Entity<InputState>,
    results: Vec<NodeId>,
    results_scroll: ScrollHandle,
    code_scroll: ScrollHandle,
    road: Option<super::road::RoadProgress>,
    focus_scroll: ScrollHandle,
    hint_metrics: Rc<std::cell::Cell<hints::Metrics>>,
    chain_scroll: ScrollHandle,
    tour_scroll: ScrollHandle,
    find_bounds: Option<Bounds<Pixels>>,
    discovery: Option<Rc<Discovery>>,
    search: Rc<Search>,
    stats: Stats,
    painted_labels: Option<(View, Camera, Vec<draw::TerritoryLabel>)>,
    strategy: Strategy,
    on_open: Option<OpenPage>,
    on_peek_action: Option<super::peek::ActionHandler>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for GraphView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl GraphView {
    /// A graph over `world`; the layout is computed (or fetched from the
    /// cache) on the background executor and the map appears when it lands.
    pub fn new(world: Arc<World>, start: Start, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self::empty(world.clone(), start, window, cx);
        let task = cx.background_executor().spawn(async move {
            let discovery = Discovery::prepare(&world);
            let layout = layout::layout_of(&world);
            (Arc::new(Scene::new(world, layout)), discovery)
        });
        this._loading = Some(cx.spawn(async move |view, cx| {
            let (scene, discovery) = task.await;
            view.update(cx, |view, cx| {
                view.scene = Some(scene);
                view.install_discovery(discovery, cx);
            })
            .ok();
        }));
        this
    }

    /// A graph over an already laid-out world (the gallery, the harness).
    pub fn with_scene(scene: Arc<Scene>, start: Start, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self::empty(scene.world.clone(), start, window, cx);
        let world = this.world.clone();
        let prepared = cx.background_executor().spawn(async move { Discovery::prepare(&world) });
        this._loading = Some(cx.spawn(async move |view, cx| {
            let prepared = prepared.await;
            view.update(cx, |view, cx| {
                view.install_discovery(prepared, cx);
            }).ok();
        }));
        this.scene = Some(scene);
        this
    }

    /// One checked worker-delivery boundary for every constructor.
    fn install_discovery(&mut self, prepared: super::discovery::PreparedDiscovery, cx: &mut Context<Self>) {
        self.discovery = Some(Rc::new(Discovery::from_prepared(prepared)));
        if let Some(intent) = self.state.exploration.preparing_tour() {
            let resolution = intent.resolve(self.state.generation, self.discovery.as_ref().and_then(|engine| engine.package_tour(intent.package)));
            if let Effect::FlyTour(node) = self.state.apply(Event::ResolveTour(intent, resolution)) {
                self.set_hover(None, None);
                if let Some(prism) = &mut self.prism { prism.target = 0.0; }
                self.fly_stop(node, cx);
            }
        }
        let query = self.find.read(cx).value().to_string();
        self.resume_search(&query, cx);
        cx.notify();
    }

    fn empty(world: Arc<World>, start: Start, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (find, subscription) = Self::find_field(window, cx);
        let search = empty_search();
        Self {
            world,
            scene: None,
            _loading: None,
            start,
            pending_enter: None,
            rig: None,
            view: None,
            hover: None,
            hover_slot: None,
            hover_a: 0.0,
            flow_a: 0.0,
            reading_a: 0.0,
            modes: crate::fluid::Modes::new(),
            hover_terr: None,
            prism: None,
            frame: None,
            rail_plan: None,
            rail_geometry: None,
            rail_scroll: ScrollHandle::new(),
            rail_text_scale: None,
            card_bounds: None,
            reading_frame: None,
            chrome_bounds: Default::default(),
            state: Navigation::default(),
            reach_started: None,
            _search_task: None,
            searching: false,
            trail: Vec::new(),
            tours: std::collections::HashMap::new(),
            drag: None,
            peek: None,
            peek_origins: Default::default(),
            prepared_peek: None,
            retiring_hovers: Default::default(),
            hover_packet: None,
            hover_sampled: false,
            terminal_hovers: Default::default(),
            motion: Motion::new(),
            moving: false,
            hover_key: 0,
            pointer: None,
            focus_handle: cx.focus_handle(),
            find,
            results: Vec::new(),
            results_scroll: ScrollHandle::new(),
            code_scroll: ScrollHandle::new(),
            road: None,
            focus_scroll: ScrollHandle::new(),
            hint_metrics: Rc::new(std::cell::Cell::new(hints::Metrics::default())),
            chain_scroll: ScrollHandle::new(),
            tour_scroll: ScrollHandle::new(),
            find_bounds: None,
            discovery: None,
            search,
            stats: Stats::default(),
            painted_labels: None,
            strategy: Strategy::default(),
            on_open: None,
            on_peek_action: None,
            _subscriptions: vec![subscription],
        }
    }

    /// Where ↵ and double-click go.
    pub fn on_open(&mut self, open: OpenPage) {
        self.on_open = Some(open);
    }

    /// Lets the host route explicit card actions, including pinned cards.
    pub fn on_peek_action(&mut self, action: super::peek::ActionHandler) { self.on_peek_action = Some(action); }

    /// Executes an explicit action on the card's own symbol.
    pub fn act_on_peek(&mut self, node: NodeId, action: super::peek::Action, window: &mut Window, cx: &mut Context<Self>) {
        if node as usize >= self.world.len() { return; }
        if let Some(handler) = self.on_peek_action.clone() { handler(node, action, window, cx); return; }
        match action {
            super::peek::Action::Focus => { window.focus(&self.focus_handle, cx); self.set_focus(Some(node), true, cx); }
            super::peek::Action::Open => if let Some(open) = self.on_open.clone() { open(node, window, cx); },
        }
    }

    /// How symbols are batched (measured in CHECKPOINT-2).
    pub fn set_strategy(&mut self, strategy: Strategy) {
        self.strategy = strategy;
    }

    /// What the last frame drew.
    #[must_use]
    pub const fn stats(&self) -> Stats {
        self.stats
    }

    /// The camera drawn last.
    #[must_use]
    pub fn camera(&self) -> Option<Camera> {
        self.rig.as_ref().map(|r| r.cam)
    }

    /// The focused symbol.
    #[must_use]
    pub const fn focused(&self) -> Option<NodeId> {
        self.state.focus
    }

    /// Whether native graph find owns the exploration surface.
    #[must_use]
    pub const fn find_open(&self) -> bool { self.state.find_open }

    /// Whether the native find input currently owns keyboard focus.
    #[must_use]
    pub fn find_focused(&self, window: &Window, cx: &App) -> bool {
        self.find.read(cx).focus_handle(cx).is_focused(window)
    }

    /// True when the map and its semantic query engine have no pending work.
    #[must_use]
    pub fn ready(&self) -> bool { self.scene.is_some() && self.discovery.is_some() && !self.searching && self.state.pending_accept.is_none() }

    /// Actual background semantic preparation time, excluding layout and mount.
    #[must_use]
    pub fn discovery_prepare_duration(&self) -> Option<std::time::Duration> { self.discovery.as_ref().map(|engine| engine.prepare_duration()) }

    /// Collection counts for checking retained state during input storms.
    #[must_use]
    pub fn retained(&self) -> Retained {
        Retained {
            motion_tracks: self.motion.len(),
            trail: self.trail.len(),
            tours: self.tours.len(),
            prism_rows: self.prism.as_ref().map_or(0, |p| p.left.iter().chain(&p.right).map(|c| c.rows.len()).sum()),
            search_cache: self.discovery.as_ref().map_or(0, |d| d.cache_len()),
            fading_symbols: self.retiring_hovers.len(),
            fading_relations: self.retiring_hovers.values().filter_map(|outgoing| outgoing.packet.as_ref()).map(|packet| packet.edges.len()).sum(),
        }
    }

    /// Projects a real symbol through the camera drawn by this region.
    #[must_use]
    pub fn screen_position(&self, node: NodeId) -> Option<(f32, f32)> {
        let (scene, view, rig) = (self.scene.as_ref()?, self.view?, self.rig.as_ref()?);
        if node as usize >= self.world.len() { return None; }
        Some(view.to_screen(&rig.cam, scene.layout.x[node as usize], scene.layout.y[node as usize]))
    }

    /// The actual input and layout state, for deterministic native assertions.
    #[cfg(feature = "gallery")]
    #[must_use]
    pub fn inspect(&self, cx: &App) -> Inspection {
        let input = self.find.read(cx);
        let offset = input.scroll_offset();
        let selection = input.selected_range();
        Inspection { find_open: self.state.find_open, discovery_ready: self.discovery.is_some(), discovery_prepare_ms: self.discovery_prepare_duration().map(|duration| duration.as_secs_f64() * 1000.0), preparing_tour: self.state.exploration.preparing_tour().map(|intent| (intent.package, intent.at)), unavailable_tour: self.state.exploration.unavailable_tour(), query: input.value().to_string(), find_scroll: (f32::from(offset.x), f32::from(offset.y)), find_selection: (selection.start, selection.end), pending_accept: self.state.pending_accept.is_some(), searching: self.searching,
            rows: self.results.clone(), chains: self.search.chains.len(), held_chain: self.state.exploration.chain().map(|c| c.path.clone()),
            hover: self.hover, hover_strength: self.hover_a, fading_hover: self.retiring_hovers.iter().max_by(|a, b| a.1.alpha.total_cmp(&b.1.alpha)).map(|(&node, outgoing)| (node, outgoing.alpha)), fading_hovers: self.retiring_hovers.iter().map(|(&node, outgoing)| (node, outgoing.alpha)).collect(), hover_slot: self.hover_slot, prism_selected: self.state.prism_sel,
            prism: self.prism.as_ref().map(|p| (p.node, p.g)), frame: self.frame.clone(), rail_viewport: self.rail_geometry.as_ref().map(|geometry| geometry.viewport), rail_offset: (f32::from(self.rail_scroll.offset().x), f32::from(self.rail_scroll.offset().y)), rail_visible: self.frame.as_ref().filter(|frame| frame.rail && frame.e >= 0.8).map_or_else(Vec::new, |frame| frame.slots.iter().filter(|slot| slot.label.is_some()).filter_map(|slot| slot.key).collect()), viewport: self.view, card_bounds: self.card_bounds, pointer: self.pointer, moving: self.moving }
    }

    /// The focused gem's current visible window bounds, for a shared
    /// graph-to-page handoff. Offscreen symbols provide no source anchor.
    #[must_use]
    pub fn focus_bounds(&self) -> Option<Bounds<Pixels>> { self.node_bounds(self.state.focus?) }

    /// The node path of the chain currently held (⏎ on a chain result),
    /// from its start through each step's producer to the output — the
    /// same path a peek's road plate names. `None` when no chain is held
    /// (holding a chain clears the focus, so the two are exclusive). S7c:
    /// the host resolves each stop through its own exact-identity service
    /// and holds them together.
    #[must_use]
    pub fn held_chain(&self) -> Option<&[NodeId]> {
        self.state.exploration.chain().map(|chain| chain.path.as_slice())
    }

    /// The exact visible glyph rectangle of a symbol, sharing the
    /// renderer's capped metrics. Used for direct-open identity handoffs.
    #[must_use]
    pub fn node_bounds(&self, node: NodeId) -> Option<Bounds<Pixels>> {
        let (scene, view, rig) = (self.scene.as_ref()?, self.view?, self.rig.as_ref()?);
        if node as usize >= self.world.len() { return None; }
        scene.node_bounds(&view, &rig.cam, node)
    }

    /// The world.
    #[must_use]
    pub fn world(&self) -> &Arc<World> {
        &self.world
    }

    /// Focuses `i` (None releases): the camera flies to it at reading scale
    /// and the prism gathers on arrival (app.js `setFocus`).
    pub fn set_focus(&mut self, i: Option<NodeId>, fly: bool, cx: &mut Context<Self>) {
        // Capture semantic endpoints before Focus retires the selected row.
        let route = i.and_then(|target| self.scene.as_ref().zip(self.view).map(|(scene, view)| {
            let follow = self.state.selected.is_some_and(|key| key.node == target);
            reading_route(scene, &view, self.state.focus, target, follow, self.card_bounds, false)
        }));
        self.state.apply(Event::Focus(i));
        self.focus_scroll.set_offset(point(px(0.0), px(0.0)));
        self._search_task = None;
        self.searching = false;
        self.reach_started = None;
        self.set_hover(None, None);
        if let Some(i) = i {
            self.visit(i);
            if let Some(p) = &mut self.prism { p.target = 0.0; }
            let (Some(scene), Some(view), Some(rig)) = (&self.scene, &self.view, &mut self.rig) else {
                // Not laid out yet: start there.
                self.start = Start::Focus(i);
                cx.notify();
                return;
            };
            if fly {
                rig.fly_with(focus_camera(scene, view, i, self.card_bounds), Some(Landing::Gather(i)), route.map_or(Travel::Focus(package_context(scene, view, i)), Travel::Reading));
            } else {
                rig.set(focus_camera(scene, view, i, self.card_bounds));
                let mut prism = Prism::of(&self.world, i);
                prism.g = 1.0;
                self.prism = Some(prism);
                self.motion.set(PRISM_KEY, 1.0);
            }
        } else if let Some(p) = &mut self.prism {
            p.target = 0.0;
        }
        cx.notify();
    }

    /// Page → graph (G): from wherever the map was left, or the first time
    /// from the package's altitude, fly to `i` and gather (§8.4).
    pub fn enter(&mut self, i: NodeId, cx: &mut Context<Self>) {
        if self.rig.is_none() || self.view.is_none() {
            self.pending_enter = Some(i);
            cx.notify();
            return;
        }
        if let (Some(scene), Some(view), Some(rig)) = (&self.scene, &self.view, &mut self.rig) {
            if self.trail.is_empty() {
                let b = scene.layout.packages[self.world.node(i).pkg as usize].bounds;
                rig.set(view.frame(b, 1.25));
            }
        }
        self.set_focus(Some(i), true, cx);
    }

    /// Explicitly opens the world route, releasing a symbol or exploration.
    pub fn show_world(&mut self, cx: &mut Context<Self>) {
        self.pending_enter = None;
        self.start = Start::World;
        self.set_focus(None, false, cx);
        if let (Some(to), Some(rig)) = (self.world_cam(), &mut self.rig) { rig.fly_with(to, None, Travel::Survey(to)); }
        cx.notify();
    }

    /// Flies the camera to `to` (no focus change).
    pub fn fly_to(&mut self, to: Camera, cx: &mut Context<Self>) {
        if let Some(rig) = &mut self.rig {
            rig.fly_with(to, None, Travel::Survey(to));
        }
        cx.notify();
    }

    /// The currently inspected change reach.
    #[must_use]
    pub fn reach(&self) -> Option<&Reach> { self.state.exploration.reach().map(Arc::as_ref) }

    /// The current package-tour stop, if a tour is being flown.
    #[must_use]
    pub fn tour_stop(&self) -> Option<NodeId> {
        self.state.exploration.tour().and_then(|(tour, at)| tour.stops.get(at)).map(|s| s.node)
    }

    /// R shows change reach; a second R restores the reading prism.
    pub fn toggle_reach(&mut self, cx: &mut Context<Self>) {
        if self.state.find_open { return; }
        let Some(source) = self.state.focus else { return };
        if self.state.exploration.reach().is_some() {
            self.set_focus(Some(source), true, cx);
            return;
        }
        let reach = Arc::new(Reach::of(&self.world, source));
        if let Some(prism) = &mut self.prism { prism.target = 0.0; }
        if !reach.all.is_empty()
            && let (Some(scene), Some(view), Some(rig)) = (&self.scene, self.view, &mut self.rig)
        {
            let mut cam = view.frame(reach.bounds(&scene.layout), 1.3);
            cam.w = cam.w.max(scene.frame_of(&view, source).w).min(scene.max_w(&view));
            rig.fly_with(cam, None, Travel::Survey(cam));
        }
        self.state.apply(Event::Reach(reach));
        self.reach_started = None;
        cx.notify();
    }

    /// Fly the shared semantic reading path of a package.
    pub fn start_tour(&mut self, package: u32, at: usize, cx: &mut Context<Self>) -> bool {
        if package as usize >= self.world.packages.len() { return false; }
        if self.discovery.is_none() {
            if self.state.find_open { return false; }
            self.pending_enter = None;
            self.state.apply(Event::PrepareTour(package, at));
            cx.notify();
            return true;
        }
        let Some(tour) = self.package_tour(package) else { return false; };
        if !tour.shown() { return false; }
        self.pending_enter = None;
        self.set_focus(None, false, cx);
        if let Effect::FlyTour(node) = self.state.apply(Event::Tour(tour, at)) { self.fly_stop(node, cx); }
        true
    }

    fn tour_status(&self) -> Option<SharedString> {
        self.state.exploration.preparing_tour().map(|_| "Preparing a guided tour…".into())
            .or_else(|| self.state.exploration.unavailable_tour().map(|_| "No guided tour is available for this package.".into()))
    }

    fn package_tour(&mut self, package: u32) -> Option<Tour> {
        let tour = self.discovery.as_ref()?.package_tour(package)?;
        Some(self.tours.entry(package).or_insert_with(|| tour.clone()).clone())
    }

    fn tour_package(&self) -> Option<u32> {
        if let Some(i) = self.state.focus { return Some(self.world.node(i).pkg); }
        if let Some((tour, _)) = self.state.exploration.tour() { return Some(tour.package); }
        if self.rig.is_none() {
            let intended = self.pending_enter.or_else(|| if let Start::Focus(node) = self.start { Some(node) } else { None });
            if let Some(node) = intended { return self.world.nodes.get(node as usize).map(|node| node.pkg); }
        }
        let (scene, rig) = (self.scene.as_ref()?, self.rig.as_ref()?);
        #[allow(clippy::cast_possible_truncation)]
        let (x, y) = (rig.cam.x as f32, rig.cam.y as f32);
        if let Some(t) = scene.territory_at(x, y) {
            return Some(t.pkg);
        }
        // At world scale the camera's exact centre can sit in the gap
        // between territories even while the view reads as "over" one
        // (S9/T: the lead's report — T over a package did nothing because
        // this fell through to `None`). The nearest territory's centre is
        // still what "T, here" means at this zoom.
        #[allow(clippy::cast_possible_truncation)]
        scene
            .layout
            .packages
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                let da = (a.x - x).powi(2) + (a.y - y).powi(2);
                let db = (b.x - x).powi(2) + (b.y - y).powi(2);
                da.total_cmp(&db)
            })
            .map(|(p, _)| p as u32)
    }

    fn go_stop(&mut self, at: usize, cx: &mut Context<Self>) {
        if let Effect::FlyTour(node) = self.state.apply(Event::TourStep(at)) { self.fly_stop(node, cx); }
    }

    fn fly_stop(&mut self, i: NodeId, cx: &mut Context<Self>) {
        self.tour_scroll.set_offset(point(px(0.0), px(0.0)));
        self.set_hover(None, None);
        self.state.prism_sel = None;
        if let (Some(scene), Some(view), Some(rig)) = (&self.scene, self.view, &mut self.rig) {
            let focused = scene.focus_cam(&view, i, 0.0);
            let width = focused.w * 2.6;
            let dy = width * f64::from(view.h / view.w) * 0.12;
            let source = self.trail.last().copied().filter(|&node| node != i);
            let route = reading_route(scene, &view, source, i, false, self.chrome_bounds.get("graph-tour-bounds").copied(), true);
            rig.fly_with(Camera::new(focused.x, focused.y + dy, width), None, Travel::Reading(route));
        }
        self.visit(i);
        cx.notify();
    }

    fn visit(&mut self, i: NodeId) {
        let t = self.world.top(i);
        if self.trail.last() != Some(&t) {
            self.trail.push(t);
            if self.trail.len() > 24 {
                self.trail.remove(0);
            }
        }
    }

    fn refresh_search(&mut self, q: &str, cx: &mut Context<Self>) {
        if !self.state.find_open { self._search_task = None; self.searching = false; return; }
        self.state.apply(Event::QueryChanged);
        self.results_scroll.set_offset(point(px(0.0), px(0.0)));
        self.resume_search(q, cx);
    }

    /// Attaching prepared indexes resumes the same query generation, including
    /// an Enter already waiting for it. Typing uses refresh_search instead.
    fn resume_search(&mut self, q: &str, cx: &mut Context<Self>) {
        if !self.state.find_open { self._search_task = None; self.searching = false; return; }
        self._search_task = None;
        if q.trim().is_empty() { self.search = empty_search(); self.results.clear(); self.searching = false; return; }
        let Some(discovery) = &self.discovery else { self.searching = true; return; };
        if let Some(search) = discovery.cached(q) {
            self.results = search.rows.iter().map(|r| r.node).collect(); self.search = search; self.searching = false; return;
        }
        self.search = empty_search(); self.results.clear(); self.searching = true;
        let prepared = discovery.prepared(); let world = self.world.clone(); let query = q.to_owned();
        let generation = self.state.generation;
        let task = cx.background_executor().spawn(async move { Discovery::query_prepared(prepared, &world, &query) });
        let query = q.to_owned();
        self._search_task = Some(cx.spawn(async move |view, cx| {
            let search = task.await;
            view.update(cx, |view, cx| {
                view.deliver_search(generation, &query, search, cx);
            }).ok();
        }));
    }

    /// Latest-wins worker delivery is also the held-completion test boundary.
    fn deliver_search(&mut self, generation: u64, query: &str, search: Search, cx: &mut Context<Self>) {
        if !self.state.accepts(generation) { return; }
        let Some(discovery) = &self.discovery else { return; };
        let search = discovery.remember(query, search);
        self.results = search.rows.iter().map(|row| row.node).collect();
        self.state.result_sel = self.state.result_sel.min((self.results.len() + search.chains.len()).saturating_sub(1));
        self.search = search;
        self.searching = false;
        cx.notify();
    }

    fn choose_result(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.searching {
            self.state.apply(Event::AwaitResult);
            cx.notify();
            return;
        }
        let chain = self.state.result_sel.checked_sub(self.results.len()).and_then(|n| self.search.chains.get(n)).cloned();
        let node = self.results.get(self.state.result_sel).copied();
        if node.is_none() && chain.is_none() { return; }
        self.close_find(window, cx);
        if let Some(chain) = chain { self.hold_chain(chain, cx); }
        else if let Some(node) = node { self.set_focus(Some(node), true, cx); }
    }

    fn hold_chain(&mut self, chain: Chain, cx: &mut Context<Self>) {
        self.chain_scroll.set_offset(point(px(0.0), px(0.0)));
        self.set_focus(None, false, cx);
        if let (Some(scene), Some(view), Some(rig)) = (&self.scene, self.view, &mut self.rig) {
            let bounds = chain.stops.iter().fold(Box2::EMPTY, |b, s| b.with(scene.layout.x[s.node as usize], scene.layout.y[s.node as usize]));
            if !chain.stops.is_empty() {
                let to = readable_frame(scene, &view, &view, bounds, chain.stops[0].node, chain_margin(&view));
                rig.fly_with(to, None, Travel::Survey(to));
            }
        }
        self.road = Some(super::road::RoadProgress::for_selection(self.road.take(), &chain, motion::now(cx).saturating_duration_since(motion::epoch(cx))));
        self.code_scroll.set_offset(point(px(0.0), px(0.0)));
        self.state.apply(Event::HoldChain(chain));
        cx.notify();
    }

    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_hover(None, None);
        self.sync_peek(window, cx);
        self.state.apply(Event::OpenFind);
        self.find.update(cx, |input, cx| {
            input.focus(window, cx);
            // Keyboard search replaces the retained query. Ordinary field
            // clicks still keep the native caret/selection behavior.
            input.select_all(window, cx);
        });
        let query = self.find.read(cx).value().to_string();
        self.refresh_search(&query, cx);
        cx.notify();
    }

    /// A fresh find field and its subscription.
    fn find_field(window: &mut Window, cx: &mut Context<Self>) -> (Entity<InputState>, Subscription) {
        let find = cx.new(|cx| InputState::new(window, cx).placeholder("Find a symbol"));
        let subscription = cx.subscribe_in(&find, window, |this: &mut Self, input, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                let q = input.read(cx).value().to_string();
                this.refresh_search(&q, cx);
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {}
            InputEvent::Focus => {
                // Native focus observers arrive after the focus-path draw.
                // Keyboard open already owns its query; a delayed observer
                // must not invalidate a type/Enter batch delivered meanwhile.
                if !this.state.find_open && this.find_focused(window, cx) {
                    this.state.apply(Event::OpenFind);
                    let q = input.read(cx).value().to_string();
                    this.refresh_search(&q, cx);
                    cx.notify();
                }
            }
            InputEvent::Blur => {
                // The engine can resume a paused caret after blur. Drop its
                // ownership rather than leaving a hidden input timer alive.
                this.reset_find(window, cx);
            }
        });
        (find, subscription)
    }

    /// Closes find and returns focus to the map. The last query is retained;
    /// the input engine retires its caret clock when it loses focus.
    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        self.reset_find(window, cx);
    }

    /// Drops the input's transient ownership without taking route focus.
    /// The query text stays in the field: reopening find restores the last
    /// search, and the blink clock's blur path already retires the caret
    /// without replacing the field.
    fn reset_find(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        // A closed field previews the beginning of its retained query,
        // independently of the scroll position used while editing it.
        self.find.update(cx, |input, cx| input.set_scroll_offset(point(px(0.0), px(0.0)), cx));
        self.state.apply(Event::CloseFind);
        self._search_task = None;
        self.searching = false;
        self.search = empty_search();
        self.results.clear();
        cx.notify();
    }

    fn world_cam(&self) -> Option<Camera> {
        Some(self.scene.as_ref()?.world_cam(self.view.as_ref()?))
    }

    /// The find field's keys, taken in the capture phase so the text engine
    /// never sees them: Esc closes, ↑↓ move the selection, ↵ flies to it.
    /// (A key the engine sees after its field lost focus restarts its caret
    /// blink for good, which would keep an idle window drawing.)
    fn reveal_result(&self) {
        if self.results.len() + self.search.chains.len() == 0 { return; }
        let row = self.state.result_sel + usize::from(!self.search.chains.is_empty() && self.state.result_sel >= self.results.len());
        self.results_scroll.scroll_to_item(row);
    }

    fn find_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.find_open {
            return;
        }
        match event.keystroke.key.as_str() {
            "escape" => self.close_find(window, cx),
            "down" => {
                self.state.result_sel = (self.state.result_sel + 1).min((self.results.len() + self.search.chains.len()).saturating_sub(1));
                self.reveal_result();
                cx.notify();
            }
            "up" => {
                self.state.result_sel = self.state.result_sel.saturating_sub(1);
                self.reveal_result();
                cx.notify();
            }
            "enter" => self.choose_result(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    // Keep `super::keys::KEYS` (the words hosts list) in step with this.
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let mods = event.keystroke.modifiers;
        if self.state.find_open {
            return;
        }
        if (key == "/" && !mods.platform && !mods.control && !mods.alt) || (key == "k" && mods.platform && !mods.alt) {
            self.open_find(window, cx); cx.stop_propagation(); return;
        }
        if mods.platform || mods.control || mods.alt { return; }
        if key == "escape" { self.escape(window, cx); cx.stop_propagation(); return; }
        if key == "r" {
            if self.state.focus.is_none() {
                let source = self.tour_stop().or_else(|| self.state.exploration.chain().and_then(|c| c.steps.last()).map(|s| s.node));
                if let Some(node) = source { self.set_focus(Some(node), false, cx); }
            }
            self.toggle_reach(cx); cx.stop_propagation(); return;
        }
        if key == "t" {
            if self.state.exploration.tour().is_some() { self.state.apply(Event::Escape); cx.notify(); }
            else if let Some(package) = self.tour_package() { self.start_tour(package, 0, cx); }
            cx.stop_propagation(); return;
        }
        if let Some((tour, at)) = self.state.exploration.tour() {
            let node = tour.stops[at].node;
            match key {
                "right" | "space" => self.go_stop(at + 1, cx),
                "left" => self.go_stop(at.saturating_sub(1), cx),
                "enter" => if let Some(open) = self.on_open.clone() { open(node, window, cx); },
                _ => {},
            }
            if matches!(key, "right" | "left" | "space" | "enter") { cx.stop_propagation(); return; }
        }
        let (Some(scene), Some(view)) = (self.scene.clone(), self.view) else { return };
        let max_w = scene.max_w(&view);
        match key {
            "enter" => {
                if let Some(f) = self.state.focus {
                    let walked = self.state.selected.map(|key| key.node);
                    if let Some(j) = walked {
                        self.set_focus(Some(j), true, cx);
                    } else if let Some(open) = self.on_open.clone() {
                        open(f, window, cx);
                    }
                }
            }
            "left" | "right" | "up" | "down" => {
                let (dx, dy): (i8, i8) = match key {
                    "left" => (-1, 0),
                    "right" => (1, 0),
                    "up" => (0, -1),
                    _ => (0, 1),
                };
                if let (Some(focus), Some(frame)) = (self.state.focus, &self.frame) {
                    let reading = frame.node == focus && self.prism.as_ref().is_some_and(|prism| prism.node == focus && prism.g == 1.0 && prism.target == 1.0) && !self.rig.as_ref().is_some_and(Rig::flying);
                    if !reading { return; }
                    if let Some(q) = frame.walk(self.state.prism_sel, dx, dy) {
                        if let Some(key) = frame.slots[q].key {
                            self.state.apply(Event::Walk(q, key));
                            if let Some(index) = self.rail_plan.as_ref().and_then(|plan| plan.child_index(key)) { self.rail_scroll.scroll_to_item(index); }
                        }
                        let node = frame.slots[q].node;
                        self.set_hover(node, Some(q));
                    }
                } else if let Some(rig) = &mut self.rig {
                    rig.nudge(f64::from(dx) * 0.15, f64::from(dy) * 0.15);
                }
            }
            "=" | "+" => {
                if let Some(rig) = &mut self.rig {
                    rig.scale(1.0 / 1.5, max_w);
                }
            }
            "-" => {
                if let Some(rig) = &mut self.rig {
                    rig.scale(1.5, max_w);
                }
            }
            "0" => self.show_world(cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.state.apply(Event::Escape) {
            Effect::CloseFind => self.close_find(window, cx),
            Effect::Focus(node) => self.set_focus(node, true, cx),
            Effect::BackOut(node) => {
                if let Some(prism) = &mut self.prism { prism.target = 0.0; }
                if let (Some(scene), Some(view), Some(rig)) = (&self.scene, self.view, &mut self.rig) { rig.fly_with(backed_out(scene, &view, node), None, Travel::Survey(package_context(scene, &view, node))); }
            }
            Effect::World => if let (Some(to), Some(rig)) = (self.world_cam(), &mut self.rig) { rig.fly_with(to, None, Travel::Survey(to)); },
            _ => {},
        }
        self.set_hover(None, None);
        self.sync_peek(window, cx);
        cx.notify();
    }

    fn set_hover(&mut self, i: Option<NodeId>, slot: Option<usize>) {
        self.hover_slot = slot;
        let i = i.filter(|&i| Some(i) != self.state.focus || slot.is_some());
        if i == self.hover { return; }
        // Preserve every sampled positive envelope, including a brief B while
        // A is still departing. Unsampled input-only targets allocate nothing.
        if let Some(node) = self.hover.filter(|_| self.hover_sampled) {
            if self.hover_a > 0.0 {
                let flow_alpha = self.flow_strength();
                self.retiring_hovers.insert(node, RetiringHover {
                    packet: self.hover_packet.take(),
                    alpha: self.hover_a, flow_alpha,
                });
            } else {
                self.terminal_hovers.insert(node);
            }
        }
        self.hover = i;
        self.hover_key = i.map_or(0, |node| node as usize);
        self.hover_packet = None;
        self.hover_sampled = false;
        self.hover_a = 0.0;
        if let Some(node) = i {
            self.terminal_hovers.remove(&node);
            if let Some(incoming) = self.retiring_hovers.remove(&node) {
                // Promote the original Arc rather than rebuilding its topology
                // and BVHs after another symbol occupied Scene's one-slot cache.
                if let (Some(scene), Some(packet)) = (&self.scene, &incoming.packet) {
                    scene.promote_neighbourhood(packet.clone());
                }
                self.hover_packet = incoming.packet;
                self.hover_a = incoming.alpha;
                self.hover_sampled = true;
            }
        }
    }

    fn pick_prism(&self, x: f32, y: f32) -> Option<usize> {
        let focus = self.state.focus?;
        if self.state.find_open || self.moving || self.rig.as_ref().is_some_and(Rig::flying) { return None; }
        let prism = self.prism.as_ref().filter(|prism| prism.node == focus && prism.target == 1.0)?;
        self.frame.as_ref().filter(|frame| frame.node == prism.node).and_then(|frame| frame.pick(x, y))
    }

    fn pointer_move(&mut self, x: f32, y: f32, pressed: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (Some(scene), Some(view), Some(rig)) = (self.scene.clone(), self.view, self.rig.as_mut()) else {
            return false;
        };
        if let (Some(drag), true) = (&mut self.drag, pressed) {
            drag.moved = drag.moved.max((x - drag.x).hypot(y - drag.y));
            drag.hist.push((motion::now(cx), x, y));
            if drag.hist.len() > 6 {
                drag.hist.remove(0);
            }
            if drag.moved > 3.0 {
                self.state.apply(Event::Pan);
                if let Some(prism) = &mut self.prism { prism.target = 0.0; }
                self.reach_started = None;
                let k = f64::from(view.w) / drag.from.w;
                let to = Camera::new(
                    drag.from.x - f64::from(x - drag.x) / k,
                    drag.from.y - f64::from(y - drag.y) / k,
                    drag.from.w,
                );
                rig.drag_to(to);
                self.set_hover(None, None);
                self.sync_peek(window, cx);
                cx.notify();
                return true;
            }
            return false;
        }
        self.state.selected = None; self.state.prism_sel = None;
        self.pointer = Some((x, y));
        let cam = rig.cam;
        let slot = self.pick_prism(x, y);
        let label = self.painted_territory(&view, &cam, x, y);
        let (hover, slot) = match slot {
            Some(q) => (self.frame.as_ref().and_then(|p| p.slots[q].node), Some(q)),
            None if label.is_some() => (None, None),
            None => (scene.pick_stable(&view, &cam, x, y, self.hover.filter(|_| self.hover_slot.is_none())), None),
        };
        let before = (self.hover, self.hover_slot, self.hover_terr);
        self.set_hover(hover, slot);
        self.hover_terr = if hover.is_none() { label.or_else(|| pointer_territory(&scene, &view, &cam, x, y)) } else { None };
        let changed = before != (self.hover, self.hover_slot, self.hover_terr);
        if changed {
            self.sync_peek(window, cx);
            cx.notify();
        }
        changed
    }

    fn pointer_left(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pointer.take().is_some() || self.hover.is_some() || self.hover_terr.is_some() {
            self.set_hover(None, None);
            self.hover_terr = None;
            self.sync_peek(window, cx);
            cx.notify();
        }
    }

    /// Only the exact text accepted by the painter owns these semantic targets.
    /// A previous projection can never route input after a resize or camera move.
    fn painted_territory(&self, view: &View, camera: &Camera, x: f32, y: f32) -> Option<Terr> {
        let (painted_view, painted_camera, labels) = self.painted_labels.as_ref()?;
        if painted_view != view || painted_camera != camera { return None; }
        let point = point(px(x), px(y));
        labels.iter().rev().find(|label| label.bounds.contains(&point)).map(|label| label.territory)
    }

    fn enter_territory(&mut self, territory: Terr, scene: &Scene, view: &View, cx: &mut Context<Self>) {
        let bounds = match territory.module {
            Some(module) => scene.layout.modules[module as usize].bounds,
            _ => scene.layout.packages[territory.pkg as usize].bounds,
        };
        self.set_focus(None, false, cx);
        if let Some(rig) = &mut self.rig {
            let to = view.frame(bounds, 1.25);
            rig.fly_with(to, None, Travel::Survey(to));
        }
        cx.notify();
    }

    fn over_chrome(&self, x: f32, y: f32) -> bool {
        let at = point(px(x), px(y));
        self.card_bounds.is_some_and(|bounds| bounds.contains(&at))
            || self.chrome_bounds.values().any(|bounds| bounds.contains(&at))
    }

    fn pointer_down(&mut self, x: f32, y: f32, window: &mut Window, cx: &mut Context<Self>) {
        if self.over_chrome(x, y) { return; }
        window.focus(&self.focus_handle, cx);
        let Some(rig) = &mut self.rig else { return };
        rig.hold();
        self.pointer = Some((x, y));
        self.drag = Some(Drag { x, y, from: rig.cam, moved: 0.0, hist: vec![(motion::now(cx), x, y)] });
        cx.notify();
    }

    fn pointer_up(&mut self, x: f32, y: f32, clicks: usize, allow_click: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Native delivery can coalesce the final move into mouse-up. Complete
        // direct manipulation before estimating coast from reliable samples.
        let complete_move = self.drag.as_ref().is_some_and(|drag| {
            drag.moved.max((x - drag.x).hypot(y - drag.y)) > 3.0
                && drag.hist.last().is_none_or(|sample| sample.1 != x || sample.2 != y)
        });
        if complete_move { self.pointer_move(x, y, true, window, cx); }
        let Some(drag) = self.drag.take() else { return };
        if drag.moved <= 3.0 && (!allow_click || self.over_chrome(x, y)) { return; }
        let (Some(scene), Some(view)) = (self.scene.clone(), self.view) else { return };
        if drag.moved <= 3.0 {
            let slot = self.pick_prism(x, y);
            let cam = self.rig.as_ref().map_or(drag.from, |r| r.cam);
            if slot.is_none() && let Some(territory) = self.painted_territory(&view, &cam, x, y) {
                self.enter_territory(territory, &scene, &view, cx);
                return;
            }
            let hit = slot
                .and_then(|q| self.frame.as_ref().and_then(|p| p.slots[q].node))
                .or_else(|| scene.pick(&view, &cam, x, y));
            if clicks >= 2 {
                if let (Some(i), Some(open)) = (hit, self.on_open.clone()) {
                    open(i, window, cx);
                    return;
                }
            }
            if let Some(i) = hit {
                self.set_focus(Some(i), true, cx);
            } else if let Some(mut t) = pointer_territory(&scene, &view, &cam, x, y) {
                if view.k(&cam) <= 1.2 { t.module = None; }
                self.enter_territory(t, &scene, &view, cx);
            } else if self.state.focus.is_some() {
                self.set_focus(None, false, cx);
            }
        } else {
            if let Some((vx, vy)) = super::interaction::release_velocity(&drag.hist, motion::now(cx), view.w, view.h) {
                if let Some(rig) = &mut self.rig {
                    let k = view.k(&rig.cam);
                    rig.fling(-vx / k, -vy / k);
                }
            }
        }
        cx.notify();
    }

    /// Navigation is not reading: a wheel or pinch drops the hover and its
    /// peek; what is under the pointer is picked again when the map settles.
    fn navigate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.apply(Event::Pan);
        if let Some(prism) = &mut self.prism { prism.target = 0.0; }
        self.reach_started = None;
        self.hover_terr = None;
        if self.hover.is_some() || self.peek.is_some() {
            self.set_hover(None, None);
            self.sync_peek(window, cx);
        }
        self.moving = true;
    }

    fn wheel(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate(window, cx);
        let (Some(scene), Some(view), Some(rig)) = (self.scene.clone(), self.view, self.rig.as_mut()) else { return };
        let (x, y) = (f32::from(event.position.x), f32::from(event.position.y));
        let max_w = scene.max_w(&view);
        let zoom = event.modifiers.platform || event.modifiers.control;
        match event.delta {
            ScrollDelta::Lines(d) => rig.zoom_about(&view, x, y, f64::from(-d.y * 0.22).exp(), max_w),
            ScrollDelta::Pixels(d) if zoom => {
                rig.zoom_about(&view, x, y, f64::from(-f32::from(d.y) * 0.01).exp(), max_w);
            }
            ScrollDelta::Pixels(d) => rig.pan_px(&view, f32::from(d.x), f32::from(d.y)),
        }
        cx.notify();
    }

    fn pinch(&mut self, event: &PinchEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate(window, cx);
        let (Some(scene), Some(view), Some(rig)) = (self.scene.clone(), self.view, self.rig.as_mut()) else { return };
        let max_w = scene.max_w(&view);
        let factor = 1.0 / (1.0 + f64::from(event.delta)).max(0.05);
        rig.zoom_about(&view, f32::from(event.position.x), f32::from(event.position.y), factor, max_w);
        cx.notify();
    }

    /// The peek's trigger key for node `i`.
    fn peek_key(i: NodeId) -> ElementId {
        ElementId::NamedInteger("graph-node".into(), u64::from(i))
    }

    fn current_peek_origin(&self, node: NodeId) -> PeekOrigin {
        if let (Some(slot), Some(frame)) = (self.hover_slot, &self.frame) {
            if let Some(key) = frame.slots.get(slot).and_then(|slot| slot.key).filter(|key| key.node == node) {
                return PeekOrigin::Proxy { owner: frame.node, key };
            }
        }
        PeekOrigin::Glyph(node)
    }

    /// Resolve the original trigger, rather than another node's currently
    /// hovered slot. A missing proxy never falls back to its home glyph.
    fn origin_anchor(&self, origin: PeekOrigin) -> Option<(Bounds<Pixels>, float::Side)> {
        match origin {
            PeekOrigin::Glyph(node) => {
                let bounds = self.node_bounds(node)?;
                let center = bounds.center();
                if self.over_chrome(f32::from(center.x), f32::from(center.y)) { return None; }
                Some((bounds, float::Side::Right))
            }
            PeekOrigin::Proxy { owner, key } => {
                let frame = self.frame.as_ref().filter(|frame| frame.node == owner && frame.e >= 0.8)?;
                let slot = &frame.slots[frame.locate(key)?];
                let [x0, y0, x1, y1] = slot.label?;
                Some((Bounds::from_corners(point(px(x0), px(y0)), point(px(x1), px(y1))), if slot.side > 0 { float::Side::Right } else { float::Side::Left }))
            }
        }
    }

    fn peek_anchor(&self, node: NodeId) -> Option<(Bounds<Pixels>, float::Side)> {
        let key = Self::peek_key(node);
        let origin = self.peek_origins.get(&key).copied().unwrap_or_else(|| self.current_peek_origin(node));
        self.origin_anchor(origin)
    }

    /// Aim protection can keep A open while B owns pending intent. Refresh
    /// all our live physical sources, without touching other window floats.
    fn anchor_live_peeks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let live = float::live_triggers(window, cx);
        let current = self.peek.as_ref().map(|(key, _)| key);
        self.peek_origins.retain(|key, _| live.contains(key) || current == Some(key));
        let owned: Vec<_> = self.peek_origins.iter().filter(|(key, _)| live.contains(key)).map(|(key, origin)| (key.clone(), *origin)).collect();
        for (key, origin) in owned {
            if let Some((anchor, _)) = self.origin_anchor(origin) {
                float::anchor(&key, anchor, window, cx);
            } else {
                float::anchor_gone(&key, window, cx);
                self.peek_origins.remove(&key);
                if self.peek.as_ref().is_some_and(|(current, _)| current == &key) { self.peek = None; }
            }
        }
    }

    /// Rests the float layer's peek on the hovered symbol (or leaves the
    /// old one): hover intent, chaining and exits are the layer's.
    fn sync_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let want = self.hover.filter(|&i| Some(i) != self.state.focus || self.hover_slot.is_some()).filter(|_| self.drag.as_ref().is_none_or(|d| d.moved <= 3.0));
        if self.peek.as_ref().map(|p| p.1) == want {
            return;
        }
        // A peek rests only on a still map (an open one follows its symbol).
        if self.moving && want.is_some() {
            return;
        }
        if let Some((key, _)) = self.peek.take() {
            float::leave(&key, window, cx);
        }
        let Some(i) = want else { return };
        let Some((anchor, side)) = self.peek_anchor(i) else { return };
        let key = Self::peek_key(i);
        let origin = self.current_peek_origin(i);
        self.peek_origins.entry(key.clone()).or_insert(origin);
        let prepared = self.prepared_peek(i);
        let request = prepared.request(key.clone(), anchor, side, self.on_open.is_some(), Self::peek_actions(cx.entity().downgrade()));
        float::rest(request, window, cx);
        self.peek = Some((key, i));
    }

    /// The prepaint transaction samples one camera before constructing any
    /// chrome. Layout can subsequently correct its target, but never steps
    /// a second camera underneath already rendered breadcrumbs or cards.
    fn advance_camera(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(view), Some(rig)) = (self.view, self.rig.as_mut()) else { return; };
        let (moving, _) = rig.step(&view, window, cx);
        if moving && !self.moving && (self.hover.is_some() || self.peek.is_some()) && self.drag.is_none() {
            self.set_hover(None, None);
            self.sync_peek(window, cx);
        }
        self.moving = moving;
    }

    /// Initializes geometry, samples one camera, then builds the immutable
    /// visual draft before this frame's chrome is constructed.
    fn prepare(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> Option<Prepared> {
        let view = View {
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            w: f32::from(bounds.size.width).max(1.0),
            h: f32::from(bounds.size.height).max(1.0),
        };
        if self.view != Some(view) { self.view = Some(view); }
        let scene = self.scene.clone()?;
        let first_camera = self.rig.is_none();
        if first_camera {
            let cam = match self.start {
                Start::World => scene.world_cam(&view),
                Start::Frame(b, pad) => view.frame(b, pad),
                Start::Cam(cam) => cam,
                Start::Focus(i) => focus_camera(&scene, &view, i, self.card_bounds),
            };
            self.rig = Some(Rig::new(cam));
            if let Start::Focus(i) = self.start && matches!(self.state.exploration, Exploration::Free) {
                self.state.focus = Some(i);
                self.reading_a = 1.0;
                self.motion.set(READING_KEY, 1.0);
                self.visit(i);
                let mut p = Prism::of(&self.world, i);
                p.g = 1.0;
                self.prism = Some(p);
                self.motion.set(PRISM_KEY, 1.0);
            }
        }
        if let Some(i) = self.pending_enter.take() {
            if self.trail.is_empty() {
                let package = scene.layout.packages[self.world.node(i).pkg as usize].bounds;
                self.rig.as_mut()?.set(view.frame(package, 1.25));
            }
            self.set_focus(Some(i), true, cx);
        }
        if first_camera {
            if let Some(node) = self.tour_stop() { self.fly_stop(node, cx); }
        }
        // Accept a queued Enter before chrome and camera sampling, with real
        // native keyboard ownership. A blur not yet observed by InputState
        // still cancels it; returning results never steal focus from a route.
        if self.state.pending_accept.is_some() {
            if !self.find_focused(window, cx) { self.reset_find(window, cx); }
            else if !self.searching {
                let has_results = !self.results.is_empty() || !self.search.chains.is_empty();
                if self.state.apply(Event::ResultsReady(self.state.generation, has_results)) == Effect::AcceptResult {
                    self.choose_result(window, cx);
                }
            }
        }
        // Sample once using this frame's actual parent viewport. Chrome is
        // constructed afterwards from this same camera and region.
        self.advance_camera(window, cx);
        let cam = self.rig.as_ref()?.cam;
        let moving = self.moving;
        let gather = self.state.focus.filter(|_| !self.state.find_open && matches!(self.state.exploration, Exploration::Free) && !self.rig.as_ref().is_some_and(Rig::flying));
        if let Some(i) = gather {
            match &mut self.prism {
                None => self.prism = Some(Prism::of(&self.world, i)),
                Some(prism) if prism.node == i => prism.target = 1.0,
                // A new focus owns navigation immediately, but its outgoing
                // presentation must finish the finite collapse before reuse.
                Some(_) => {},
            }
        }
        // Publish one continuous sample per frame. A new owner gathers on
        // the frame after the old owner's exact terminal zero, using the
        // latest canonical focus rather than a consumed landing callback.
        if let Some(p) = &mut self.prism {
            p.g = self.motion.animate_from(PRISM_KEY, 0.0, p.target, motion::spec::DESCENT, window, cx);
            if p.target == 0.0 && p.g == 0.0 {
                self.prism = None;
                self.frame = None;
                if gather.is_some() { motion::request_frame(window, cx); }
            }
        }
        self.sample_hover(window, cx);
        let active_hover = self.hover.map(|_| ElementId::from((HOVER_KEY, self.hover_key)));
        let prism_key: ElementId = PRISM_KEY.into();
        self.fade_retiring(window, cx);
        let retiring_keys: std::collections::HashSet<_> = self.retiring_hovers.keys().map(|&node| ElementId::from((HOVER_KEY, node as usize))).collect();
        let flow_key: ElementId = FLOW_KEY.into();
        let reading_key: ElementId = READING_KEY.into();
        self.motion.retain(|key| active_hover.as_ref() == Some(key) || key == &prism_key || key == &flow_key || key == &reading_key || retiring_keys.contains(key));
        #[allow(clippy::cast_possible_truncation)]
        let flow = (motion::now(cx).saturating_duration_since(motion::epoch(cx)).as_secs_f64() * 18.0).rem_euclid(45.0) as f32;
        let flow_alpha = self.flow_strength();
        let reach_wave = if let Exploration::Reach(reach) = &self.state.exploration {
            let started = &mut self.reach_started;
            let depth = u8::try_from(reach.waves.len()).unwrap_or(8);
            if moving && started.is_none() { 0.0 } else if motion::reduced(cx) { f32::from(depth) } else {
                let started = started.get_or_insert_with(|| motion::now(cx));
                let progress = motion::now(cx).saturating_duration_since(*started).as_secs_f32() / 0.24;
                if progress < f32::from(depth) { motion::request_frame(window, cx); }
                progress.min(f32::from(depth))
            }
        } else { 0.0 };
        let selected_chain = self.state.exploration.chain().or_else(|| self.state.result_sel.checked_sub(self.results.len()).filter(|_| self.state.find_open).and_then(|n| self.search.chains.get(n)));
        let elapsed = motion::now(cx).saturating_duration_since(motion::epoch(cx));
        let chain_progress = if let Some(chain) = selected_chain {
            self.road = Some(super::road::RoadProgress::for_selection(self.road.take(), chain, elapsed));
            let road = self.road.as_mut().expect("selected road clock");
            let sample = road.sample(elapsed, motion::reduced(cx));
            let (started, budget) = road.timing();
            crate::probe::record_track(cx, || crate::probe::TrackSample { key: "graph-chain-road".into(), kind: crate::probe::TrackKind::Tween, value: sample.progress, target: 1.0, velocity: road.velocity(), started_ms: started.as_secs_f64() * 1000.0, budget_ms: budget.as_secs_f64() * 1000.0, at_ms: elapsed.as_secs_f64() * 1000.0, live: sample.moving, overshoot_ratio: 0.0, overshoot_absolute: 0.0, group: None });
            if sample.moving { motion::request_frame(window, cx); }
            sample
        } else { self.road = None; super::road::RoadSample { progress: 1.0, arcs: 0, moving: false } };
        let facet = cx.facet();
        Some(Prepared {
            scene,
            view,
            cam,
            palette: facet.palette(),
            text_scale: facet.text_scale,
            hover: self.hover,
            hover_a: self.hover_a,
            retiring_hovers: self.retiring_packets(),
            hover_terr: self.hover_terr,
            focus: self.state.focus,
            reading_a: self.reading_a,
            prism: (!self.state.find_open).then(|| self.prism.clone()).flatten(),
            prism_sel: self.state.prism_sel,
            hover_slot: self.hover_slot,
            flow,
            flow_alpha,
            trail: self.trail.clone(),
            reach: self.state.exploration.reach().cloned(),
            reach_wave,
            search: (self.state.find_open && !self.find.read(cx).value().trim().is_empty()).then(|| self.search.clone()),
            chain: selected_chain.map(|chain| chain.stops.clone()),
            chain_progress,
            tour: self.state.exploration.tour().map(|(t, at)| TourRoad { stops: t.stops.iter().map(|s| s.node).collect(), at }),
            strategy: self.strategy,
            occupied: None,
            reserved: Vec::new(),
            frame: None,
        })
    }

    /// Reconciles navigation with chrome measured in this frame. A room
    /// correction preserves exploration and gathered relation identity.
    fn reconcile_reading_room(&mut self, prepared: &Prepared, window: &mut Window, cx: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let intent = if self.state.find_open { None } else {
            match &self.state.exploration {
                Exploration::Free | Exploration::PreparingTour(_) | Exploration::TourUnavailable { .. } => self.state.focus.map(ReadingIntent::Focus),
                Exploration::Reach(reach) => Some(ReadingIntent::Reach(reach.source)),
                Exploration::Tour { data, at } => Some(ReadingIntent::Tour(data.stops[*at].node)),
                Exploration::Chain(_) => Some(ReadingIntent::Chain(self.state.generation)),
            }
        };
        let Some(intent) = intent else { self.reading_frame = None; return None; };
        let occupied = match intent {
            ReadingIntent::Focus(_) | ReadingIntent::Reach(_) => self.card_bounds,
            ReadingIntent::Tour(_) => self.chrome_bounds.get("graph-tour-bounds").copied(),
            ReadingIntent::Chain(_) => self.chrome_bounds.get("graph-chain-bounds").copied(),
        };
        let key = (intent, prepared.view, occupied);
        if self.reading_frame == Some(key) { return occupied; }
        let first = self.reading_frame.is_none();
        self.reading_frame = Some(key);
        let room = Scene::free_view(&prepared.view, occupied);
        let to = match &self.state.exploration {
            Exploration::Free | Exploration::PreparingTour(_) | Exploration::TourUnavailable { .. } => focus_camera(&prepared.scene, &prepared.view, self.state.focus.expect("focused reading"), occupied),
            Exploration::Reach(reach) => {
                if reach.all.is_empty() { focus_camera(&prepared.scene, &prepared.view, reach.source, occupied) }
                else { readable_frame(&prepared.scene, &prepared.view, &room, reach.bounds(&prepared.scene.layout), reach.source, 1.3) }
            }
            Exploration::Chain(chain) => {
                let bounds = chain.stops.iter().fold(Box2::EMPTY, |b, stop| b.with(prepared.scene.layout.x[stop.node as usize], prepared.scene.layout.y[stop.node as usize]));
                if chain.stops.is_empty() { return occupied; }
                readable_frame(&prepared.scene, &prepared.view, &room, bounds, chain.stops[0].node, chain_margin(&prepared.view))
            }
            Exploration::Tour { data, at } => {
                let node = data.stops[*at].node;
                let (x, y) = (prepared.scene.layout.x[node as usize], prepared.scene.layout.y[node as usize]);
                let width = prepared.scene.focus_cam(&room, node, 0.0).w * 2.6;
                let height = width * f64::from(room.h / room.w);
                #[allow(clippy::cast_possible_truncation)]
                let bounds = Box2 { x0: x - (width * 0.5) as f32, y0: y - (height * 0.5) as f32, x1: x + (width * 0.5) as f32, y1: y + (height * 0.5) as f32 };
                prepared.view.frame_in(bounds, &room, 1.0)
            }
        };
        if let Some(rig) = &mut self.rig {
            if motion::reduced(cx) {
                // This frame already has the actual measured reading room.
                // Reduced motion lands and publishes here, without queuing
                // a flight whose only purpose would be next-frame snapping.
                rig.snap_to(to, cx);
                self.moving = false;
            } else if first && matches!(intent, ReadingIntent::Focus(_)) && !rig.flying() { rig.set(to); }
            else {
                let anchor = if matches!(intent, ReadingIntent::Tour(_) | ReadingIntent::Chain(_)) { (room.x + room.w * 0.5, room.y + room.h * 0.5) } else { Scene::focus_anchor(&room) };
                rig.reframe_in(to, prepared.view.flight_room(&room, anchor));
            }
            self.moving |= rig.flying();
        }
        if self.moving { motion::request_frame(window, cx); }
        occupied
    }

    fn finalize(&mut self, prepared: &mut Prepared, window: &mut Window, cx: &mut Context<Self>) {
        prepared.occupied = self.reconcile_reading_room(prepared, window, cx);
        self.reading_a = self.motion.animate_from(READING_KEY, 0.0, if self.state.focus.is_some() { 1.0 } else { 0.0 }, motion::spec::REVEAL, window, cx);
        if self.state.focus.is_none() && self.reading_a == 0.0 { self.motion.replay(READING_KEY); }
        prepared.reading_a = self.reading_a;
        prepared.cam = self.camera().unwrap_or(prepared.cam);
        prepared.reserved = self.chrome_bounds.values().copied().collect();
        if crate::probe::enabled(cx) {
            for (key, handle, visible) in [
                ("graph-focus-scroll", &self.focus_scroll, self.state.focus.is_some() && !self.state.find_open),
                ("graph-chain-scroll", &self.chain_scroll, self.state.exploration.chain().is_some()),
                ("graph-tour-scroll", &self.tour_scroll, self.state.exploration.tour().is_some()),
                ("graph-chain-code-scroll", &self.code_scroll, self.state.exploration.chain().is_some() && window.modifiers().alt),
            ] {
                if visible { if let Some(content) = handle.bounds_for_item(0) {
                    crate::probe::record_scroll(cx, &key.into(), handle.bounds(), content);
                } }
            }
            if self.state.find_open {
                if let Some(mut content) = self.results_scroll.bounds_for_item(0) {
                    for n in 1..self.results.len() + self.search.chains.len() + usize::from(!self.search.chains.is_empty()) {
                        if let Some(row) = self.results_scroll.bounds_for_item(n) { content = content.union(&row); }
                    }
                    crate::probe::record_scroll(cx, &"graph-results-scroll".into(), self.results_scroll.bounds(), content);
                }
            }
        }
        prepared.frame = prepared.prism.as_ref().map(|prism| prism::layout_with_rail(prism, &prepared.scene, &prepared.view, &prepared.cam, prepared.text_scale, self.card_bounds, self.rail_plan.as_ref().zip(self.rail_geometry.as_ref()), window));
        self.frame = prepared.frame.clone();
        if let Some(key) = self.state.selected {
            self.state.prism_sel = self.frame.as_ref().filter(|frame| self.state.focus == Some(frame.node)).and_then(|frame| frame.locate(key));
            if self.state.prism_sel.is_none() { self.state.selected = None; self.set_hover(None, None); }
            else { self.set_hover(Some(key.node), self.state.prism_sel); }
        } else if !self.moving && !self.state.find_open && self.drag.as_ref().is_none_or(|d| d.moved <= 3.0) {
            if let Some((x, y)) = self.pointer {
                let slot = self.pick_prism(x, y);
                if self.over_chrome(x, y) && slot.is_none() { self.set_hover(None, None); }
                else {
                    let node = slot.and_then(|q| self.frame.as_ref().and_then(|frame| frame.slots[q].node))
                        .or_else(|| if self.painted_territory(&prepared.view, &prepared.cam, x, y).is_some() { None }
                            else { prepared.scene.pick_stable(&prepared.view, &prepared.cam, x, y, self.hover.filter(|_| self.hover_slot.is_none())) });
                    self.set_hover(node, slot);
                }
            }
        }
        if self.moving { self.set_hover(None, None); }
        self.hover_terr = self.pointer.filter(|&(x, y)| self.hover.is_none() && !self.moving && !self.state.find_open && !self.over_chrome(x, y))
            .and_then(|(x, y)| self.painted_territory(&prepared.view, &prepared.cam, x, y).or_else(|| pointer_territory(&prepared.scene, &prepared.view, &prepared.cam, x, y)));
        self.sample_hover(window, cx);
        self.fade_retiring(window, cx);
        prepared.retiring_hovers = self.retiring_packets();
        prepared.hover = self.hover; prepared.hover_a = self.hover_a; prepared.hover_slot = self.hover_slot; prepared.prism_sel = self.state.prism_sel;
        self.flow_a = self.motion.animate_from(FLOW_KEY, 0.0, if self.moving { 1.0 } else { 0.0 }, motion::spec::REVEAL, window, cx);
        // Publish the exact terminal sample before releasing this finite key.
        if !self.moving && self.flow_a == 0.0 { self.motion.replay(FLOW_KEY); }
        prepared.flow_alpha = self.flow_strength();
        prepared.hover_terr = self.hover_terr;
        self.sync_peek(window, cx);
        self.report_peek(window, cx);
        self.anchor_live_peeks(window, cx);
    }

    /// Chrome owning the reading room in this same prepaint transaction.
    fn primary_chrome(&self) -> Option<Bounds<Pixels>> {
        if self.state.find_open { return None; }
        match &self.state.exploration {
            Exploration::Chain(_) => self.chrome_bounds.get("graph-chain-bounds").copied(),
            Exploration::Tour { .. } => self.chrome_bounds.get("graph-tour-bounds").copied(),
            _ => self.state.focus.and(self.card_bounds),
        }
    }

    fn flow_strength(&self) -> f32 {
        let gather = self.prism.as_ref().map_or(0.0, |prism| (prism.target - prism.g).abs());
        let hover = if self.hover.is_some() { 1.0 - self.hover_a } else { 0.0 };
        self.flow_a.max(gather).max(hover).clamp(0.0, 1.0)
    }

    /// Called only by prepaint: input handlers cannot request Motion frames.
    fn sample_hover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for node in std::mem::take(&mut self.terminal_hovers) {
            self.motion.set((HOVER_KEY, node as usize), 0.0);
            self.motion.animate((HOVER_KEY, node as usize), 0.0, motion::spec::HOVER, window, cx);
            self.motion.replay((HOVER_KEY, node as usize));
        }
        self.hover_a = if let Some(node) = self.hover {
            self.hover_sampled = true;
            debug_assert_eq!(self.hover_key, node as usize);
            let alpha = self.motion.animate_from((HOVER_KEY, self.hover_key), 0.0, 1.0, motion::spec::REVEAL, window, cx);
            if alpha > 0.0 && self.prism.is_none() && self.hover_packet.is_none() {
                self.hover_packet = self.scene.as_ref().map(|scene| scene.neighbourhood(node));
            }
            alpha
        } else { 0.0 };
    }

    fn retiring_packets(&self) -> Vec<(Arc<super::scene::Neighbourhood>, f32, f32)> {
        self.retiring_hovers.values().filter_map(|outgoing| outgoing.packet.as_ref().map(|packet| (packet.clone(), outgoing.alpha, outgoing.flow_alpha))).collect()
    }

    fn fade_retiring(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.retiring_hovers.retain(|&node, outgoing| {
            outgoing.alpha = self.motion.animate((HOVER_KEY, node as usize), 0.0, motion::spec::HOVER, window, cx);
            if outgoing.alpha == 0.0 {
                // animate published the true terminal sample before removal.
                self.motion.replay((HOVER_KEY, node as usize));
                false
            } else { true }
        });
    }

    fn prepared_peek(&mut self, node: NodeId) -> super::peek::Prepared {
        if self.prepared_peek.as_ref().is_none_or(|prepared| prepared.node() != node) {
            self.prepared_peek = Some(super::peek::Prepared::new(self.world.clone(), node));
        }
        self.prepared_peek.as_ref().expect("prepared hover content").clone()
    }

    fn peek_actions(view: gpui::WeakEntity<Self>) -> super::peek::ActionHandler {
        Rc::new(move |node, action, window, cx| {
            let Some(view) = view.upgrade() else { return; };
            view.update(cx, |view, cx| view.act_on_peek(node, action, window, cx));
        })
    }

    fn report_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((key, node)) = self.peek.clone() else { return; };
        let Some((anchor, side)) = self.peek_anchor(node) else { return; };
        let prepared = self.prepared_peek(node);
        let view = cx.entity().downgrade();
        let key_copy = key.clone();
        let open_page = self.on_open.is_some();
        float::report(&key, anchor, self.hover == Some(node) && !self.moving,
            move || prepared.request(key_copy, anchor, side, open_page, Self::peek_actions(view)), window, cx);
    }

    /// Releases this region's transient input/float ownership at route
    /// suspension while retaining its reading camera and focused symbol.
    pub fn suspend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.apply(Event::Suspend);
        if self.state.find_open { self.reset_find(window, cx); }
        self._search_task = None; self.searching = false; self.road = None;
        self.pointer = None; self.drag = None; self.set_hover(None, None); self.sync_peek(window, cx);
        for key in self.peek_origins.keys() { float::anchor_gone(key, window, cx); }
        self.peek_origins.clear();
        cx.notify();
    }

    /// The package › module under the camera, and the altitude.
    fn where_line(&self) -> Option<WhereLine> {
        let (scene, view, rig) = (self.scene.as_ref()?, self.view.as_ref()?, self.rig.as_ref()?);
        let altitude = Altitude::of(view.k(&rig.cam));
        #[allow(clippy::cast_possible_truncation)]
        let at = scene.territory_at(rig.cam.x as f32, rig.cam.y as f32);
        let package = at.map(|t| self.world.packages[t.pkg as usize].name.clone());
        let module = at.and_then(|t| t.module).filter(|_| view.k(&rig.cam) > 1.5).map(|m| {
            let path = &self.world.modules[m as usize].path;
            if path.is_empty() { SharedString::new_static("(root)") } else { path.clone() }
        });
        Some(WhereLine { package: package.unwrap_or_default(), module, altitude })
    }
}

/// How far into the world the camera is: the word the where-line ends with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Altitude {
    /// The whole world.
    World,
    /// Packages fill the view.
    Packages,
    /// Modules fill the view.
    Modules,
    /// Symbols fill the view.
    Symbols,
    /// Members fill the view.
    Members,
}

impl Altitude {
    /// The altitude at camera scale `k`.
    fn of(k: f64) -> Self {
        if k < 0.9 {
            Self::World
        } else if k < 3.0 {
            Self::Packages
        } else if k < 12.0 {
            Self::Modules
        } else if k < 40.0 {
            Self::Symbols
        } else {
            Self::Members
        }
    }

    /// The word the where-line shows.
    const fn word(self) -> &'static str {
        match self {
            Self::World => "world",
            Self::Packages => "packages",
            Self::Modules => "modules",
            Self::Symbols => "symbols",
            Self::Members => "members",
        }
    }
}

/// What the camera is looking at, in words: the package and module under
/// its centre, and how far in it is.
struct WhereLine {
    package: SharedString,
    module: Option<SharedString>,
    altitude: Altitude,
}

/// A frame's inputs, owned (the painter must not borrow the view).
struct Prepared {
    scene: Arc<Scene>,
    view: View,
    cam: Camera,
    palette: &'static crate::tokens::Palette,
    text_scale: f32,
    hover: Option<NodeId>,
    hover_a: f32,
    retiring_hovers: Vec<(Arc<super::scene::Neighbourhood>, f32, f32)>,
    hover_terr: Option<Terr>,
    focus: Option<NodeId>,
    reading_a: f32,
    prism: Option<Prism>,
    prism_sel: Option<usize>,
    hover_slot: Option<usize>,
    flow: f32,
    flow_alpha: f32,
    trail: Vec<NodeId>,
    reach: Option<Arc<Reach>>,
    reach_wave: f32,
    tour: Option<TourRoad>,
    search: Option<Rc<Search>>,
    chain: Option<Vec<RoadStop>>,
    chain_progress: super::road::RoadSample,
    strategy: Strategy,
    occupied: Option<Bounds<Pixels>>,
    reserved: Vec<Bounds<Pixels>>,
    frame: Option<PrismFrame>,
}

/// Records the actual card layout before the canvas paints its prism.
/// Returning the child's layout id keeps the absolute plate in the same
/// coordinate space as the canvas, with no guessed height or extra wrapper.
struct MeasuredCard { child: AnyElement, view: Entity<GraphView> }
impl IntoElement for MeasuredCard {
    type Element = Self;
    fn into_element(self) -> Self { self }
}
impl Element for MeasuredCard {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> { None }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }
    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }
    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        self.child.prepaint(window, cx);
        crate::probe::record_bounds(cx, &ElementId::from("graph-focus-card"), bounds);
        self.view.update(cx, |graph, _| {
            graph.chrome_bounds.insert("graph-focus-card", bounds);
            // GraphFrame reconciles this measurement in the same prepaint.
            graph.card_bounds = Some(bounds);
        });
    }
    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        self.child.paint(window, cx);
    }
}

/// Reading scale projected into the actual canvas space left by the card.
/// A territory hit belongs to the current projection, never a cached hover.
fn pointer_territory(scene: &Scene, view: &View, cam: &Camera, x: f32, y: f32) -> Option<Terr> {
    if view.k(cam) >= 6.0 { return None; }
    let (wx, wy) = view.to_world(cam, x, y);
    #[allow(clippy::cast_possible_truncation)]
    scene.territory_at(wx as f32, wy as f32)
}

/// Empty or folded content keeps a readable scale instead of an infinitesimal box.
fn readable_frame(scene: &Scene, view: &View, room: &View, bounds: Box2, source: NodeId, pad: f64) -> Camera {
    let minimum = scene.focus_cam(room, source, 0.0).w;
    let (x, y) = bounds.center();
    #[allow(clippy::cast_possible_truncation)]
    let (half_w, half_h) = (bounds.width().max(minimum as f32) * 0.5, bounds.height().max((minimum * f64::from(room.h / room.w)) as f32) * 0.5);
    view.frame_in(Box2 { x0: x - half_w, y0: y - half_h, x1: x + half_w, y1: y + half_h }, room, pad)
}

fn chain_margin(view: &View) -> f64 { f64::from(crate::tokens::fluid::CHAIN_MARGIN.at(crate::fluid::Room::new(px(view.w), 1.0))) }

/// Reading provenance is constant-time: symbol endpoints, one shared module
/// or package, and an explicit selected relation. Motion classifies the route
/// from these landmarks rather than card-shifted camera centres.
fn reading_route(scene: &Scene, view: &View, source: Option<NodeId>, target: NodeId, follow: bool, occupied: Option<Bounds<Pixels>>, centered: bool) -> FocusRoute {
    let point = |node: NodeId| (f64::from(scene.layout.x[node as usize]), f64::from(scene.layout.y[node as usize]));
    let arrival = point(target);
    let target_node = scene.world.node(target);
    let source = source.filter(|&node| node != target);
    let kind = if follow { FocusKind::Follow } else if source.is_some_and(|node| scene.world.node(node).module == target_node.module) { FocusKind::Handoff } else { FocusKind::Transfer };
    let context = source.map_or_else(|| package_context(scene, view, target), |node| {
        let from = scene.world.node(node);
        if from.module == target_node.module {
            view.frame(scene.layout.modules[target_node.module as usize].bounds, 1.2)
        } else if from.pkg == target_node.pkg {
            package_context(scene, view, target)
        } else {
            let departure = point(node);
            let corridor = Box2 { x0: scene.layout.x[node as usize].min(scene.layout.x[target as usize]), y0: scene.layout.y[node as usize].min(scene.layout.y[target as usize]), x1: scene.layout.x[node as usize].max(scene.layout.x[target as usize]), y1: scene.layout.y[node as usize].max(scene.layout.y[target as usize]) };
            let frame = view.frame(corridor, 1.25);
            Camera::new((departure.0 + arrival.0) * 0.5, (departure.1 + arrival.1) * 0.5, frame.w.max(package_context(scene, view, target).w))
        }
    });
    let room = Scene::free_view(view, occupied);
    let anchor = if centered { (room.x + room.w * 0.5, room.y + room.h * 0.5) } else { Scene::focus_anchor(&room) };
    FocusRoute { departure: source.map(point), arrival, context, room: view.flight_room(&room, anchor), kind }
}

fn package_context(scene: &Scene, view: &View, node: NodeId) -> Camera {
    view.frame(scene.layout.packages[scene.world.node(node).pkg as usize].bounds, 1.2)
}

fn reading_plate_height(view: Option<View>) -> f32 {
    view.map_or(360.0, |v| (v.h * 0.45).min((v.h - 84.0).max(36.0)))
}

fn focus_camera(scene: &Scene, view: &View, i: NodeId, card: Option<Bounds<Pixels>>) -> Camera {
    let base = scene.focus_cam(view, i, 0.0);
    let Some(card) = card else { return scene.focus_cam(view, i, Scene::card_room(view)); };
    let free = Scene::free_view(view, Some(card));
    let (cx, cy) = Scene::focus_anchor(&free);
    let k = f64::from(view.w) / base.w;
    Camera::new(base.x + f64::from(view.x + view.w * 0.5 - cx) / k, base.y + f64::from(view.y + view.h * 0.5 - cy) / k, base.w)
}

/// One measured chrome rectangle, committed with the same frame as map
/// labels. No overlay's height or text-scale geometry is guessed.
struct MeasuredChrome { child: AnyElement, view: Entity<GraphView>, key: &'static str }
impl MeasuredChrome {
    fn new(key: &'static str, child: impl IntoElement, view: Entity<GraphView>) -> Self { Self { child: child.into_any_element(), view, key } }
}
impl IntoElement for MeasuredChrome { type Element = Self; fn into_element(self) -> Self { self } }
impl Element for MeasuredChrome {
    type RequestLayoutState = (); type PrepaintState = bool;
    fn id(&self) -> Option<ElementId> { None }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }
    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) { (self.child.request_layout(window, cx), ()) }
    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> bool {
        if self.key == "graph-where-bounds" {
            let graph = self.view.read(cx);
            if let (Some(view), Some(primary)) = (graph.view, graph.primary_chrome()) {
                let room = Scene::free_view(&view, Some(primary));
                // Bottom reading chrome owns the footer context. Its exact
                // measured strip suppresses the world breadcrumb; a side
                // card keeps it whenever the measured rectangles are clear.
                if (room.y == view.y && room.h < view.h) || primary.intersects(&bounds) { return false; }
            }
        }
        self.child.prepaint(window, cx);
        self.view.update(cx, |view, _| {
            view.chrome_bounds.insert(self.key, bounds);
            if self.key == "graph-find-bounds" { view.find_bounds = Some(bounds); }
        });
        crate::probe::record_bounds(cx, &self.key.into(), bounds);
        true
    }
    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), visible: &mut bool, window: &mut Window, cx: &mut App) {
        if *visible { self.child.paint(window, cx); }
    }
}

type FrameCell = Rc<RefCell<Option<Prepared>>>;
struct GraphFrame { child: AnyElement, view: Entity<GraphView>, draft: FrameCell }
impl IntoElement for GraphFrame { type Element = Self; fn into_element(self) -> Self { self } }
impl Element for GraphFrame {
    type RequestLayoutState = (); type PrepaintState = ();
    fn id(&self) -> Option<ElementId> { None }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }
    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) { (self.child.request_layout(window, cx), ()) }
    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        self.child.prepaint(window, cx);
        if let Some(prepared) = self.draft.borrow_mut().as_mut() { self.view.update(cx, |view, cx| view.finalize(prepared, window, cx)); }
    }
    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) { self.child.paint(window, cx); }
}

/// The painted map.
struct Canvas {
    view: Entity<GraphView>,
    draft: FrameCell,
}

impl IntoElement for Canvas {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Canvas {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

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
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        *self.draft.borrow_mut() = self.view.update(cx, |view, cx| view.prepare(bounds, window, cx));
        hitbox
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _layout: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = cx.palette();
        let Some(p) = self.draft.borrow_mut().take() else {
            window.paint_quad(gpui::fill(bounds, palette.g0));
            return;
        };
        let occupied = p.occupied;
        let frame = p.frame.as_ref();
        let look = Look {
            scene: &p.scene,
            view: p.view,
            cam: p.cam,
            palette: p.palette,
            text_scale: p.text_scale,
            hover: p.hover,
            hover_a: p.hover_a,
            retiring_hovers: &p.retiring_hovers,
            hover_terr: p.hover_terr,
            focus: p.focus,
            reading_a: p.reading_a,
            prism: frame,
            flow: p.flow,
            flow_alpha: p.flow_alpha,
            trail: &p.trail,
            exploration: if let Some(search) = p.search.as_deref() {
                if let Some(chain) = p.chain.as_deref() { draw::Exploration::Chain { stops: chain, progress: p.chain_progress } } else { draw::Exploration::Search(search) }
            } else if let Some(reach) = p.reach.as_deref() { draw::Exploration::Reach { data: reach, wave: p.reach_wave } }
            else if let Some(tour) = p.tour.as_ref() { draw::Exploration::Tour(tour) }
            else if let Some(chain) = p.chain.as_deref() { draw::Exploration::Chain { stops: chain, progress: p.chain_progress } }
            else { draw::Exploration::Free },
            occupied,
            reserved: &p.reserved,
            strategy: p.strategy,
        };
        let stats = window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            let painted = draw::paint_with_regions(&look, window, cx);
            if let Some(f) = frame {
                prism::paint(f, &look, p.prism_sel, p.hover_slot, window, cx);
            }
            painted
        });
        let hovering = p.hover.is_some() || p.hover_slot.is_some() || p.hover_terr.is_some();
        self.view.update(cx, |view, cx| {
            view.stats = stats.stats;
            view.painted_labels = Some((p.view, p.cam, stats.territory_labels));
            // If newly accepted text covers a parked pointer, let the next
            // committed frame retire its expanded glyph hover exactly once.
            if view.hover.is_some() && view.pointer.is_some_and(|(x, y)| view.painted_territory(&p.view, &p.cam, x, y).is_some()) { cx.notify(); }
        });
        if hovering {
            window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        }
        // Pointer input.
        let view = self.view.clone();
        let hit = hitbox.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Capture { return; }
            let dragging = event.pressed_button == Some(MouseButton::Left);
            let (x, y) = (f32::from(event.position.x), f32::from(event.position.y));
            view.update(cx, |v, cx| {
                let owns_drag = dragging && v.drag.is_some();
                let rail = v.frame.as_ref().filter(|frame| frame.rail && v.state.focus == Some(frame.node))
                    .and_then(|frame| frame.pick(x, y).and_then(|slot| frame.slots[slot].key.map(|key| (frame.node, key))));
                if !owns_drag && let Some((owner, key)) = rail {
                    // Native foreground rows share capture picking with the
                    // canvas, before the float collector observes this move.
                    // Jitter therefore retains one uninterrupted peek intent.
                    v.rail_pointer(owner, key, event, window, cx);
                } else if !owns_drag && (!hit.is_hovered(window) || v.over_chrome(x, y)) {
                    v.pointer_left(window, cx);
                } else {
                    v.pointer_move(x, y, dragging, window, cx);
                }
                // Float collection runs after this capture listener: publish
                // the newly picked identity on every move, including jitter.
                v.report_peek(window, cx);
            });
        });
        let view = self.view.clone();
        window.on_mouse_event(move |_: &MouseExitEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |v, cx| v.pointer_left(window, cx));
            }
        });
        let view = self.view.clone();
        let hit = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left || !hit.is_hovered(window) {
                return;
            }
            let (x, y) = (f32::from(event.position.x), f32::from(event.position.y));
            view.update(cx, |v, cx| v.pointer_down(x, y, window, cx));
        });
        let view = self.view.clone();
        let hit = hitbox.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase != DispatchPhase::Capture || event.button != MouseButton::Left { return; }
            let (x, y) = (f32::from(event.position.x), f32::from(event.position.y));
            let moved = view.read(cx).drag.as_ref().map(|drag| drag.moved.max((x - drag.x).hypot(y - drag.y)));
            let Some(moved) = moved else { return };
            let allow_click = hit.is_hovered(window);
            // Clear every owned press before a child can stop bubbling. Actual
            // drags retain capture across foreground cards; controls still
            // receive click-sized releases with canvas ownership gone.
            view.update(cx, |v, cx| v.pointer_up(x, y, event.click_count, allow_click, window, cx));
            if moved > 3.0 { cx.stop_propagation(); }
        });
        let view = self.view.clone();
        let hit = hitbox.clone();
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hit.is_hovered(window) {
                return;
            }
            view.update(cx, |v, cx| {
                if !v.over_chrome(f32::from(event.position.x), f32::from(event.position.y)) { v.wheel(event, window, cx); }
            });
        });
        let view = self.view.clone();
        let hit = hitbox.clone();
        window.on_mouse_event(move |event: &PinchEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hit.is_hovered(window) {
                return;
            }
            view.update(cx, |v, cx| {
                if !v.over_chrome(f32::from(event.position.x), f32::from(event.position.y)) { v.pinch(event, window, cx); }
            });
        });
    }
}

impl Render for GraphView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.palette();
        let draft = Rc::new(RefCell::new(None));
        let view = cx.entity();
        let root = div().id("graph").key_context("Graph").track_focus(&self.focus_handle)
            .relative().size_full().overflow_hidden().bg(palette.g0.hsla())
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.find_key(event, window, cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.key(event, window, cx)))
            .on_modifiers_changed(cx.listener(|_, _, _, cx| cx.notify()))
            .child(Canvas { view: cx.entity(), draft: draft.clone() })
            // GPUI's container query constructs the same detached child once,
            // after Canvas has sampled this frame's actual parent geometry.
            .child(div().id("graph-chrome").absolute().top_0().left_0().size_full()
                .child(gpui::container_query(move |size, window, cx| view.update(cx, |graph, cx| graph.chrome(size, window, cx)))));
        GraphFrame { child: root.into_any_element(), view: cx.entity(), draft }
    }
}

impl GraphView {
    fn chrome(&mut self, size: gpui::Size<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let width = size.width;
        let measure = Measure::new(width, &facet);
        self.chrome_bounds.clear();
        let mut root = div().relative().w(size.width).h(size.height);
        // Find: a quiet field at the top left; results under it.
        let find_w = (f32::from(width) - 32.0).min(300.0);
        let find_measure = Measure::new(px(find_w), &facet);
        let mut find_stack = div().absolute().left(px(16.0)).top(px(17.0)).w(px(find_w)).flex().flex_col().gap(px(8.0))
            .child(MeasuredChrome::new("graph-find-bounds", div().relative().w_full()
                .child(field("graph-find", &self.find, &find_measure).quiet().opaque().icon(Icon::Search))
                .children((!self.state.find_open && window.modifiers().platform).then(|| {
                    div().absolute().right(px(10.0)).top_0().bottom_0().flex().items_center().child(kbd("/", &find_measure))
                })), cx.entity()));
        if self.state.focus.is_none() {
            if let Some(status) = self.tour_status() { find_stack = find_stack.child(MeasuredChrome::new("graph-tour-status-bounds", div().w_full().child(graph_text("graph-tour-status", status, ty::SMALL, &find_measure, palette.ink2, crate::probe::TextOverflow::Wrap)), cx.entity())); }
        }
        root = root.child(find_stack);
        if self.state.find_open {
            root = root.child(MeasuredChrome::new("graph-results-bounds", self.results_list(&measure, window, cx), cx.entity()));
        }
        if self.state.exploration.chain().is_some() { root = root.child(MeasuredChrome::new("graph-chain-bounds", self.chain_plate(&measure, window.modifiers().alt, window, cx), cx.entity())); }
        if self.state.exploration.tour().is_some() { root = root.child(MeasuredChrome::new("graph-tour-bounds", self.tour_plate(&measure, cx), cx.entity())); }
        if let Some(i) = self.state.focus.filter(|_| !self.state.find_open) {
            let card = self.focus_card(i, &measure, window.modifiers().platform, cx);
            root = root.child(MeasuredCard { child: card, view: cx.entity() });
        } else { self.card_bounds = None; }
        if let Some(WhereLine { package: pkg, module, altitude }) = self.where_line() {
            let mut line = div()
                .absolute()
                .left(px(18.0))
                .bottom(px(21.0))
                .flex()
                .items_center()
                .gap(px(10.0))
                .whitespace_nowrap()
                .set(ty::MONO_SMALL, &measure)
                .text_color(palette.ink2.hsla());
            if !pkg.is_empty() {
                line = line.child(div().font_weight(gpui::FontWeight(600.0)).text_color(palette.ink1.hsla()).child(pkg));
            }
            if let Some(m) = module {
                line = line.child(div().text_color(palette.ink3.hsla()).child("›")).child(m);
            }
            line = line.child(div().ml(px(8.0)).child(graph_text("graph-where-level", altitude.word(), ty::STATUS, &measure, palette.ink2, crate::probe::TextOverflow::Clip)));
            if self.state.exploration.tour().is_none() {
                if let Some(package) = self.tour_package().filter(|&package| self.discovery.as_ref().and_then(|discovery| discovery.package_tour(package)).is_some_and(Tour::shown)) {
                    line = line.child(div().id("graph-start-here").cursor_pointer().ml(px(8.0)).flex().items_center().gap(px(5.0))
                        .child(kbd("T", &measure)).child("start here")
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| { window.focus(&this.focus_handle, cx); this.start_tour(package, 0, cx); })));
                }
            }
            root = root.child(MeasuredChrome::new("graph-where-bounds", line, cx.entity()));
        }
        // Late native fallback sees this frame's measured chrome, then feeds
        // the same row geometry to GraphFrame before Canvas paint.
        root.child(div().id("graph-prism-rail-host").absolute().top_0().left_0().size_full()
            .child(rail::Rail { view: cx.entity() })).into_any_element()
    }
}

impl GraphView {
    fn find_hints(&self, measure: &Measure, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.palette();
        let hints = [
            ("Value", "a name"),
            ("path -> maybe text", "a shape: what it takes, what it gives"),
            ("Invocation -> list of text", "what you have → what you need, in steps"),
        ];
        div().flex().flex_col().gap(px(4.0)).children(hints.into_iter().enumerate().map(|(n, (query, note))| {
            div().id(("graph-find-hint", n)).cursor_pointer().px(px(8.0)).py(px(8.0)).flex().flex_col().gap(px(4.0))
                .child(div().set(ty::MONO_SMALL, measure).text_color(palette.ink1.hsla()).child(query))
                .child(div().set(ty::SMALL, measure).text_color(palette.ink4.hsla()).child(note))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| {
                    this.find.update(cx, |input, cx| input.set_value(query, window, cx));
                    this.refresh_search(query, cx);
                }))
        })).into_any_element()
    }

    fn chain_plate(&self, measure: &Measure, xray: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let chain = self.state.exploration.chain().expect("a held chain plate has a chain");
        let palette = cx.palette();
        let from = crate::semantics::recipes::key_words(&self.world, &chain.from);
        let to = crate::semantics::recipes::key_words(&self.world, &chain.output);
        let steps = ["zero", "one", "two", "three", "four"].get(chain.steps.len()).copied().unwrap_or("many");
        let title = format!("{from} to {to}, in {steps} steps");
        let lead = format!("from your {from}{}", chain.via.as_ref().map_or_else(String::new, |via| format!(", a {}", via.trim_start_matches("any "))));
        let mut rail = div().flex().flex_wrap().items_center().gap(px(8.0))
            .child(div().set(ty::SMALL, measure).text_color(palette.ink3.hsla()).child(lead));
        for (n, step) in chain.steps.iter().enumerate() {
            if n > 0 { rail = rail.child(div().text_color(palette.ink4.hsla()).child("→")); }
            let node = step.node;
            rail = rail.child(div().id(("graph-chain-step", n)).cursor_pointer().px(px(8.0)).py(px(8.0))
                .border_b_1().border_color(palette.peri.base.hsla()).set(ty::MONO_ROW, measure).text_color(palette.ink0.hsla())
                .child(step.verb.clone())
                .children(step.fails.then(|| div().set(ty::SMALL, measure).text_color(palette.ink3.hsla()).child("or fails")))
                .children(step.maybe.then(|| div().set(ty::SMALL, measure).text_color(palette.ink3.hsla()).child("maybe")))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| { window.focus(&this.focus_handle, cx); this.set_focus(Some(node), true, cx); })));
            for rider in &step.riders {
                rail = rail.child(div().set(ty::SMALL, measure).text_color(palette.ink4.hsla()).child(format!("+ {rider}")));
            }
        }
        let code = SharedString::from(chain.code.clone());
        let natural = crate::probe::natural_width(&code, measure.role(ty::MONO_SMALL), 1.0, window);
        let viewport = (f32::from(measure.width()) - 68.0).min(724.0).max(1.0);
        let code_body = div().id("graph-chain-code-horizontal").w(px(viewport)).overflow_x_scroll().track_scroll(&self.code_scroll)
            .child(div().w(natural.max(px(viewport))).child(graph_text("graph-chain-code", code, ty::MONO_SMALL, measure, palette.ink2, crate::probe::TextOverflow::Clip)));
        let body = div().flex().flex_col().gap(px(12.0))
            .child(graph_text("graph-chain-title", title, ty::TITLE, measure, palette.ink1, crate::probe::TextOverflow::Wrap))
            .children((!xray).then_some(rail))
            .children(xray.then_some(code_body))
            .child(graph_text("graph-chain-controls", "your value is the spine; the rest rides along · ⌥ for code · esc to let go", ty::SMALL, measure, palette.ink4, crate::probe::TextOverflow::Wrap));
        cut().chamfer(Chamfer::Md).bevel(Bevel::Rest).plate(Plate::Flat)
            .fill(tone(palette.g1, 0.94)).absolute().left(px(16.0)).right(px(16.0)).bottom(px(50.0))
            .max_w(px(760.0)).max_h(px(reading_plate_height(self.view))).px(px(18.0)).py(px(16.0))
            .child(div().id("graph-chain-scroll").track_scroll(&self.chain_scroll).max_h(px((reading_plate_height(self.view) - 32.0).max(4.0))).overflow_y_scroll().child(crate::probe::measure("graph-chain-body", body)))
            .id("graph-chain").into_any_element()
    }

    fn tour_plate(&self, measure: &Measure, cx: &mut Context<Self>) -> AnyElement {
        let (tour, at) = self.state.exploration.tour().expect("a tour plate has a tour");
        let palette = cx.palette();
        let stop = tour.stops[at];
        let node = self.world.node(stop.node);
        let count = ["zero", "one", "two", "three", "four", "five", "six"][tour.stops.len()];
        let title = format!("{} in {count} stops · {} of {}", self.world.packages[tour.package as usize].name, at + 1, tour.stops.len());
        let mut strip = div().flex().flex_wrap().gap(px(8.0));
        for (n, stop) in tour.stops.iter().enumerate() {
            strip = strip.child(div().id(("graph-tour-stop", n)).cursor_pointer().px(px(6.0)).py(px(6.0))
                .flex().items_center().gap(px(6.0)).when_selected(n == at, palette)
                .child(icons::kind_mark(crate::anatomy::icon_kind(self.world.node(stop.node).kind), KindSize::Sm, palette))
                .child(div().set(ty::MONO_SMALL, measure).text_color(if n == at { palette.ink0 } else { palette.ink3 }.hsla()).child(self.world.name_of(stop.node)))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.go_stop(n, cx))));
        }
        let mut body = div().flex().flex_col().gap(px(10.0))
            .child(graph_text("graph-tour-title", title, ty::TITLE, measure, palette.ink1, crate::probe::TextOverflow::Wrap))
            .child(strip)
            .child(div().flex().flex_wrap().items_baseline().gap(px(8.0))
                .child(div().set(ty::MARGIN, measure).text_color(palette.ink3.hsla()).child(stop.role.text()))
                .child(div().set(ty::MONO_ROW, measure).text_color(palette.ink0.hsla()).child(self.world.name_of(stop.node)))
                .child(div().set(ty::SMALL, measure).text_color(palette.ink3.hsla()).child(stop.why.text(&self.world))));
        if let Some(doc) = &node.doc {
            body = body.child(div().set(ty::MARGIN, measure).text_color(palette.ink2.hsla()).child(tour::plain(doc)));
        }
        body = body.child(graph_text("graph-tour-controls", "→ next · ← back · ↵ open its page · esc end", ty::SMALL, measure, palette.ink4, crate::probe::TextOverflow::Wrap));
        cut().chamfer(Chamfer::Md).bevel(Bevel::Rest).plate(Plate::Flat)
            .fill(tone(palette.g1, 0.94)).absolute().left(px(16.0)).right(px(16.0)).bottom(px(50.0))
            .max_w(px(760.0)).max_h(px(reading_plate_height(self.view))).px(px(18.0)).py(px(16.0))
            .child(div().id("graph-tour-scroll").track_scroll(&self.tour_scroll).max_h(px((reading_plate_height(self.view) - 32.0).max(4.0))).overflow_y_scroll().child(crate::probe::measure("graph-tour-body", body)))
            .id("graph-tour").into_any_element()
    }

    fn results_list(&self, measure: &Measure, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.palette();
        let q = self.find.read(cx).value().to_lowercase();
        let q = q.rsplit("::").next().unwrap_or("").trim().to_owned();
        let rows = self.results.iter().enumerate().map(|(n, &i)| {
            let node = self.world.node(i);
            let name = SharedString::from(self.search.rows[n].label.clone());
            let at = super::highlight::match_range(&name, &q);
            let mut runs = Vec::new();
            let base = TextRun {
                len: 0,
                font: crate::data::text::font(measure.role(ty::MONO_ROW)),
                color: palette.ink0.hsla(),
                background_color: None,
                underline: None,
                strikethrough: None,
                letter_spacing: None,
            };
            match at {
                Some(range) if !q.is_empty() => {
                    let (a, b) = (range.start, range.end);
                    if a > 0 {
                        runs.push(TextRun { len: a, ..base.clone() });
                    }
                    runs.push(TextRun {
                        len: b - a,
                        underline: Some(UnderlineStyle { thickness: px(1.5), color: Some(palette.peri.base.hsla()), wavy: false }),
                        ..base.clone()
                    });
                    if b < name.len() {
                        runs.push(TextRun { len: name.len() - b, ..base.clone() });
                    }
                }
                _ => runs.push(TextRun { len: name.len(), ..base.clone() }),
            }
            let selected = n == self.state.result_sel;
            div()
                .id(("graph-result", i as usize))
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(8.0))
                .py(px(7.0))
                .when_selected(selected, palette)
                .child(icons::kind_mark(crate::anatomy::icon_kind(node.kind), KindSize::Sm, palette))
                .child(div().flex().flex_col().min_w(px(0.0))
                    .child(div().set(ty::MONO_ROW, measure).child(StyledText::new(name).with_runs(runs)))
                    .children((!self.search.rows[n].shape.is_empty()).then(|| div().set(ty::SMALL, measure).text_color(palette.ink3.hsla()).child(self.search.rows[n].shape.clone()))))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_right()
                        .set(ty::MONO_SMALL, measure)
                        .text_color(palette.ink4.hsla())
                        .child(self.world.qual(i)),
                )
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    this.close_find(window, cx);
                    this.set_focus(Some(i), true, cx);
                }))
        });
        let top = self.find_bounds.map_or(54.0, |b| f32::from(b.bottom()) - self.view.map_or(0.0, |v| v.y) + 8.0);
        let available = self.view.map_or(480.0, |v| (v.h - top - 18.0).max(24.0));
        let content = div().id("graph-results-scroll")
            .max_h(px((available - 12.0).max(4.0))).overflow_y_scroll().track_scroll(&self.results_scroll)
            .children(rows)
            .children(self.find.read(cx).value().trim().is_empty().then(|| self.find_hints(measure, cx)))
            .children((!self.find.read(cx).value().trim().is_empty() && self.results.is_empty() && self.search.chains.is_empty()).then(|| div().px(px(8.0)).py(px(10.0)).set(ty::SMALL, measure).text_color(palette.ink3.hsla()).child(if let Some(issue) = self.search.issue { issue } else if self.discovery.is_none() || self.searching { "Finding…" } else if self.search.shaped { "nothing in this world has that shape" } else { "no symbols match" })))
            .children((!self.search.chains.is_empty()).then(|| div().px(px(8.0)).pt(px(8.0)).set(ty::SMALL, measure).text_color(palette.ink4.hsla()).child(if self.results.is_empty() { "no one call does it; in steps" } else { "or, in steps" })))
            .children(self.search.chains.iter().enumerate().map(|(n, chain)| {
                let selected = self.state.result_sel == self.results.len() + n;
                div().id(("graph-chain-result", n)).px(px(8.0)).py(px(8.0)).cursor_pointer().when_selected(selected, palette)
                    .flex().items_start().gap(px(8.0)).set(ty::MONO_SMALL, measure).text_color(palette.ink1.hsla())
                    .child(graph_text(format!("graph-chain-calls-{n}"), "◆".repeat(chain.steps.len()), ty::SMALL, measure, palette.peri.base, crate::probe::TextOverflow::Wrap))
                    .child(div().min_w(px(0.0)).child(chain.brief.clone()))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| { this.state.result_sel = this.results.len() + n; this.choose_result(window, cx); }))
            }))
            .children((!self.search.lit.is_empty()).then(|| div().px(px(8.0)).pt(px(8.0)).pb(px(4.0)).set(ty::SMALL, measure).text_color(palette.ink4.hsla()).child(format!("{} lit in the graph, across {} package{}", self.search.lit.len(), self.search.packages, if self.search.packages == 1 { "" } else { "s" }))));
        cut().chamfer(Chamfer::Sm).plate(Plate::Flat).fill(tone(palette.glass, 0.94))
            .absolute().left(px(16.0)).top(px(top)).w(px((f32::from(measure.width()) - 32.0).min(420.0)))
            .max_h(px(available)).p(px(6.0)).child(content).into_any_element()
    }

    fn focus_card(&self, i: NodeId, measure: &Measure, show_keys: bool, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.palette();
        let facet = cx.facet();
        let world = &self.world;
        let node = world.node(i);
        let prepared = self.discovery.as_ref().and_then(|discovery| discovery.focus_facts(i));
        let mut facts: Vec<String> = Vec::with_capacity(4);
        if let Some(prepared) = prepared {
            for (n, word) in prepared.counts.into_iter().zip(["variant", "field", "method"]) {
                if n > 0 { facts.push(format!("{n} {word}{}", if n == 1 { "" } else { "s" })); }
            }
            facts.push(format!("used in {} place{}", prepared.used, if prepared.used == 1 { "" } else { "s" }));
        } else { facts.push("Preparing symbol details…".into()); }
        let yours = prepared.map_or(0, |facts| facts.yours);
        let caps = prepared.map_or_else(Vec::new, |facts| facts.caps.clone());
        let tour_eligible = self.discovery.as_ref().and_then(|discovery| discovery.package_tour(node.pkg)).is_some_and(Tour::shown);
        let card_w = (f32::from(measure.width()) - 32.0).min(340.0);
        let card = Measure::new(px(card_w), &facet);
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .min_w(px(0.0))
                    .child(icons::kind_mark(crate::anatomy::icon_kind(node.kind), KindSize::Md, palette))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w(px(0.0))
                            .child(graph_text("graph-focus-title", node.name.clone(), ty::TITLE, &card, palette.ink0, crate::probe::TextOverflow::Ellipsis))
                            .child(graph_text("graph-focus-qual", world.qual(i), ty::MONO_SMALL, &card, palette.ink2, crate::probe::TextOverflow::Wrap)),
                    ),
            );
        if let Some(status) = self.tour_status() { body = body.child(graph_text("graph-tour-status", status, ty::SMALL, &card, palette.ink2, crate::probe::TextOverflow::Wrap)); }
        if let Some(doc) = &node.doc {
            body = body.child(graph_text("graph-focus-doc", doc.clone(), ty::MARGIN, &card, palette.ink1, crate::probe::TextOverflow::Wrap));
        }
        if !caps.is_empty() {
            body = body.child(crate::anatomy::can(("graph-can", i as usize), caps, &card).bare());
        }
        let mut fx = div().flex().flex_wrap().gap_x(px(8.0)).gap_y(px(4.0)).set(ty::SMALL, &card).text_color(palette.ink2.hsla());
        for (n, f) in facts.into_iter().enumerate() {
            if n > 0 {
                fx = fx.child(div().text_color(palette.ink4.hsla()).child("·"));
            }
            fx = fx.child(div().whitespace_nowrap().child(graph_text(format!("graph-focus-count-{n}"), f, ty::SMALL, &card, palette.ink2, crate::probe::TextOverflow::Wrap)));
        }
        if yours > 0 {
            fx = fx
                .child(div().text_color(palette.ink4.hsla()).child("·"))
                .child(div().whitespace_nowrap().child(graph_text("graph-focus-yours", format!("{yours} in your code"), ty::SMALL, &card, palette.ink2, crate::probe::TextOverflow::Wrap)));
        }
        body = body.child(fx);
        if let Some(reach) = self.state.exploration.reach() {
            body = body.child(graph_text("graph-reach-summary", reach.summary(), ty::SMALL, &card, palette.ink2, crate::probe::TextOverflow::Wrap));
            if reach.packages.len() > 1 {
                let places = reach.packages.iter().take(3).map(|&(p, n)| format!("{} {n}", world.package_short(p))).collect::<Vec<_>>().join(" · ");
                body = body.child(div().set(ty::SMALL, &card).text_color(palette.ink2.hsla()).child(format!("most in {places}")));
            }
        }
        // The prototype's card names its keys at rest.
        let key = |cap: &'static str, what: &'static str| {
            div().flex().items_center().gap(px(5.0)).child(kbd(cap, &card)).child(what)
        };
        let hint = div()
                .flex()
                .flex_wrap()
                .gap(px(14.0))
                .pt(px(2.0))
                .set(ty::STATUS, &card)
                .text_color(palette.ink2.hsla())
                .child(key("↵", "open page"))
                .child(key("R", "reach"))
                .children(tour_eligible.then(|| key("T", "start here")))
                .child(key("esc", "back out"));
        // The card is a card beside the map or a sheet under it, held through
        // its hysteresis band so a window on the edge does not flip it.
        let below = self.modes.settle(&crate::tokens::fluid::CARD, measure.fluid_room()).mode == crate::tokens::fluid::Card::Below;
        let content_w = if below { f32::from(measure.width()) - 24.0 - 36.0 } else { card_w - 36.0 };
        let content_w = content_w.max(1.0);
        let body = hints::StableHints::new(body.w(px(content_w)).flex_shrink_0(), hint.w(px(content_w)).flex_shrink_0(), show_keys, content_w, (reading_plate_height(self.view) - 32.0).max(4.0), self.focus_scroll.clone(), self.hint_metrics.clone());
        cut()
            .chamfer(Chamfer::Md)
            .bevel(Bevel::Rest)
            .plate(Plate::Flat)
            .fill(tone(palette.g1, 0.84))
            .absolute()
            .map(|card| {
                // Narrow: the card moves under the map, full width.
                if below {
                    card.left(px(12.0)).right(px(12.0)).bottom(px(40.0))
                } else {
                    card.right(px(16.0)).top(px(14.0)).w(px(card_w))
                }
            })
            .px(px(18.0))
            .py(px(16.0))
            .max_h(px(reading_plate_height(self.view)))
            .child(body)
            .id("graph-focus-card")
            .into_any_element()
    }
}

fn empty_search() -> Rc<Search> { Rc::new(Search { shaped: false, issue: None, rows: Vec::new(), lit: Vec::new(), lit_set: Default::default(), packages: 0, chains: Vec::new() }) }

fn graph_text(key: impl Into<ElementId>, content: impl Into<SharedString>, role: crate::tokens::TypeRole, measure: &Measure, tone: crate::tokens::Tone, overflow: crate::probe::TextOverflow) -> AnyElement {
    let content = content.into();
    let child = div().set(role, measure).text_color(tone.hsla()).map(|div| {
        if overflow == crate::probe::TextOverflow::Ellipsis { div.overflow_hidden().text_ellipsis().whitespace_nowrap() } else { div }
    }).child(content.clone());
    crate::probe::text(key, content, measure.role(role), 1.0, overflow, child).into_any_element()
}

trait Selected {
    fn when_selected(self, on: bool, palette: &crate::tokens::Palette) -> Self;
}

impl<E: Styled> Selected for E {
    fn when_selected(self, on: bool, palette: &crate::tokens::Palette) -> Self {
        if on {
            self.bg(palette.plate2.hsla()).border_l_2().border_color(palette.peri.base.hsla())
        } else {
            self
        }
    }
}


/// Frames a symbol's neighbourhood widened (Esc backs out to it).
#[must_use]
pub fn backed_out(scene: &Scene, view: &View, i: NodeId) -> Camera {
    let to = scene.frame_of(view, i);
    Camera::new(to.x, to.y, (to.w * 6.0).min(view.fit_w(scene.layout.bounds, 1.06)))
}


#[cfg(test)]
mod tests {
    use super::{Camera, GraphView, Start};
    use crate::graph::camera::View;
    use crate::graph::layout::Layout;
    use crate::graph::layout::tests_support::synthetic;
    use crate::graph::prism::Prism;
    use crate::graph::scene::Scene;
    use crate::theme::{Facet, set_facet};
    use gpui::{Focusable, Modifiers, TestAppContext, VisualTestContext, point, px};
    use std::sync::Arc;
    use std::time::Duration;

    fn frame(cx: &mut VisualTestContext) { frame_after(cx, Duration::from_millis(16)); }

    fn frame_after(cx: &mut VisualTestContext, elapsed: Duration) {
        cx.executor().advance_clock(elapsed);
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }

    fn frames(cx: &mut VisualTestContext, n: usize) {
        for _ in 0..n {
            frame(cx);
        }
    }

    #[cfg(feature = "gallery")] // uses the gallery aligner and gui-harness types
    #[gpui::test]
    fn keyboard_hover_reentry_keeps_the_actual_fractional_envelope(cx: &mut TestAppContext) {
        use crate::gallery::align::{self, Check, Tolerance};
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); crate::probe::enable(cx); });
        for cadence in [4_u64, 16] {
            let base = crate::graph::model::tests::tiny();
            let mut edges = base.edges.clone();
            // Outgoing USES is not a callable prism row. Pin a real call
            // alongside the return type so Down selects a different action.
            edges.push(crate::graph::model::Edge { from: 3, to: 4, rel: crate::graph::model::Rel::CALLS });
            let world = Arc::new(crate::graph::model::World::new(base.packages.clone(), base.modules.clone(), base.nodes.clone(), edges).expect("valid pinned callable with return and call rows"));
            let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
            let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::Focus(3), window, cx));
            frames(cx, 100);
            cx.update(|window, cx| { window.focus(&view.focus_handle(cx), cx); crate::probe::take(cx); });
            cx.simulate_keystrokes("right");
            let a = view.read_with(cx, |v, _| v.state.selected.expect("first native row selection").node);
            let mut seen = Vec::new();
            for n in 0..48 / cadence { seen.push(hover_observed(cx, Duration::from_millis(cadence), usize::from(n == 0))); }
            cx.simulate_keystrokes("down");
            let b = view.read_with(cx, |v, _| v.state.selected.expect("second native row selection").node);
            assert_eq!((a, b), (5, 4), "native Right/Down must choose the pinned return and call actions");
            assert_ne!(a, b, "the pinned outgoing rows must exercise a genuine handoff");
            for n in 0..32 / cadence { seen.push(hover_observed(cx, Duration::from_millis(cadence), usize::from(n == 0))); }
            let strength = view.read_with(cx, |v, _| v.retiring_hovers.get(&a).expect("first row is still visibly fading").alpha);
            assert!(strength > 0.0 && strength < 1.0, "native reentry must reclaim a fractional painted envelope");
            cx.simulate_keystrokes("up");
            view.read_with(cx, |v, _| {
                assert_eq!(v.state.selected.map(|key| key.node), Some(a));
                assert_eq!(v.hover, Some(a));
                assert_eq!(v.hover_a, strength, "keyboard selection must not hard-set a returning symbol to full opacity");
            });
            for n in 0..200_u64.div_ceil(cadence) { seen.push(hover_observed(cx, Duration::from_millis(cadence), usize::from(n == 0))); }
            let report = align::analyze(&seen, Tolerance::default());
            assert!(report.stats.get(&Check::Continuity).is_some_and(|stat| stat.evaluated > 10));
            assert!(report.of(Check::Continuity).next().is_none(), "{cadence}ms keyboard envelope continuity: {:?}", report.findings);
            cx.simulate_keystrokes("escape");
            frames(cx, 100);
            assert!(view.read_with(cx, |v, _| v.hover.is_none() && v.retiring_hovers.is_empty()));
            assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
        }
    }

    #[cfg(feature = "gallery")] // uses the gallery aligner and gui-harness types
    #[gpui::test]
    fn latest_focus_waits_for_true_prism_collapse_without_reusing_outgoing_hits(cx: &mut TestAppContext) {
        use crate::gallery::align::{self, Check, Tolerance};
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); crate::probe::enable(cx); });
        for cadence in [4_u64, 16] {
            let world = Arc::new(crate::graph::model::tests::tiny());
            let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
            let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::Focus(3), window, cx));
            frames(cx, 100);
            cx.update(|window, cx| { window.focus(&view.focus_handle(cx), cx); crate::probe::take(cx); });
            cx.simulate_keystrokes("right");
            frames(cx, 12);
            let first = view.read_with(cx, |v, _| v.state.selected.expect("native selected relation").node);
            cx.simulate_keystrokes("enter");
            assert_eq!(view.read_with(cx, |v, _| v.state.focus), Some(first));
            let latest = if first == 5 { 4 } else { 5 };
            let mut seen = Vec::new();
            let mut overlapping = false;
            let mut old_terminal = false;
            let mut saw_latest = false;
            for n in 0..1800_u64.div_ceil(cadence) {
                if n == 32 / cadence { view.update(cx, |v, cx| v.set_focus(Some(latest), true, cx)); }
                frame_after(cx, Duration::from_millis(cadence));
                let (at_ms, mut ledger) = cx.update(|_, cx| {
                    let at_ms = u64::try_from(crate::motion::now(cx).saturating_duration_since(crate::motion::epoch(cx)).as_millis()).expect("test clock fits u64");
                    (at_ms, crate::probe::take(cx))
                });
                ledger.tracks.retain(|track| track.key == super::PRISM_KEY);
                old_terminal |= ledger.tracks.iter().any(|track| track.target == 0.0 && track.value == 0.0 && !track.live);
                ledger.bounds.clear(); ledger.texts.clear();
                seen.push(align::Observed { drawn: backend_gui_harness::Drawn { at_ms, invalidations: 1, callbacks: 0, cpu: Duration::ZERO, input_cpu: Duration::ZERO, input_events: usize::from(n == 0 || n == 32 / cadence), input_max: Duration::ZERO, viewport: backend_gui_harness::Viewport { width: 1024, height: 768, scale: 1 }, captured: false }, ledger, events: usize::from(n == 0 || n == 32 / cadence), state: None });
                view.read_with(cx, |v, _| {
                    if let Some(prism) = &v.prism {
                        if prism.node == 3 && prism.g > 0.0 {
                            overlapping |= !v.rig.as_ref().expect("camera").flying();
                            assert_eq!(prism.target, 0.0);
                            if let Some(frame) = &v.frame {
                                for slot in &frame.slots {
                                    if let Some([x0,y0,x1,y1]) = slot.label { assert_eq!(v.pick_prism((x0+x1)*0.5, (y0+y1)*0.5), None, "retained outgoing rows never own the new focus's input"); }
                                }
                            }
                        } else if prism.node == latest {
                            assert!(old_terminal, "the latest prism cannot replace a positive outgoing gather");
                            saw_latest = true;
                        }
                    }
                });
            }
            assert!(overlapping, "the camera must land while the old owner's finite collapse is still visible");
            assert!(old_terminal && saw_latest);
            assert_eq!(view.read_with(cx, |v, _| v.prism.as_ref().map(|prism| (prism.node, prism.g, prism.target))), Some((latest, 1.0, 1.0)));
            let report = align::analyze(&seen, Tolerance::default());
            assert!(report.stats.get(&Check::Continuity).is_some_and(|stat| stat.evaluated > 50));
            assert!(report.of(Check::Continuity).next().is_none(), "{cadence}ms owner-changing prism continuity: {:?}", report.findings);
            assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
        }
    }

    #[gpui::test]
    fn reduced_static_first_prepaint_has_no_redundant_measurement_frame(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); crate::probe::enable(cx); });
        for (width, text_scale) in [(1024.0, 1.0), (480.0, 2.0), (640.0, 2.0), (760.0, 2.0)] {
            cx.update(|cx| set_facet(Facet { reduced_motion: true, appearance: crate::tokens::Appearance::Glacier, text_scale, ..Facet::default() }, cx));
            for start in [Start::World, Start::Focus(3)] {
                let world = Arc::new(crate::graph::model::tests::tiny());
                let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
                let baseline = cx.update(|cx| { crate::probe::take(cx); crate::motion::frames_requested(cx) });
                let (view, cx) = cx.add_window_view(|window, cx| {
                    // Isolate geometry from asynchronous index delivery. The
                    // opening resize still exercises the actual native dirty
                    // draw after the first window-sized chrome measurement.
                    let mut graph = GraphView::empty(world.clone(), start, window, cx);
                    graph.scene = Some(scene.clone());
                    graph
                });
                cx.update(|window, _| window.set_scale_factor(2.0));
                cx.simulate_resize(gpui::size(px(width), px(824.0)));
                cx.run_until_parked();
                cx.update(|window, cx| window.draw(cx).clear(cx));
                let actual = view.read_with(cx, |v, _| {
                    let region = v.view.expect("actual initial viewport");
                    assert_eq!((region.w, region.h), (width, 824.0));
                    assert!(v.find_bounds.is_some());
                    if let Start::Focus(node) = start {
                        assert_eq!(v.prism.as_ref().map(|prism| (prism.node, prism.g)), Some((node, 1.0)));
                        let card = v.card_bounds.expect("current measured card");
                        let glyph = v.node_bounds(node).expect("visible actual focused glyph");
                        assert!(!glyph.intersects(&card), "{width}px/text{text_scale}: the first pose must clear the measured card");
                        assert_eq!(v.camera(), Some(super::focus_camera(&scene, &region, node, Some(card))));
                    }
                    assert!(!v.moving && !v.rig.as_ref().expect("camera").flying());
                    v.camera().expect("actual final pose")
                });
                let ledger = cx.update(|_, cx| crate::probe::take(cx));
                for (key, coordinate) in [("graph-camera.x", actual.x), ("graph-camera.y", actual.y), ("graph-camera.w", actual.w)] {
                    let track = ledger.tracks.iter().rev().find(|track| track.key == key).expect("first draw publishes its actual camera");
                    assert!(!track.live);
                    assert!((f64::from(track.value) - coordinate).abs() <= coordinate.abs().max(1.0) * 1e-6, "{width}px/text{text_scale}: final camera ledger matches rendered geometry");
                }
                assert_eq!(cx.update(|_, cx| crate::motion::frames_requested(cx)), baseline, "{width}px/text{text_scale}: opening measurements require no motion lease");
                assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0, "static reduced first draw leaves no opening callback");
                // Repeating an identical opening paint cannot rediscover a
                // correction, change pose, or schedule an idle callback.
                cx.update(|window, cx| { window.refresh(); window.draw(cx).clear(cx); });
                assert_eq!(view.read_with(cx, |v, _| v.camera()), Some(actual));
                assert_eq!(cx.update(|_, cx| crate::motion::frames_requested(cx)), baseline);
                assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
            }
        }
    }

    #[gpui::test]
    fn coalesced_native_release_cannot_fling_the_map_into_empty_space(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); });
        for (span, deliver_move) in [(0, false), (1, false), (0, true), (1, true)] {
            let world = Arc::new(crate::graph::model::tests::tiny());
            let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
            let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::World, window, cx));
            frames(cx, 30);
            let region = view.read_with(cx, |v, _| v.view.expect("viewport"));
            let start = point(px(region.x + region.w * 0.6), px(region.y + region.h * 0.75));
            cx.simulate_mouse_move(start, None, Modifiers::none());
            cx.simulate_mouse_down(start, gpui::MouseButton::Left, Modifiers::none());
            let before = view.read_with(cx, |v, _| v.camera().expect("pressed camera"));
            let k = region.k(&before);
            let held = Camera::new(before.x - 250.0 / k, before.y + 50.0 / k, before.w);
            cx.executor().advance_clock(Duration::from_millis(span));
            let end = point(start.x + px(250.0), start.y - px(50.0));
            if deliver_move { cx.simulate_mouse_move(end, Some(gpui::MouseButton::Left), Modifiers::none()); }
            cx.simulate_mouse_up(end, gpui::MouseButton::Left, Modifiers::none());
            assert_eq!(view.read_with(cx, |v, _| v.camera()), Some(held), "mouse-up completes direct movement even without a delivered move");
            frames(cx, 160);
            assert_eq!(view.read_with(cx, |v, _| v.camera()), Some(held), "{span}ms coalesced deliveries (move={deliver_move}) cannot estimate a giant physical velocity");
            assert!(view.read_with(cx, |v, _| v.drag.is_none()));
            assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
        }
    }

    #[gpui::test]
    fn owned_canvas_drag_crosses_card_bounds_without_pausing_or_jumping(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::Focus(5), window, cx));
        frames(cx, 30);
        let (camera, region, card) = view.read_with(cx, |v, _| (v.camera().expect("camera"), v.view.expect("viewport"), v.card_bounds.expect("card")));
        let start = point(card.left() - px(40.0), card.top() + card.size.height * 0.5);
        cx.simulate_mouse_move(start, None, Modifiers::none());
        cx.simulate_mouse_down(start, gpui::MouseButton::Left, Modifiers::none());
        assert!(view.read_with(cx, |v, _| v.drag.is_some()), "the canvas must own this initial press");
        for dx in [43.0, 80.0, 120.0] {
            let at = point(start.x + px(dx), start.y);
            cx.simulate_mouse_move(at, Some(gpui::MouseButton::Left), Modifiers::none());
            let expected = camera.x - f64::from(dx) / region.k(&camera);
            assert!((view.read_with(cx, |v, _| v.camera().expect("drag camera").x) - expected).abs() < 1e-9, "an owned drag follows the exact displacement across foreground geometry");
        }
        cx.simulate_mouse_up(point(start.x + px(120.0), start.y), gpui::MouseButton::Left, Modifiers::none());
        assert!(view.read_with(cx, |v, _| v.drag.is_none()));
    }

    #[gpui::test]
    fn brief_hover_handoff_preserves_the_stronger_departing_envelope(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); });
        let mut preserved_old = false;
        let mut replaced_with_new = false;
        let mut observed = Vec::new();
        // Observe strengths at real 8ms and 32ms native frame cadences;
        // handoffs must preserve the maximum visible envelope regardless
        // of which packet dominates the actual delivery schedule.
        for brief_ms in [8, 32] {
            let world = Arc::new(crate::graph::model::tests::tiny());
            let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
            let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::World, window, cx));
            frames(cx, 30);
            let positions = [0, 3, 5].map(|node| view.read_with(cx, |v, _| v.screen_position(node).expect("visible native target")));
            let at = |n: usize| point(px(positions[n].0), px(positions[n].1));
            cx.simulate_mouse_move(at(0), None, Modifiers::none());
            frames(cx, 20);
            assert_eq!(view.read_with(cx, |v, _| v.hover), Some(0), "actual native pointer first lights A");
            assert_eq!(view.read_with(cx, |v, _| v.hover_a), 1.0);
            cx.simulate_mouse_move(at(1), None, Modifiers::none());
            frame_after(cx, Duration::from_millis(brief_ms));
            let (old, current) = view.read_with(cx, |v, _| {
                assert_eq!(v.hover, Some(3), "the brief native pointer actually entered B");
                let outgoing = v.retiring_hovers.get(&0).expect("A is still fading");
                assert_eq!(outgoing.packet.as_ref().expect("painted packet").node, 0);
                ((outgoing.packet.as_ref().expect("painted packet").node, outgoing.packet.as_ref().expect("painted identity").node as usize, outgoing.alpha, outgoing.flow_alpha),
                 (3, v.hover_key, v.hover_a, v.flow_a.max(1.0 - v.hover_a)))
            });
            observed.push((brief_ms, old.2, current.2));
            let expected = if old.2 > current.2 {
                preserved_old = true; old
            } else {
                replaced_with_new = true; current
            };
            cx.simulate_mouse_move(at(2), None, Modifiers::none());
            view.read_with(cx, |v, _| {
                assert_eq!(v.hover, Some(5), "C owns the actual native pick immediately");
                assert_eq!(v.retiring_hovers.len(), 2, "both positive departures remain visible");
                for prior in [old, current] {
                    let outgoing = v.retiring_hovers.get(&prior.0).expect("each positive envelope retires");
                    assert_eq!((outgoing.packet.as_ref().expect("painted packet").node, outgoing.packet.as_ref().expect("painted identity").node as usize), (prior.0, prior.1));
                    assert_eq!(outgoing.alpha, prior.2, "handoff preserves each independently observed envelope");
                    assert_eq!(outgoing.flow_alpha, prior.3, "new activity cannot revive outgoing flow");
                }
            });
            frames(cx, 2);
            assert!(view.read_with(cx, |v, _| v.retiring_hovers.get(&expected.0).is_none_or(|outgoing| outgoing.alpha < expected.2)), "the selected packet continues its bounded fade");
            frames(cx, 60);
            assert!(view.read_with(cx, |v, _| v.retiring_hovers.is_empty() && v.motion.len() <= 2));
            assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
        }
        assert!(preserved_old && replaced_with_new, "real native timings must exercise both dominant-packet decisions: {observed:?}");
    }

    #[gpui::test]
    fn same_symbol_reentry_reverses_one_visible_envelope_without_discarding_it(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::World, window, cx));
        frames(cx, 30);
        let (x, y) = view.read_with(cx, |v, _| v.screen_position(0).expect("visible native target"));
        cx.simulate_mouse_move(point(px(x), px(y)), None, Modifiers::none());
        frame(cx);
        let epoch = view.read_with(cx, |v, _| v.hover_key);
        for _ in 0..16 {
            cx.simulate_mouse_move(point(px(-100.0), px(-100.0)), None, Modifiers::none());
            frame(cx);
            let fading = view.read_with(cx, |v, _| {
                assert_eq!(v.hover, None);
                let outgoing = v.retiring_hovers.get(&0).expect("packet still visibly fading at 16ms");
                assert_eq!(outgoing.packet.as_ref().expect("painted identity").node as usize, epoch);
                outgoing.alpha
            });
            assert!(fading > 0.0, "positive fade is the reacquisition precondition");
            cx.simulate_mouse_move(point(px(x), px(y)), None, Modifiers::none());
            view.read_with(cx, |v, _| {
                assert_eq!(v.hover, Some(0));
                assert_eq!(v.hover_key, epoch, "reentry owns the same envelope, not a replacement epoch");
                assert_eq!(v.hover_a, fading, "same-clock retarget preserves the actual visible strength");
                assert!(v.retiring_hovers.is_empty(), "the same symbol is not painted twice");
            });
            frame(cx);
            assert!(view.read_with(cx, |v, _| v.motion.len() <= 2));
        }
        cx.simulate_mouse_move(point(px(-100.0), px(-100.0)), None, Modifiers::none());
        frames(cx, 60);
        assert!(view.read_with(cx, |v, _| v.hover.is_none() && v.retiring_hovers.is_empty() && v.motion.len() <= 2));
        assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
    }

    #[cfg(feature = "gallery")] // uses the gallery aligner and gui-harness types
    fn hover_observed(cx: &mut VisualTestContext, elapsed: Duration, events: usize) -> crate::gallery::align::Observed {
        frame_after(cx, elapsed);
        let (at_ms, mut ledger) = cx.update(|_, cx| {
            let at_ms = u64::try_from(crate::motion::now(cx).saturating_duration_since(crate::motion::epoch(cx)).as_millis()).expect("test clock fits u64");
            (at_ms, crate::probe::take(cx))
        });
        ledger.tracks.retain(|track| track.key.starts_with("graph-hover-"));
        ledger.bounds.clear(); ledger.texts.clear();
        crate::gallery::align::Observed { drawn: backend_gui_harness::Drawn { at_ms, invalidations: 1, callbacks: 0, cpu: Duration::ZERO, input_cpu: Duration::ZERO, input_events: events, input_max: Duration::ZERO, viewport: backend_gui_harness::Viewport { width: 1024, height: 768, scale: 1 }, captured: false }, ledger, events, state: None }
    }
    #[cfg(feature = "gallery")] // uses the gallery aligner and gui-harness types
    #[gpui::test]
    fn direct_hover_return_swaps_existing_packets_and_keeps_both_tracks_continuous(cx: &mut TestAppContext) {
        use crate::gallery::align::{self, Check, Tolerance};
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); crate::probe::enable(cx); });
        for cadence in [4_u64, 16] {
            let world = Arc::new(crate::graph::model::tests::tiny());
            let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
            let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::World, window, cx));
            frames(cx, 30); cx.update(|_, cx| { crate::probe::take(cx); });
            let points = [0, 3].map(|node| view.read_with(cx, |v, _| v.screen_position(node).expect("visible real symbol")));
            let at = |n: usize| point(px(points[n].0), px(points[n].1));
            let mut seen = Vec::new();
            cx.simulate_mouse_move(at(0), None, Modifiers::none());
            for n in 0..200_u64.div_ceil(cadence) { seen.push(hover_observed(cx, Duration::from_millis(cadence), usize::from(n == 0))); }
            let a_epoch = view.read_with(cx, |v, _| { assert_eq!(v.hover, Some(0)); assert_eq!(v.hover_a, 1.0); v.hover_key });
            let packet_a = scene.neighbourhood(0);
            cx.simulate_mouse_move(at(1), None, Modifiers::none());
            for n in 0..64 / cadence { seen.push(hover_observed(cx, Duration::from_millis(cadence), usize::from(n == 0))); }
            let (b_epoch, a_strength, b_strength) = view.read_with(cx, |v, _| {
                assert_eq!(v.hover, Some(3));
                let outgoing = v.retiring_hovers.get(&0).expect("A has not finished its fade");
                assert_eq!(outgoing.packet.as_ref().expect("painted identity").node as usize, a_epoch); assert!(Arc::ptr_eq(outgoing.packet.as_ref().expect("painted A packet"), &packet_a));
                assert!(outgoing.alpha > 0.0 && v.hover_a > 0.0, "both visible envelopes are the swap precondition");
                (v.hover_key, outgoing.alpha, v.hover_a)
            });
            let packet_b = scene.neighbourhood(3);
            cx.simulate_mouse_move(at(0), None, Modifiers::none());
            view.read_with(cx, |v, _| {
                assert_eq!(v.hover, Some(0)); assert_eq!(v.hover_key, a_epoch); assert_eq!(v.hover_a, a_strength);
                let outgoing = v.retiring_hovers.get(&3).expect("B now departs without being discarded");
                assert_eq!(outgoing.packet.as_ref().expect("painted identity").node as usize, b_epoch); assert_eq!(outgoing.alpha, b_strength);
                assert!(Arc::ptr_eq(outgoing.packet.as_ref().expect("painted B packet"), &packet_b));
                assert!(Arc::ptr_eq(&scene.neighbourhood(0), &packet_a), "A's existing packet is promoted rather than rebuilt");
            });
            for n in 0..200_u64.div_ceil(cadence) { seen.push(hover_observed(cx, Duration::from_millis(cadence), usize::from(n == 0))); }
            cx.simulate_mouse_move(point(px(-100.0), px(-100.0)), None, Modifiers::none());
            for n in 0..200_u64.div_ceil(cadence) { seen.push(hover_observed(cx, Duration::from_millis(cadence), usize::from(n == 0))); }
            let report = align::analyze(&seen, Tolerance::default());
            assert!(report.stats.get(&Check::Continuity).is_some_and(|stat| stat.evaluated > 20), "actual per-frame samples must exercise continuity");
            assert!(report.of(Check::Continuity).next().is_none(), "{cadence}ms A/B/A continuity: {:?}", report.findings);
            assert!(view.read_with(cx, |v, _| v.hover.is_none() && v.retiring_hovers.is_empty() && v.motion.len() <= 2));
            assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
        }
    }

    #[cfg(feature = "gallery")] // uses the gallery aligner and gui-harness types
    #[gpui::test]
    fn distinct_hover_handoff_preserves_every_visible_envelope_within_motion_budget(cx: &mut TestAppContext) {
        use crate::gallery::align::{self, Check, Tolerance};
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); crate::probe::enable(cx); });
        for cadence in [4_u64, 16] {
            let world = Arc::new(crate::graph::model::tests::tiny());
            let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
            let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::World, window, cx));
            frames(cx, 30); cx.update(|_, cx| { crate::probe::take(cx); });
            let points = [0, 3, 5].map(|node| view.read_with(cx, |v, _| v.screen_position(node).expect("visible native A/B/C")));
            let at = |n: usize| point(px(points[n].0), px(points[n].1));
            let mut seen = Vec::new();
            cx.simulate_mouse_move(at(0), None, Modifiers::none());
            for n in 0..200_u64.div_ceil(cadence) { seen.push(hover_observed(cx, Duration::from_millis(cadence), usize::from(n == 0))); }
            cx.simulate_mouse_move(at(1), None, Modifiers::none());
            for n in 0..32 / cadence { seen.push(hover_observed(cx, Duration::from_millis(cadence), usize::from(n == 0))); }
            view.read_with(cx, |v, _| {
                assert_eq!(v.hover, Some(3));
                assert!(v.hover_a > 0.0 && v.retiring_hovers.get(&0).is_some_and(|packet| packet.alpha > 0.0), "both actual visible packets are the three-symbol handoff precondition");
            });
            cx.simulate_mouse_move(at(2), None, Modifiers::none());
            view.read_with(cx, |v, _| {
                assert_eq!(v.hover, Some(5));
                assert_eq!(v.retiring_hovers.len(), 2, "A and brief B both retain their positive visual envelopes");
                assert!(v.retiring_hovers.values().all(|outgoing| outgoing.alpha > 0.0 && outgoing.packet.is_some()));
            });
            for n in 0..200_u64.div_ceil(cadence) {
                seen.push(hover_observed(cx, Duration::from_millis(cadence), usize::from(n == 0)));
                if n == 0 {
                    view.read_with(cx, |v, _| {
                        let expected: usize = v.retiring_hovers.values().filter(|outgoing| outgoing.alpha > 0.001).filter_map(|outgoing| outgoing.packet.as_ref()).map(|packet| packet.edges.len()).sum();
                        assert!(expected > 0, "native retiree relations are actually painted");
                        assert_eq!(v.stats.fading_hover_relations as usize, expected, "paint counters aggregate all positive departing packets");
                    });
                }
            }
            cx.simulate_mouse_move(point(px(-100.0), px(-100.0)), None, Modifiers::none());
            for n in 0..200_u64.div_ceil(cadence) { seen.push(hover_observed(cx, Duration::from_millis(cadence), usize::from(n == 0))); }
            let report = align::analyze(&seen, Tolerance::default());
            assert!(report.stats.get(&Check::Continuity).is_some_and(|stat| stat.evaluated > 20));
            assert!(report.of(Check::Continuity).next().is_none(), "{cadence}ms distinct A/B/C continuity: {:?}", report.findings);
            assert!(view.read_with(cx, |v, _| v.hover.is_none() && v.retiring_hovers.is_empty() && v.motion.len() <= 2));
            assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
        }
    }

    #[gpui::test]
    fn hover_retirement_is_world_bounded_and_unsampled_input_allocates_nothing(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::World, window, cx));
        frames(cx, 30);
        let points = [0, 3, 5].map(|node| view.read_with(cx, |v, _| v.screen_position(node).expect("visible storm target")));
        let at = |n: usize| point(px(points[n].0), px(points[n].1));
        cx.update(|window, cx| {
            // VisualTestContext::simulate_mouse_move yields after each event,
            // allowing notified native draws. Batch actual platform dispatches
            // and inspect before this update yields to prove an input-only burst.
            let baseline = view.read(cx).motion.len();
            for n in 0..1000 {
                window.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                    position: at(n % 3), pressed_button: None, modifiers: Modifiers::none(),
                }), cx);
            }
            let v = view.read(cx);
            assert_eq!(v.motion.len(), baseline, "input-only targets create no scalar tracks");
            assert!(v.retiring_hovers.is_empty() && v.terminal_hovers.is_empty());
            assert!(v.hover_packet.is_none() && !v.hover_sampled, "no prepaint sampled the event burst before this assertion");
            assert_eq!(v.hover, Some(0), "the latest actual native pick owns the first draw");
        });
        for n in 0..1000 {
            cx.simulate_mouse_move(at(n % 3), None, Modifiers::none());
            frame_after(cx, Duration::from_millis(4));
            view.read_with(cx, |v, _| {
                assert!(v.retiring_hovers.len() <= world.len());
                assert!(v.terminal_hovers.len() <= world.len());
                assert!(v.motion.len() <= world.len() + 3, "one track per symbol plus prism/flow/reading");
                let active_edges = v.hover_packet.as_ref().map_or(0, |packet| packet.edges.len());
                assert!(v.retained().fading_relations + active_edges <= world.edges.len() * 4, "raw member and rolled item adjacency retain only a linear number of relations");
                assert!(v.retiring_hovers.keys().all(|&node| Some(node) != v.hover), "no active symbol is also retiring");
            });
        }
        cx.simulate_mouse_move(point(px(-100.0), px(-100.0)), None, Modifiers::none());
        frames(cx, 60);
        assert!(view.read_with(cx, |v, _| v.retiring_hovers.is_empty() && v.terminal_hovers.is_empty() && v.hover_packet.is_none() && v.motion.len() <= 2));
        assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
    }

    #[gpui::test]
    fn reading_flight_retains_symbol_and_relation_provenance_through_measured_reframe(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::Focus(3), window, cx));
        frames(cx, 30);
        view.update(cx, |v, cx| {
            let frame = v.frame.as_ref().expect("gathered actual relations");
            let key = frame.slots.iter().find_map(|slot| slot.key.filter(|key| key.node != 3)).expect("actual relation target");
            v.state.selected = Some(key);
            v.set_focus(Some(key.node), true, cx);
            let super::Travel::Reading(route) = v.rig.as_ref().expect("flight rig").travel().expect("reading intent") else { panic!("symbol focus must carry reading provenance") };
            assert_eq!(route.kind, super::FocusKind::Follow);
            assert_eq!(route.departure, Some((f64::from(scene.layout.x[3]), f64::from(scene.layout.y[3]))));
            assert_eq!(route.arrival, (f64::from(scene.layout.x[key.node as usize]), f64::from(scene.layout.y[key.node as usize])));
            let mut room = route.room; room.anchor.0 += 0.02;
            let rig = v.rig.as_mut().expect("reading rig");
            let target = rig.target();
            rig.reframe_in(target, room);
            let super::Travel::Reading(reframed) = rig.travel().expect("retained reading intent") else { panic!("measurement cannot erase the reading route") };
            assert_eq!(reframed.departure, route.departure); assert_eq!(reframed.arrival, route.arrival); assert_eq!(reframed.kind, route.kind); assert_eq!(reframed.context, route.context); assert_eq!(reframed.room, room);
        });
        frames(cx, 160);
        assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
    }

    #[gpui::test]
    fn reading_ink_retargets_continuously_and_finishes_without_an_idle_lease(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::World, window, cx));
        frames(cx, 30);
        assert_eq!(view.read_with(cx, |v, _| v.reading_a), 0.0);
        view.update(cx, |v, cx| v.set_focus(Some(0), true, cx));
        frames(cx, 2);
        let entering = view.read_with(cx, |v, _| v.reading_a);
        assert!(entering > 0.0 && entering < 1.0, "focus ink is an observed finite envelope");
        view.update(cx, |v, cx| v.set_focus(Some(3), true, cx));
        assert_eq!(view.read_with(cx, |v, _| v.reading_a), entering, "another reading target cannot reset the same envelope");
        frames(cx, 160);
        assert_eq!(view.read_with(cx, |v, _| v.reading_a), 1.0);
        assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
        view.update(cx, |v, cx| v.set_focus(None, false, cx));
        frames(cx, 2);
        let leaving = view.read_with(cx, |v, _| v.reading_a);
        assert!(leaving > 0.0 && leaving < 1.0, "clearing focus fades ambient ink instead of switching a boolean");
        frames(cx, 60);
        assert_eq!(view.read_with(cx, |v, _| v.reading_a), 0.0);
        assert!(view.read_with(cx, |v, _| v.motion.len() <= 2));
        assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
        cx.update(|_, cx| set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx));
        view.update(cx, |v, cx| v.set_focus(Some(0), false, cx)); frame(cx);
        assert_eq!(view.read_with(cx, |v, _| v.reading_a), 1.0);
        view.update(cx, |v, cx| v.set_focus(None, false, cx)); frame(cx);
        assert_eq!(view.read_with(cx, |v, _| v.reading_a), 0.0);
    }

    #[gpui::test]
    fn camera_flow_fades_after_landing_and_releases_its_frame_lease(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let viewport = View { x: 0.0, y: 0.0, w: 1024.0, h: 768.0 };
        let camera = scene.focus_cam(&viewport, 0, 0.0);
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::Cam(camera), window, cx));
        frames(cx, 30);
        view.update(cx, |v, cx| v.show_world(cx));
        let mut saw_motion = false;
        let mut landed = None;
        for _ in 0..200 {
            frame(cx);
            let (moving, alpha) = view.read_with(cx, |v, _| (v.moving, v.flow_a));
            saw_motion |= moving;
            if saw_motion && !moving { landed = Some(alpha); break; }
        }
        let landed = landed.expect("the actual world flight must settle within its finite budget");
        assert!(landed > 0.0, "landing retains the last visible edge activity rather than dropping it");
        frames(cx, 2);
        let fading = view.read_with(cx, |v, _| v.flow_a);
        assert!(fading > 0.0 && fading < landed, "the finite envelope decreases smoothly after landing");
        frames(cx, 30);
        assert_eq!(view.read_with(cx, |v, _| v.flow_a), 0.0);
        assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0, "edge activity cannot lease frames at rest");
        cx.update(|_, cx| set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx));
        view.update(cx, |v, cx| v.set_focus(Some(0), true, cx));
        frames(cx, 30);
        assert_eq!(view.read_with(cx, |v, _| v.flow_a), 0.0);
        assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
    }

    #[gpui::test]
    fn outgoing_hover_packet_fades_without_retaining_pick_or_idle_frames(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let viewport = super::View { x: 0.0, y: 0.0, w: 1024.0, h: 768.0 };
        let camera = scene.focus_cam(&viewport, 0, 0.0);
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::Cam(camera), window, cx));
        frames(cx, 30);
        let (x, y) = view.read_with(cx, |v, _| v.screen_position(0).expect("actual symbol position"));
        cx.simulate_mouse_move(point(px(x), px(y)), None, Modifiers::none());
        frames(cx, 30);
        assert_eq!(view.read_with(cx, |v, _| v.hover), Some(0), "the real native pointer must light this symbol first");
        cx.simulate_mouse_move(point(px(-100.0), px(-100.0)), None, Modifiers::none());
        let departure = view.read_with(cx, |v, _| {
            assert_eq!(v.hover, None, "interaction ownership clears immediately");
            let outgoing = v.retiring_hovers.get(&0).expect("bounded outgoing visual");
            assert_eq!(outgoing.packet.as_ref().expect("painted packet").node, 0);
            outgoing.alpha
        });
        assert!(departure > 0.0, "departure retains the actually visible neighborhood");
        frames(cx, 2);
        let alpha = view.read_with(cx, |v, _| v.retiring_hovers.get(&0).map_or(0.0, |outgoing| outgoing.alpha));
        assert!(alpha < departure, "the same immutable packet fades rather than disappearing abruptly");
        frames(cx, 40);
        assert!(view.read_with(cx, |v, _| v.retiring_hovers.is_empty() && v.hover.is_none() && v.motion.len() <= 2));
        assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0, "the terminal fade cannot retain an idle frame lease");
    }

    #[gpui::test]
    fn clicking_measured_focus_card_padding_cannot_start_canvas_drag_or_release_focus(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::Focus(5), window, cx));
        frames(cx, 30);
        let (before, card) = view.read_with(cx, |v, _| (v.camera(), v.card_bounds.expect("actual card bounds")));
        let at = point(card.left() + px(3.0), card.top() + card.size.height * 0.5);
        cx.simulate_mouse_move(at, None, Modifiers::none());
        cx.simulate_mouse_down(at, gpui::MouseButton::Left, Modifiers::none());
        assert!(view.read_with(cx, |v, _| v.drag.is_none()), "native card padding is foreground input, even without a child control hitbox");
        cx.simulate_mouse_up(at, gpui::MouseButton::Left, Modifiers::none());
        frame(cx);
        assert_eq!(view.read_with(cx, |v, _| (v.focused(), v.camera())), (Some(5), before));
    }

    #[gpui::test]
    fn short_find_reveals_seventh_row_and_enter_opens_that_exact_symbol(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { text_scale: 2.0, reduced_motion: true, ..Facet::default() }, cx); });
        let world = Arc::new(synthetic(2, 3));
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::World, window, cx));
        cx.simulate_resize(gpui::size(px(480.0), px(420.0)));
        frames(cx, 30);
        cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
        cx.simulate_keystrokes("/");
        cx.simulate_input("I");
        frames(cx, 30);
        assert_eq!(view.read_with(cx, |v, _| v.results.len()), 7, "the broad fixture query must exercise all seven real rows");
        cx.update(|window, cx| assert!(view.read(cx).find_focused(window, cx)));
        for selected in 1..7 {
            cx.simulate_keystrokes("down");
            frame(cx);
            view.read_with(cx, |v, _| {
                assert_eq!(v.state.result_sel, selected);
                let row = v.results_scroll.bounds_for_item(selected).expect("real selected row");
                let viewport = v.results_scroll.bounds();
                let offset = v.results_scroll.offset();
                assert!(row.top() + offset.y >= viewport.top() - px(0.5));
                assert!(row.bottom() + offset.y <= viewport.bottom() + px(0.5), "the actual native scroll viewport must reveal the selected row");
            });
        }
        let target = view.read_with(cx, |v, _| {
            assert!(v.results_scroll.offset().y < px(0.0), "this must exercise overflowing rows rather than a trivially fitting list");
            v.results[6]
        });
        cx.simulate_keystrokes("enter");
        frames(cx, 30);
        assert_eq!(view.read_with(cx, |v, _| v.focused()), Some(target));
    }

    #[gpui::test]
    fn empty_reach_folded_chain_and_long_tour_keep_readable_visible_content(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { text_scale: 2.0, reduced_motion: true, ..Facet::default() }, cx); });
        let mut fixture = crate::graph::model::tests::tiny();
        for node in &mut fixture.nodes { node.vis = Some("pub".into()); }
        let verb = "read".repeat(80);
        fixture.nodes[2].name = verb.clone().into();
        fixture.nodes[2].recv = Some("reads".into());
        fixture.nodes[2].ret = Some("String".into());
        fixture.nodes[3].ret = Some("Result<(), Error>".into());
        fixture.nodes[3].doc = Some(format!("{}.", "A documented path through values ".repeat(80)).into());
        let world = Arc::new(fixture);
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::Focus(0), window, cx));
        cx.simulate_resize(gpui::size(px(480.0), px(400.0)));
        frames(cx, 30);
        view.update(cx, |v, cx| v.toggle_reach(cx));
        frames(cx, 30);
        view.read_with(cx, |v, _| {
            assert!(v.reach().expect("reach mode").all.is_empty());
            let region = v.view.expect("region");
            assert_eq!(v.camera(), Some(super::focus_camera(&scene, &region, 0, v.card_bounds)), "empty reach must preserve reading scale");
            assert!(!v.node_bounds(0).expect("visible source").intersects(&v.card_bounds.expect("card")));
        });
        let chain = super::Chain { cost: 1.0, from: "#0".into(), output: "text".into(), via: None,
            steps: vec![super::super::discovery::ChainStep { node: 2, verb: verb.clone(), input: "#0".into(), output: "text".into(), riders: vec![], fails: false, maybe: false }],
            path: vec![0, 2], stops: vec![super::RoadStop { node: 0, label: "Page".into(), calls: vec![2], yours: true }],
            brief: "Page to text".into(), rail: "Page reads text".into(), code: format!("page.{verb}()") };
        view.update(cx, |v, cx| v.hold_chain(chain, cx));
        cx.simulate_modifiers_change(Modifiers { alt: true, ..Modifiers::none() });
        frames(cx, 30);
        view.read_with(cx, |v, _| {
            let region = v.view.expect("region");
            let plate = v.chrome_bounds["graph-chain-bounds"];
            assert!(f32::from(plate.size.height) <= super::reading_plate_height(Some(region)) + 1.0);
            assert!(v.camera().expect("camera").w >= scene.focus_cam(&region, 0, 0.0).w, "one folded stop must not produce infinitesimal framing");
            assert!(!v.node_bounds(0).expect("visible chain source").intersects(&plate));
        });
        cx.simulate_modifiers_change(Modifiers::none());
        view.update(cx, |v, cx| assert!(v.start_tour(1, 1, cx)));
        frames(cx, 30);
        view.read_with(cx, |v, _| {
            assert_eq!(v.tour_stop(), Some(3));
            let region = v.view.expect("region");
            let plate = v.chrome_bounds["graph-tour-bounds"];
            assert!(f32::from(plate.size.height) <= super::reading_plate_height(Some(region)) + 1.0);
            assert!(!v.node_bounds(3).expect("visible tour stop").intersects(&plate));
        });
        assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0, "all finite room correction must be idle after settlement");
    }

    /// S7c: `held_chain` names exactly the road ⏎ on a chain result holds,
    /// and is `None` at rest and again once a plain focus replaces it
    /// (holding a chain and having a focus are mutually exclusive).
    #[gpui::test]
    fn held_chain_names_the_road_a_chain_result_holds(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::Focus(0), window, cx));
        frames(cx, 10);
        view.read_with(cx, |v, _| assert_eq!(v.held_chain(), None, "nothing held at rest"));
        let chain = super::Chain { cost: 1.0, from: "#0".into(), output: "text".into(), via: None,
            steps: vec![super::super::discovery::ChainStep { node: 2, verb: "read".into(), input: "#0".into(), output: "text".into(), riders: vec![], fails: false, maybe: false }],
            path: vec![0, 2], stops: vec![super::RoadStop { node: 0, label: "Page".into(), calls: vec![2], yours: true }],
            brief: "Page to text".into(), rail: "Page reads text".into(), code: "page.read()".into() };
        view.update(cx, |v, cx| v.hold_chain(chain, cx));
        frames(cx, 10);
        view.read_with(cx, |v, _| assert_eq!(v.held_chain(), Some([0, 2].as_slice()), "the held road's own path"));
        view.update(cx, |v, cx| v.set_focus(Some(1), false, cx));
        frames(cx, 10);
        view.read_with(cx, |v, _| assert_eq!(v.held_chain(), None, "a plain focus is not a held chain"));
    }

    #[gpui::test]
    fn modifier_hints_reserve_measured_card_space_without_camera_bob_or_hidden_scroll_tail(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); crate::probe::enable(cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::Focus(0), window, cx));
        for (width, scale) in [(1440.0, 1.0), (480.0, 2.0)] {
            cx.simulate_resize(gpui::size(px(width), px(900.0)));
            cx.update(|_, cx| set_facet(Facet { reduced_motion: true, text_scale: scale, ..Facet::default() }, cx));
            cx.simulate_modifiers_change(Modifiers::none());
            frames(cx, 30);
            let measured = view.read_with(cx, |v, _| v.hint_metrics.get());
            assert!(measured.core > 0.0 && measured.hint > 0.0, "actual core and control row must be measured: {measured:?}");
            // Derive a viewport which positively exercises a partially fitting
            // row, rather than guessing font-dependent card heights.
            let half_hint = (measured.hint + 10.0) * 0.5;
            let height = (measured.core + half_hint + 32.0) / 0.45;
            cx.simulate_resize(gpui::size(px(width), px(height)));
            frames(cx, 30);
            let (bounds, camera) = view.read_with(cx, |v, _| {
                let m = v.hint_metrics.get();
                assert!(m.core < m.budget && m.core + m.hint + 10.0 > m.budget, "fixture must exercise partial fit: {m:?}");
                assert_eq!(m.reserved, m.budget);
                assert!(v.focus_scroll.max_offset().y <= px(0.5), "hidden controls must not create a blank scroll tail");
                (v.card_bounds.expect("actual card"), v.camera().expect("camera"))
            });
            for platform in [true, false, true, false] {
                cx.update(|_, cx| { crate::probe::take(cx); });
                cx.simulate_modifiers_change(Modifiers { platform, ..Modifiers::none() });
                frame(cx);
                let ledger = cx.update(|_, cx| crate::probe::take(cx));
                for key in ["graph-focus-title", "graph-focus-qual"] {
                    let text = ledger.texts.iter().find(|text| text.key == key).expect("actual focus content must paint in every modifier state");
                    assert!(!text.content.is_empty());
                    assert!(text.bounds.x >= f32::from(bounds.left()) - 0.5 && text.bounds.y >= f32::from(bounds.top()) - 0.5
                        && text.bounds.x + text.bounds.width <= f32::from(bounds.right()) + 0.5 && text.bounds.y + text.bounds.height <= f32::from(bounds.bottom()) + 0.5,
                        "painted content must be positioned inside its real native card, not a detached measurement root: {text:?} vs {bounds:?}");
                }
                view.read_with(cx, |v, _| {
                    assert_eq!(v.card_bounds, Some(bounds), "Cmd visibility cannot resize the measured card");
                    assert_eq!(v.camera(), Some(camera), "Cmd visibility cannot reframe the graph");
                    if platform { assert!(v.focus_scroll.max_offset().y > px(0.0), "real hint content must remain scroll-reachable"); }
                    else { assert!(v.focus_scroll.max_offset().y <= px(0.5), "hidden hint must not remain in scroll content"); }
                });
            }
            frames(cx, 30);
            assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
        }
    }

    #[gpui::test]
    fn native_enter_commits_the_same_preview_road_without_teleporting_its_value(cx: &mut TestAppContext) {
        use std::rc::Rc;
        use crate::motion;
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::World, window, cx));
        frames(cx, 30);
        cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
        cx.simulate_keystrokes("/");
        frame(cx);
        cx.update(|window, cx| assert!(view.read(cx).find_focused(window, cx)));
        cx.simulate_input("Page -> text");
        cx.run_until_parked();
        let chain = super::Chain { cost: 1.0, from: "#0".into(), output: "text".into(), via: None,
            steps: vec![super::super::discovery::ChainStep { node: 2, verb: "new".into(), input: "#0".into(), output: "text".into(), riders: vec![], fails: false, maybe: false }],
            path: vec![0, 2, 3], stops: vec![super::RoadStop { node: 0, label: "Page".into(), calls: vec![2], yours: true }, super::RoadStop { node: 3, label: "from_str".into(), calls: vec![3], yours: false }],
            brief: "Page to text".into(), rail: "Page reads text".into(), code: "page.new()".into() };
        // Deliver one pinned worker answer through the real result selection
        // boundary; native Enter must commit that exact displayed path.
        view.update(cx, |v, cx| {
            v._search_task = None; v.searching = false; v.results.clear();
            v.search = Rc::new(super::Search { shaped: true, issue: None, rows: vec![], lit: vec![0, 3], lit_set: [0, 3].into_iter().collect(), packages: 2, chains: vec![chain.clone()] });
            v.state.result_sel = 0; cx.notify();
        });
        frame(cx);
        frame_after(cx, Duration::from_millis(320));
        let epoch = cx.update(|_, cx| motion::epoch(cx));
        let (timing, before) = view.read_with(cx, |v, cx| {
            assert!(v.state.find_open && v.state.exploration.chain().is_none());
            let road = v.road.as_ref().expect("visible preview clock");
            (road.timing(), road.clone().sample(motion::now(cx).saturating_duration_since(epoch), false).progress)
        });
        assert!(before > 0.1 && before < 0.9, "actual preview must be mid-trip before commit: {before}");
        cx.simulate_keystrokes("enter");
        frame_after(cx, Duration::from_millis(8));
        view.read_with(cx, |v, cx| {
            assert!(!v.state.find_open);
            assert_eq!(v.state.exploration.chain().expect("held actual path").path, chain.path);
            let road = v.road.as_ref().expect("held clock");
            assert_eq!(road.timing(), timing, "same semantic path must keep its original animation budget");
            let after = road.clone().sample(motion::now(cx).saturating_duration_since(epoch), false).progress;
            assert!(after >= before && after < 1.0, "native hold must continue rather than restart or snap: {before} -> {after}");
        });
        frames(cx, 120);
        assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
    }

    #[gpui::test]
    fn reduced_first_draw_chrome_uses_the_same_camera_and_bottom_reading_owns_the_footer(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); crate::probe::enable(cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let survey = View { x: 0.0, y: 0.0, w: 1440.0, h: 900.0 }.frame(scene.layout.bounds, 30.0);
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::Cam(survey), window, cx));
        frames(cx, 30);
        let before = view.read_with(cx, |v, _| v.where_line().expect("initial context").altitude);
        cx.update(|_, cx| { crate::probe::take(cx); });
        view.update(cx, |v, cx| v.set_focus(Some(3), true, cx));
        frame(cx);
        let expected = view.read_with(cx, |v, _| v.where_line().expect("sampled reading context").altitude);
        assert_ne!(before, expected, "the reduced jump must change altitude for a meaningful chrome assertion");
        let ledger = cx.update(|_, cx| crate::probe::take(cx));
        let levels: Vec<_> = ledger.texts.iter().filter(|text| text.key == "graph-where-level").collect();
        assert!(!levels.is_empty(), "the first reduced draw must paint the actual breadcrumb");
        assert!(levels.iter().all(|text| text.content == expected.word()), "every breadcrumb in the first draw must use the sampled camera: {levels:?}");
        cx.simulate_resize(gpui::size(px(480.0), px(600.0)));
        cx.update(|_, cx| set_facet(Facet { text_scale: 2.0, reduced_motion: true, ..Facet::default() }, cx));
        frames(cx, 30);
        cx.update(|_, cx| { crate::probe::take(cx); });
        frame(cx);
        view.read_with(cx, |v, _| {
            let primary = v.primary_chrome().expect("measured bottom focus card");
            let region = v.view.expect("viewport");
            let room = Scene::free_view(&region, Some(primary));
            assert!(room.y == region.y && room.h < region.h, "the actual measured reading room must be above the bottom card");
            assert!(!v.chrome_bounds.contains_key("graph-where-bounds"), "bottom reading context cannot leave a world footer behind its card");
        });
        let ledger = cx.update(|_, cx| crate::probe::take(cx));
        assert!(ledger.texts.iter().all(|text| text.key != "graph-where-level"), "hidden footer must paint no text or hitboxes");
        assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
    }

    #[gpui::test]
    fn keyboard_find_reopen_replaces_retained_query_and_focuses_the_second_result(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::World, window, cx));
        frames(cx, 30);
        cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
        cx.simulate_keystrokes("/");
        frame(cx);
        cx.update(|window, cx| assert!(view.read(cx).find_focused(window, cx)));
        cx.simulate_input("Page");
        frames(cx, 30);
        assert!(view.read_with(cx, |v, _| v.results.contains(&0)), "the first typed native query must resolve Page");
        cx.simulate_keystrokes("enter");
        frames(cx, 30);
        assert_eq!(view.read_with(cx, |v, _| v.focused()), Some(0));
        assert_eq!(view.read_with(cx, |v, cx| v.find.read(cx).value().to_string()), "Page", "closing find retains the first query");
        assert_eq!(view.read_with(cx, |v, cx| v.find.read(cx).scroll_offset()), point(px(0.0), px(0.0)), "closed retained query starts at its beginning");
        cx.simulate_keystrokes("/");
        frame(cx);
        cx.update(|window, cx| assert!(view.read(cx).find_focused(window, cx)));
        cx.simulate_input("from_str");
        frames(cx, 30);
        view.read_with(cx, |v, cx| {
            assert_eq!(v.find.read(cx).value().as_ref(), "from_str", "keyboard reopen selects the retained query for replacement");
            assert!(v.results.contains(&3), "the replacement query must resolve the actual second symbol");
        });
        cx.simulate_keystrokes("enter");
        frames(cx, 100);
        assert_eq!(view.read_with(cx, |v, _| v.focused()), Some(3));
        assert_eq!(view.read_with(cx, |v, _| v.frame.as_ref().map(|frame| frame.node)), Some(3));
        cx.update(|window, cx| assert!(!view.read(cx).find_focused(window, cx)));
        assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
    }

    #[gpui::test]
    fn cold_discovery_completion_after_blur_cannot_restart_search(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let prepared = super::Discovery::prepare(&world);
        let (view, cx) = cx.add_window_view(|window, cx| {
            // Hold the prepared engine at the worker-delivery boundary.
            let mut graph = GraphView::empty(world.clone(), Start::World, window, cx);
            graph.scene = Some(scene.clone());
            graph
        });
        frame(cx);
        cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
        cx.simulate_keystrokes("/");
        frame(cx);
        cx.update(|window, cx| assert!(view.read(cx).find_focused(window, cx), "find must actually own native keyboard focus before blur"));
        cx.simulate_input("Error");
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.searching && v.discovery.is_none()));
        let typed_query = view.read_with(cx, |v, cx| v.find.read(cx).value().to_string());
        assert_eq!(typed_query, "Error", "the real focused input accepted the pending query");
        cx.update(|window, cx| {
            window.focus(&view.focus_handle(cx), cx);
            assert!(!view.read(cx).find_focused(window, cx), "native keyboard focus actually left the input");
        });
        // GPUI publishes old/current focus paths after a draw. The native
        // window schedules this frame automatically; the headless driver
        // must draw it before expecting the Blur observer.
        frame(cx);
        cx.run_until_parked();
        assert!(!view.read_with(cx, |v, _| v.state.find_open || v.searching));
        let after_blur = view.read_with(cx, |v, cx| v.find.read(cx).value().to_string());
        view.update(cx, |v, cx| {
            v.discovery = Some(std::rc::Rc::new(super::Discovery::from_prepared(prepared)));
            // Deliver the pending nonempty query captured before blur. Its
            // completion cannot take ownership from the now-closed input.
            v.refresh_search(&typed_query, cx);
        });
        frames(cx, 20);
        view.read_with(cx, |v, cx| {
            assert!(v.ready() && v._search_task.is_none() && !v.state.find_open && !v.searching, "an obsolete cold-load query cannot reopen find or hang quiet readiness");
            assert_eq!(v.find.read(cx).value().to_string(), after_blur, "stale completion cannot alter the field's post-blur value");
            assert!(v.results.is_empty());
        });
        cx.update(|window, cx| assert!(!view.read(cx).find_focused(window, cx), "stale completion cannot reclaim native keyboard focus"));
    }

    #[gpui::test]
    fn explicit_world_releases_focus_and_pending_initial_entry(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut graph = GraphView::with_scene(scene.clone(), Start::Focus(5), window, cx);
            graph.pending_enter = Some(3);
            graph.show_world(cx);
            graph
        });
        frames(cx, 120);
        assert_eq!(view.read_with(cx, |v, _| (v.state.focus, v.prism.is_some(), v.pending_enter)), (None, false, None));
        view.update(cx, |v, cx| v.set_focus(Some(5), false, cx));
        frames(cx, 30);
        cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
        cx.simulate_keystrokes("0");
        frames(cx, 150);
        let (focus, prism, actual, viewport) = view.read_with(cx, |v, _| (v.state.focus, v.prism.is_some(), v.camera(), v.view));
        assert_eq!((focus, prism), (None, false));
        assert_eq!(actual, Some(scene.world_cam(&viewport.expect("viewport"))));
    }

    #[gpui::test]
    fn native_resize_schedules_responsive_chrome_then_returns_to_idle(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::Focus(5), window, cx));
        cx.simulate_resize(gpui::size(px(1440.0), px(900.0)));
        frames(cx, 30);
        assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0, "setup must be idle before the resize");
        for width in [782.0, 492.0] {
            cx.simulate_resize(gpui::size(px(width), px(704.0)));
            cx.run_until_parked();
            // This is the resize callback's initial dirty frame. Subsequent
            // draws are allowed only when product code requested them.
            cx.update(|window, cx| window.draw(cx).clear(cx));
            assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0, "reduced responsive chrome and camera correction finish in the same dirty draw");
            let (region, card) = view.read_with(cx, |v, _| (v.view.expect("viewport"), v.card_bounds.expect("measured card")));
            assert_eq!(region.w, width);
            if width < 640.0 {
                assert!((f32::from(card.origin.x) - (region.x + 12.0)).abs() < 1.0);
                assert!((f32::from(card.size.width) - (region.w - 24.0)).abs() < 1.0, "narrow chrome must move below the map without another input");
                assert!(f32::from(card.origin.y) > region.y + region.h * 0.5);
            }
            assert_eq!(view.read_with(cx, |v, _| v.prism.as_ref().map(|p| (p.node, p.g))), Some((5, 1.0)));
        }
    }

    #[gpui::test]
    fn find_during_focus_flight_defers_gather_until_escape(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        for reduced_motion in [false, true] {
            cx.update(|cx| set_facet(Facet { reduced_motion, ..Facet::default() }, cx));
            let world = Arc::new(crate::graph::model::tests::tiny());
            let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
            let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::World, window, cx));
            frame(cx);
            // Overlay ownership changes before the next camera step, including
            // the reduced-motion step that would otherwise consume landing.
            cx.update(|window, cx| view.update(cx, |v, cx| {
                v.set_focus(Some(5), true, cx);
                v.open_find(window, cx);
            }));
            frames(cx, 150);
            assert_eq!(view.read_with(cx, |v, _| (v.state.focus, v.state.find_open, v.prism.is_some())), (Some(5), true, false));
            cx.simulate_keystrokes("escape");
            frames(cx, 120);
            assert_eq!(view.read_with(cx, |v, _| v.prism.as_ref().map(|p| (p.node, p.g, p.target))), Some((5, 1.0, 1.0)), "find must defer, rather than cancel, focused reading (reduced={reduced_motion})");
            assert!(!view.read_with(cx, |v, _| v.state.find_open));
        }
    }

    #[gpui::test]
    fn measured_card_reframing_preserves_gathered_prism(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::Focus(5), window, cx));
        cx.simulate_resize(gpui::size(px(1440.0), px(900.0)));
        frames(cx, 120);
        assert_eq!(view.read_with(cx, |v, _| v.prism.as_ref().map(|p| (p.node, p.g))), Some((5, 1.0)));
        cx.simulate_resize(gpui::size(px(1000.0), px(900.0)));
        cx.update(|_, cx| set_facet(Facet { text_scale: 2.0, ..Facet::default() }, cx));
        for sample in 0..150 {
            frame(cx);
            assert_eq!(view.read_with(cx, |v, _| v.prism.as_ref().map(|p| (p.node, p.g))), Some((5, 1.0)), "room correction must not regather an existing prism at sample {sample}");
        }
        let (actual, region, card) = view.read_with(cx, |v, _| (v.camera(), v.view, v.card_bounds));
        assert_eq!(actual, Some(super::focus_camera(&scene, &region.expect("current viewport"), 5, card)));
    }

    #[gpui::test]
    fn old_focus_frame_cannot_redirect_new_focus_keyboard_navigation(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); });
        let world = Arc::new(synthetic(6, 3));
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let owner = world.items.iter().copied().find(|&i| !Prism::of(&world, i).right.is_empty()).expect("outgoing relation");
        let next = world.items.iter().copied().find(|&i| i != owner).expect("second focus");
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::Focus(owner), window, cx));
        frames(cx, 120);
        cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
        // Exercise the handler in the same event turn: the native test
        // driver otherwise draws automatically between separate updates.
        cx.update(|window, cx| view.update(cx, |v, cx| {
            v.set_focus(Some(next), false, cx);
            assert_eq!(v.frame.as_ref().map(|f| f.node), Some(owner), "old presentation is still retained until prepaint");
            let event = gpui::KeyDownEvent { keystroke: gpui::Keystroke::parse("right").expect("right key"), is_held: false, prefer_character_input: false };
            v.key(&event, window, cx);
            assert!(v.state.selected.is_none(), "a previous focus's rows cannot be selected by the current focus");
            assert_eq!(v.state.focus, Some(next), "an old presentation frame must preserve the new focus intent");
        }));
    }

    #[gpui::test]
    fn suspension_releases_find_without_focusing_hidden_graph(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::Focus(5), window, cx));
        frame(cx);
        cx.update(|window, cx| view.update(cx, |v, cx| v.open_find(window, cx)));
        assert!(view.read_with(cx, |v, _| v.state.find_open));
        cx.update(|window, cx| {
            view.update(cx, |v, cx| v.suspend(window, cx));
            assert!(!view.focus_handle(cx).is_focused(window), "route cleanup must not focus a hidden graph");
        });
        assert!(!view.read_with(cx, |v, _| v.state.find_open || v.searching || v.peek.is_some()));
    }

    #[gpui::test]
    fn reach_replaces_an_interrupted_gather_and_restores_the_prism(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); });
        let world = Arc::new(crate::graph::model::tests::tiny());
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::World, window, cx));
        frame(cx);
        view.update(cx, |v, cx| { v.set_focus(Some(5), true, cx); v.toggle_reach(cx); });
        frames(cx, 150);
        let (focus, reach, prism) = view.read_with(cx, |v, _| (v.state.focus, v.reach().map(|r| r.all.clone()), v.prism.is_some()));
        assert_eq!(focus, Some(5));
        assert_eq!(reach, Some(vec![0, 3]));
        assert!(!prism, "a late focus landing must not gather over reach");
        view.update(cx, |v, cx| v.toggle_reach(cx));
        frames(cx, 150);
        assert_eq!(view.read_with(cx, |v, _| v.prism.as_ref().map(|p| (p.node, p.g))), Some((5, 1.0)));
        assert!(view.read_with(cx, |v, _| v.reach().is_none()));
    }

    #[gpui::test]
    fn tour_keys_fly_stops_open_the_current_page_and_end(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); });
        let mut fixture = crate::graph::model::tests::tiny();
        for node in &mut fixture.nodes { node.vis = Some("pub".into()); }
        fixture.nodes[3].ret = Some("Result<(), Error>".into());
        let world = Arc::new(fixture);
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let opened = std::rc::Rc::new(std::cell::Cell::new(None));
        let opened_hook = opened.clone();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut graph = GraphView::with_scene(scene.clone(), Start::Focus(3), window, cx);
            graph.on_open(std::rc::Rc::new(move |node, _, _| opened_hook.set(Some(node))));
            graph
        });
        frame(cx);
        view.update(cx, |v, cx| assert!(v.start_tour(1, 0, cx)));
        frames(cx, 120);
        assert_eq!(view.read_with(cx, |v, _| v.tour_stop()), Some(4), "the pinned package begins with its Visitor trait");
        cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
        cx.simulate_keystrokes("right");
        frames(cx, 120);
        assert_eq!(view.read_with(cx, |v, _| v.tour_stop()), Some(3), "the next pinned stop is the from_str door");
        assert_eq!(view.read_with(cx, |v, _| v.state.focus), None);
        cx.simulate_keystrokes("enter");
        assert_eq!(opened.get(), Some(3));
        cx.simulate_keystrokes("escape");
        frame(cx);
        assert_eq!(view.read_with(cx, |v, _| v.tour_stop()), None);
    }

    /// The journey the prototype's graph is for: focus a symbol (the camera
    /// flies, the prism gathers), rest on a gathered row with the real
    /// pointer, walk the prism with the arrows, ↵ flies to the walked row,
    /// Esc releases it and backs out.
    #[gpui::test]
    fn focus_flies_gathers_walks_and_backs_out(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            set_facet(Facet::default(), cx);
        });
        let world = Arc::new(synthetic(6, 3));
        let scene = Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))));
        let target = world
            .items
            .iter()
            .copied()
            .find(|&i| {
                let p = Prism::of(&world, i);
                p.left.iter().map(|c| c.rows.len()).sum::<usize>() >= 1 && p.right.iter().map(|c| c.rows.len()).sum::<usize>() >= 1
            })
            .unwrap_or_else(|| panic!("the synthetic world has no symbol with relations on both sides"));
        let (view, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene.clone(), Start::World, window, cx));
        frame(cx);
        let start = view.read_with(cx, |v, _| v.camera()).unwrap_or_else(|| panic!("no camera after the first frame"));

        // Focus: fly, then gather.
        view.update(cx, |v, cx| v.set_focus(Some(target), true, cx));
        frame(cx);
        let flying = view.read_with(cx, |v, _| v.rig.as_ref().is_some_and(super::Rig::flying));
        assert!(flying, "focus must start a flight");
        frames(cx, 110);
        let (cam, g, flying) = view.read_with(cx, |v, _| (v.camera(), v.prism.as_ref().map(|p| p.g), v.rig.as_ref().is_some_and(super::Rig::flying)));
        assert!(!flying, "still flying after 1.8 s");
        let view_box = view.read_with(cx, |v, _| v.view).unwrap_or_else(|| panic!("no view"));
        let card = view.read_with(cx, |v, _| v.card_bounds);
        let want = super::focus_camera(&scene, &view_box, target, card);
        assert_eq!(cam, Some(want), "the camera lands on the symbol at reading scale");
        assert_ne!(cam, Some(start));
        assert_eq!(g, Some(1.0), "the prism gathers on arrival");

        // Rest the real pointer on a gathered row: the hover follows the row.
        let frame_now = view.read_with(cx, |v, _| v.frame.clone()).unwrap_or_else(|| panic!("no prism frame"));
        let q = frame_now.real()[0];
        let row = frame_now.slots[q].clone();
        cx.simulate_mouse_move(point(px(row.px), px(row.py)), None, Modifiers::none());
        frame(cx);
        let (hover, slot) = view.read_with(cx, |v, _| (v.hover, v.hover_slot));
        assert_eq!((hover, slot), (row.node, Some(q)), "resting on a row hovers its symbol");

        // Walk with the arrows, then ↵ flies to the walked row.
        cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
        cx.simulate_keystrokes("right");
        frame(cx);
        let sel = view.read_with(cx, |v, _| v.state.prism_sel).unwrap_or_else(|| panic!("→ did not walk"));
        let walked = frame_now.slots[sel].node;
        assert_eq!(frame_now.slots[sel].side, 1, "→ walks into the right column");
        cx.simulate_keystrokes("enter");
        frame(cx);
        assert_eq!(view.read_with(cx, |v, _| v.state.focus), walked, "↵ focuses the walked row");
        frames(cx, 110);
        let g = view.read_with(cx, |v, _| v.prism.as_ref().map(|p| (p.node, p.g)));
        assert_eq!(g, walked.map(|w| (w, 1.0)), "the walked symbol's prism gathers");

        // Esc releases the prism and backs out.
        cx.simulate_keystrokes("escape");
        frames(cx, 110);
        let (focus, prism, cam) = view.read_with(cx, |v, _| (v.state.focus, v.prism.is_some(), v.camera()));
        assert_eq!(focus, None, "Esc releases the focus");
        assert!(!prism, "the prism has released");
        assert!(cam.is_some_and(|c| c.w > want.w * 2.0), "Esc backs the camera out ({cam:?})");
    }
}

#[cfg(test)]
#[path = "view/peek_tests.rs"]
mod peek_tests;

#[path="view/hints.rs"]
mod hints;

#[cfg(test)]
#[path="view/frame_tests.rs"]
mod frame_tests;

#[path="view/rail.rs"]
mod rail;

#[cfg(test)]
#[path="view/rail_tests.rs"]
mod rail_tests;
