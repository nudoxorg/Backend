//! The **fixture** world: a snapshot of the prototype graph's
//! `Nudox-Design-System/v4/graph/world.json` (with the `repo/` and
//! `registry/` sources it was extracted from), loaded once per process and
//! joined to indexed declarations by exact source identity.
//!
//! It is a stand-in. The graph body and the symbol page's anatomy both read
//! facet's [`World`]; until the runtime projects one from the index's own
//! data, this snapshot is that world. What it does not know — a declaration
//! outside the snapshot, a package it never walked, a line that has moved
//! since it was taken — gets no anatomy, and the page keeps the body built
//! from the index's page data: the anatomy is an enhancement, never a gate.
//!
//! Callers ask for a declaration's `(World, NodeId)` through [`anatomy`] and
//! map a node back through [`symbol_of`]; none of them names a file, so a
//! world projected from the index replaces this module and nothing else.

use crate::model::pages::{DeclRef, OutlineTree, PackageRef, SymbolRef};
use crate::shell::bodies::graph::identity::IdentityAdapter;
use facet::anatomy::Links;
use facet::graph::{NodeId, Package, World};
use facet::overlay::peek::Peek;
use facet::semantics::model::{RecipeView, Use};
use facet::semantics::names::Names;
use facet::semantics::page::{self, Page};
use facet::semantics::recipes::{PreparedRecipes, Recipes};
use facet::semantics::tour::{self, Tour};
use facet::semantics::types::Target;
use gpui::{App, Global, SharedString};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

/// The fixture world and its join to indexed declarations.
pub(crate) type Loaded = (Arc<World>, Arc<IdentityAdapter>);

/// The snapshot's folder.
fn folder() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Nudox-Design-System/v4/graph")
}

static FIXTURE: OnceLock<Result<Loaded, String>> = OnceLock::new();

/// The fixture world, loaded on first call and shared by every later one.
/// Blocks the calling thread while it parses (16 MB): call it from a
/// background task.
pub(crate) fn blocking() -> Result<Loaded, String> {
    FIXTURE
        .get_or_init(|| {
            let path = folder().join("world.json");
            let reading = std::time::Instant::now();
            let bytes = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            let world = Arc::new(World::from_json(&bytes).map_err(|error| error.to_string())?);
            super::trace::span("world.parse", reading, format_args!("{} bytes", bytes.len()));
            let joining = std::time::Instant::now();
            let identities = Arc::new(IdentityAdapter::load(
                &world,
                &Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
            ));
            super::trace::span("world.identities", joining, "IdentityAdapter::load");
            Ok((world, identities))
        })
        .clone()
}

/// What the symbol page draws from the world for one declaration.
pub(crate) struct Anatomy {
    /// The world it was read from.
    pub world: Arc<World>,
    /// The declaration's node.
    pub node: NodeId,
    /// Shape, `can` line and Does.
    pub page: Page,
    /// Getting one (a type) or Calling it (a callable), when it has one.
    pub recipe: Option<RecipeView>,
    /// Up to three statements that use it, with each caller's display name.
    pub uses: Vec<(Use, SharedString)>,
}

/// Where statement text comes from.
#[cfg_attr(test, allow(dead_code, reason = "tests install given texts"))]
enum Sources {
    /// The snapshot's `repo/` and `registry/` folders.
    Folder(PathBuf),
    /// Given texts by file (tests).
    #[cfg(test)]
    Given(HashMap<String, Arc<str>>),
}

impl Sources {
    fn read(&self, package: &Package, file: &str) -> Option<Arc<str>> {
        match self {
            Self::Folder(root) => {
                let dir = if package.external { "registry" } else { "repo" };
                std::fs::read_to_string(root.join(dir).join(file)).ok().map(Arc::from)
            }
            #[cfg(test)]
            Self::Given(files) => files.get(file).cloned(),
        }
    }
}

/// The page side's tables over one world, built on the UI thread (the
/// recipe table keeps per-thread caches).
struct Tables {
    world: Arc<World>,
    identities: Arc<IdentityAdapter>,
    names: Names,
    recipes: Recipes,
    sources: Sources,
    pages: HashMap<NodeId, Rc<Anatomy>>,
    readings: HashMap<u32, Option<Rc<Reading>>>,
    /// The last hand arranged, by what it held.
    hand: Option<(Vec<crate::model::hand::Held>, Rc<HandView>)>,
}

