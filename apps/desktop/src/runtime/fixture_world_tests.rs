//! W-Open I3: what still reads the fixture world (the hand, the graph's load,
//! a launch that needs it) never blocks the UI thread and never hangs.
//!
//! - an empty hand asks nothing of the world and starts none;
//! - a hand's arrangement lands off the UI thread, in the view that asked
//!   and no other, and a card touched since costs no second walk;
//! - a world thread that panics is one typed fault everyone can read, not a
//!   window waiting for ever;
//! - a restored window retains its route and hand before the owner answers.
//!
//! The page's anatomy tests are gone with the anatomy: the symbol page draws
//! from the index alone, and nothing read it.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::model::hand::HeldWhy;
use crate::model::pages::PageKey;
use crate::navigation::{Coordinate, OrbitRoute};
use crate::runtime::wait;
use crate::shell::tests::{PACKAGE, coordinate, page_route, view_route};
use facet::graph::{Edge, Kind, Module, Node, Package, Rel};
use gpui::{AppContext as _, Entity, IntoElement, ParentElement, Render, StyleRefinement, TestAppContext, VisualTestContext, Window, div};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// `RelationLabel` (an enum whose variant carries a `SemanticLinkKind`), and
/// the function `relation_label` that gives one. At the page fixture's line,
/// so the product's own join (`IdentityAdapter`: file, line, name) finds them.
fn world() -> Arc<World> {
    let node = |kind, name: &str, line, parent: Option<u32>| {
        let mut node = Node::new(kind, name, 0, 0);
        node.line = line;
        node.parent = parent;
        node.vis = Some("pub".into());
        node
    };
    let label = node(Kind::Enum, "RelationLabel", 138, None);
    let mut typed = node(Kind::Variant, "Typed", 139, Some(0));
    typed.ty = Some("SemanticLinkKind".into());
    let related = node(Kind::Variant, "Related", 140, Some(0));
    let mut function = node(Kind::Function, "relation_label", 138, None);
    function.params = vec!["link: &Link".into()];
    function.ret = Some("RelationLabel".into());
    let kind = node(Kind::Enum, "SemanticLinkKind", 138, None);
    Arc::new(
        World::new(
            vec![Package { name: "present".into(), version: "0.4.2".into(), yours: true, external: false, deps: vec![] }],
            vec![Module { pkg: 0, path: "glyph".into(), file: "glyph.rs".into() }],
            vec![label, typed, related, function, kind],
            vec![Edge { from: 0, to: 4, rel: Rel::HAS }, Edge { from: 3, to: 0, rel: Rel::GIVES }],
        )
        .expect("world"),
    )
}

fn package() -> PackageRef {
    PackageRef::parse(PACKAGE).expect("package")
}

fn identities(world: &World) -> Arc<IdentityAdapter> {
    Arc::new(IdentityAdapter::synthetic(world, package()))
}

fn card(name: &str, touched_at: u64) -> Held {
    Held {
        package: crate::core::PackageId::new(PACKAGE).expect("package"),
        id: Some(Coordinate::new(&coordinate(name)).expect("coordinate")),
        why: HeldWhy::Pin,
        held_at: 1,
        touched_at,
    }
}

/// A view that draws the hand it is given, from `hand_view_for`, and keeps
/// the last view it drew.
struct HandProbe {
    hand: Hand,
    drew: Rc<RefCell<Rc<HandView>>>,
    renders: Rc<Cell<u32>>,
}

impl Render for HandProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        let view = hand_view_for(&self.hand, cx);
        let words = view.cards.iter().map(|card| format!("{}:{:?}", card.name, card.kind)).collect::<Vec<_>>().join(" ");
        *self.drew.borrow_mut() = view;
        div().child(words)
    }
}

/// The window's root: it holds the probe as a cached region, the way the shell
/// holds its regions, so the probe renders when it is notified and no
/// oftener (an uncached root renders on every draw).
struct Frame {
    child: Entity<HandProbe>,
}

impl Render for Frame {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child(self.child.clone().cached(StyleRefinement::default()))
    }
}

/// A window on one hand probe.
struct Drawn {
    cx: &'static mut VisualTestContext,
    drew: Rc<RefCell<Rc<HandView>>>,
    renders: Rc<Cell<u32>>,
}

impl Drawn {
    /// Draws what changed, as the platform would, and returns the hand drawn.
    fn draw(&mut self) -> Rc<HandView> {
        self.cx.run_until_parked();
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        Rc::clone(&self.drew.borrow())
    }
}

