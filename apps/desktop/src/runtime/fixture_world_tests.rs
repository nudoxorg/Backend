//! W-Open I3: the symbol page's anatomy is computed off the UI thread.
//!
//! Every assertion reads what was painted (the words the reader carried, the
//! probe ledger's boxes) or which thread ran a computation, never the model
//! the page was built from:
//!
//! - the first frame after the world lands already has the anatomy of the
//!   page the window was left on, one redraw brings it, and none of it was
//!   computed on the UI thread;
//! - a visit's page lands with its anatomy already there, because the store
//!   asked the world thread when it submitted the read;
//! - a declaration nobody asked for grows the page once, when its anatomy
//!   lands, never twice;
//! - the restored hand is arranged before the window sees the world, and an
//!   empty hand needs no producer table at all;
//! - the restored route is wanted from the world before any window exists,
//!   and the world thread starts before the workspace is discovered.
//!
//! The probe is off while regions are counted: recording forces every region
//! to repaint each frame.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::model::hand::HeldWhy;
use crate::model::pages::{PageValue, ReadFailure};
use crate::navigation::Intent;
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use crate::shell::tests::{Fixture, PACKAGE, Rig, coordinate, page_route, rig, rig_with_reads};
use facet::graph::{Edge, Kind, Module, Node, Rel};
use gpui::TestAppContext;
use std::thread::ThreadId;
use std::time::Duration;

/// `print_labels` (lines 3-6) uses `RelationLabel`; `show_kind` (lines 8-11)
/// uses `SemanticLinkKind`.
const GLYPH: &str = "//! Glyphs.\n\npub fn print_labels(out: &mut String) {\n    let label = RelationLabel::Related;\n    out.push_str(label.as_str());\n}\n\npub fn show_kind(out: &mut String) {\n    let kind = SemanticLinkKind::Plain;\n    out.push_str(kind.name());\n}\n";

/// Node ids of the fixture world below.
const RELATION_LABEL: NodeId = 0;
const SEMANTIC_LINK_KIND: NodeId = 5;

/// `RelationLabel` (an enum whose variant carries a `SemanticLinkKind`), the
/// function `relation_label` that gives one, a caller of each enum. At the
/// page fixture's line, so the product's own join (`IdentityAdapter`: file,
/// line, name) finds them.
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
    let mut print = node(Kind::Function, "print_labels", 3, None);
    print.end = Some(6);
    print.file = Some("glyph.rs".into());
    let kind = node(Kind::Enum, "SemanticLinkKind", 138, None);
    let mut show = node(Kind::Function, "show_kind", 8, None);
    show.end = Some(11);
    show.file = Some("glyph.rs".into());
    Arc::new(
        World::new(
            vec![Package { name: "present".into(), version: "0.4.2".into(), yours: true, external: false, deps: vec![] }],
            vec![Module { pkg: 0, path: "glyph".into(), file: "glyph.rs".into() }],
            vec![label, typed, related, function, print, kind, show],
            vec![
                Edge { from: 4, to: 0, rel: Rel::USES },
                Edge { from: 0, to: 5, rel: Rel::HAS },
                Edge { from: 3, to: 0, rel: Rel::GIVES },
                Edge { from: 6, to: 5, rel: Rel::USES },
            ],
        )
        .expect("world"),
    )
}

fn package() -> PackageRef {
    PackageRef::parse(PACKAGE).expect("package")
}

fn decl(name: &str) -> DeclRef {
    DeclRef::from_label(&coordinate(name), None, None, None).expect("declaration")
}

fn identities(world: &World) -> Arc<IdentityAdapter> {
    Arc::new(IdentityAdapter::synthetic(world, package()))
}

fn files() -> HashMap<String, Arc<str>> {
    HashMap::from([("glyph.rs".to_owned(), Arc::<str>::from(GLYPH))])
}

/// The world thread serves `world`, having computed `wanted` before it said
/// so, as the product's does. The window draws once as it is installed.
fn serve_world(rig: &mut Rig, id: WorldId, world: &Arc<World>, wanted: &[&str]) {
    let world = Arc::clone(world);
    let identities = identities(&world);
    let wanted = wanted.iter().map(|name| Want { decl: decl(name), package: package() }).collect::<Vec<_>>();
    rig.cx.update(|_, cx| install_serving(id, world, identities, files(), &wanted, Vec::new(), cx));
}