/// One card of the hand, as the rungs draw it.
#[derive(Clone, Debug)]
pub(crate) struct Card {
    /// What is held.
    pub held: crate::model::hand::Held,
    /// Its name (`Value`, `Page::new`).
    pub name: SharedString,
    /// Its mark.
    pub kind: facet::icons::Kind,
}

/// A road of cards and the verbs between them.
#[derive(Clone, Debug)]
pub(crate) struct RoadView {
    /// Indexes into [`HandView::cards`], in flow order.
    pub cards: Vec<usize>,
    /// Between each card and the next: the verb (`as`, `as_table`), or
    /// nothing for a direct feed.
    pub verbs: Vec<SharedString>,
    /// "from text to Table, in two steps".
    pub sentence: String,
}

/// The hand arranged: cards in the order they are shown (the roads, then
/// what stands apart in the order it was held).
#[derive(Clone, Debug, Default)]
pub(crate) struct HandView {
    /// Every card, in shown order.
    pub cards: Vec<Card>,
    /// The joined roads.
    pub roads: Vec<RoadView>,
    /// Cards that join nothing.
    pub apart: Vec<usize>,
}

/// A package's reading path: facet's tour over the world.
pub(crate) struct Reading {
    /// The world it was read from.
    pub world: Arc<World>,
    /// The stops, in the order a newcomer meets them.
    pub tour: Tour,
}

/// The tables' thread-free parts, built off the UI thread (the recipe
/// table takes ~100 ms on the full snapshot).
struct Prepared {
    world: Arc<World>,
    identities: Arc<IdentityAdapter>,
    names: Names,
    recipes: PreparedRecipes,
}

impl Prepared {
    fn new(world: Arc<World>, identities: Arc<IdentityAdapter>) -> Self {
        let preparing = std::time::Instant::now();
        let prepared = Self {
            names: Names::new(&world),
            recipes: Recipes::new(&world).into_prepared(),
            world,
            identities,
        };
        super::trace::span("world.tables", preparing, "Names + Recipes");
        prepared
    }
}

impl Tables {
    fn new(prepared: Prepared, sources: Sources) -> Self {
        Self {
            world: prepared.world,
            identities: prepared.identities,
            names: prepared.names,
            recipes: Recipes::from_prepared(prepared.recipes),
            sources,
            pages: HashMap::new(),
            readings: HashMap::new(),
            hand: None,
        }
    }

    fn node_of(&self, held: &crate::model::hand::Held) -> Option<NodeId> {
        let id = held.id.as_ref()?;
        let decl = DeclRef::from_label(id.as_str(), None, None, None)?;
        let package = PackageRef::parse(held.package.as_str()).ok()?;
        match self.identities.candidates(&decl, &package)[..] {
            [node] => Some(node),
            _ => None,
        }
    }

    fn hand(&mut self, held: &[crate::model::hand::Held]) -> Rc<HandView> {
        if let Some((known, view)) = &self.hand
            && known.as_slice() == held
        {
            return Rc::clone(view);
        }
        let world = &self.world;
        let nodes: Vec<Option<NodeId>> = held.iter().map(|card| self.node_of(card)).collect();
        let connector = facet::semantics::recipes::chain::Connector::new(&self.recipes, world);
        let known: Vec<NodeId> = nodes.iter().flatten().copied().collect();
        let arrangement = connector.arrange(&known);
        let card_of = |node: NodeId| held.iter().zip(&nodes).find(|(_, n)| **n == Some(node)).map(|(card, _)| card.clone());
        let make = |held: crate::model::hand::Held, node: Option<NodeId>| Card {
            name: node.map_or_else(|| name_of(&held), |node| world.name_of(node)),
            kind: node.map_or(facet::icons::Kind::Unknown, |node| crate::shell::kit::world_kind(world.node(node).kind)),
            held,
        };
        let mut view = HandView::default();
        let twins: std::collections::HashSet<String> = {
            let mut seen = std::collections::HashSet::new();
            known.iter().map(|&n| world.node(n).name.to_string()).filter(|name| !seen.insert(name.clone())).collect()
        };
        for road in &arrangement.roads {
            let mut cards = Vec::new();
            let mut verbs = Vec::new();
            for link in &road.links {
                if let Some(join) = &link.join {
                    verbs.push(match &join.how {
                        facet::semantics::recipes::chain::JoinHow::Direct => SharedString::default(),
                        facet::semantics::recipes::chain::JoinHow::As => SharedString::new_static("as"),
                        facet::semantics::recipes::chain::JoinHow::Steps(_) => connector.verbs(join).join(" · ").into(),
                    });
                }
                if let Some(card) = card_of(link.piece.node) {
                    cards.push(view.cards.len());
                    view.cards.push(make(card, Some(link.piece.node)));
                }
            }
            view.roads.push(RoadView { cards, verbs, sentence: connector.sentence(road, &twins).text() });
        }
        // What joins nothing, and what the world does not know, in the
        // order it was held.
        let shown: Vec<crate::model::hand::Held> = view.cards.iter().map(|card| card.held.clone()).collect();
        for (card, node) in held.iter().zip(&nodes) {
            if shown.iter().any(|seen| seen.same(card)) {
                continue;
            }
            view.apart.push(view.cards.len());
            view.cards.push(make(card.clone(), *node));
        }
        let view = Rc::new(view);
        self.hand = Some((held.to_vec(), Rc::clone(&view)));
        view
    }

