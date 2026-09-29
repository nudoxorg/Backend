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
//!
//! **Nothing here runs on the UI thread except a lookup** (W-Open I3). One
//! thread, `nudox-world`, is started by [`preload`] as the process starts:
//! it parses the world, then builds the identity join and the recipe table
//! side by side, then computes the anatomy of every declaration the window
//! is known to want ([`want`]: the restored route), and then stays to
//! compute any other declaration a visit asks for ([`warm`]), reading its
//! statement files itself. The UI thread installs what the thread published
//! (one redraw) and afterwards only looks results up; a lookup that misses
//! answers `None` and asks the thread, and the answer redraws once when it
//! lands.

use crate::model::hand::Held;
use crate::model::pages::{DeclRef, OutlineTree, PackageRef, SymbolRef};
use crate::runtime::wake::{WakeReceiver, wake_channel};
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
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, Once, PoisonError};
use std::time::{Duration, Instant};

/// The fixture world and its join to indexed declarations.
pub(crate) type Loaded = (Arc<World>, Arc<IdentityAdapter>);

/// Which world an engine serves: a process-wide count, so a trace line or a
/// test can say "this world" without an address.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct WorldId(u64);

impl WorldId {
    pub(crate) fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

impl fmt::Display for WorldId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "world {}", self.0)
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
    /// The world thread ended before it finished.
    Stopped,
}

impl fmt::Display for WorldFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { path, error } => write!(formatter, "{}: {error}", path.display()),
            Self::Malformed(why) => formatter.write_str(why),
            Self::NotStarted(why) => write!(formatter, "start the world thread: {why}"),
            Self::Stopped => formatter.write_str("the world thread stopped before it finished"),
        }
    }
}

impl From<WorldFault> for String {
    fn from(fault: WorldFault) -> Self {
        fault.to_string()
    }
}

/// A UI-thread entry point that took this long is one `world.ui` span.
const SLOW_UI_CALL: Duration = Duration::from_micros(100);

/// The snapshot's folder.
fn folder() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Nudox-Design-System/v4/graph")
}

/// A value one thread sets once and any thread may wait for.
struct Stage<T>(Mutex<Option<T>>, Condvar);

impl<T: Clone> Stage<T> {
    const fn new() -> Self {
        Self(Mutex::new(None), Condvar::new())
    }

    /// Sets the value unless one is there (the first setter wins).
    fn set(&self, value: T) {
        let mut slot = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(value);
        }
        drop(slot);
        self.1.notify_all();
    }

    fn is_set(&self) -> bool {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).is_some()
    }

    fn wait(&self) -> T {
        let mut slot = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(value) = slot.as_ref() {
                return value.clone();
            }
            slot = self.1.wait(slot).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// The load, as its two stages: the world with its join (all the graph
/// waits for), and everything the symbol page's anatomy needs.
struct Job {
    loaded: Stage<Result<Loaded, WorldFault>>,
    ready: Stage<Result<Arc<Ready>, WorldFault>>,
}

static JOB: Job = Job { loaded: Stage::new(), ready: Stage::new() };

/// A declaration the window will ask the world for.
#[derive(Clone, Debug)]
pub(crate) struct Want {
    pub decl: DeclRef,
    pub package: PackageRef,
}

/// What the window is known to want before the world exists: the restored
/// route's declaration and the restored hand (none held is an empty hand).
struct Wants {
    decls: Vec<Want>,
    hand: Vec<Held>,
}

static WANTS: Mutex<Wants> = Mutex::new(Wants { decls: Vec::new(), hand: Vec::new() });

fn wants() -> std::sync::MutexGuard<'static, Wants> {
    WANTS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts the load on its own thread, once per process. Call it as early as
/// the process can: the world takes ~200 ms to be ready, and every later
/// caller only waits for what is left.
pub(crate) fn preload() {
    let _ = job();
}

fn job() -> &'static Job {
    static START: Once = Once::new();
    START.call_once(|| {
        if let Err(error) = std::thread::Builder::new().name("nudox-world".to_owned()).spawn(run) {
            fail(&WorldFault::NotStarted(error.to_string()));
        }
    });
    &JOB
}

/// Settles both stages as failed (a no-op for a stage already set), so a
/// waiter never hangs on a thread that stopped.
fn fail(fault: &WorldFault) {
    JOB.loaded.set(Err(fault.clone()));
    JOB.ready.set(Err(fault.clone()));
}