/// A frame as the platform draws it: what changed, no forced refresh.
fn frame(rig: &mut Rig) {
    rig.cx.run_until_parked();
    rig.draw();
}

/// Whether the reader carries `words` now.
fn says(rig: &mut Rig, words: &str) -> bool {
    rig.said().iter().any(|line| line.contains(words))
}

/// Draws frames until `done`, or fails naming `what`.
fn until(rig: &mut Rig, what: &str, mut done: impl FnMut(&mut Rig) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        frame(rig);
        if done(rig) {
            return;
        }
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The threads that ran `work` for `world`.
fn ran(world: WorldId, work: Work) -> Vec<ThreadId> {
    ran_on()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter(|ran| ran.work == work && ran.world == world)
        .map(|ran| ran.thread)
        .collect()
}

/// Asserts `work` ran at least once, and never on this (the UI) thread.
fn only_off_the_ui_thread(world: WorldId, work: Work) {
    let here = std::thread::current().id();
    let threads = ran(world, work);
    assert!(!threads.is_empty(), "{work:?} never ran");
    assert!(threads.iter().all(|thread| *thread != here), "{work:?} ran on the UI thread ({here:?}): {threads:?}");
}

fn loading(rig: &mut Rig) -> bool {
    rig.cx.update(|_, cx| is_loading(cx))
}

fn card(name: &str) -> Held {
    Held {
        package: crate::core::PackageId::new(PACKAGE).expect("package"),
        id: Some(crate::navigation::Coordinate::new(&coordinate(name)).expect("coordinate")),
        why: HeldWhy::Pin,
        held_at: 1,
        touched_at: 1,
    }
}

/// What a `RelationLabel` page says only when the world knows it: the
/// statement's caller, its place and the line itself.
const ANATOMY_WORDS: [&str; 3] = ["print_labels", "present · glyph.rs:4", "let label = RelationLabel::Related;"];

fn has_anatomy(rig: &mut Rig) -> bool {
    ANATOMY_WORDS.iter().all(|words| says(rig, words))
}

#[gpui::test]
fn the_first_frame_after_the_world_lands_has_the_anatomy_and_none_of_it_was_computed_on_the_ui_thread(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    assert!(!ANATOMY_WORDS.iter().any(|words| says(&mut rig, words)), "the page drew world anatomy with no world: {:?}", rig.said());

    // The world thread has computed the anatomy of the page the window was
    // left on before it says the world is there: the frame the world's
    // arrival draws is the one that has it.
    let world = world();
    let id = WorldId::next();
    let before = rig.counts();
    serve_world(&mut rig, id, &world, &["RelationLabel"]);
    let after = rig.counts();
    assert!(has_anatomy(&mut rig), "the first frame after the world landed lacks its anatomy: {:?}", rig.said());
    assert_eq!(after.reader - before.reader, 1, "one redraw brought all of it: {before:?} -> {after:?}");
    for _ in 0..5 {
        frame(&mut rig);
    }
    let settled = rig.counts();
    assert_eq!(
        (settled.reader, settled.shelf, settled.status),
        (after.reader, after.shelf, after.status),
        "and nothing redrew after it: {after:?} -> {settled:?}"
    );
    only_off_the_ui_thread(id, Work::Anatomy(RELATION_LABEL));

    // Painted, not only said: the caller's box is on the page, with a size.
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    rig.repaint();
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    let caller = ledger.texts.iter().find(|text| text.key == "page-site-0-caller").expect("the caller was painted");
    assert_eq!(caller.content, "print_labels");
    assert!(caller.bounds.width > 0.0 && caller.bounds.height > 0.0, "and it has a box: {:?}", caller.bounds);
}

/// Holds the reads of one declaration's page until the test opens it.
struct HeldReads {
    name: &'static str,
    open: Arc<(Mutex<bool>, Condvar)>,
}

impl PageReader for HeldReads {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        if let ReadRequest::Symbol(symbol) = request
            && symbol.identity().name() == self.name
        {
            let (state, opened) = &*self.open;
            let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
            while !*state {
                state = opened.wait(state).unwrap_or_else(PoisonError::into_inner);
            }
        }
        Fixture.read(request, context)
    }
}