fn window(cx: &mut TestAppContext, hand: Hand) -> Drawn {
    let drew = Rc::new(RefCell::new(Rc::new(HandView::default())));
    let renders = Rc::new(Cell::new(0));
    let (seen, counted) = (Rc::clone(&drew), Rc::clone(&renders));
    let opened = cx.update(|cx| {
        cx.open_window(gpui::WindowOptions::default(), move |_, cx| {
            let child = cx.new(|_| HandProbe { hand, drew: seen, renders: counted });
            cx.new(|_| Frame { child })
        })
        .expect("window")
    });
    Drawn { cx: VisualTestContext::from_window(opened.into(), cx).into_mut(), drew, renders }
}

fn install_test_world(cx: &mut TestAppContext) -> Arc<WorldHandle> {
    cx.update(|cx| {
        let world = world();
        let identities = identities(&world);
        install_world(world, identities, cx)
    })
}

#[gpui::test]
fn an_empty_hand_asks_nothing_of_the_world_and_starts_none(cx: &mut TestAppContext) {
    let view = cx.update(|cx| hand_view(&Hand::default(), cx));
    assert!(view.cards.is_empty() && view.roads.is_empty() && view.apart.is_empty(), "an empty hand is empty");
    cx.update(|cx| {
        assert!(cx.try_global::<Hands>().is_none(), "no arrangement was asked for");
        assert!(cx.try_global::<WorldSlot>().is_none(), "no world was installed or started for it");
        assert!(!is_loading(cx), "and nothing is loading");
    });
}

#[gpui::test]
fn the_first_answer_never_waits_for_the_walk_and_the_arrangement_lands_after(cx: &mut TestAppContext) {
    install_test_world(cx);
    let hand = Hand::of([card("SemanticLinkKind", 1), card("RelationLabel", 1)]);
    // The very call that asks answers at once: the cards, apart and unmarked.
    let first = cx.update(|cx| hand_view(&hand, cx));
    assert_eq!(first.cards.len(), 2, "both cards are in the first answer");
    assert!(first.roads.is_empty() && first.apart == [0, 1], "standing apart until the walk lands");
    assert!(first.cards.iter().all(|card| card.kind == facet::icons::Kind::Unknown), "with no marks yet");
    assert!(cx.update(|cx| is_loading(cx)), "the window knows the walk is in flight (the harness waits on it)");

    cx.run_until_parked();
    assert!(!cx.update(|cx| is_loading(cx)), "nothing is left in flight");
    let arranged = cx.update(|cx| hand_view(&hand, cx));
    let names = arranged.cards.iter().map(|card| card.name.to_string()).collect::<Vec<_>>();
    assert!(
        names.len() == 2 && names.contains(&"RelationLabel".to_owned()) && names.contains(&"SemanticLinkKind".to_owned()),
        "both cards are still there: {names:?}"
    );
    assert!(arranged.cards.iter().all(|card| card.kind != facet::icons::Kind::Unknown), "now with the world's marks");
    let again = cx.update(|cx| hand_view(&hand, cx));
    assert!(Rc::ptr_eq(&arranged, &again), "asked again, the same hand is the same view");
}

#[gpui::test]
fn a_hand_asked_while_the_world_is_still_loading_is_arranged_when_it_lands_not_left_apart_for_good(cx: &mut TestAppContext) {
    // The restored hand is asked at the first frame, ~100 ms before the world
    // has loaded: a world thread that takes its time.
    let handle = Arc::new(WorldHandle::new());
    let (loaded, joined) = (world(), identities(&world()));
    load_on_a_thread(&handle, move || {
        std::thread::sleep(Duration::from_millis(150));
        Ok(LoadedWorld { world: loaded, identities: joined })
    });
    cx.update(|cx| cx.set_global(WorldSlot(Arc::clone(&handle))));
    let hand = Hand::of([card("SemanticLinkKind", 1), card("RelationLabel", 1)]);
    let first = cx.update(|cx| hand_view(&hand, cx));
    assert!(first.cards.iter().all(|card| card.kind == facet::icons::Kind::Unknown), "the first answer has no marks: the world is not there");
    assert!(cx.update(|cx| is_loading(cx)), "the window knows the world is on its way");
    wait::until("the hand was arranged", || {
        cx.run_until_parked();
        !cx.update(|cx| is_loading(cx))
    });
    let arranged = cx.update(|cx| hand_view(&hand, cx));
    assert!(
        arranged.cards.iter().all(|card| card.kind != facet::icons::Kind::Unknown),
        "the world's marks are on the hand once it loads: {:?}",
        arranged.cards.iter().map(|card| (card.name.to_string(), card.kind)).collect::<Vec<_>>()
    );
}