/// Fails the stages if the thread leaves them unset (an early return or a
/// panic), so nobody waits forever.
struct Finish;

impl Drop for Finish {
    fn drop(&mut self) {
        if !JOB.loaded.is_set() || !JOB.ready.is_set() {
            fail(&WorldFault::Stopped);
        }
    }
}

/// The fixture world, loaded on the world thread and shared by every later
/// call. Blocks the calling thread until the world and its join are there:
/// call it from a background task.
pub(crate) fn blocking() -> Result<Loaded, WorldFault> {
    job().loaded.wait()
}

/// The world thread's body.
fn run() {
    let _finish = Finish;
    let path = folder().join("world.json");
    let reading = Instant::now();
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => return fail(&WorldFault::Unreadable { path, error: error.to_string() }),
    };
    let world = match World::from_json(&bytes) {
        Ok(world) => Arc::new(world),
        Err(error) => return fail(&WorldFault::Malformed(error.to_string())),
    };
    super::trace::span("world.parse", reading, format_args!("{} bytes", bytes.len()));
    drop(bytes);

    // The identity join reads manifests; the recipe table walks the world.
    // Neither needs the other, and the graph is waiting only for the join.
    let preparing = Instant::now();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let (identities, names, recipes) = std::thread::scope(|scope| {
        let joining = scope.spawn(|| {
            let joining = Instant::now();
            let identities = Arc::new(IdentityAdapter::load(&world, &repo));
            super::trace::span("world.identities", joining, "IdentityAdapter::load");
            JOB.loaded.set(Ok((Arc::clone(&world), Arc::clone(&identities))));
            identities
        });
        let naming = Instant::now();
        let names = Names::new(&world);
        super::trace::span("world.names", naming, "Names::new");
        let building = Instant::now();
        let recipes = Recipes::new(&world);
        super::trace::span("world.recipes", building, "Recipes::new");
        (joining.join(), names, recipes)
    });
    let Ok(identities) = identities else { return fail(&WorldFault::Stopped) };
    super::trace::span("world.tables", preparing, "identities, Names and Recipes, side by side");

    let engine = Arc::new(Engine {
        id: WorldId::next(),
        world,
        identities,
        names,
        prepared: recipes.into_prepared(),
        sources: Sources::Folder(folder()),
    });
    let wanted = {
        let mut wants = wants();
        Wanted {
            nodes: std::mem::take(&mut wants.decls).iter().filter_map(|want| engine.node_of(&want.decl, &want.package)).collect(),
            hand: std::mem::take(&mut wants.hand),
        }
    };
    serve(&engine, wanted, |ready| {
        super::trace::mark("world.ready", format_args!("{} anatomies computed", ready.first_count()));
        JOB.ready.set(Ok(ready));
    });
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

/// Everything about one world that any thread may read: the world, its join
/// to indexed declarations, and the tables the page side derives from it.
/// It computes; it holds no per-thread state (a [`Recipes`] is handed in).
struct Engine {
    id: WorldId,
    world: Arc<World>,
    identities: Arc<IdentityAdapter>,
    names: Names,
    prepared: PreparedRecipes,
    sources: Sources,
}

impl Engine {
    /// The one node `decl` (in `package`) names, when the world knows it.
    fn node_of(&self, decl: &DeclRef, package: &PackageRef) -> Option<NodeId> {
        match self.identities.candidates(decl, package)[..] {
            [node] => Some(node),
            _ => None,
        }
    }

    fn node_of_held(&self, held: &Held) -> Option<NodeId> {
        let id = held.id.as_ref()?;
        let decl = DeclRef::from_label(id.as_str(), None, None, None)?;
        let package = PackageRef::parse(held.package.as_str()).ok()?;
        self.node_of(&decl, &package)
    }

    /// Records that this world computed `work`, on this thread (tests).
    #[cfg(test)]
    fn ran(&self, work: Work) {
        ran_on().lock().unwrap_or_else(PoisonError::into_inner).push(Ran { work, world: self.id, thread: std::thread::current().id() });
    }

