//! The desktop's retained world graph. Until the index serves a world query,
//! the map reads the prototype fixture (§8.5); page routes always resolve
//! through the real read pool, never through an invented fixture coordinate.

mod identity;
use identity::{IdentityAdapter, MatchFailure, ResolvedSymbol};

use crate::core::{Activity, Resource, ResourceTerminal, VersionedRoot};
use crate::model::pages::{PackageRef, PageKey, SearchContinuation, SearchQuery};
use crate::navigation::{Intent, Route, View};
use crate::runtime::store::{Branch, StoreEvent, route_symbol};
use crate::shell::region::Links;
use facet::graph::{GraphView, NodeId, Start, World};
use gpui::{
    App, AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement, Render,
    Styled, Subscription, Task, Window, div, px,
};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

pub(crate) use crate::runtime::ui_graph::GraphDestination as OpenView;
use crate::runtime::ui_graph::GraphViewRequest;

/// One map for the reader's lifetime: its rig survives page/code visits.
pub(crate) struct Map {
    links: Links,
    graph: Option<Entity<GraphView>>,
    loading: Option<Task<()>>,
    ready_scene: Option<(Arc<facet::graph::scene::Scene>, Arc<IdentityAdapter>)>,
    identities: Option<Arc<IdentityAdapter>>,
    entry_origin: Option<facet::motion::shared::Endpoint>,
    painted_focus: Option<(NodeId, gpui::Bounds<gpui::Pixels>, bool)>,
    canvas_transform: gpui::LayerTransform,
    error: Option<String>,
    load_error: Option<String>,
    visible: bool,
    focus_on_mount: bool,
    route: Option<Route>,
    routed_focus: Option<NodeId>,
    semantic_focus: Option<NodeId>,
    revealed_focus: Option<NodeId>,
    resolved: BTreeMap<NodeId, ResolvedSymbol>,
    open_generation: u64,
    pending: Option<OpenRequest>,
    _graph_events: Option<Subscription>,
    _open_intents: Option<Subscription>,
    _events: Subscription,
}

/// Unit integration tests inject a versioned synthetic map. They never
/// derive expected declaration values from the mutable live world fixture.
#[cfg(test)]
struct TestFixture {
    scene: Arc<facet::graph::scene::Scene>,
    identities: Arc<IdentityAdapter>,
}
#[cfg(test)]
impl gpui::Global for TestFixture {}

#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) struct TestCanvasLayer {
    pub scale: f32,
    pub x: f32,
    pub y: f32,
}
#[cfg(test)]
impl gpui::Global for TestCanvasLayer {}

#[cfg(test)]
pub(crate) fn install_test_fixture(cx: &mut App) {
    use facet::graph::{Kind, Module, Node, Package};
    let mut nodes = vec![
        Node::new(Kind::Enum, "RelationLabel", 0, 0),
        Node::new(Kind::Enum, "RelationDirection", 0, 0),
        Node::new(Kind::Struct, "Unindexed", 0, 0),
    ];
    nodes[0].line = 138;
    nodes[1].line = 138;
    nodes[2].line = 999;
    let world = Arc::new(
        World::new(
            vec![Package {
                name: "synthetic-present-v1".into(),
                version: "1.0.0".into(),
                yours: true,
                external: false,
                deps: vec![],
            }],
            vec![Module {
                pkg: 0,
                path: "glyph".into(),
                file: "glyph.rs".into(),
            }],
            nodes,
            vec![],
        )
        .expect("synthetic graph v1"),
    );
    let identities = Arc::new(IdentityAdapter::synthetic(
        &world,
        PackageRef::parse("/fixture/present").expect("typed package"),
    ));
    let layout = facet::graph::layout::layout_of(&world);
    cx.set_global(TestFixture {
        scene: Arc::new(facet::graph::scene::Scene::new(world, layout)),
        identities,
    });
}

