//! The **fixture** world: a snapshot of the prototype graph's
//! `Nudox-Design-System/v4/graph/world.json` (with the `repo/` and
//! `registry/` sources it was extracted from), loaded once per process and
//! joined to indexed declarations by exact source identity.
//!
//! It is a stand-in. The graph body and the hand's roads read facet's
//! [`World`]; until the runtime projects one from the index's own data, this
//! snapshot is that world. What it does not know (a declaration outside the
//! snapshot, a package it never walked) gets no mark and no road: the world is
//! an enhancement, never a gate. Callers map a node back through
//! [`symbol_of`]; none of them names a file, so a world projected from the
//! index replaces this module and nothing else.
//!
//! **The world is loaded when something reads it, and only then**, and never
//! on the UI thread (W-Open I3):
//!
//! - one thread, `nudox-world`, parses the 16 MB snapshot and joins it to the
//!   index's identities. A window that needs it at launch (a graph, a hand of
//!   cards: [`launch_need`]) starts it in `prepare` ([`preload`]); anything
//!   else starts it on the first ask ([`blocking`] for the graph, a non-empty
//!   hand for [`hand_view`]). An empty hand asks nothing of it;
//! - the hand's arrangement (the producer table walk, ~10 ms) is a
//!   [`Memo`] keyed by which cards are held, never by when they were touched:
//!   a touch changes no road, so it costs no walk. Until it lands the cards
//!   stand apart, as they do while the world loads;
//! - a panic on either becomes a typed [`WorldFault`], not a window that waits
//!   for ever ([`fault`] says it).
//!
//! The page's anatomy (`Anatomy`, `anatomy()`, `warm()`) is gone: the symbol
//! page draws from the index alone and nothing read it.

use crate::model::hand::{Hand, Held};
use crate::model::pages::{DeclRef, OutlineTree, PackageRef, SymbolRef};
use crate::navigation::{Route, View};
use crate::runtime::offload::{Answer, Asker, Memo};
use crate::shell::bodies::graph::identity::IdentityAdapter;
use facet::graph::{NodeId, World};
use facet::semantics::recipes::{PreparedRecipes, Recipes};
use gpui::{App, Context, Global, SharedString};
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// How many hands' arrangements are kept (five cards at most, a few hands).
const HANDS_KEPT: NonZeroUsize = NonZeroUsize::new(8).expect("eight is not zero");

/// The world and its join, as the callers that destructure a pair take them
/// (the graph body, the page-link resolver).
pub(crate) type WorldPair = (Arc<World>, Arc<IdentityAdapter>);

/// The fixture world and its join to indexed declarations.
#[derive(Clone)]
pub(crate) struct LoadedWorld {
    pub world: Arc<World>,
    pub identities: Arc<IdentityAdapter>,
}

impl LoadedWorld {
    /// As a pair, for the callers that destructure one.
    fn into_pair(self) -> WorldPair {
        (self.world, self.identities)
    }
}

/// Why the world could not be loaded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorldFault {
    /// `world.json` could not be read.
    Unreadable {
        /// Where it was looked for.
        path: PathBuf,
        /// The operating system's words.
        error: String,
    },
    /// `world.json` is not a world.
    Malformed(String),
    /// The world thread could not be started.
    NotStarted(String),
    /// The world thread panicked, and said so.
    Panicked(Arc<str>),
}

impl fmt::Display for WorldFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { path, error } => write!(formatter, "{}: {error}", path.display()),
            Self::Malformed(why) => formatter.write_str(why),
            Self::NotStarted(why) => write!(formatter, "start the world thread: {why}"),
            Self::Panicked(what) => write!(formatter, "the world thread panicked: {what}"),
        }
    }
}

impl From<WorldFault> for String {
    fn from(fault: WorldFault) -> Self {
        fault.to_string()
    }
}

/// What loading the world came to.
type Loaded = Result<LoadedWorld, WorldFault>;