    fn reading(&mut self, package: u32) -> Option<Rc<Reading>> {
        let (world, names) = (&self.world, &self.names);
        self.readings
            .entry(package)
            .or_insert_with(|| {
                let tour = tour::of(world, names, package);
                tour.shown().then(|| Rc::new(Reading { world: Arc::clone(world), tour }))
            })
            .clone()
    }

    fn anatomy(&mut self, node: NodeId) -> Rc<Anatomy> {
        if let Some(anatomy) = self.pages.get(&node) {
            return Rc::clone(anatomy);
        }
        let world = &self.world;
        let recipe = self
            .recipes
            .getting_one(world, node)
            .or_else(|| self.recipes.calling_it(world, node))
            .map(|section| section.view(world, node));
        let sources = &self.sources;
        let uses = page::in_use(world, node, &mut |package, file| sources.read(package, file))
            .into_iter()
            .map(|found| {
                let caller = world.name_of(found.caller);
                (found, caller)
            })
            .collect();
        let anatomy = Rc::new(Anatomy {
            world: Arc::clone(world),
            node,
            page: page::page(world, &self.names, node),
            recipe,
            uses,
        });
        self.pages.insert(node, Rc::clone(&anatomy));
        anatomy
    }
}

/// The service's state in the app.
#[cfg_attr(test, allow(dead_code, reason = "tests install a ready world and never load"))]
enum Service {
    Loading,
    Ready(Rc<RefCell<Tables>>),
    Failed,
}

impl Global for Service {}

/// Whether the optional pinned semantic world is still being prepared.
/// Failed or not-yet-requested worlds are settled states for the shell.
pub(crate) fn is_loading(cx: &App) -> bool {
    matches!(cx.try_global::<Service>(), Some(Service::Loading))
}

/// The anatomy of `decl` (in `package`) when the fixture world knows it:
/// exactly one node at its file, line and name.
///
/// `None` while the snapshot loads (the first ask starts it in the
/// background, and every window redraws once when it lands), when it failed
/// to load, or when it does not know the declaration. The page then keeps
/// its index-built body.
pub(crate) fn anatomy(decl: &DeclRef, package: &PackageRef, cx: &mut App) -> Option<Rc<Anatomy>> {
    let tables = match cx.try_global::<Service>() {
        Some(Service::Ready(tables)) => Rc::clone(tables),
        Some(Service::Loading | Service::Failed) => return None,
        None => {
            start(cx);
            return None;
        }
    };
    let mut tables = tables.borrow_mut();
    let [node] = tables.identities.candidates(decl, package)[..] else {
        return None;
    };
    Some(tables.anatomy(node))
}

/// The reading path of `package` (facet's tour, at least three stops), when
/// the world knows the package: it is found through the first of `decls` (the
/// package's own declarations) that joins to exactly one node.
pub(crate) fn reading<'a>(
    package: &PackageRef,
    decls: impl IntoIterator<Item = &'a DeclRef>,
    cx: &mut App,
) -> Option<Rc<Reading>> {
    let tables = match cx.try_global::<Service>() {
        Some(Service::Ready(tables)) => Rc::clone(tables),
        Some(Service::Loading | Service::Failed) => return None,
        None => {
            start(cx);
            return None;
        }
    };
    let mut tables = tables.borrow_mut();
    let owner = decls.into_iter().take(64).find_map(|decl| match tables.identities.candidates(decl, package)[..] {
        [node] => Some(tables.world.node(node).pkg),
        _ => None,
    })?;
    tables.reading(owner)
}