    /// The anatomy of `node`: pure, and the only place its statement files
    /// are read. Never call it from the UI thread.
    fn compute(&self, recipes: &Recipes, node: NodeId) -> Anatomy {
        let computing = Instant::now();
        #[cfg(test)]
        self.ran(Work::Anatomy(node));
        let world = &self.world;
        let recipe = recipes
            .getting_one(world, node)
            .or_else(|| recipes.calling_it(world, node))
            .map(|section| section.view(world, node));
        let sources = &self.sources;
        let uses = page::in_use(world, node, &mut |package, file| sources.read(package, file))
            .into_iter()
            .map(|found| {
                let caller = world.name_of(found.caller);
                (found, caller)
            })
            .collect();
        let anatomy = Anatomy {
            world: Arc::clone(world),
            node,
            page: page::page(world, &self.names, node),
            recipe,
            uses,
        };
        super::trace::span("world.anatomy", computing, format_args!("{} node {node}", self.id));
        anatomy
    }

    /// The hand arranged by what feeds what.
    fn arrange(&self, recipes: &Recipes, held: &[Held]) -> HandView {
        // Nothing held is nothing arranged: no producer table to walk (the
        // walk costs ~9 ms and its answer for no cards is this).
        if held.is_empty() {
            return HandView::default();
        }
        self.arrange_all(recipes, held)
    }

