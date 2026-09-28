//! The graph as part of the app (§15), through the real shell: its keys,
//! its tour, the hand and the jump bar on a graph focus, the notice. The
//! graph's fixture is the anatomy tests' small world (the page fixture's
//! `present`, plus an `app` that uses it), joined to the page fixture by
//! the product's own identity adapter; every assertion reads what the
//! shell painted or routed.

use super::anatomy_tests::{self, painted};
use super::tests::{PACKAGE, Rig, indexed_view_route, page_route, rig, view_route};
use crate::model::pages::PackageRef;
use crate::navigation::{Intent, Route, SettingsPage, View};
use crate::shell::bodies::graph::identity::IdentityAdapter;
use gpui::TestAppContext;
use std::sync::Arc;

/// Node ids in the anatomy world.
pub(super) const RELATION_LABEL: u32 = 0;
pub(super) const RELATION_LABEL_FN: u32 = 3;
pub(super) const MAIN: u32 = 7;

/// A shell whose graph (and page anatomy) read the small world, at `route`.
pub(super) fn world_rig(cx: &mut TestAppContext, route: Route) -> Rig {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    let world = anatomy_tests::world();
    let identities = Arc::new(IdentityAdapter::synthetic(&world, PackageRef::parse(PACKAGE).expect("package")));
    rig.cx.update(|_, cx| super::bodies::graph::install_test_world(world, identities, cx));
    anatomy_tests::install(&mut rig);
    rig.go(Intent::Navigate(route));
    rig
}

/// The name of the graph's current tour stop.
pub(super) fn tour_stop(rig: &mut Rig) -> Option<String> {
    rig.shell.read_with(rig.cx, |shell, cx| {
        let graph = shell.graph_entity(cx)?;
        let graph = graph.read(cx);
        let stop = graph.tour_stop()?;
        Some(graph.world().node(stop).name.to_string())
    })
}

/// Space belongs to the graph while it has the keyboard: in a tour it is
/// the next stop (it was the shell's Peek, which found nothing to peek).
#[gpui::test]
fn space_in_a_graph_tour_goes_to_the_next_stop(cx: &mut TestAppContext) {
    let mut rig = world_rig(cx, Route::World);
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(RELATION_LABEL, cx));
    rig.settle();
    rig.keys("t");
    assert_eq!(tour_stop(&mut rig).as_deref(), Some("relation_label"), "T tours the focus's package from its first stop");
    rig.keys("space");
    assert_eq!(tour_stop(&mut rig).as_deref(), Some("RelationLabel"), "Space is the tour's next stop");
    rig.keys("space");
    assert_eq!(tour_stop(&mut rig).as_deref(), Some("SemanticLinkKind"));
}

/// Settings › Keys lists the graph's own keys after the shell's.
#[gpui::test]
fn settings_keys_list_the_graphs_own_keys(cx: &mut TestAppContext) {
    let mut rig = world_rig(cx, Route::World);
    rig.go(Intent::OpenSettings(SettingsPage::Help));
    let _ = painted(&mut rig);
    let said = rig.said();
    let from = said.iter().position(|line| line == "In the graph").unwrap_or_else(|| panic!("no graph keys: {said:#?}"));
    let tour = said[from..].iter().position(|line| line == "tour the package here; again to stop").expect("T's words");
    assert_eq!(said[from + tour - 1], "T", "its cap beside it: {:#?}", &said[from..]);
    assert!(said[from..].iter().any(|line| line == "the tour's next stop (← the one before)"), "{:#?}", &said[from..]);
}

/// The painted words of the jump bar's plate, up to and including the
/// name, and whether a segment is a live target.
fn plate(rig: &mut Rig) -> (Vec<String>, Vec<String>) {
    let ledger = painted(rig);
    let here = ledger.texts.iter().position(|text| text.key == "graph-test-here-name").expect("the name");
    let words = ledger.texts[here.saturating_sub(4)..=here].iter().map(|text| text.content.clone()).collect();
    let targets = ledger.targets.iter().filter(|target| target.key.starts_with("jump-seg-")).map(|target| target.key.clone()).collect();
    (words, targets)
}