#[gpui::test]
fn an_arranged_hand_lands_in_the_view_that_asked_and_no_other(cx: &mut TestAppContext) {
    install_test_world(cx);
    let mut asking = window(cx, Hand::of([card("SemanticLinkKind", 1), card("RelationLabel", 1)]));
    let mut idle = window(cx, Hand::default());
    for _ in 0..4 {
        asking.draw();
        idle.draw();
    }
    let arranged = asking.draw();
    assert!(arranged.cards.iter().all(|card| card.kind != facet::icons::Kind::Unknown), "the asker drew the world's marks");
    assert_eq!(asking.renders.get(), 2, "the asker drew twice: once apart, once arranged");
    assert_eq!(idle.renders.get(), 1, "a window that asked nothing never redrew");
}

#[gpui::test]
fn touching_a_card_walks_nothing_again_and_the_card_carries_its_new_time(cx: &mut TestAppContext) {
    let handle = install_test_world(cx);
    let walks = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&walks);
    cx.update(|cx| {
        cx.set_global(Hands::arranging(move |key| {
            counted.fetch_add(1, Ordering::SeqCst);
            arrange(&handle, &key.held)
        }));
    });
    let ask = |cx: &mut TestAppContext, hand: &Hand| {
        cx.update(|cx| hand_view(hand, cx));
        cx.run_until_parked();
        cx.update(|cx| hand_view(hand, cx))
    };
    let held = Hand::of([card("SemanticLinkKind", 10), card("RelationLabel", 10)]);
    let first = ask(cx, &held);
    assert_eq!(walks.load(Ordering::SeqCst), 1, "the first hand was walked once");
    assert!(first.cards.iter().all(|card| card.held.touched_at == 10));

    // Every press of a card's number key touches it: the times change, the cards do not.
    for now in [20, 30, 40] {
        let touched = Hand::of([card("SemanticLinkKind", now), card("RelationLabel", now)]);
        let view = ask(cx, &touched);
        assert!(view.cards.iter().all(|card| card.held.touched_at == now), "the cards carry the time they were touched: {now}");
    }
    assert_eq!(walks.load(Ordering::SeqCst), 1, "three touches walked the producer table zero times");

    // A different hand is a different walk.
    ask(cx, &Hand::of([card("RelationLabel", 50)]));
    assert_eq!(walks.load(Ordering::SeqCst), 2, "a card let go is a new hand");
}

#[test]
fn a_world_thread_that_panics_is_one_typed_fault_not_a_hang() {
    let handle = Arc::new(WorldHandle::new());
    load_on_a_thread(&handle, || panic!("the snapshot is not a world"));
    let outcome = wait::until_some("the world thread's panic left everyone waiting for ever", || handle.loaded.get());
    assert!(
        matches!(outcome, Err(WorldFault::Panicked(what)) if what.contains("the snapshot is not a world")),
        "the fault says what happened: {:?}",
        outcome.as_ref().err()
    );
    assert_eq!(
        outcome.as_ref().err().map(ToString::to_string).as_deref(),
        Some("the world thread panicked: the snapshot is not a world"),
        "and says it in words"
    );
}

#[gpui::test]
fn a_faulted_world_is_readable_by_the_window_and_leaves_the_hand_standing_apart(cx: &mut TestAppContext) {
    let handle = Arc::new(WorldHandle::new());
    load_on_a_thread(&handle, || Err(WorldFault::Malformed("not json".to_owned())));
    wait::until("the world thread reported its fault", || handle.loaded.get().is_some());
    cx.update(|cx| cx.set_global(WorldSlot(Arc::clone(&handle))));
    assert_eq!(cx.update(|cx| fault(cx)), Some(WorldFault::Malformed("not json".to_owned())), "the window can read why");
    let mut asking = window(cx, Hand::of([card("RelationLabel", 1)]));
    asking.draw();
    let after = asking.draw();
    assert_eq!(after.apart, [0], "the card stands apart: no world, no road");
    assert!(after.cards.iter().all(|card| card.kind == facet::icons::Kind::Unknown), "and unmarked");
    assert!(!cx.update(|cx| is_loading(cx)), "a faulted world is a settled state, not a wait");
}

#[test]
fn a_restored_window_says_what_it_needs_of_the_world() {
    let empty = Hand::default();
    let holding = Hand::of([card("RelationLabel", 1)]);
    let home = Route::Orbit(OrbitRoute::Home);
    assert_eq!(launch_need(&Route::World, &empty), Some(LaunchNeed::Graph), "the graph waits for the world");
    assert_eq!(launch_need(&view_route("RelationLabel", View::Graph), &empty), Some(LaunchNeed::Graph), "so does a declaration's graph");
    assert_eq!(launch_need(&page_route("RelationLabel"), &empty), None, "a declaration page reads the index alone");
    assert_eq!(launch_need(&home, &empty), None, "and so does home");
    assert_eq!(launch_need(&page_route("RelationLabel"), &holding), Some(LaunchNeed::Hand), "a held card needs its marks and roads");
    assert_eq!(launch_need(&home, &holding), Some(LaunchNeed::Hand));
}