#[gpui::test]
fn a_visit_asks_the_world_thread_while_its_page_is_read_so_the_page_lands_with_its_anatomy(cx: &mut TestAppContext) {
    let open = Arc::new((Mutex::new(false), Condvar::new()));
    let gate = Arc::clone(&open);
    let pool = ReadPool::start(2, move |_| HeldReads { name: "SemanticLinkKind", open: Arc::clone(&gate) }).expect("pool");
    let mut rig = rig_with_reads(cx, Some(page_route("RelationLabel")), 1440.0, 900.0, pool);
    let world = world();
    let id = WorldId::next();
    serve_world(&mut rig, id, &world, &["RelationLabel"]);
    until(&mut rig, "the first page has its anatomy", has_anatomy);

    // Visit another declaration. Its page read is held (a slow owner); the
    // store asked the world thread for its anatomy when it submitted it.
    rig.graph.root.update(rig.cx, |root, cx| root.queue(Intent::Navigate(page_route("SemanticLinkKind")), cx));
    until(&mut rig, "the visit's anatomy was computed while its page is still being read", |rig| {
        !ran(id, Work::Anatomy(SEMANTIC_LINK_KIND)).is_empty() && !loading(rig)
    });
    assert!(!says(&mut rig, "The readable label of SemanticLinkKind."), "the page must still be on its way: {:?}", rig.said());

    // The read lands: the first frame that shows the page shows its anatomy.
    {
        let (state, opened) = &*open;
        *state.lock().unwrap_or_else(PoisonError::into_inner) = true;
        opened.notify_all();
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while !says(&mut rig, "The readable label of SemanticLinkKind.") {
        assert!(Instant::now() < deadline, "the visited page never landed");
        std::thread::sleep(Duration::from_millis(2));
        frame(&mut rig);
    }
    assert!(
        says(&mut rig, "show_kind") && says(&mut rig, "let kind = SemanticLinkKind::Plain;"),
        "the first frame that shows the visited page already has the caller the world knows: {:?}",
        rig.said()
    );
    only_off_the_ui_thread(id, Work::Anatomy(RELATION_LABEL));
    only_off_the_ui_thread(id, Work::Anatomy(SEMANTIC_LINK_KIND));
}

#[gpui::test]
fn a_declaration_nobody_asked_for_grows_the_page_once_when_its_anatomy_lands(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);

    // A world that was told nothing, and whose thread is held: the page's
    // own lookup is the first ask, and it answers with the page as it is.
    let world = world();
    let id = WorldId::next();
    let gate = hold_requests(id);
    serve_world(&mut rig, id, &world, &[]);
    for _ in 0..3 {
        frame(&mut rig);
    }
    assert!(!ANATOMY_WORDS.iter().any(|words| says(&mut rig, words)), "the page grew before its anatomy was computed: {:?}", rig.said());
    assert!(loading(&mut rig), "the window knows an anatomy is on its way (the harness waits on it)");
    let before = rig.counts();

    open_gate(&gate);
    until(&mut rig, "the anatomy landed and the page grew", has_anatomy);
    for _ in 0..5 {
        frame(&mut rig);
    }
    let after = rig.counts();
    assert_eq!(after.reader - before.reader, 1, "the page grew in exactly one redraw: {before:?} -> {after:?}");
    assert!(!loading(&mut rig), "nothing is left in flight");
    only_off_the_ui_thread(id, Work::Anatomy(RELATION_LABEL));
}

#[test]
fn a_hand_restored_from_the_last_launch_is_arranged_before_the_window_sees_the_world() {
    let world = world();
    let id = WorldId::next();
    let cards = vec![card("SemanticLinkKind"), card("RelationLabel")];
    let ready = spawn_serving(id, Arc::clone(&world), identities(&world), files(), Wanted { nodes: Vec::new(), hand: cards.clone() });
    let arranged = ran(id, Work::Hand);
    assert_eq!(arranged.len(), 1, "the restored hand was arranged once, before the world was announced");
    assert!(arranged[0] != std::thread::current().id(), "and not on the UI thread");

    let mut tables = Tables::new(ready);
    let view = tables.hand(&cards);
    let names = view.cards.iter().map(|card| card.name.to_string()).collect::<Vec<_>>();
    assert!(
        names.len() == 2 && names.contains(&"RelationLabel".to_owned()) && names.contains(&"SemanticLinkKind".to_owned()),
        "both cards are in the first view of the hand: {names:?}"
    );
    assert!(
        view.cards.iter().all(|card| card.kind != facet::icons::Kind::Unknown),
        "with the world's marks, not the placeholder's: {:?}",
        view.cards.iter().map(|card| card.kind).collect::<Vec<_>>()
    );
    assert_eq!(ran(id, Work::Hand).len(), 1, "asking for the same hand arranged nothing again, here or anywhere");
}