    /// [`Self::arrange`] with no shortcut.
    fn arrange_all(&self, recipes: &Recipes, held: &[Held]) -> HandView {
        #[cfg(test)]
        self.ran(Work::Hand);
        let world = &self.world;
        let nodes: Vec<Option<NodeId>> = held.iter().map(|card| self.node_of_held(card)).collect();
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
        // What joins nothing, and what the world does not know, in the
        // order it was held.
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
}

/// What the world thread hands the UI thread, once: the engine, the results
/// it computed for what was wanted, and the two ends of its lane.
struct Ready {
    engine: Arc<Engine>,
    first: Mutex<Option<First>>,
    /// Nodes to compute, from the UI thread to the world thread.
    requests: mpsc::Sender<NodeId>,
    /// Computed anatomies (each knows its node), from the world thread to the
    /// UI thread.
    outbox: Arc<Mutex<Vec<Anatomy>>>,
    /// The UI thread's end of the wake the world thread rings when it
    /// pushes to the outbox (taken once, by the installer).
    wake: Mutex<Option<WakeReceiver>>,
}

/// What was computed before the window was told the world is ready.
struct First {
    anatomies: Vec<Anatomy>,
    hand: Option<ArrangedHand>,
}

/// A hand and how it was arranged.
struct ArrangedHand {
    held: Vec<Held>,
    view: HandView,
}

/// What the window will ask of a world at once: the declarations of its
/// restored route and the cards of its restored hand.
struct Wanted {
    nodes: Vec<NodeId>,
    hand: Vec<Held>,
}

impl Ready {
    fn first_count(&self) -> usize {
        self.first
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map_or(0, |first| first.anatomies.len())
    }
}

/// Computes what was wanted, publishes the result, and then serves the UI
/// thread's requests until every sender is gone. This is the world thread's
/// life after the load (and, in tests, a thread of its own).
fn serve(engine: &Arc<Engine>, wanted: Wanted, publish: impl FnOnce(Arc<Ready>)) {
    // This thread's recipe caches persist for its life: the second
    // declaration of a package finds the first one's run warm.
    let recipes = Recipes::from_prepared(engine.prepared.clone());
    let mut seen = HashSet::new();
    let anatomies = wanted
        .nodes
        .into_iter()
        .filter(|node| seen.insert(*node))
        .map(|node| engine.compute(&recipes, node))
        .collect();
    let hand = (!wanted.hand.is_empty()).then(|| {
        let view = engine.arrange(&recipes, &wanted.hand);
        ArrangedHand { held: wanted.hand, view }
    });
    let (requests, inbox) = mpsc::channel();
    let (wake, receiver) = wake_channel();
    let outbox = Arc::new(Mutex::new(Vec::new()));
    publish(Arc::new(Ready {
        engine: Arc::clone(engine),
        first: Mutex::new(Some(First { anatomies, hand })),
        requests,
        outbox: Arc::clone(&outbox),
        wake: Mutex::new(Some(receiver)),
    }));
    while let Ok(node) = inbox.recv() {
        #[cfg(test)]
        pass_gate(engine.id);
        let anatomy = engine.compute(&recipes, node);
        outbox.lock().unwrap_or_else(PoisonError::into_inner).push(anatomy);
        wake.wake();
    }
}

/// The page side's state over one world, on the UI thread: what the world
/// thread computed, what was asked of it, and the recipe table the hand's
/// arrangement (a change after launch) still walks here.
struct Tables {
    ready: Arc<Ready>,
    engine: Arc<Engine>,
    recipes: Recipes,
    pages: HashMap<NodeId, Rc<Anatomy>>,
    /// Nodes asked of the world thread (landed or on their way).
    asked: HashSet<NodeId>,
    readings: HashMap<u32, Option<Rc<Reading>>>,
    /// The last hand arranged, by what it held.
    hand: Option<Arranged>,
    /// What a lookup that misses does. The product always asks the world
    /// thread; tests that install a world compute in place.
    #[cfg(test)]
    on_a_miss: OnAMiss,
}

/// The hand last arranged on this thread, and what it held.
struct Arranged {
    held: Vec<Held>,
    view: Rc<HandView>,
}

/// What a lookup that misses does (tests choose; the product asks).
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OnAMiss {
    /// Ask the world thread and answer `None` until it lands.
    AskTheWorldThread,
    /// Compute here, as the UI thread did before the world thread.
    ComputeHere,
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

/// A package's reading path: facet's tour over the world.
pub(crate) struct Reading {
    /// The world it was read from.
    pub world: Arc<World>,
    /// The stops, in the order a newcomer meets them.
    pub tour: Tour,
}

impl Tables {
    fn new(ready: Arc<Ready>) -> Self {
        let engine = Arc::clone(&ready.engine);
        let first = ready.first.lock().unwrap_or_else(PoisonError::into_inner).take();
        let mut tables = Self {
            recipes: Recipes::from_prepared(engine.prepared.clone()),
            ready,
            engine,
            pages: HashMap::new(),
            asked: HashSet::new(),
            readings: HashMap::new(),
            hand: None,
            #[cfg(test)]
            on_a_miss: OnAMiss::AskTheWorldThread,
        };
        if let Some(first) = first {
            for anatomy in first.anatomies {
                tables.asked.insert(anatomy.node);
                tables.pages.insert(anatomy.node, Rc::new(anatomy));
            }
            tables.hand = first.hand.map(|arranged| Arranged { held: arranged.held, view: Rc::new(arranged.view) });
        }
        tables
    }

    /// Moves what the world thread finished into the pages; whether any
    /// arrived.
    fn landed(&mut self) -> bool {
        let done = std::mem::take(&mut *self.ready.outbox.lock().unwrap_or_else(PoisonError::into_inner));
        let any = !done.is_empty();
        for anatomy in done {
            self.pages.insert(anatomy.node, Rc::new(anatomy));
        }
        any
    }

    /// Asks the world thread for `node` unless it was asked already.
    fn ask(&mut self, node: NodeId) {
        if !self.pages.contains_key(&node) && self.asked.insert(node) {
            let _ = self.ready.requests.send(node);
        }
    }

    /// Whether an anatomy asked of the world thread has not landed.
    fn pending(&self) -> bool {
        self.asked.iter().any(|node| !self.pages.contains_key(node))
    }

    fn hand(&mut self, held: &[Held]) -> Rc<HandView> {
        if let Some(arranged) = &self.hand
            && arranged.held.as_slice() == held
        {
            return Rc::clone(&arranged.view);
        }
        let view = Rc::new(self.engine.arrange(&self.recipes, held));
        self.hand = Some(Arranged { held: held.to_vec(), view: Rc::clone(&view) });
        view
    }

    fn reading(&mut self, package: u32) -> Option<Rc<Reading>> {
        let (world, names) = (&self.engine.world, &self.engine.names);
        self.readings
            .entry(package)
            .or_insert_with(|| {
                let tour = tour::of(world, names, package);
                tour.shown().then(|| Rc::new(Reading { world: Arc::clone(world), tour }))
            })
            .clone()
    }

    /// The anatomy of `node` when the world thread has computed it; else
    /// `None`, and the thread is asked.
    fn anatomy(&mut self, node: NodeId) -> Option<Rc<Anatomy>> {
        self.landed();
        if let Some(anatomy) = self.pages.get(&node) {
            return Some(Rc::clone(anatomy));
        }
        #[cfg(test)]
        if self.on_a_miss == OnAMiss::ComputeHere {
            let anatomy = Rc::new(self.engine.compute(&self.recipes, node));
            self.pages.insert(node, Rc::clone(&anatomy));
            return Some(anatomy);
        }
        self.ask(node);
        None
    }
}

/// Time spent on the UI thread inside this module, for the ledger: every
/// entry point that took 0.1 ms or more is one `world.ui` trace span.
struct UiTime(Entry, Instant);

/// The module's UI-thread entry points.
#[derive(Clone, Copy, Debug)]
enum Entry {
    Anatomy,
    HandView,
    Reading,
}

impl UiTime {
    fn start(entry: Entry) -> Self {
        Self(entry, Instant::now())
    }
}

impl Drop for UiTime {
    fn drop(&mut self) {
        if self.1.elapsed() >= SLOW_UI_CALL {
            super::trace::span("world.ui", self.1, format_args!("{:?}", self.0));
        }
    }
}

/// The service's state in the app.
#[cfg_attr(test, allow(dead_code, reason = "tests install a ready world and never load"))]
enum Service {
    Loading,
    Ready(Rc<RefCell<Tables>>),
    Failed(WorldFault),
}

impl Global for Service {}

/// Whether the optional pinned semantic world is still being prepared, or an
/// anatomy asked of it has not come back. Failed or not-yet-requested worlds
/// are settled states for the shell.
pub(crate) fn is_loading(cx: &App) -> bool {
    match cx.try_global::<Service>() {
        Some(Service::Loading) => true,
        Some(Service::Ready(tables)) => tables.borrow().pending(),
        _ => false,
    }
}

/// The anatomy of `decl` (in `package`) when the fixture world knows it:
/// exactly one node at its file, line and name.
///
/// `None` while the snapshot loads (the first ask starts it in the
/// background, and every window redraws once when it lands), when it failed
/// to load, when it does not know the declaration, or when the anatomy has
/// not been computed yet (it is asked for now, computed off the UI thread,
/// and every window redraws once when it lands: [`warm`] asks earlier, so a
/// visit's page usually finds it there). The page then keeps its index-built
/// body.
pub(crate) fn anatomy(decl: &DeclRef, package: &PackageRef, cx: &mut App) -> Option<Rc<Anatomy>> {
    let _ui = UiTime::start(Entry::Anatomy);
    let tables = match cx.try_global::<Service>() {
        Some(Service::Ready(tables)) => Rc::clone(tables),
        Some(Service::Loading | Service::Failed(_)) => return None,
        None => {
            start(cx);
            return None;
        }
    };
    let mut tables = tables.borrow_mut();
    let [node] = tables.engine.identities.candidates(decl, package)[..] else {
        return None;
    };
    tables.anatomy(node)
}

/// Asks for `decl`'s anatomy ahead of the page that will draw it: called
/// when a visit's page is requested, so the world thread computes while the
/// index reads. Cheap to repeat.
pub(crate) fn warm(decl: &DeclRef, package: &PackageRef, cx: &mut App) {
    match cx.try_global::<Service>() {
        Some(Service::Ready(tables)) => {
            let mut tables = tables.borrow_mut();
            if let Some(node) = tables.engine.node_of(decl, package) {
                tables.ask(node);
            }
        }
        Some(Service::Failed(_)) => {}
        Some(Service::Loading) => want(Want { decl: decl.clone(), package: package.clone() }),
        None => {
            want(Want { decl: decl.clone(), package: package.clone() });
            start(cx);
        }
    }
}

/// [`warm`] for a declaration named by its coordinate alone (a page key):
/// the package is the one the coordinate spells.
pub(crate) fn warm_symbol(symbol: &SymbolRef, cx: &mut App) {
    if let Some(decl) = DeclRef::from_label(symbol.as_str(), None, None, None)
        && let Some(package) = symbol.package()
    {
        warm(&decl, &package, cx);
    }
}

/// Records that the window will want `decl`'s anatomy as soon as the world
/// is there, so the world thread computes it before it says it is ready. The
/// restored route is wanted from `prepare`, before any window exists.
pub(crate) fn want(want: Want) {
    let mut wants = wants();
    if !wants.decls.iter().any(|known| known.decl.coordinate == want.decl.coordinate && known.package == want.package) {
        wants.decls.push(want);
    }
}

/// Records the restored hand, arranged on the world thread before the world
/// is announced (an empty hand is nothing to arrange).
pub(crate) fn want_hand(hand: &[Held]) {
    wants().hand = hand.to_vec();
}

/// The reading path of `package` (facet's tour, at least three stops), when
/// the world knows the package: it is found through the first of `decls` (the
/// package's own declarations) that joins to exactly one node.
pub(crate) fn reading<'a>(
    package: &PackageRef,
    decls: impl IntoIterator<Item = &'a DeclRef>,
    cx: &mut App,
) -> Option<Rc<Reading>> {
    let _ui = UiTime::start(Entry::Reading);
    let tables = match cx.try_global::<Service>() {
        Some(Service::Ready(tables)) => Rc::clone(tables),
        Some(Service::Loading | Service::Failed(_)) => return None,
        None => {
            start(cx);
            return None;
        }
    };
    let mut tables = tables.borrow_mut();
    let owner = decls.into_iter().take(64).find_map(|decl| match tables.engine.identities.candidates(decl, package)[..] {
        [node] => Some(tables.engine.world.node(node).pkg),
        _ => None,
    })?;
    tables.reading(owner)
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

/// The hand arranged by what feeds what. Without the world (still loading,
/// or never loaded) every card stands apart, in the order it was held.
pub(crate) fn hand_view(hand: &crate::model::hand::Hand, cx: &mut App) -> Rc<HandView> {
    let _ui = UiTime::start(Entry::HandView);
    let tables = match cx.try_global::<Service>() {
        Some(Service::Ready(tables)) => Some(Rc::clone(tables)),
        Some(Service::Loading | Service::Failed(_)) => None,
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
        Some(Service::Ready(tables)) => tables.borrow().engine.identities.outline_symbol(node, package, tree),
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
            Some((Arc::clone(&tables.engine.world), Arc::clone(&tables.engine.identities)))
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

/// Installs a published world in the app: the tables, the wake that turns a
/// computed anatomy into one redraw, and the one redraw for the world itself.
fn install_ready(ready: Arc<Ready>, cx: &mut App) -> Rc<RefCell<Tables>> {
    let wake = ready.wake.lock().unwrap_or_else(PoisonError::into_inner).take();
    let tables = Rc::new(RefCell::new(Tables::new(ready)));
    if let Some(mut wake) = wake {
        let weak = Rc::downgrade(&tables);
        cx.spawn(async move |cx| {
            while wake.wait().await.is_some() {
                let Some(tables) = weak.upgrade() else { break };
                let landed = tables.borrow_mut().landed();
                if landed {
                    let _ = cx.update(|cx| cx.refresh_windows());
                }
            }
        })
        .detach();
    }
    tables
}

/// Attaches the app to the world thread (tests never do: they [`install`] a
/// world). The load is normally already under way ([`preload`]).
fn start(cx: &mut App) {
    #[cfg(test)]
    {
        let _ = cx;
    }
    #[cfg(not(test))]
    {
        cx.set_global(Service::Loading);
        let job = job();
        let load = cx.background_executor().spawn(async move { job.ready.wait() });
        cx.spawn(async move |cx| {
            let loaded = load.await;
            let _ = cx.update(|cx| {
                let service = match loaded {
                    Ok(ready) => {
                        let tables = install_ready(ready, cx);
                        // Anything asked while the world was loading and
                        // not wanted in time: ask now.
                        let late = std::mem::take(&mut wants().decls);
                        {
                            let mut tables = tables.borrow_mut();
                            for want in &late {
                                if let Some(node) = tables.engine.node_of(&want.decl, &want.package) {
                                    tables.ask(node);
                                }
                            }
                        }
                        Service::Ready(tables)
                    }
                    Err(fault) => {
                        super::trace::mark("world.failed", &fault);
                        Service::Failed(fault)
                    }
                };
                super::trace::mark("world.installed", "Service installed");
                cx.set_global(service);
                cx.refresh_windows();
            });
        })
        .detach();
    }
}

/// What the world's computations were (tests).
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Work {
    Anatomy(NodeId),
    Hand,
}

/// One computation: what, for which world, on which thread (tests).
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
struct Ran {
    work: Work,
    world: WorldId,
    thread: std::thread::ThreadId,
}

/// Every computation so far, in the order they ran (tests: what ran where).
#[cfg(test)]
fn ran_on() -> &'static Mutex<Vec<Ran>> {
    static RAN: Mutex<Vec<Ran>> = Mutex::new(Vec::new());
    &RAN
}

/// Whether a gate lets the world thread answer requests (tests).
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Latch {
    #[default]
    Closed,
    Open,
}

/// A gate holding the world thread's answers to requests (tests).
#[cfg(test)]
#[derive(Default)]
struct Gate {
    latch: Mutex<Latch>,
    opened: Condvar,
}

#[cfg(test)]
fn gates() -> &'static Mutex<HashMap<WorldId, Arc<Gate>>> {
    static GATES: std::sync::OnceLock<Mutex<HashMap<WorldId, Arc<Gate>>>> = std::sync::OnceLock::new();
    GATES.get_or_init(Mutex::default)
}

/// Holds the world thread's answers to requests for `world` (not what it
/// computed before it published) until [`open_gate`] (tests). Call it before
/// the world is served.
#[cfg(test)]
fn hold_requests(world: WorldId) -> Arc<Gate> {
    let gate = Arc::new(Gate::default());
    gates().lock().unwrap_or_else(PoisonError::into_inner).insert(world, Arc::clone(&gate));
    gate
}

#[cfg(test)]
fn open_gate(gate: &Gate) {
    *gate.latch.lock().unwrap_or_else(PoisonError::into_inner) = Latch::Open;
    gate.opened.notify_all();
}

#[cfg(test)]
fn pass_gate(world: WorldId) {
    let gate = gates().lock().unwrap_or_else(PoisonError::into_inner).get(&world).cloned();
    if let Some(gate) = gate {
        let mut latch = gate.latch.lock().unwrap_or_else(PoisonError::into_inner);
        while *latch == Latch::Closed {
            latch = gate.opened.wait(latch).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// What the window has said it will want of the world before it exists
/// (tests take and clear it).
#[cfg(test)]
pub(crate) fn take_wanted() -> Vec<Want> {
    std::mem::take(&mut wants().decls)
}

/// Installs `world` as the fixture, with statement text by file (tests): the
/// anatomy of what a test asks for is computed in place, as it was before
/// the world thread, so a test reads it in its first frame.
#[cfg(test)]
pub(crate) fn install(world: Arc<World>, identities: Arc<IdentityAdapter>, files: HashMap<String, Arc<str>>, cx: &mut App) {
    let ready = spawn_serving(WorldId::next(), world, identities, files, Wanted { nodes: Vec::new(), hand: Vec::new() });
    let mut tables = Tables::new(ready);
    tables.on_a_miss = OnAMiss::ComputeHere;
    cx.set_global(Service::Ready(Rc::new(RefCell::new(tables))));
}

/// Installs `world` as the fixture the way the product does (tests): the
/// world thread serves it as `id`, computing the anatomy of `wanted` (and
/// arranging `hand`) before it publishes, and everything else as it is
/// asked. Returns after the thread has published (a test drives the window
/// from there).
#[cfg(test)]
pub(crate) fn install_serving(
    id: WorldId,
    world: Arc<World>,
    identities: Arc<IdentityAdapter>,
    files: HashMap<String, Arc<str>>,
    wanted: &[Want],
    hand: Vec<Held>,
    cx: &mut App,
) {
    let engine = test_engine(id, world, identities, files);
    let nodes = wanted.iter().filter_map(|want| engine.node_of(&want.decl, &want.package)).collect();
    let ready = serve_on_a_thread(engine, Wanted { nodes, hand });
    let tables = install_ready(ready, cx);
    cx.set_global(Service::Ready(tables));
    cx.refresh_windows();
}

#[cfg(test)]
fn spawn_serving(
    id: WorldId,
    world: Arc<World>,
    identities: Arc<IdentityAdapter>,
    files: HashMap<String, Arc<str>>,
    wanted: Wanted,
) -> Arc<Ready> {
    serve_on_a_thread(test_engine(id, world, identities, files), wanted)
}

#[cfg(test)]
fn test_engine(id: WorldId, world: Arc<World>, identities: Arc<IdentityAdapter>, files: HashMap<String, Arc<str>>) -> Arc<Engine> {
    Arc::new(Engine {
        id,
        names: Names::new(&world),
        prepared: Recipes::new(&world).into_prepared(),
        world,
        identities,
        sources: Sources::Given(files),
    })
}

#[cfg(test)]
fn serve_on_a_thread(engine: Arc<Engine>, wanted: Wanted) -> Arc<Ready> {
    let (sent, received) = mpsc::channel();
    std::thread::Builder::new()
        .name("nudox-world-test".to_owned())
        .spawn(move || serve(&engine, wanted, |ready| drop(sent.send(ready))))
        .expect("the test world thread");
    received.recv().expect("the world thread publishes")
}

#[cfg(test)]
#[path = "fixture_world_tests.rs"]
mod tests;