/// The state a window is left in, prepared as `main` prepares it.
fn boot_of(route: crate::model::PersistedRoute, hand: Vec<crate::model::persistence::PersistedHeld>) -> crate::host::launch::Boot {
    let state = crate::model::PersistedDesktopState { route, hand, ..crate::model::PersistedDesktopState::default() };
    // Scratch lives under the repo's `.local/` (never /tmp), private (0700), and goes when the test does.
    let id = NEXT.fetch_add(1, Ordering::SeqCst);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local/scratch")
        .join(format!("i3-boot-{}-{id}", std::process::id()));
    // Unix sockets have a short sockaddr path limit; keep only this endpoint outside the deep repo path.
    let endpoint =
        Path::new("/tmp").join(format!("nx-i3-boot-{}-{id}.sock", std::process::id()));
    let _ = std::fs::remove_file(&endpoint);
    let project = root.join("project");
    let data = root.join("data");
    crate::host::private_dir(&project.join("src")).expect("project");
    std::fs::write(project.join("Cargo.toml"), b"[package]\nname='wanted'\nversion='0.1.0'\nedition='2024'\n").expect("manifest");
    crate::host::private_dir(&data).expect("data");
    std::fs::write(data.join("desktop-state.json"), serde_json::to_vec(&state).expect("state")).expect("write state");
    let paths =
        backend_runtime::WorkspacePaths::discover(Some(project), Some(data), Some(endpoint.clone()))
            .expect("paths");
    let boot = crate::host::launch::prepare(Ok(paths), |_, _| None);
    let _ = std::fs::remove_file(&endpoint);
    let _ = std::fs::remove_dir_all(&root);
    boot
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

#[test]
fn prepare_restores_the_route_and_hand_before_the_owner_answers() {
    use crate::model::PersistedRoute;
    let held = crate::model::persistence::PersistedHeld {
        package: PACKAGE.to_owned(),
        coordinate: Some(coordinate("RelationLabel")),
        why: "pin".to_owned(),
        held_at: 1,
        touched_at: 1,
    };
    let symbol = |view: &str| PersistedRoute::Symbol {
        project: None,
        package: PACKAGE.to_owned(),
        id: coordinate("RelationLabel"),
        at: None,
        view: view.to_owned(),
        line: None,
    };
    let world = boot_of(PersistedRoute::World, Vec::new());
    assert_eq!(launch_need(world.snapshot.route(), &world.snapshot.session().hand), Some(LaunchNeed::Graph));
    let graph = boot_of(symbol("graph"), Vec::new());
    assert_eq!(launch_need(graph.snapshot.route(), &graph.snapshot.session().hand), Some(LaunchNeed::Graph));
    let holding = boot_of(symbol("page"), vec![held]);
    assert_eq!(launch_need(holding.snapshot.route(), &holding.snapshot.session().hand), Some(LaunchNeed::Hand));
    let page = boot_of(symbol("page"), Vec::new());
    assert_eq!(launch_need(page.snapshot.route(), &page.snapshot.session().hand), None, "a page and no cards need no world at launch");
}

#[gpui::test]
fn a_world_that_could_not_be_read_is_told_to_the_window_once(cx: &mut TestAppContext) {
    use crate::model::AppSnapshot;
    let snapshot = Arc::new(AppSnapshot::empty(crate::core::VersionedRoot::unserved()));
    let store = cx.update(|cx| crate::runtime::store::DataStore::install(cx, snapshot, None));
    let notice = |cx: &mut TestAppContext| store.read_with(cx, |store, _| store.notice().map(|notice| notice.message.to_string()));
    store.update(cx, |store, cx| store.ensure(PageKey::Orbit, cx));
    assert_eq!(notice(cx), None, "a world nobody asked for has nothing to say");

    cx.update(|cx| install_fault(WorldFault::Malformed("not a world".to_owned()), cx));
    store.update(cx, |store, cx| store.ensure(PageKey::Orbit, cx));
    let told = notice(cx).expect("the window is told");
    assert!(told.contains("The world could not be read") && told.contains("not a world"), "in words, with the cause: {told}");
    store.update(cx, |store, cx| store.set_notice(None, cx));
    store.update(cx, |store, cx| store.ensure(PageKey::Orbit, cx));
    assert_eq!(notice(cx), None, "told once: a cleared notice does not come back on every frame");
}