/// One world: what the thread loaded (set once, by that thread), and the
/// producer table the hand's arrangement walks (built by the first hand that
/// needs it, on whichever worker asks first).
pub(crate) struct WorldHandle {
    loaded: OnceLock<Loaded>,
    producers: OnceLock<PreparedRecipes>,
}

impl WorldHandle {
    fn new() -> Self {
        Self { loaded: OnceLock::new(), producers: OnceLock::new() }
    }

    /// The world, when it has loaded.
    fn world(&self) -> Option<&LoadedWorld> {
        self.loaded.get()?.as_ref().ok()
    }
}

/// The process's world; the first [`started`] starts it.
static PROCESS: OnceLock<Arc<WorldHandle>> = OnceLock::new();

/// The process's world, started now if it was not.
fn started() -> &'static Arc<WorldHandle> {
    PROCESS.get_or_init(|| {
        let handle = Arc::new(WorldHandle::new());
        load_on_a_thread(&handle, load);
        handle
    })
}

/// The app's own world (tests install a small one; the product has none and
/// uses the process's).
struct WorldSlot(Arc<WorldHandle>);

impl Global for WorldSlot {}

/// The app's world if there is one: the installed one, else the process's if
/// it was started. Asking never starts it.
fn world_of(cx: &App) -> Option<Arc<WorldHandle>> {
    cx.try_global::<WorldSlot>().map_or_else(|| PROCESS.get().cloned(), |slot| Some(Arc::clone(&slot.0)))
}

/// The app's world, starting the process's if nothing has.
fn world_or_start(cx: &App) -> Arc<WorldHandle> {
    cx.try_global::<WorldSlot>().map_or_else(|| Arc::clone(started()), |slot| Arc::clone(&slot.0))
}

/// The snapshot's folder.
fn folder() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Nudox-Design-System/v4/graph")
}

/// Parses the snapshot and joins it to the index's identities. Blocks the
/// calling thread: only the world thread calls it.
fn load() -> Loaded {
    let path = folder().join("world.json");
    let reading = Instant::now();
    let bytes = std::fs::read(&path).map_err(|error| WorldFault::Unreadable { path: path.clone(), error: error.to_string() })?;
    let world = Arc::new(World::from_json(&bytes).map_err(|error| WorldFault::Malformed(error.to_string()))?);
    super::trace::span("world.parse", reading, format_args!("{} bytes", bytes.len()));
    drop(bytes);
    let joining = Instant::now();
    let identities = Arc::new(IdentityAdapter::load(&world, &Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")));
    super::trace::span("world.identities", joining, "IdentityAdapter::load");
    Ok(LoadedWorld { world, identities })
}

/// Loads `handle`'s world with `load` on its own thread; a panic there is the
/// world's fault, not a thread that vanished with everyone waiting for it.
fn load_on_a_thread(handle: &Arc<WorldHandle>, load: impl FnOnce() -> Loaded + Send + 'static) {
    let loading = Arc::clone(handle);
    let spawned = std::thread::Builder::new().name("nudox-world".to_owned()).spawn(move || {
        let result = std::panic::catch_unwind(AssertUnwindSafe(load))
            .unwrap_or_else(|panic| Err(WorldFault::Panicked(super::offload::describe(panic.as_ref()))));
        let _ = loading.loaded.set(result);
    });
    if let Err(error) = spawned {
        let _ = handle.loaded.set(Err(WorldFault::NotStarted(error.to_string())));
    }
}

/// The fixture world, loaded on the world thread and shared by every later
/// call. Blocks the calling thread until the world and its join are there:
/// call it from a background task. (A pair, for the graph body that
/// destructures one.)
pub(crate) fn blocking() -> Result<WorldPair, WorldFault> {
    started().loaded.wait().clone().map(LoadedWorld::into_pair)
}

/// Why a restored window needs the world before anything has asked for it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LaunchNeed {
    /// The window opens on a graph: it waits for the world and its join.
    Graph,
    /// The window holds cards: their marks and roads come from the world.
    Hand,
}

/// What a window restored onto `route` holding `hand` needs of the world at
/// launch: `None` when it needs nothing (the world then loads on the first
/// ask, if there ever is one).
pub(crate) fn launch_need(route: &Route, hand: &Hand) -> Option<LaunchNeed> {
    match route {
        Route::World => Some(LaunchNeed::Graph),
        Route::Symbol(symbol) if symbol.view == View::Graph => Some(LaunchNeed::Graph),
        Route::Orbit(_) | Route::Package(_) | Route::Symbol(_) => (!hand.is_empty()).then_some(LaunchNeed::Hand),
    }
}

/// Starts the world on its own thread for a window that [`launch_need`]s it:
/// the world takes ~170 ms to be ready, and every later caller only waits for
/// what is left.
pub(crate) fn preload(need: LaunchNeed) {
    super::trace::mark("world.preload", format_args!("{need:?}"));
    let _ = started();
}

/// The world's fault, when it could not be loaded (the store posts it as a
/// notice once).
pub(crate) fn fault(cx: &App) -> Option<WorldFault> {
    world_of(cx)?.loaded.get()?.as_ref().err().cloned()
}

/// Whether the world is still being loaded, or a hand's arrangement is still
/// being walked: the harness waits for neither before it captures.
#[cfg(any(test, feature = "visual-harness"))]
pub(crate) fn is_loading(cx: &App) -> bool {
    let loading = world_of(cx).is_some_and(|world| world.loaded.get().is_none());
    loading || cx.try_global::<Hands>().is_some_and(|hands| hands.memo.reading() > 0)
}

/// One card of the hand, as the rungs draw it.
#[derive(Clone, Debug)]
pub(crate) struct Card {
    /// What is held.
    pub held: Held,
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

/// Which cards a hand holds, in order, by what each is. When a card was held
/// or touched changes no road, so it is not part of the key: the walk runs
/// when the cards change, never when one is touched.
#[derive(Clone, Debug)]
struct HandKey {
    held: Vec<Held>,
}

impl PartialEq for HandKey {
    fn eq(&self, other: &Self) -> bool {
        self.held.len() == other.held.len() && self.held.iter().zip(&other.held).all(|(a, b)| a.same(b))
    }
}

impl Eq for HandKey {}

impl Hash for HandKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for card in &self.held {
            card.package.hash(state);
            card.id.hash(state);
        }
    }
}