/// A held thing's name from its own spelling (a declaration's last segment,
/// a package's display name).
fn name_of(held: &crate::model::hand::Held) -> SharedString {
    match &held.id {
        Some(id) => SharedString::from(backend_present::Identity::parse(id.as_str()).name().to_owned()),
        None => PackageRef::parse(held.package.as_str())
            .map_or_else(|_| SharedString::from(held.package.as_str().to_owned()), |p| SharedString::from(p.display_name().to_owned())),
    }
}

/// The hand arranged by what feeds what. Without the world (still loading,
/// or never loaded) every card stands apart, in the order it was held.
pub(crate) fn hand_view(hand: &crate::model::hand::Hand, cx: &mut App) -> Rc<HandView> {
    let tables = match cx.try_global::<Service>() {
        Some(Service::Ready(tables)) => Some(Rc::clone(tables)),
        Some(Service::Loading | Service::Failed) => None,
        None => {
            start(cx);
            None
        }
    };
    match tables {
        Some(tables) => tables.borrow_mut().hand(hand.held()),
        None => Rc::new(HandView {
            cards: hand
                .held()
                .iter()
                .map(|held| Card { name: name_of(held), kind: facet::icons::Kind::Unknown, held: held.clone() })
                .collect(),
            roads: Vec::new(),
            apart: (0..hand.held().len()).collect(),
        }),
    }
}

/// The indexed declaration a node of the fixture world names: the one entry
/// of `package`'s outline `tree` that joins to exactly `node`.
pub(crate) fn symbol_of(node: NodeId, package: &PackageRef, tree: &OutlineTree, cx: &App) -> Option<SymbolRef> {
    match cx.try_global::<Service>() {
        Some(Service::Ready(tables)) => tables.borrow().identities.outline_symbol(node, package, tree),
        _ => None,
    }
}

/// Exact, immutable identity inputs for a page link. A caller pins these
/// Arcs for the lifetime of its index lookup and rejects a changed world.
/// This never builds page anatomy or rereads source on the UI thread.
pub(crate) fn link_world(cx: &App) -> Option<(Arc<World>, Arc<IdentityAdapter>)> {
    match cx.try_global::<Service>() {
        Some(Service::Ready(tables)) => {
            let tables = tables.borrow();
            Some((Arc::clone(&tables.world), Arc::clone(&tables.identities)))
        }
        _ => None,
    }
}

/// What the page's links know: whether a target is yours (its link reads
/// mint), and the peek it raises — the graph's own symbol card, so page and
/// graph peek alike.
pub(crate) fn links(world: &Arc<World>) -> Links {
    let yours = Arc::clone(world);
    let peeks = Arc::clone(world);
    Links {
        yours: Rc::new(move |target| matches!(target, Target::Node(node) if yours.yours(*node))),
        peek: Rc::new(move |target| match target {
            Target::Node(node) => Some(Peek::Symbol(facet::graph::peek::symbol_peek(&peeks, *node))),
            Target::Path(_) => None,
        }),
    }
}

/// Starts the load (tests never do: they [`install`] a world).
fn start(cx: &mut App) {
    #[cfg(test)]
    {
        let _ = cx;
    }
    #[cfg(not(test))]
    {
        cx.set_global(Service::Loading);
        let load = cx
            .background_executor()
            .spawn(async { blocking().map(|(world, identities)| Prepared::new(world, identities)) });
        cx.spawn(async move |cx| {
            let loaded = load.await;
            let _ = cx.update(|cx| {
                let service = match loaded {
                    Ok(prepared) => Service::Ready(Rc::new(RefCell::new(Tables::new(prepared, Sources::Folder(folder()))))),
                    Err(_) => Service::Failed,
                };
                cx.set_global(service);
                cx.refresh_windows();
            });
        })
        .detach();
    }
}

/// Installs `world` as the fixture, with statement text by file (tests).
#[cfg(test)]
pub(crate) fn install(world: Arc<World>, identities: Arc<IdentityAdapter>, files: HashMap<String, Arc<str>>, cx: &mut App) {
    let prepared = Prepared::new(world, identities);
    cx.set_global(Service::Ready(Rc::new(RefCell::new(Tables::new(prepared, Sources::Given(files))))));
}