#[test]
fn an_empty_hand_is_arranged_without_a_producer_table_and_the_answer_is_the_same() {
    let world = world();
    let id = WorldId::next();
    let ready = spawn_serving(id, Arc::clone(&world), identities(&world), files(), Wanted { nodes: Vec::new(), hand: Vec::new() });
    let engine = &ready.engine;
    let recipes = Recipes::from_prepared(engine.prepared.clone());
    let shortcut = format!("{:?}", engine.arrange(&recipes, &[]));
    assert!(ran(id, Work::Hand).is_empty(), "the shortcut walked no table");
    let walked = format!("{:?}", engine.arrange_all(&recipes, &[]));
    assert_eq!(ran(id, Work::Hand).len(), 1, "the full path did");
    assert_eq!(shortcut, walked, "no cards, no arrangement: the same answer either way");
}

#[test]
fn a_world_that_failed_to_load_settles_every_waiter_instead_of_hanging_them() {
    let loaded: Stage<Result<Loaded, WorldFault>> = Stage::new();
    let ready: Stage<Result<Arc<Ready>, WorldFault>> = Stage::new();
    loaded.set(Err(WorldFault::Malformed("one".to_owned())));
    let other = world();
    loaded.set(Ok((Arc::clone(&other), identities(&other))));
    assert!(matches!(loaded.wait(), Err(WorldFault::Malformed(why)) if why == "one"), "the first setter wins");
    assert!(!ready.is_set());
    let waiting = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| ready.wait());
        std::thread::sleep(Duration::from_millis(20));
        ready.set(Err(WorldFault::Stopped));
        waiter.join().expect("the waiter")
    });
    assert!(matches!(waiting, Err(WorldFault::Stopped)), "a waiter wakes when the stage is settled");
    assert_eq!(String::from(WorldFault::Stopped), "the world thread stopped before it finished", "and a fault says why in words");
}

#[test]
fn a_restored_declaration_page_is_wanted_from_the_world_before_any_window_exists() {
    take_wanted();
    let state = crate::model::PersistedDesktopState {
        route: crate::model::PersistedRoute::Symbol {
            project: None,
            package: PACKAGE.to_owned(),
            id: coordinate("RelationLabel"),
            at: None,
            view: "page".to_owned(),
            line: None,
        },
        ..crate::model::PersistedDesktopState::default()
    };
    // Scratch lives under the repo's `.local/` (never /tmp), and goes when the test does.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local/scratch").join(format!("i3-wants-{}", std::process::id()));
    let project = root.join("project");
    let data = root.join("data");
    std::fs::create_dir_all(project.join("src")).expect("project");
    std::fs::write(project.join("Cargo.toml"), b"[package]\nname='wanted'\nversion='0.1.0'\nedition='2024'\n").expect("manifest");
    std::fs::create_dir_all(&data).expect("data");
    std::fs::write(data.join("desktop-state.json"), serde_json::to_vec(&state).expect("state")).expect("write state");
    let paths = backend_runtime::WorkspacePaths::discover(Some(project), Some(data), Some(root.with_extension("sock"))).expect("paths");
    let boot = crate::host::launch::prepare(Ok(paths), |_, _| None);
    drop(boot);
    let _ = std::fs::remove_dir_all(&root);
    let wanted = take_wanted();
    assert!(
        wanted.iter().any(|want| want.decl.coordinate.as_str() == coordinate("RelationLabel") && want.package == package()),
        "the route the window was left on is wanted from the world: {:?}",
        wanted.iter().map(|want| want.decl.coordinate.as_str()).collect::<Vec<_>>()
    );
}

#[test]
fn the_world_thread_starts_before_the_workspace_is_even_discovered() {
    let launch = include_str!("../host/launch.rs");
    let body = launch.split("pub fn main_entry").nth(1).expect("main_entry");
    let body = body.split("\n}\n").next().expect("main_entry's body");
    let preload = body.find("fixture_world::preload()").expect("main_entry starts the world thread");
    let discover = body.find("paths::discover()").expect("main_entry discovers the workspace");
    assert!(preload < discover, "the world thread must start before the workspace is discovered (13 ms of the ~200 ms it needs)");
}