/// Whether a hand's view is still standing in for its arrangement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Standing {
    /// Every card apart, in the order held: the walk has not landed.
    Provisional,
    /// The walk's answer.
    Arranged,
}

/// The hand last shown, by exactly what it held (times included): the same
/// hand asked again is the same `Rc`.
#[derive(Clone)]
struct Shown {
    held: Vec<Held>,
    view: Rc<HandView>,
    standing: Standing,
}

/// The hand's arrangements, off the UI thread.
struct Hands {
    memo: Memo<HandKey, HandView>,
    shown: RefCell<Option<Shown>>,
}

impl Global for Hands {}

impl Hands {
    fn over(world: Arc<WorldHandle>) -> Self {
        Self::arranging(move |key| arrange(&world, &key.held))
    }

    fn arranging(arranger: impl Fn(&HandKey) -> HandView + Send + Sync + 'static) -> Self {
        Self { memo: Memo::new(HANDS_KEPT, arranger), shown: RefCell::new(None) }
    }
}

/// The hand arranged by what feeds what: waits for the world, walks the
/// producer table (built by the first hand that needs it), and arranges. Runs
/// on a background thread, never the UI's.
fn arrange(world: &WorldHandle, held: &[Held]) -> HandView {
    // The world may still be loading when the first hand is asked (a restored
    // hand is asked at the first frame): this thread waits for it, so the
    // arrangement is the world's, never the cards standing apart for good.
    let Some(loaded) = world.loaded.wait().as_ref().ok() else { return standing_apart(held) };
    let prepared = world.producers.get_or_init(|| {
        let building = Instant::now();
        let prepared = Recipes::new(&loaded.world).into_prepared();
        super::trace::span("world.recipes", building, "Recipes::new");
        prepared
    });
    let walking = Instant::now();
    let view = arrange_with(loaded, &Recipes::from_prepared(prepared.clone()), held);
    super::trace::span("world.hand", walking, format_args!("{} cards", held.len()));
    view
}