impl Map {
    pub(crate) fn new(links: Links, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let events = cx.subscribe_in(
            &links.store,
            window,
            |map, _, event: &StoreEvent, window, cx| {
                if event.is_branch(Branch::Route)
                    || event.is_branch(Branch::Root)
                    || event.is_branch(Branch::Overlay)
                {
                    map.invalidate_open();
                    map.error = None;
                    if event.is_branch(Branch::Root) {
                        map.resolved.clear();
                        map.routed_focus = None;
                    }
                    map.publish_focus(cx);
                }
                if matches!(event, StoreEvent::Resource(_)) {
                    map.resolve_open(window, cx);
                    map.publish_focus(cx);
                    cx.notify();
                }
            },
        );
        let open_intents = links.root.upgrade().map(|root| {
            cx.subscribe_in(
                &root,
                window,
                |map, owner, request: &GraphViewRequest, window, cx| {
                    let snapshot = map.links.snapshot(cx);
                    if owner.read(cx).graph_view_generation() == request.sequence
                        && snapshot.route() == &request.route
                        && snapshot.key() == request.root
                        && snapshot.overlay().is_none()
                        && map.visible
                    {
                        map.open_current(request.target, window, cx);
                    }
                },
            )
        });
        #[cfg(not(test))]
        let injected: Option<(Arc<facet::graph::scene::Scene>, Arc<IdentityAdapter>)> = None;
        #[cfg(test)]
        let injected = cx
            .try_global::<TestFixture>()
            .map(|fixture| (fixture.scene.clone(), fixture.identities.clone()));
        let (loading, ready_scene) = if let Some(scene) = injected {
            (None, Some(scene))
        } else {
            let load = cx.background_executor().spawn(async {
                let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../Nudox-Design-System/v4/graph/world.json");
                let bytes =
                    std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
                let world = Arc::new(World::from_json(&bytes).map_err(|error| error.to_string())?);
                let identities = Arc::new(IdentityAdapter::load(
                    &world,
                    &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
                ));
                let layout = facet::graph::layout::layout_of(&world);
                Ok::<_, String>((
                    Arc::new(facet::graph::scene::Scene::new(world, layout)),
                    identities,
                ))
            });
            let loading = cx.spawn(async move |map, cx| {
                let loaded = load.await;
                let _ = map.update(cx, |map, cx| {
                    map.loading = None;
                    match loaded {
                        Ok(scene) => {
                            // Creation needs the window, so retain the scene until render.
                            map.ready_scene = Some(scene);
                        }
                        Err(error) => map.load_error = Some(error),
                    }
                    cx.notify();
                });
            });
            (Some(loading), None)
        };
        Self {
            links,
            graph: None,
            loading,
            ready_scene,
            identities: None,
            entry_origin: None,
            painted_focus: None,
            canvas_transform: gpui::LayerTransform::IDENTITY,
            error: None,
            load_error: None,
            visible: false,
            focus_on_mount: false,
            route: None,
            routed_focus: None,
            semantic_focus: None,
            revealed_focus: None,
            resolved: BTreeMap::new(),
            pending: None,
            open_generation: 0,
            _graph_events: None,
            _open_intents: open_intents,
            _events: events,
        }
    }

    /// A capture waits for the mounted map's discovery index and any routed
    /// lookup, as well as the fixture parsing/layout worker.
    pub(crate) fn ready(&self, cx: &App) -> bool {
        self.pending.is_none()
            && (!self.visible
                || self.load_error.is_some()
                || (self.loading.is_none()
                    && self.ready_scene.is_none()
                    && self.pending.is_none()
                    && self
                        .graph
                        .as_ref()
                        .is_some_and(|graph| graph.read(cx).ready())))
    }

    pub(crate) fn report(&self, cx: &App) -> String {
        self.graph.as_ref().map_or_else(
            || {
                self.load_error
                    .clone()
                    .or_else(|| self.error.clone())
                    .unwrap_or_else(|| "loading".into())
            },
            |graph| {
                let entity = graph.entity_id();
                let graph = graph.read(cx);
                format!(
                    "fixture {} nodes, entity {entity:?}, focus {:?}, camera {:?}{}",
                    graph.world().len(),
                    graph.focused(),
                    graph.camera(),
                    self.error
                        .as_ref()
                        .map_or(String::new(), |error| format!(", {error}"))
                )
            },
        )
    }

    #[cfg(feature = "visual-harness")]
    pub(crate) fn inspection(&self, cx: &App) -> facet::gallery::json::Json {
        use facet::gallery::json::Json;
        let rect = |bounds: gpui::Bounds<gpui::Pixels>| {
            Json::obj([
                ("x", Json::num(f32::from(bounds.origin.x))),
                ("y", Json::num(f32::from(bounds.origin.y))),
                ("width", Json::num(f32::from(bounds.size.width))),
                ("height", Json::num(f32::from(bounds.size.height))),
            ])
        };
        let graph = self.graph.as_ref().map(|graph| graph.read(cx));
        Json::obj([
            ("visible", Json::Bool(self.visible)),
            ("ready", Json::Bool(self.ready(cx))),
            (
                "entity",
                self.graph.as_ref().map_or(Json::Null, |graph| {
                    Json::str(format!("{:?}", graph.entity_id()))
                }),
            ),
            (
                "nodes",
                graph.map_or(Json::Null, |graph| Json::num(graph.world().len() as f64)),
            ),
            (
                "focus",
                graph.map_or(Json::Null, |graph| Json::opt(graph.focused())),
            ),
            (
                "camera",
                graph
                    .and_then(GraphView::camera)
                    .map_or(Json::Null, |camera| {
                        Json::obj([
                            ("x", Json::num(camera.x)),
                            ("y", Json::num(camera.y)),
                            ("w", Json::num(camera.w)),
                        ])
                    }),
            ),
            (
                "focus_bounds",
                graph
                    .and_then(GraphView::focus_bounds)
                    .map_or(Json::Null, rect),
            ),
            (
                "painted_gem",
                self.painted_focus
                    .map_or(Json::Null, |(node, bounds, morphing)| {
                        Json::obj([
                            ("node", Json::num(node)),
                            ("bounds", rect(bounds)),
                            ("morphing", Json::Bool(morphing)),
                        ])
                    }),
            ),
            ("request_generation", Json::num(self.open_generation as f64)),
            (
                "pending_node",
                Json::opt(self.pending.as_ref().map(|request| request.node)),
            ),
            (
                "error",
                self.load_error
                    .as_ref()
                    .or(self.error.as_ref())
                    .map_or(Json::Null, Json::str),
            ),
        ])
    }