/// On a declaration's graph, the plate follows the graph's focus, and its
/// segments open the focus's own pages (package, module siblings).
#[gpui::test]
fn the_jump_bar_follows_the_graph_focus(cx: &mut TestAppContext) {
    let mut rig = world_rig(cx, view_route("RelationLabel", View::Graph));
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(RELATION_LABEL_FN, cx));
    rig.settle();
    let (words, targets) = plate(&mut rig);
    assert_eq!(words, ["present", "›", "glyph", "›", "relation_label"], "package › module › the focus");
    assert!(targets.contains(&"jump-seg-0".to_owned()), "the package is a door: {targets:?}");
    let names: Vec<String> = rig.graph.store.read_with(rig.cx, |store, _| {
        let route = super::jump::bar_route(&store.snapshot(), store);
        super::jump::siblings(&route, 2, store).real.into_iter().map(|s| s.name.to_string()).collect()
    });
    assert_eq!(names, ["RelationLabel", "RelationDirection", "KindGlyph", "relation_label"], "the focus's siblings, not the route's");
    // A focus in another package (`app`'s `main`) reads its own package,
    // not the route's `present › glyph`.
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(MAIN, cx));
    rig.settle();
    let (words, _) = plate(&mut rig);
    assert_eq!(words[words.len() - 3..], ["app", "›", "main"], "{words:?}");
    rig.keys("cmd-[");
    assert_eq!(rig.route(), Route::Orbit(crate::navigation::OrbitRoute::Home), "walking the graph made no place");
}

/// A focus the index has no row for reads package › module › name, quiet
/// and inert: the bar never guesses a page.
#[gpui::test]
fn an_unindexed_graph_focus_is_named_not_guessed(cx: &mut TestAppContext) {
    let mut rig = world_rig(cx, Route::World);
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(RELATION_LABEL, cx));
    rig.settle();
    let (words, targets) = plate(&mut rig);
    assert_eq!(words, ["present", "›", "glyph", "›", "RelationLabel"]);
    assert!(targets.iter().all(|target| target == "jump-seg-2"), "no segment before the name is a door: {targets:?}");
}

/// A graph focus is not a place (Back does not fill with walks), but the
/// graph comes back as you left it after its focus's page: Back, and
/// Forward then Back again. (The page and the graph of one declaration are
/// one place, so coming back to that place is coming back to its graph.)
#[gpui::test]
fn the_graph_comes_back_as_you_left_it(cx: &mut TestAppContext) {
    let mut rig = world_rig(cx, page_route("RelationLabel"));
    rig.keys("g");
    let graph = view_route("RelationLabel", View::Graph);
    assert_eq!(rig.route(), graph);
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(RELATION_LABEL_FN, cx));
    rig.settle();
    let report = |rig: &mut Rig| rig.shell.read_with(rig.cx, |shell, cx| shell.graph_report(cx));
    let before = report(&mut rig);
    assert!(before.contains("focus Some(3)"), "{before}");
    // G with a focus opens its page: a place.
    rig.keys("g");
    let opened = indexed_view_route("relation_label", View::Page);
    assert_eq!(rig.route(), opened, "the focus's page");
    rig.keys("cmd-[");
    assert_eq!(rig.route(), graph);
    assert_eq!(report(&mut rig), before, "Back: the same focus and camera");
    rig.keys("cmd-]");
    assert_eq!(rig.route(), opened);
    rig.keys("cmd-[");
    assert_eq!(report(&mut rig), before, "Forward, then Back: still the same");
}