fn node_of(loaded: &LoadedWorld, held: &Held) -> Option<NodeId> {
    let id = held.id.as_ref()?;
    let decl = DeclRef::from_label(id.as_str(), None, None, None)?;
    let package = PackageRef::parse(held.package.as_str()).ok()?;
    match loaded.identities.candidates(&decl, &package)[..] {
        [node] => Some(node),
        _ => None,
    }
}

/// Every card apart, in the order held: what the hand is while the world
/// loads or the walk runs, and when there is no world.
fn standing_apart(held: &[Held]) -> HandView {
    HandView {
        cards: held
            .iter()
            .map(|held| Card { name: name_of(held), kind: facet::icons::Kind::Unknown, held: held.clone() })
            .collect(),
        roads: Vec::new(),
        apart: (0..held.len()).collect(),
    }
}

fn arrange_with(loaded: &LoadedWorld, recipes: &Recipes, held: &[Held]) -> HandView {
    let world = &loaded.world;
    let nodes: Vec<Option<NodeId>> = held.iter().map(|card| node_of(loaded, card)).collect();
    let connector = facet::semantics::recipes::chain::Connector::new(recipes, world);
    let known: Vec<NodeId> = nodes.iter().flatten().copied().collect();
    let arrangement = connector.arrange(&known);
    let card_of = |node: NodeId| held.iter().zip(&nodes).find(|(_, n)| **n == Some(node)).map(|(card, _)| card.clone());
    let make = |held: Held, node: Option<NodeId>| Card {
        name: node.map_or_else(|| name_of(&held), |node| world.name_of(node)),
        kind: node.map_or(facet::icons::Kind::Unknown, |node| crate::shell::kit::world_kind(world.node(node).kind)),
        held,
    };
    let mut view = HandView::default();
    let twins: HashSet<String> = {
        let mut seen = HashSet::new();
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
    // What joins nothing, and what the world does not know, in the order it
    // was held.
    let shown: Vec<Held> = view.cards.iter().map(|card| card.held.clone()).collect();
    for (card, node) in held.iter().zip(&nodes) {
        if shown.iter().any(|seen| seen.same(card)) {
            continue;
        }
        view.apart.push(view.cards.len());
        view.cards.push(make(card.clone(), *node));
    }
    view
}

/// A held thing's name from its own spelling (a declaration's last segment,
/// a package's display name).
fn name_of(held: &Held) -> SharedString {
    match &held.id {
        Some(id) => SharedString::from(backend_present::Identity::parse(id.as_str()).name().to_owned()),
        None => PackageRef::parse(held.package.as_str())
            .map_or_else(|_| SharedString::from(held.package.as_str().to_owned()), |p| SharedString::from(p.display_name().to_owned())),
    }
}

/// `view` with each card's `held` replaced by the one now held (the same
/// card, touched since the walk).
fn refreshed(view: &HandView, held: &[Held]) -> HandView {
    let mut view = view.clone();
    for card in &mut view.cards {
        if let Some(now) = held.iter().find(|now| now.same(&card.held)) {
            card.held = now.clone();
        }
    }
    view
}

/// Time spent on the UI thread inside [`hand_view`], for the ledger: a call
/// that took 0.1 ms or more is one `world.ui` trace span.
struct UiTime(Instant);

impl Drop for UiTime {
    fn drop(&mut self) {
        const SLOW: Duration = Duration::from_micros(100);
        if self.0.elapsed() >= SLOW {
            super::trace::span("world.ui", self.0, "hand_view");
        }
    }
}

/// The hand arranged by what feeds what. Without the world (still loading,
/// or never loaded) or before its arrangement lands, every card stands apart,
/// in the order it was held. An empty hand is empty whatever the world is: it
/// asks nothing and starts nothing.
///
/// Redraws every window when the arrangement lands: a caller that has a
/// `Context` uses [`hand_view_for`] and redraws only itself.
pub(crate) fn hand_view(hand: &Hand, cx: &mut App) -> Rc<HandView> {
    hand_view_asked_by(hand, Asker::Everyone, cx)
}

/// [`hand_view`] for the view `cx` belongs to: only it redraws when the
/// arrangement lands.
#[cfg_attr(not(test), allow(dead_code, reason = "the shell's hand callers move to it (MIGRATE.md, R-Open3); delete this allow with that move"))]
pub(crate) fn hand_view_for<T: 'static>(hand: &Hand, cx: &mut Context<T>) -> Rc<HandView> {
    let asker = Asker::View(cx.entity_id());
    hand_view_asked_by(hand, asker, cx)
}