    fn invalidate_open(&mut self) {
        self.open_generation = self.open_generation.wrapping_add(1);
        self.pending = None;
    }

    pub(crate) fn suspend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.visible {
            self.invalidate_open();
            if let Some(graph) = &self.graph {
                graph.update(cx, |graph, cx| graph.suspend(window, cx));
            }
        }
        self.visible = false;
        self.publish_focus(cx);
    }

    pub(crate) fn show(
        &mut self,
        route: &Route,
        source: Option<&crate::model::pages::SymbolRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changed_route = self.route.as_ref() != Some(route);
        let arriving = !self.visible || changed_route;
        if arriving {
            self.focus_on_mount = true;
            self.painted_focus = None;
            self.entry_origin = route_symbol(route).and_then(|symbol| {
                let key = crate::shell::kit::shared_id(&symbol);
                if source == Some(&symbol) {
                    facet::motion::shared::capture(key, window, cx)
                } else {
                    facet::motion::shared::forget(key, window, cx);
                    None
                }
            });
            self.route = Some(route.clone());
            if changed_route {
                self.routed_focus = None;
            }
            self.invalidate_open();
            self.error = None;
        }
        self.visible = true;
        if arriving {
            self.publish_focus(cx);
        }
        let Some(graph) = self.graph.clone() else {
            return;
        };
        if changed_route && matches!(route, Route::World) && self.revealed_focus.is_none() {
            graph.update(cx, |graph, cx| graph.show_world(cx));
            self.painted_focus = None;
        }
        if let Some(at) = route.at() {
            self.error = Some(format!(
                "Graph fixture is pinned; release {} is not re-scoped by this map.",
                at.as_str()
            ));
            return;
        }
        if let Some(node) = self.revealed_focus.take() {
            self.routed_focus = Some(node);
            graph.update(cx, |graph, cx| graph.enter(node, cx));
            return;
        }
        let Some(symbol) = route_symbol(route) else {
            return;
        };
        if self.routed_focus.is_some() {
            return;
        }
        let resource = self.links.store.read(cx).symbol(&symbol);
        let Some(page) = resource.loaded_value() else {
            return;
        };
        if resource.value_root() != Some(self.links.snapshot(cx).key()) {
            return;
        }
        let Some(identities) = &self.identities else {
            return;
        };
        let package = match route {
            Route::Symbol(route) => PackageRef::parse(route.package.as_str()).ok(),
            _ => None,
        };
        let Some(package) = package else { return };
        let candidates = identities.candidates(&page.identity, &package);
        if let [id] = candidates.as_slice() {
            self.routed_focus = Some(*id);
            self.resolved.insert(
                *id,
                ResolvedSymbol {
                    symbol,
                    package,
                    line: page.identity.line,
                },
            );
            self.publish_focus(cx);
            if graph.read(cx).focused() != Some(*id) {
                graph.update(cx, |graph, cx| graph.enter(*id, cx));
            }
        } else {
            self.error = Some(format!(
                "This indexed declaration has {} exact matches in the graph fixture.",
                candidates.len()
            ));
        }
    }

    /// An explicit new World command is distinct from restoring a World
    /// history entry whose route has the same value.
    pub(crate) fn reset_world(&mut self, cx: &mut Context<Self>) {
        self.invalidate_open();
        self.routed_focus = None;
        self.revealed_focus = None;
        self.painted_focus = None;
        self.entry_origin = None;
        if let Some(graph) = &self.graph {
            graph.update(cx, |graph, cx| graph.show_world(cx));
        }
    }

    pub(crate) fn focused(&self, cx: &App) -> bool {
        self.graph
            .as_ref()
            .is_some_and(|graph| graph.read(cx).focused().is_some())
    }

    /// Uses the same indexed open path as Enter/double-click; the graph's
    /// current focus, rather than its original route, chooses the page.
    pub(crate) fn open_current(
        &mut self,
        target: OpenView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(node) = self
            .graph
            .as_ref()
            .and_then(|graph| graph.read(cx).focused())
        else {
            self.error = Some("Choose a graph symbol to open its page or code.".into());
            cx.notify();
            return;
        };
        self.open(node, target, OpenOrigin::Graph, window, cx);
    }

    #[cfg(test)]
    pub(crate) fn gem_morphing(&self) -> bool {
        self.painted_focus.is_some_and(|(_, _, morphing)| morphing)
    }

    #[cfg(test)]
    pub(crate) fn find_state(&self, window: &Window, cx: &App) -> (bool, bool) {
        self.graph.as_ref().map_or((false, false), |graph| {
            let graph = graph.read(cx);
            (graph.find_open(), graph.find_focused(window, cx))
        })
    }

    #[cfg(test)]
    pub(crate) fn focus_node(&mut self, node: NodeId, cx: &mut Context<Self>) {
        if let Some(graph) = &self.graph {
            graph.update(cx, |graph, cx| graph.enter(node, cx));
        }
    }

    /// Publish only semantic selection changes. Neither camera nor hover is
    /// part of this model; the data plane additionally equality-gates events.
    fn publish_focus(&self, cx: &mut Context<Self>) {
        use backend_library::DeclarationKind as D;
        use facet::graph::Kind as G;
        let snapshot = self.links.snapshot(cx);
        let focus = self.graph.as_ref().and_then(|graph| {
            if !self.visible || !is_graph(snapshot.route()) || snapshot.route().at().is_some() {
                return None;
            }
            let graph = graph.read(cx);
            let id = graph.focused()?;
            let world = graph.world();
            let node = world.node(id);
            let indexed = self
                .resolved
                .get(&id)
                .map(|resolved| (resolved.package.clone(), resolved.symbol.clone()))
                .or_else(|| {
                    let package = crate::runtime::store::route_package(snapshot.route())?;
                    let store = self.links.store.read(cx);
                    let resource = store.package(&package);
                    if resource.value_root() != Some(snapshot.key()) {
                        return None;
                    }
                    let tree = resource.loaded_value()?.outline.known()?;
                    let symbol = self
                        .identities
                        .as_ref()?
                        .outline_symbol(id, &package, tree)?;
                    Some((package, symbol))
                });
            Some(crate::runtime::graph_focus::GraphFocus {
                visit: snapshot.route().clone(),
                root: snapshot.key(),
                node: id,
                name: Arc::from(world.name_of(id).as_ref()),
                package: Arc::from(world.packages[node.pkg as usize].name.as_ref()),
                module: Arc::from(world.modules[node.module as usize].path.as_ref()),
                kind: match node.kind {
                    G::Struct => D::Struct,
                    G::Enum => D::Enum,
                    G::Union => D::Union,
                    G::Trait => D::Trait,
                    G::Type => D::Type,
                    G::Function => D::Function,
                    G::Method => D::Method,
                    G::Macro => D::Macro,
                    G::Constant => D::Constant,
                    G::Field => D::Field,
                    G::Variant => D::Variant,
                    G::Other => D::Unknown,
                },
                indexed,
            })
        });
        let notice = (!self.visible)
            .then(|| self.error.as_ref())
            .flatten()
            .map(|error| crate::runtime::graph_focus::GraphNotice {
                visit: snapshot.route().clone(),
                root: snapshot.key(),
                message: Arc::from(error.as_str()),
            });
        self.links
            .store
            .update(cx, |store, cx| store.admit_graph_focus(focus, notice, cx));
    }

    fn callback_basis(&self, cx: &App) -> CallbackBasis {
        let snapshot = self.links.snapshot(cx);
        CallbackBasis {
            route: snapshot.route().clone(),
            root: snapshot.key(),
            generation: self.open_generation,
        }
    }

    fn callback_current(&self, basis: &CallbackBasis, cx: &App) -> bool {
        let snapshot = self.links.snapshot(cx);
        basis.generation == self.open_generation
            && basis.route == *snapshot.route()
            && basis.root == snapshot.key()
            && snapshot.overlay().is_none()
    }

    fn peek_action(
        &mut self,
        node: NodeId,
        action: facet::graph::peek::Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.links.snapshot(cx);
        if snapshot.overlay().is_some() {
            return;
        }
        if self
            .route
            .as_ref()
            .is_some_and(|route| route.at().is_some())
        {
            self.error = Some("This graph fixture cannot focus a symbol at the viewed release; return to your pin first.".into());
            self.publish_focus(cx);
            cx.notify();
            return;
        }
        let Some(graph) = self.graph.clone() else {
            return;
        };
        if node as usize >= graph.read(cx).world().len() {
            return;
        }
        match action {
            facet::graph::peek::Action::Open => {
                self.open(node, OpenView::Page, OpenOrigin::Peek, window, cx)
            }
            facet::graph::peek::Action::Focus => {
                self.invalidate_open();
                self.error = None;
                self.routed_focus = Some(node);
                if self.visible && is_graph(snapshot.route()) {
                    graph.update(cx, |graph, cx| graph.enter(node, cx));
                } else {
                    self.revealed_focus = Some(node);
                    let route = self.route.clone().filter(is_graph).unwrap_or(Route::World);
                    self.links.dispatch(Intent::Navigate(route), cx);
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn graph_entity(&self) -> Option<Entity<GraphView>> {
        self.graph.clone()
    }

    fn open(
        &mut self,
        node: NodeId,
        target: OpenView,
        origin: OpenOrigin,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.links.snapshot(cx);
        let focus = self
            .graph
            .as_ref()
            .and_then(|graph| graph.read(cx).focused());
        if snapshot.overlay().is_some()
            || (origin == OpenOrigin::Graph
                && (!self.visible
                    || !is_graph(snapshot.route())
                    || self.route.as_ref() != Some(snapshot.route())))
        {
            return;
        }
        if (origin == OpenOrigin::Graph && snapshot.route().at().is_some())
            || (origin == OpenOrigin::Peek
                && self
                    .route
                    .as_ref()
                    .is_some_and(|route| route.at().is_some()))
        {
            self.error = Some("This graph fixture cannot open a symbol at the viewed release; return to your pin first.".into());
            self.publish_focus(cx);
            cx.notify();
            return;
        }
        self.error = None;
        self.invalidate_open();
        self.publish_focus(cx);
        if let Some(resolved) = self.resolved.get(&node).cloned() {
            self.navigate(
                resolved,
                target,
                self.visible.then(|| self.anchor_for(node, cx)).flatten(),
                window,
                cx,
            );
            return;
        }
        let Some(graph) = &self.graph else { return };
        let Ok(query) = SearchQuery::new(graph.read(cx).world().node(node).name.as_ref(), 200)
        else {
            return;
        };
        self.pending = Some(OpenRequest {
            generation: self.open_generation,
            node,
            target,
            origin,
            continuations: Vec::new(),
            focus_at_open: focus,
            source_at_open: self.visible.then(|| self.anchor_for(node, cx)).flatten(),
            query: query.clone(),
            route: snapshot.route().clone(),
            root: snapshot.key(),
        });
        self.links.store.update(cx, |store, cx| {
            store.ensure(PageKey::Search(query), cx);
        });
        self.resolve_open(window, cx);
        cx.notify();
    }

    fn resolve_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(request) = self.pending.clone() else {
            return;
        };
        let (snapshot, resource) = {
            let store = self.links.store.read(cx);
            (store.snapshot(), store.search(&request.query))
        };
        let focus = self
            .graph
            .as_ref()
            .and_then(|graph| graph.read(cx).focused());
        if !request.accepts(
            self.open_generation,
            snapshot.route(),
            snapshot.key(),
            focus,
            (self.visible || request.origin == OpenOrigin::Peek) && snapshot.overlay().is_none(),
        ) {
            self.invalidate_open();
            return;
        }
        let page = match open_value(&resource, request.root) {
            Ok(Some(page)) => page,
            Ok(None) => return,
            Err(error) => {
                self.error = Some(error);
                self.pending = None;
                return;
            }
        };
        let Some(identities) = &self.identities else {
            return;
        };
        let resolution = identities.resolve(request.node, &page.rows);
        // One row in a partial search is not proof of unique identity: an
        // exact duplicate on a later page must remain an explicit ambiguity.
        if page.next.is_some() && matches!(&resolution, Ok(_) | Err(MatchFailure::MissingIndex)) {
            let next = page.next.expect("guarded continuation");
            if request.continuations.contains(&next) {
                self.pending = None;
                self.error = Some(
                    "The index repeated a search continuation without resolving this graph symbol."
                        .into(),
                );
                return;
            }
            self.pending
                .as_mut()
                .expect("current request")
                .continuations
                .push(next);
            let issued = self
                .links
                .store
                .update(cx, |store, cx| store.load_more(&request.query, cx));
            if !issued {
                self.pending = None;
                self.error = Some("The index cannot continue this graph symbol lookup.".into());
            }
            return;
        }
        match resolution {
            Ok(resolved) => {
                self.pending = None;
                self.resolved.insert(request.node, resolved.clone());
                // Re-read this same node after resize or camera motion. An
                // offscreen/disappeared source supplies no morph anchor.
                self.navigate(
                    resolved,
                    request.target,
                    request
                        .source_at_open
                        .and_then(|_| self.anchor_for(request.node, cx)),
                    window,
                    cx,
                );
            }
            Err(error) => {
                self.pending = None;
                self.error = Some(format!(
                    "{error}{}. Its page is unavailable.",
                    if page.next.is_some() {
                        " in this search page"
                    } else {
                        ""
                    }
                ))
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn canvas_geometry(
        &self,
        node: NodeId,
        cx: &App,
    ) -> (
        Option<gpui::Bounds<gpui::Pixels>>,
        Option<gpui::Bounds<gpui::Pixels>>,
        gpui::LayerTransform,
    ) {
        (
            self.anchor_for(node, cx),
            self.painted_focus.map(|(_, bounds, _)| bounds),
            self.canvas_transform,
        )
    }

    fn anchor_for(&self, node: NodeId, cx: &App) -> Option<gpui::Bounds<gpui::Pixels>> {
        let graph = self.graph.as_ref()?.read(cx);
        handoff_anchor(
            node,
            graph.focused(),
            graph
                .node_bounds(node)
                .map(|bounds| self.canvas_transform.apply_bounds(bounds)),
            self.painted_focus,
        )
    }

    fn navigate(
        &self,
        ResolvedSymbol {
            symbol,
            package,
            line,
        }: ResolvedSymbol,
        target: OpenView,
        anchor: Option<gpui::Bounds<gpui::Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = crate::shell::kit::shared_id(&symbol);
        if let Some(bounds) = anchor {
            facet::motion::shared::remember(key, bounds, window, cx);
        } else {
            facet::motion::shared::forget(key, window, cx);
        }
        let snapshot = self.links.snapshot(cx);
        if route_symbol(snapshot.route()).as_ref() == Some(&symbol) {
            if let Some(route) = snapshot.route().with_view(target.view()) {
                self.links.dispatch(Intent::Navigate(route), cx);
            }
        } else if let Some(route) =
            crate::shell::kit::symbol_view_route(package.as_str(), &symbol, target.view(), line)
        {
            self.links.dispatch(Intent::Navigate(route), cx);
        }
    }
}

/// A last-good value cannot settle the latest root's open. Activity is
/// checked first because an active retry retains its previous terminal too.
fn open_value<T>(resource: &Resource<T>, root: VersionedRoot) -> Result<Option<&T>, String> {
    if matches!(
        resource.activity(),
        Activity::Waiting | Activity::Working | Activity::NotYet
    ) {
        return Ok(None);
    }
    match resource.terminal() {
        ResourceTerminal::Fault(error) => Err(error.message().to_owned()),
        ResourceTerminal::Unavailable(_) => {
            Err("The local index cannot resolve this graph symbol.".into())
        }
        ResourceTerminal::Complete if resource.value_root() == Some(root) => {
            Ok(resource.loaded_value())
        }
        ResourceTerminal::Complete => {
            Err("The index did not return this graph symbol at the current root.".into())
        }
    }
}

/// The map is deliberately a fixture until a typed world query exists.
pub(crate) fn is_graph(route: &Route) -> bool {
    matches!(
        route,
        Route::World
            | Route::Symbol(crate::navigation::SymbolRoute {
                view: View::Graph,
                ..
            })
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OpenOrigin {
    Graph,
    Peek,
}

/// A deferred native callback cannot survive a different content visit.
#[derive(Clone)]
struct CallbackBasis {
    route: Route,
    root: VersionedRoot,
    generation: u64,
}

/// An open belongs to the exact visible route, root and focused symbol.
/// Even returning to the same route cannot revive a superseded generation.
#[derive(Clone, Debug)]
struct OpenRequest {
    generation: u64,
    node: NodeId,
    target: OpenView,
    origin: OpenOrigin,
    continuations: Vec<SearchContinuation>,
    focus_at_open: Option<NodeId>,
    source_at_open: Option<gpui::Bounds<gpui::Pixels>>,
    query: SearchQuery,
    route: Route,
    root: VersionedRoot,
}

impl OpenRequest {
    fn accepts(
        &self,
        generation: u64,
        route: &Route,
        root: VersionedRoot,
        focus: Option<NodeId>,
        visible: bool,
    ) -> bool {
        self.generation == generation
            && self.route == *route
            && self.root == root
            && focus == self.focus_at_open
            && visible
            && (self.origin == OpenOrigin::Peek || is_graph(route))
    }
}

/// A different focused node cannot supply the opened node's geometry.
/// An in-progress shared gem does supply its actual paint for interruption;
/// a resting gem uses this frame's node projection, including a resize.
fn handoff_anchor(
    node: NodeId,
    focus: Option<NodeId>,
    actual: Option<gpui::Bounds<gpui::Pixels>>,
    painted: Option<(NodeId, gpui::Bounds<gpui::Pixels>, bool)>,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    let actual = actual?;
    if let Some((painted_node, bounds, true)) = painted
        && painted_node == node
        && focus == Some(node)
    {
        return Some(bounds);
    }
    Some(actual)
}

/// Layout-transparent canvas endpoint. It rereads the moving node during
/// prepaint, after GraphView prepared this frame's camera. At rest the
/// canvas draws the node; the gem is visible only during a real shared morph.
struct FocusMark {
    graph: Entity<GraphView>,
    owner: gpui::WeakEntity<Map>,
}

impl IntoElement for FocusMark {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for FocusMark {
    type RequestLayoutState = ();
    type PrepaintState = Option<gpui::AnyElement>;
    fn id(&self) -> Option<gpui::ElementId> {
        Some("graph-focus-shared".into())
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, ()) {
        let mut style = gpui::Style::default();
        style.size.width = gpui::relative(1.0).into();
        style.size.height = gpui::relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: gpui::Bounds<gpui::Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Option<gpui::AnyElement> {
        let owner = self.owner.upgrade()?;
        let visible = owner.update(cx, |map, _| {
            // GraphView supplies layout-space window bounds. The actual
            // canvas also inherits every native parent compositing layer.
            map.canvas_transform = window.layer_transform();
            map.visible
        });
        if !visible {
            return None;
        }
        let graph = self.graph.read(cx);
        let Some(node) = graph.focused() else {
            let _ = self.owner.update(cx, |map, _| map.painted_focus = None);
            return None;
        };
        let bounds = match graph.node_bounds(node) {
            Some(bounds) => bounds,
            None => {
                let _ = self.owner.update(cx, |map, _| map.painted_focus = None);
                return None;
            }
        };
        let kind = match graph.world().node(node).kind {
            facet::graph::Kind::Trait => facet::icons::Kind::Trait,
            facet::graph::Kind::Function | facet::graph::Kind::Method => {
                facet::icons::Kind::Function
            }
            facet::graph::Kind::Enum => facet::icons::Kind::Enum,
            _ => facet::icons::Kind::Struct,
        };
        let Some((symbol, origin, previous_node)) = owner.update(cx, |map, _| {
            let Some(resolved) = map.resolved.get(&node) else {
                // A producer-root change can remove the indexed join while
                // the fixture's selected node remains. No old ghost survives.
                map.painted_focus = None;
                return None;
            };
            Some((
                resolved.symbol.clone(),
                map.entry_origin.take(),
                map.painted_focus.map(|(node, _, _)| node),
            ))
        }) else {
            return None;
        };
        let key = crate::shell::kit::shared_id(&symbol);
        if let Some(origin) = origin {
            if !facet::motion::shared::resume(origin, key.clone(), window, cx) {
                facet::motion::shared::forget(key.clone(), window, cx);
            }
        } else if previous_node != Some(node) {
            // Canvas selection is not a handoff from a hidden previous page.
            facet::motion::shared::forget(key.clone(), window, cx);
        }
        let size = f32::from(bounds.size.height);
        let morphing = Rc::new(std::cell::Cell::new(false));
        let seen_morph = morphing.clone();
        let mut mark = facet::motion::shared::shared_with(key.clone(), move |morph| {
            seen_morph.set(morph.from.is_some());
            facet::paint::gem(kind)
                .size(size)
                .opacity(if morph.from.is_some() {
                    1.0 - morph.t
                } else {
                    0.0
                })
        })
        .timing(
            std::time::Duration::from_millis(460),
            facet::tokens::motion::GLIDE,
        )
        .into_any_element();
        mark.layout_as_root(bounds.size.into(), window, cx);
        mark.prepaint_at(bounds.origin, window, cx);
        let painted = facet::motion::shared::last_bounds(key, window, cx);
        let _ = self.owner.update(cx, |map, _| {
            map.painted_focus = painted.map(|bounds| (node, bounds, morphing.get()))
        });
        Some(mark)
    }
    fn paint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: gpui::Bounds<gpui::Pixels>,
        _: &mut (),
        mark: &mut Option<gpui::AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(mark) = mark {
            mark.paint(window, cx);
        }
    }
}

impl Render for Map {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some((scene, identities)) = self.ready_scene.take() {
            self.identities = Some(identities);
            let owner = cx.entity().downgrade();
            let peek_owner = owner.clone();
            self.graph = Some(cx.new(|cx| {
                let mut graph = GraphView::with_scene(scene, Start::World, window, cx);
                graph.on_open(Rc::new(move |node, window, cx| {
                    let Some(map) = owner.upgrade() else {
                        return;
                    };
                    let basis = map.read(cx).callback_basis(cx);
                    window.defer(cx, move |window, cx| {
                        map.update(cx, |map, cx| {
                            if map.callback_current(&basis, cx) {
                                map.open(node, OpenView::Page, OpenOrigin::Graph, window, cx);
                            }
                        });
                    });
                }));
                graph.on_peek_action(Rc::new(move |node, action, window, cx| {
                    let Some(map) = peek_owner.upgrade() else {
                        return;
                    };
                    let basis = map.read(cx).callback_basis(cx);
                    window.defer(cx, move |window, cx| {
                        map.update(cx, |map, cx| {
                            if map.callback_current(&basis, cx) {
                                map.peek_action(node, action, window, cx);
                            }
                        });
                    });
                }));
                graph
            }));
            if let Some(graph) = &self.graph {
                self._graph_events =
                    Some(cx.observe(graph, |map, graph, cx| {
                        if map.pending.as_ref().is_some_and(|request| {
                            graph.read(cx).focused() != request.focus_at_open
                        }) {
                            map.invalidate_open();
                        }
                        let focus = graph.read(cx).focused();
                        if map.semantic_focus != focus {
                            map.semantic_focus = focus;
                            map.publish_focus(cx);
                        }
                        // GPUI dirties the child view's ancestor path; its
                        // canvas and this endpoint share the same prepaint.
                    }));
            }
        }
        let route = self.route.clone();
        if self.visible
            && let Some(route) = route
        {
            self.show(&route, None, window, cx);
        }
        let mut root = div().relative().size_full();
        if let Some(graph) = &self.graph {
            if self.focus_on_mount && self.visible {
                graph.focus_handle(cx).focus(window, cx);
                self.focus_on_mount = false;
            }
            root = root.child(graph.clone()).child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .child(FocusMark {
                        graph: graph.clone(),
                        owner: cx.entity().downgrade(),
                    }),
            );
        } else {
            root = root.flex().items_center().justify_center().child(
                self.load_error
                    .clone()
                    .unwrap_or_else(|| "Laying out the graph fixture…".into()),
            );
        }
        let root = root.child(
            div()
                .absolute()
                .bottom(px(8.0))
                .left(px(16.0))
                .text_size(px(11.0))
                .child(
                    self.load_error
                        .clone()
                        .or_else(|| self.error.clone())
                        .unwrap_or_else(|| {
                            if self.pending.is_some() {
                                "Resolving this fixture symbol in the local index…".into()
                            } else {
                                "Graph fixture · pages resolve through your local index".into()
                            }
                        }),
                ),
        );
        #[cfg(test)]
        {
            let layer = cx
                .try_global::<TestCanvasLayer>()
                .copied()
                .unwrap_or(TestCanvasLayer {
                    scale: 1.0,
                    x: 0.0,
                    y: 0.0,
                });
            gpui::layer(root)
                .origin(0.0, 0.0)
                .scale(layer.scale)
                .translate(gpui::point(px(layer.x), px(layer.y)))
        }
        #[cfg(not(test))]
        {
            root
        }
    }
}

// Graphs mount at the reader's full bounds; a leaving reader page has no
// duplicate graph entity or placeholder gem.
pub(super) fn body(_: &Route, _: &super::Pages, _: &mut super::Ctx<'_>) -> Vec<super::Leaf> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_value_failure_settles_but_active_retry_waits_for_its_new_root() {
        use crate::core::{FaultCode, UnavailableReason};
        let r1 = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("graph".into(), "resource-v1".into())]),
            1,
        );
        let r2 = r1.with_generation(2);
        let old = Resource::loaded_at(7_u32, r1);
        assert_eq!(open_value(&old.clone().waiting(), r2), Ok(None));
        let failed = old
            .clone()
            .mark_error(FaultCode::Transport, "new-root lookup failed");
        assert_eq!(
            failed.loaded_value(),
            Some(&7),
            "the test really retains the old value"
        );
        assert_eq!(
            open_value(&failed, r2),
            Err("new-root lookup failed".into())
        );
        assert_eq!(
            open_value(&failed.clone().waiting(), r2),
            Ok(None),
            "a new retry retains the previous fault while queued"
        );
        assert_eq!(
            open_value(&failed.working(), r2),
            Ok(None),
            "a running retry owns its next result"
        );
        assert_eq!(
            open_value(&Resource::loaded_at(9_u32, r2), r2),
            Ok(Some(&9))
        );
        let unavailable = old.mark_unavailable(UnavailableReason::Unsupported);
        assert!(
            open_value(&unavailable, r2).is_err(),
            "terminal unavailability cannot hang behind retained data"
        );
    }

    #[test]
    fn clicked_node_geometry_survives_resize_interruption_and_disappearance() {
        let at = |x| {
            gpui::Bounds::new(
                gpui::point(gpui::px(x), gpui::px(40.0)),
                gpui::size(gpui::px(12.0), gpui::px(12.0)),
            )
        };
        let a = at(100.0);
        let b = at(300.0);
        let flying_b = at(220.0);
        assert_eq!(
            handoff_anchor(2, Some(1), Some(b), Some((1, a, true))),
            Some(b),
            "doubleclicked B never borrows A's geometry"
        );
        assert_eq!(
            handoff_anchor(2, Some(2), Some(b), Some((2, flying_b, true))),
            Some(flying_b),
            "interruption starts at the painted gem"
        );
        assert_eq!(
            handoff_anchor(2, Some(2), Some(b), Some((2, a, false))),
            Some(b),
            "resize uses the new projection at rest"
        );
        assert_eq!(
            handoff_anchor(2, Some(2), None, Some((2, flying_b, true))),
            None,
            "a disappeared source cannot seed stale geometry"
        );
    }

    #[test]
    fn an_open_rejects_changed_focus_route_root_visibility_or_generation() {
        let root = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("graph".to_owned(), "requests".to_owned())]),
            1,
        );
        let request = OpenRequest {
            generation: 5,
            node: 7,
            target: OpenView::Page,
            origin: OpenOrigin::Graph,
            continuations: Vec::new(),
            focus_at_open: Some(7),
            source_at_open: None,
            query: SearchQuery::new("Value", 200).expect("query"),
            route: Route::World,
            root,
        };
        assert!(request.accepts(5, &Route::World, root, Some(7), true));
        assert!(
            !request.accepts(6, &Route::World, root, Some(7), true),
            "a newer request supersedes the previous one"
        );
        assert!(
            !request.accepts(5, &Route::World, root, Some(8), true),
            "the camera's selected symbol changed"
        );
        assert!(
            !request.accepts(5, &Route::World, root, None, true),
            "the source disappeared"
        );
        assert!(
            !request.accepts(5, &Route::World, root.with_generation(2), Some(7), true),
            "a retained last-good read is stale"
        );
        assert!(!request.accepts(
            5,
            &Route::Orbit(crate::navigation::OrbitRoute::Home),
            root,
            Some(7),
            true
        ));
        assert!(
            !request.accepts(5, &Route::World, root, Some(7), false),
            "overlays or hidden graphs cannot complete an open"
        );
        let doubleclicked = OpenRequest {
            focus_at_open: Some(8),
            ..request.clone()
        };
        assert!(
            doubleclicked.accepts(5, &Route::World, root, Some(8), true),
            "doubleclick B may open B while A remains focused"
        );
        assert!(
            !doubleclicked.accepts(5, &Route::World, root, Some(9), true),
            "a later focus supersedes doubleclicked B"
        );
        let graph_route = crate::shell::tests::view_route("RelationLabel", View::Graph);
        let scoped = OpenRequest {
            route: graph_route.clone(),
            ..request
        };
        let other_release = graph_route.with_release(Some(
            crate::navigation::ReleaseId::new("0.3.0").expect("release"),
        ));
        assert!(!scoped.accepts(5, &other_release, root, Some(7), true));
    }
}