/// The hand stays in the foot while the graph speaks: its marks at the
/// reader's edge, the graph's line to their right on the same row.
#[gpui::test]
fn the_hand_stays_in_the_foot_while_the_graph_speaks(cx: &mut TestAppContext) {
    let mut rig = world_rig(cx, page_route("RelationLabel"));
    rig.keys("cmd-d");
    rig.keys("g");
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(RELATION_LABEL_FN, cx));
    rig.settle();
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.status_marks(cx)), 1, "the held card is still in the foot");
    let ledger = painted(&mut rig);
    let open = ledger.texts.iter().find(|text| text.key == "hand-open").expect("the hand's chevron");
    let line = ledger.texts.iter().find(|text| text.key.starts_with("address:0:")).expect("the graph's line");
    assert_eq!(line.content, "Graph fixture · present::glyph::relation_label");
    assert!(line.bounds.x > open.bounds.x + open.bounds.width, "to the marks' right: {} vs {}", line.bounds.x, open.bounds.x);
    assert!((line.bounds.y + line.bounds.height / 2.0 - (open.bounds.y + open.bounds.height / 2.0)).abs() < 6.0, "on their row");
}

/// ⌘D on the graph holds its focus, not the route's declaration, and the
/// stone leaves from the focus's glyph on the canvas.
#[gpui::test]
fn cmd_d_on_a_graph_focus_holds_the_focus_from_its_glyph(cx: &mut TestAppContext) {
    let mut rig = world_rig(cx, view_route("RelationLabel", View::Graph));
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(RELATION_LABEL_FN, cx));
    rig.settle();
    rig.repaint();
    let glyph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_focus_glyph(cx)).expect("the focus's glyph is on screen");
    let _ = rig.cx.update(|_, cx| facet::probe::take(cx));
    rig.cx.simulate_keystrokes("cmd-d");
    for _ in 0..40 {
        rig.frame(16);
    }
    let held: Vec<String> = rig.graph.store.read_with(rig.cx, |store, _| {
        store.snapshot().session().hand.held().iter().filter_map(|held| held.id.as_ref().map(|id| id.as_str().to_owned())).collect()
    });
    assert_eq!(held, [super::tests::coordinate("relation_label")], "the focus, not the route's RelationLabel");
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    let key = format!("shared.hand:{}.y", super::tests::coordinate("relation_label"));
    let y: Vec<&facet::probe::TrackSample> = ledger.tracks.iter().filter(|track| track.key == key).collect();
    let (first, last) = (y.first().expect("the stone travelled"), y.last().expect("and landed"));
    let glyph_y = f32::from(glyph.center().y);
    assert!((first.value - glyph_y).abs() < 2.0, "it starts over the glyph ({glyph_y}): {}", first.value);
    assert!(last.value > 860.0 && !last.live, "it lands in the foot: {} (live {})", last.value, last.live);
}

/// T on a package page: the world, flying the package's reading path from
/// its first stop (the graph was not mounted yet: the cold path). Back
/// returns to the page, and T again flies again.
#[gpui::test]
fn t_on_the_package_page_flies_its_tour_from_the_first_stop(cx: &mut TestAppContext) {
    let mut rig = world_rig(cx, anatomy_tests::package_route());
    rig.keys("t");
    assert_eq!(rig.route(), Route::World);
    assert_eq!(tour_stop(&mut rig).as_deref(), Some("relation_label"), "the first stop");
    rig.keys("cmd-[");
    assert_eq!(rig.route(), anatomy_tests::package_route(), "a tour is a place: Back returns");
    rig.keys("t");
    assert_eq!(rig.route(), Route::World);
    assert_eq!(tour_stop(&mut rig).as_deref(), Some("relation_label"), "asked again, flown again");
}

/// The package outline does not pretend a fixture-ranked tour is evidence;
/// the explicit T command still opens the graph tour when requested.
#[gpui::test]
fn the_recorded_outline_keeps_the_graph_tour_explicit(cx: &mut TestAppContext) {
    let mut rig = world_rig(cx, anatomy_tests::package_route());
    let ledger = painted(&mut rig);
    let said = rig.said();
    assert!(said.iter().any(|line| line == "Recorded outline"));
    assert!(!ledger.targets.iter().any(|target| target.key == "tour-fly"));
    assert_eq!(rig.route(), anatomy_tests::package_route());
    rig.keys("t");
    assert_eq!(rig.route(), Route::World);
    assert_eq!(tour_stop(&mut rig).as_deref(), Some("relation_label"));
}