fn hand_view_asked_by(hand: &Hand, asker: Asker, cx: &mut App) -> Rc<HandView> {
    let _ui = UiTime(Instant::now());
    if hand.is_empty() {
        return Rc::new(HandView::default());
    }
    if cx.try_global::<Hands>().is_none() {
        let world = world_or_start(cx);
        cx.set_global(Hands::over(world));
    }
    let hands = cx.global::<Hands>();
    let memo = hands.memo.clone();
    let remembered = hands.shown.borrow().clone();
    if let Some(shown) = &remembered
        && shown.standing == Standing::Arranged
        && shown.held.as_slice() == hand.held()
    {
        return Rc::clone(&shown.view);
    }
    let key = HandKey { held: hand.held().to_vec() };
    let (view, standing) = match memo.ask(&key, asker, cx) {
        Answer::Ready(arranged) => (Rc::new(refreshed(&arranged, hand.held())), Standing::Arranged),
        Answer::Reading | Answer::Deferred | Answer::Failed(_) => match &remembered {
            Some(shown) if shown.standing == Standing::Provisional && shown.held.as_slice() == hand.held() => return Rc::clone(&shown.view),
            _ => (Rc::new(standing_apart(hand.held())), Standing::Provisional),
        },
    };
    *cx.global::<Hands>().shown.borrow_mut() = Some(Shown { held: hand.held().to_vec(), view: Rc::clone(&view), standing });
    view
}

/// The indexed declaration a node of the fixture world names: the one entry
/// of `package`'s outline `tree` that joins to exactly `node`.
pub(crate) fn symbol_of(node: NodeId, package: &PackageRef, tree: &OutlineTree, cx: &App) -> Option<SymbolRef> {
    world_of(cx)?.world()?.identities.outline_symbol(node, package, tree)
}

/// Exact, immutable identity inputs for a page link. A caller pins these
/// Arcs for the lifetime of its index lookup and rejects a changed world.
pub(crate) fn link_world(cx: &App) -> Option<WorldPair> {
    world_of(cx)?.world().cloned().map(LoadedWorld::into_pair)
}

/// Installs `world` as the app's world (tests): loaded already, joined by
/// `identities`. The anatomy's statement files are gone with the anatomy; the
/// parameter stays so `shell/anatomy_tests.rs` compiles until it is retired.
#[cfg(test)]
pub(crate) fn install(
    world: Arc<World>,
    identities: Arc<IdentityAdapter>,
    _files: std::collections::HashMap<String, Arc<str>>,
    cx: &mut App,
) {
    install_world(world, identities, cx);
}

/// [`install`] without the retired parameter: returns the handle.
#[cfg(test)]
fn install_world(world: Arc<World>, identities: Arc<IdentityAdapter>, cx: &mut App) -> Arc<WorldHandle> {
    let handle = Arc::new(WorldHandle::new());
    let _ = handle.loaded.set(Ok(LoadedWorld { world, identities }));
    cx.set_global(WorldSlot(Arc::clone(&handle)));
    handle
}

/// Installs a world that could not be loaded (tests).
#[cfg(test)]
fn install_fault(fault: WorldFault, cx: &mut App) {
    let handle = Arc::new(WorldHandle::new());
    let _ = handle.loaded.set(Err(fault));
    cx.set_global(WorldSlot(handle));
}

#[cfg(test)]
#[path = "fixture_world_tests.rs"]
mod tests;
