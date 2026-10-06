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

#[path = "graph_production_tests.rs"]
mod production;

/// Node ids in the anatomy world.
pub(super) const RELATION_LABEL: u32 = 0;
pub(super) const RELATION_LABEL_FN: u32 = 3;
pub(super) const MAIN: u32 = 7;

/// A shell whose graph (and page anatomy) read the small world, at `route`.
pub(super) fn world_rig(cx: &mut TestAppContext, route: Route) -> Rig {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    let world = anatomy_tests::world();
    let identities = Arc::new(IdentityAdapter::synthetic(&world, PackageRef::parse(PACKAGE).expect("package")));
    let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    rig.cx.update(|_, cx| super::bodies::graph::install_test_world(root, world, identities, cx));
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
    rig.keys("secondary-[");
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
    rig.keys("secondary-[");
    assert_eq!(rig.route(), graph);
    assert_eq!(report(&mut rig), before, "Back: the same focus and camera");
    rig.keys("secondary-]");
    assert_eq!(rig.route(), opened);
    rig.keys("secondary-[");
    assert_eq!(report(&mut rig), before, "Forward, then Back: still the same");
}

/// The hand stays in the foot while the graph speaks: its marks at the
/// reader's edge, the graph's line to their right on the same row.
#[gpui::test]
fn the_hand_stays_in_the_foot_while_the_graph_speaks(cx: &mut TestAppContext) {
    let mut rig = world_rig(cx, page_route("RelationLabel"));
    rig.keys("secondary-d");
    rig.keys("g");
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(RELATION_LABEL_FN, cx));
    rig.settle();
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.status_marks(cx)), 1, "the held card is still in the foot");
    let origin = rig.graph.store.read_with(rig.cx, |store, _| {
        store.graph_focus().map(|focus| focus.origin.fixture_identity().map(str::to_owned))
    });
    assert_eq!(origin.flatten().as_deref(), Some("single-package anatomy fixture"), "the test owner is explicit and app-scoped");
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
    rig.cx.simulate_keystrokes("secondary-d");
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

/// ⌘D on a graph focus the index has no row for holds nothing, and says so
/// through the one visit-scoped Notice (§15 ruling 1) instead of failing
/// silently. Navigating away clears it: the Notice never outlives its visit.
#[gpui::test]
fn cmd_d_on_an_unindexed_graph_focus_speaks_through_the_notice(cx: &mut TestAppContext) {
    let mut rig = world_rig(cx, Route::World);
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(RELATION_LABEL, cx));
    rig.settle();
    rig.keys("secondary-d");
    rig.settle();
    let held = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().hand.held().len());
    assert_eq!(held, 0, "not indexed: nothing to hold");
    let message = rig.graph.store.read_with(rig.cx, |store, _| store.notice().map(|notice| notice.message.to_string()));
    assert_eq!(message.as_deref(), Some("present::glyph::RelationLabel isn't in the index"));
    let ledger = painted(&mut rig);
    let line = ledger.texts.iter().find(|text| text.key.starts_with("address:0:")).expect("the notice is drawn in the foot");
    assert!(line.content.contains("isn't in the index"), "{}", line.content);
    rig.go(Intent::Navigate(crate::navigation::Route::Orbit(crate::navigation::OrbitRoute::Home)));
    assert!(rig.graph.store.read_with(rig.cx, |store, _| store.notice().is_none()), "a notice does not survive navigation");
}

#[gpui::test]
fn graph_messages_keep_full_native_semantics_inside_their_painted_bounds(cx: &mut TestAppContext) {
    let mut rig = world_rig(cx, Route::World);
    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
    rig.cx.update(|window, _| window.set_a11y_forced(true));
    for appearance in [crate::model::AppearancePreference::Abyss, crate::model::AppearancePreference::Glacier] {
        rig.go(Intent::SetAppearance(appearance));
        for percent in [100_u16, 150, 200] {
            rig.go(Intent::ZoomTo { display: display.clone(), percent });
            for width in [360.0, 480.0, 663.0, 1440.0] {
                rig.cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(900.0)));
                rig.settle();
                rig.cx.update(|_, cx| { let _ = facet::probe::take(cx); });
                let ledger = painted(&mut rig);
                let caption = ledger.texts.iter().find(|text| text.key == "graph-projection-status").expect("painted graph status");
                assert!(!caption.clipped_without_ellipsis() && !caption.clipped_vertically());
                assert!(caption.bounds.x >= -0.5 && caption.bounds.x + caption.bounds.width <= width + 0.5);
                let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native graph tree");
                let tree: serde_json::Value = serde_json::from_str(&json).expect("native tree JSON");
                assert!(tree["nodes"].as_object().expect("nodes").values().any(|node|
                    node["aria"]["role"].as_str() == Some("Status") && node["aria"]["label"].as_str() == Some(caption.content.as_str())), "the native status must preserve the exact projected coverage words");
            }
        }
    }
    // Advance the fixture owner's exact authority without fabricating a
    // projection for the new root. Its unavailable state must be native too.
    let old_root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    rig.go(Intent::RefreshRoot { basis: old_root, request: crate::navigation::RequestId::from_authority(old_root, 801) });
    rig.cx.update(|_, cx| { let _ = facet::probe::take(cx); });
    let ledger = painted(&mut rig);
    let terminal = ledger.texts.iter().find(|text| text.key == "graph-terminal-message").expect("actual unavailable graph message");
    let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native terminal graph tree");
    let tree: serde_json::Value = serde_json::from_str(&json).expect("native tree JSON");
    assert!(tree["nodes"].as_object().expect("nodes").values().any(|node|
        node["aria"]["role"].as_str() == Some("Status") && node["aria"]["label"].as_str() == Some(terminal.content.as_str())));
    assert!(!terminal.clipped_without_ellipsis() && !terminal.clipped_vertically());
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
    rig.keys("secondary-[");
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
    assert!(ledger.texts.iter().any(|text| text.key.contains("shingles-region-glyph")), "the package page draws its recorded outline as the territory: {said:#?}");
    assert!(!ledger.targets.iter().any(|target| target.key == "tour-fly"));
    assert_eq!(rig.route(), anatomy_tests::package_route());
    rig.keys("t");
    assert_eq!(rig.route(), Route::World);
    assert_eq!(tour_stop(&mut rig).as_deref(), Some("relation_label"));
}

/// The lead's report: T pressed while already on World (no page-side ask,
/// no focus) must still be the graph's own key, not a dead one. S9 scoped
/// the shell's T to `!Graph` so the canvas keeps it; with nothing focused,
/// `tour_package` falls back to the camera's own territory, so landing on
/// World over a package and pressing T starts that package's tour.
#[gpui::test]
fn t_on_world_over_a_package_starts_its_tour_with_nothing_focused(cx: &mut TestAppContext) {
    let mut rig = world_rig(cx, Route::World);
    assert_eq!(tour_stop(&mut rig).as_deref(), None, "no tour yet");
    rig.keys("t");
    assert_eq!(rig.route(), Route::World, "T in the graph never navigates");
    assert!(tour_stop(&mut rig).is_some(), "T over a package starts its tour from the camera's own territory");
}


/// Source-fixture rows reconstructed from Run19's two physical files. These
/// are not claimed to be a decoded owner response. They exercise the exact
/// production row mapper and identity adapter, then real native activation.
fn canary_native_rig(cx: &mut TestAppContext, width: f32, scale: f32, appearance: facet::tokens::Appearance) -> (Rig, crate::runtime::owner::OwnerGate) {
    use backend_library::{Basis, DeclarationKind, Row, RowId, SourceAvailability, SourceLocation, object_version, symbol_key, view_state_root};
    use facet::graph::{Module, Package, World};
    use crate::core::VersionedRoot;
    use crate::runtime::reads::ReadPool;
    use super::bodies::graph::identity::ResolvedSymbol;
    use std::collections::BTreeMap;
    use facet::theme::ActiveFacet;
    let launch_root = VersionedRoot::synthetic(view_state_root(&[("shell".to_owned(), "tests".to_owned())]), 4);
    let gate = crate::runtime::owner::OwnerGate::ready(launch_root, crate::model::ServiceMode::Attached);
    let mut rig = super::tests::rig_with_engine_gate(cx, None, width, 900.0,
        ReadPool::start(2, |_| super::tests::Fixture).expect("fixture pool"), super::tests::RootOnly, Some(gate.clone()));
    let preference = match appearance {
        facet::tokens::Appearance::Abyss => crate::model::AppearancePreference::Abyss,
        facet::tokens::Appearance::Glacier => crate::model::AppearancePreference::Glacier,
    };
    rig.go(Intent::SetAppearance(preference));
    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
    rig.go(Intent::ZoomTo { display, percent: (scale * 100.0) as u16 });
    // Startup admits the owner and RootOnly can publish a newer cursor before
    // this fixture is installed. The projection must bind to the same exact
    // authority the current Reader will request, not the launch gate's key.
    let root = rig.graph.store.read_with(rig.cx, |store, _| {
        assert!(store.current_owner_attachment().is_some(), "canary needs a serving owner attachment");
        store.snapshot().key()
    });
    let model_root = rig.graph.root.read_with(rig.cx, |model, _| model.snapshot().key());
    assert!(root.same_authority(model_root), "canary store/model authority diverged before projection: store={root:?}, model={model_root:?}");
    assert_eq!(root.root(), launch_root.root(), "canary startup changed the certified fixture root");
    let package = PackageRef::parse("/fixture/real-rust-canary").expect("package");
    let basis = Basis::new(view_state_root(&[]), object_version(b"Run19 source fixture"));
    let mut exact = BTreeMap::new();
    let mut nodes = Vec::new();
    for (index, (kind, path, line, module)) in [
        (DeclarationKind::Import, "src/lib.rs", 17, 0),
        (DeclarationKind::Function, "src/cadence.rs", 8, 1),
    ].into_iter().enumerate() {
        let label = format!("{}::semantic::{}::advance_signal", package.as_str(), if index == 0 { "1".repeat(64) } else { "2".repeat(64) });
        let symbol = crate::model::pages::SymbolRef::new(&label).expect("symbol");
        let mut row = Row::new(RowId::Symbol(symbol_key(&label)), basis, label);
        row.kind = Some(kind);
        row.source = SourceAvailability::Captured(SourceLocation::new(path, line).expect("source"));
        row.document = vec![backend_library::Fragment::Text("Advance a signal by one tick using saturating arithmetic.".into())].into_boxed_slice();
        nodes.push(crate::runtime::indexed_world::project_declaration(&row, &symbol, 0, module));
        exact.insert(index as u32, ResolvedSymbol { symbol, package: package.clone(), line: Some(line) });
    }
    let mut world = World::new(vec![Package { name: "real-rust-canary".into(), version: "0.0.1".into(), yours: true, external: false, deps: vec![] }],
        vec![Module { pkg: 0, path: "".into(), file: "src/lib.rs".into() }, Module { pkg: 0, path: "cadence".into(), file: "src/cadence.rs".into() }], nodes, vec![]).expect("canary graph");
    world.knowledge = crate::runtime::indexed_world::projection_knowledge();
    let world = Arc::new(world);
    let identities = Arc::new(IdentityAdapter::indexed(&world, &BTreeMap::from([(package, 0)]), exact));
    rig.cx.update(|_, cx| super::bodies::graph::install_test_world(root, world, identities, cx));
    rig.go(Intent::Navigate(Route::World));
    let at_graph = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    assert!(root.same_authority(at_graph), "canary projection became stale before Graph mounted: installed={root:?}, current={at_graph:?}");
    let mounted = rig.shell.read_with(rig.cx, |shell, cx| {
        (shell.graph_entity(cx).is_some(), shell.graph_report(cx))
    });
    assert!(mounted.0, "current canary projection did not mount: installed={root:?}, current={at_graph:?}, graph={}", mounted.1);
    rig.cx.update(|_, cx| {
        assert_eq!(cx.facet().appearance, appearance);
        assert_eq!(cx.facet().text_scale, scale);
    });
    (rig, gate)
}

#[gpui::test]
fn real_canary_shape_has_native_exact_selection_at_each_text_scale(cx: &mut TestAppContext) {
    for appearance in [facet::tokens::Appearance::Abyss, facet::tokens::Appearance::Glacier] {
        for width in [360.0, 480.0, 663.0, 1440.0] {
            for scale in [1.0, 1.5, 2.0] {
                let (mut rig, _) = canary_native_rig(cx, width, scale, appearance);
                let toggle = super::tests::native_bounds(&mut rig, "Button", "Declarations", true).expect("native declaration chooser");
                rig.cx.simulate_click(toggle.center(), gpui::Modifiers::none());
                rig.settle();
                let name = "Select real-rust-canary::cadence::advance_signal · function. src/cadence.rs:8";
                let definition = super::tests::native_bounds(&mut rig, "Button", name, true).expect("exact definition action");
                assert!(definition.left() >= gpui::px(0.0) && definition.right() <= gpui::px(width), "{width}px/text{scale}: native row is contained");
                let _ = super::tests::native_bounds(&mut rig, "Button", "Select real-rust-canary::advance_signal · import / re-export. src/lib.rs:17", true).expect("separate re-export action");
                rig.cx.update(|_, cx| facet::probe::enable(cx));
                rig.cx.simulate_click(definition.center(), gpui::Modifiers::none());
                rig.settle();
                if width == 1440.0 && scale == 1.0 {
                    let graph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("graph");
                    assert_eq!(graph.read_with(rig.cx, |graph, _| graph.stats().selected_labels), 1, "the actual painter admitted a selected caption");
                }
                let focus = rig.graph.store.read_with(rig.cx, |store, _| store.graph_focus().cloned()).expect("selected declaration");
                assert_eq!(focus.node, 1);
                assert_eq!(focus.kind, backend_library::DeclarationKind::Function);
                assert_eq!(focus.module.as_ref(), "cadence");
                assert!(focus.indexed.expect("exact join").1.as_str().contains(&"2".repeat(64)));
                assert!(super::tests::native_bounds(&mut rig, "Label", "src/cadence.rs:8", false).is_some(), "source capability is accessible");
                assert!(super::tests::native_bounds(&mut rig, "Label", "0 referring declarations observed", false).is_some(), "zero observed is not an absence claim");
            }
        }
    }
}

fn graph_native_evidence(rig: &mut Rig) -> String {
    #[cfg(debug_assertions)]
    {
        let reader = rig.shell.read_with(rig.cx, |shell, _| shell.reader_entity());
        let graph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx));
        return rig.cx.update(|window, cx| format!("focus={:?}; {}; graph={}", window.focused(cx),
            reader.read(cx).graph_native_diagnostic(cx), graph.as_ref().map_or_else(|| "not mounted".into(), |graph| graph.read(cx).native_focus_diagnostic(window, cx))));
    }
    #[cfg(not(debug_assertions))]
    { let _ = rig; "native diagnostic requires debug assertions".into() }
}

fn graph_native_inventory(rig: &mut Rig) -> (Option<String>, Vec<serde_json::Value>) {
    let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native tree");
    let tree: serde_json::Value = serde_json::from_str(&json).expect("tree");
    let focused = tree["gpui_focus"].as_str().map(|id| tree["nodes"][id]["aria"]["label"].as_str().unwrap_or("<unlabelled native focus>").to_owned());
    let controls = tree["nodes"].as_object().expect("native nodes").values()
        .filter(|node| matches!(node["aria"]["role"].as_str(), Some("Button" | "TextInput")))
        .cloned().collect();
    (focused, controls)
}

fn tab_to_graph_control(rig: &mut Rig, label: &str) {
    rig.cx.update(|window, _| window.set_a11y_forced(true));
    rig.repaint();
    let (_, controls) = graph_native_inventory(rig);
    assert!(controls.iter().any(|node| node["aria"]["label"].as_str() == Some(label)
        && node["aria"]["disabled"] != true
        && node["aria"]["on_action"].as_array().is_some_and(|actions| actions.iter().any(|action| action == "Click"))),
        "requested graph stop is not an enabled mounted Click control: {label}; {controls:?}");
    let mut walked = Vec::new();
    for _ in 0..64 {
        rig.keys("tab");
        let (focused, _) = graph_native_inventory(rig);
        if focused.as_deref() == Some(label) { return; }
        walked.push(focused);
    }
    let (_, controls) = graph_native_inventory(rig);
    panic!("native Tab never reached {label}; actual focus walk={walked:?}; mounted controls={controls:?}");
}

#[gpui::test]
fn native_tab_and_enter_select_the_exact_definition(cx: &mut TestAppContext) {
    let (mut rig, _) = canary_native_rig(cx, 663.0, 1.5, facet::tokens::Appearance::Abyss);
    tab_to_graph_control(&mut rig, "Declarations");
    rig.native_press("enter"); rig.settle();
    let import = "Select real-rust-canary::advance_signal · import / re-export. src/lib.rs:17";
    let definition = "Select real-rust-canary::cadence::advance_signal · function. src/cadence.rs:8";
    rig.keys("tab");
    assert_eq!(graph_native_inventory(&mut rig).0.as_deref(), Some(import), "Tab takes the first actual mounted row");
    rig.keys("shift-tab");
    assert_eq!(graph_native_inventory(&mut rig).0.as_deref(), Some("Hide declarations"), "Shift-Tab returns to the same real chooser handle");
    rig.keys("tab"); rig.keys("tab");
    assert_eq!(graph_native_inventory(&mut rig).0.as_deref(), Some(definition), "bounded native order preserves the distinct definition");
    rig.native_press("enter");
    let focus = rig.graph.store.read_with(rig.cx, |store, _| store.graph_focus().cloned()).expect("native selected definition");
    assert_eq!(focus.node, 1);
    assert_eq!(focus.kind, backend_library::DeclarationKind::Function);
}

#[gpui::test]
fn native_graph_tab_denial_preserves_focus_before_owner_repaint(cx: &mut TestAppContext) {
    let (mut rig, gate) = canary_native_rig(cx, 663.0, 1.5, facet::tokens::Appearance::Abyss);
    tab_to_graph_control(&mut rig, "Declarations");
    rig.native_press("enter"); rig.settle();
    let before = rig.cx.update(|window, cx| window.focused(cx));
    let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    gate.publish(crate::runtime::owner::OwnerState::Starting);
    gate.publish(crate::runtime::owner::OwnerState::Ready { key: root, mode: crate::model::ServiceMode::Attached });
    // Real native key, no watcher, draw, refresh or input between renewal and
    // dispatch. Denied is consumed rather than mistaken for a zone boundary.
    rig.cx.simulate_keystrokes("tab");
    assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), before);
    assert!(rig.graph.store.read_with(rig.cx, |store, _| store.graph_focus().is_none()));
}

#[gpui::test]
fn graph_find_uses_guarded_component_tab_and_stays_locally_editable(cx: &mut TestAppContext) {
    let (mut rig, gate) = canary_native_rig(cx, 663.0, 1.5, facet::tokens::Appearance::Abyss);
    tab_to_graph_control(&mut rig, "Declarations");
    rig.keys("shift-tab");
    let graph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("graph");
    assert!(rig.cx.update(|window, cx| graph.read(cx).find_focused(window, cx)), "Shift-Tab reaches the real local text engine");
    let before = graph_native_evidence(&mut rig);
    let actions = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let observed = actions.clone();
    let _keys = rig.cx.update(|_, cx| cx.observe_keystrokes(move |event, _, _| observed.borrow_mut().push(format!("{event:?}"))));
    rig.keys("tab");
    let after = graph_native_evidence(&mut rig);
    assert_eq!(graph_native_inventory(&mut rig).0.as_deref(), Some("Graph coverage"), "Input's existing component Tab reaches the same actual native order; before={before}; after={after}; dispatched={:?}", actions.borrow());
    rig.keys("shift-tab");
    assert_eq!(graph_native_inventory(&mut rig).0.as_deref(), Some("Declarations"), "blur restores the collapsed chooser in its authored order");
    rig.keys("shift-tab");
    assert!(rig.cx.update(|window, cx| graph.read(cx).find_focused(window, cx)));
    let before = rig.cx.update(|window, cx| window.focused(cx));
    gate.publish(crate::runtime::owner::OwnerState::Starting);
    rig.cx.simulate_keystrokes("tab");
    assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), before, "component Root cannot bypass revoked resource focus admission");
    rig.cx.simulate_input("cadence");
    rig.draw();
    assert!(rig.cx.update(|window, _| window.a11y_tree().expect("local editor frame").nodes.iter()
        .any(|(_, node)| node.value() == Some("cadence"))), "the actual native text engine retained the local edit");
    assert!(rig.cx.update(|window, cx| graph.read(cx).find_focused(window, cx)), "owner absence does not disable the local editor");
    rig.native_press("enter");
    assert_eq!(rig.route(), Route::World, "local editing grants no producer navigation");
    assert!(graph.read_with(rig.cx, |graph, _| graph.focused()).is_none());
}

#[gpui::test]
fn freshly_painted_owner_failed_graph_has_local_find_and_real_native_boundaries(cx: &mut TestAppContext) {
    let (mut rig, gate) = canary_native_rig(cx, 663.0, 1.5, facet::tokens::Appearance::Abyss);
    tab_to_graph_control(&mut rig, "Declarations");
    gate.publish(crate::runtime::owner::OwnerState::Failed("fixture owner unavailable".into()));
    // This assertion concerns a genuinely new, unavailable painted frame,
    // unlike the separate revoked-before-repaint denial test.
    rig.draw(); rig.repaint(); rig.settle();
    let graph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("retained native scene");
    rig.cx.update(|window, _| {
        let tree = window.a11y_tree().expect("unavailable native frame");
        for label in ["Declarations", "Graph coverage"] {
            let node = tree.nodes.iter().find(|(_, node)| node.label() == Some(label)).map(|(_, node)| node).expect("visible disabled resource control");
            assert!(node.is_disabled(), "{label} is painted unavailable");
            assert!(!node.supports_action(gpui::AccessibleAction::Click), "{label} cannot advertise an activation");
        }
    });
    let before = graph_native_evidence(&mut rig);
    rig.keys("tab");
    let after = graph_native_evidence(&mut rig);
    assert!(rig.cx.update(|window, cx| graph.read(cx).find_focused(window, cx)), "the first enabled stop is the actual local editor; before={before}; after={after}");
    rig.cx.simulate_input("cadence"); rig.draw();
    assert!(rig.cx.update(|window, _| window.a11y_tree().expect("local edit").nodes.iter().any(|(_, node)| node.value() == Some("cadence"))));
    rig.native_press("enter");
    assert!(graph.read_with(rig.cx, |graph, _| graph.focused()).is_none(), "editing grants no resource selection");
    rig.keys("tab");
    assert!(!rig.cx.update(|window, cx| graph.read(cx).owns_native_focus(window, cx)), "a current Find-only frame has a real forward boundary");
    let mut returned = false;
    // Four existing Shell zones, with their real native stops; no enlarged
    // 64-step search can hide a graph-region trap or focus a disabled leaf.
    for _ in 0..8 {
        rig.keys("shift-tab");
        let (focused, controls) = graph_native_inventory(&mut rig);
        assert!(!controls.iter().any(|node| node["aria"]["label"].as_str() == focused.as_deref()
            && node["aria"]["disabled"] == true), "Shift-Tab cannot focus any disabled native resource: {focused:?}");
        if rig.cx.update(|window, cx| graph.read(cx).find_focused(window, cx)) { returned = true; break; }
    }
    assert!(returned, "Shift-Tab returns through the mounted zones to the admitted local editor");
    rig.keys("shift-tab");
    assert!(!rig.cx.update(|window, cx| graph.read(cx).owns_native_focus(window, cx)), "the local editor also has a real backward boundary");
    assert_eq!(rig.route(), Route::World);
    assert!(rig.graph.store.read_with(rig.cx, |store, _| store.graph_focus().is_none()));
}

#[gpui::test]
fn graph_native_release_cannot_adopt_a_replacement_owner(cx: &mut TestAppContext) {
    use gpui::{KeyDownEvent, KeyUpEvent, Keystroke};
    let (mut rig, gate) = canary_native_rig(cx, 663.0, 1.5, facet::tokens::Appearance::Abyss);
    let toggle = super::tests::native_bounds(&mut rig, "Button", "Declarations", true).expect("chooser");
    rig.cx.simulate_click(toggle.center(), gpui::Modifiers::none());
    rig.settle();
    let graph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("graph");
    tab_to_graph_control(&mut rig, "Select real-rust-canary::cadence::advance_signal · function. src/cadence.rs:8");
    rig.cx.simulate_event(KeyDownEvent { keystroke: Keystroke::parse("enter").expect("key"), is_held: false, prefer_character_input: false });
    let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    gate.publish(crate::runtime::owner::OwnerState::Starting);
    gate.publish(crate::runtime::owner::OwnerState::Ready { key: root, mode: crate::model::ServiceMode::Attached });
    // No repaint or UI watcher between owner renewal and the old native up.
    rig.cx.simulate_event(KeyUpEvent { keystroke: Keystroke::parse("enter").expect("key") });
    assert!(graph.read_with(rig.cx, |graph, _| graph.focused()).is_none(), "the old native callback cannot adopt a new exact-root owner attachment");
}


#[gpui::test]
fn newly_entered_world_native_page_and_code_open_exact_semantic_selection(cx: &mut TestAppContext) {
    for (view, label) in [(View::Page, "Show Page view"), (View::Code, "Show Code view")] {
        let (mut rig, _) = canary_native_rig(cx, 1440.0, 1.0, facet::tokens::Appearance::Abyss);
        tab_to_graph_control(&mut rig, "Declarations");
        rig.native_press("enter");
        tab_to_graph_control(&mut rig, "Select real-rust-canary::cadence::advance_signal · function. src/cadence.rs:8");
        rig.native_press("enter");
        let action = super::tests::native_bounds(&mut rig, "Button", label, true).expect("native view control");
        rig.cx.simulate_click(action.center(), gpui::Modifiers::none());
        rig.settle();
        let Route::Symbol(route) = rig.route() else { panic!("native {label} did not leave newly entered World"); };
        assert_eq!(route.view, view);
        assert!(route.id.as_str().contains(&"2".repeat(64)), "the opaque semantic coordinate is retained exactly");
        assert_eq!(route.package.as_str(), "/fixture/real-rust-canary");
    }
}

/// The header must capture the same selected node that native Enter opens.
/// Window::dispatch_action defers routing to a later focus tree; this oracle
/// observes the typed GraphViewRequest generation in the activation event
/// itself, before that later tree or a second input can choose another node.
#[gpui::test]
fn graph_header_requests_the_exact_selection_at_native_activation_time(cx: &mut TestAppContext) {
    for (view, id) in [(View::Page, "view-page"), (View::Code, "view-code")] {
        let mut rig = world_rig(cx, view_route("RelationLabel", View::Graph));
        rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(RELATION_LABEL_FN, cx));
        rig.settle();
        let action = rig.shell.read_with(rig.cx, |shell, cx| shell.titlebar_target_action(id, cx))
            .expect("painted selected graph control");
        let before = rig.graph.root.read_with(rig.cx, |root, _| root.graph_view_generation());
        let root = rig.graph.root.clone();
        rig.cx.update(|window, cx| {
            action(window, cx);
            assert_eq!(root.read(cx).graph_view_generation(), before + 1,
                "the event owns the typed graph request before any deferred focus dispatch");
        });
        rig.settle();
        assert_eq!(rig.route(), indexed_view_route("relation_label", view),
            "the header opens the selected B, not the route's RelationLabel");
        let back = super::tests::native_bounds(&mut rig, "Button", "Back to graph", true)
            .expect("the selected page exposes a named return to its graph");
        assert!(painted(&mut rig).texts.iter().any(|text| text.content == "Graph"),
            "the return route is visible as text, not only a native label");
        rig.cx.simulate_click(back.center(), gpui::Modifiers::none());
        rig.settle();
        assert_eq!(rig.route(), view_route("RelationLabel", View::Graph));
        assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_report(cx)).contains("focus Some(3)"),
            "Back restores the chosen graph node and its retained scene");
    }
}

#[gpui::test]
fn graph_header_never_substitutes_a_later_selection_for_its_activated_one(cx: &mut TestAppContext) {
    let mut rig = world_rig(cx, view_route("RelationLabel", View::Graph));
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(RELATION_LABEL_FN, cx));
    rig.settle();
    let action = rig.shell.read_with(rig.cx, |shell, cx| shell.titlebar_target_action("view-page", cx))
        .expect("B's current header action");
    let shell = rig.shell.clone();
    rig.cx.update(|window, cx| {
        action(window, cx);
        shell.update(cx, |shell, cx| shell.focus_graph_node(RELATION_LABEL, cx));
    });
    rig.settle();
    assert_ne!(rig.route(), page_route("RelationLabel"),
        "a later selected A cannot replace the B captured by the activated header control");
    assert!(matches!(rig.route(), Route::Symbol(ref route) if route.view == View::Graph)
        || rig.route() == indexed_view_route("relation_label", View::Page),
        "either the exact B opens or the newer input cancels it; no route target is guessed");
}

#[gpui::test]
fn captured_graph_header_action_cannot_cross_same_root_owner_replacement(cx: &mut TestAppContext) {
    let (mut rig, gate) = canary_native_rig(cx, 1440.0, 1.0, facet::tokens::Appearance::Abyss);
    tab_to_graph_control(&mut rig, "Declarations");
    rig.native_press("enter");
    tab_to_graph_control(&mut rig, "Select real-rust-canary::cadence::advance_signal · function. src/cadence.rs:8");
    rig.native_press("enter");
    rig.settle();
    let action = rig.shell.read_with(rig.cx, |shell, cx| shell.titlebar_target_action("view-page", cx))
        .expect("captured current header action");
    let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    gate.publish(crate::runtime::owner::OwnerState::Starting);
    gate.publish(crate::runtime::owner::OwnerState::Ready { key: root, mode: crate::model::ServiceMode::Attached });
    let before = rig.graph.root.read_with(rig.cx, |root, _| root.graph_view_generation());
    rig.cx.update(|window, cx| action(window, cx));
    assert_eq!(rig.graph.root.read_with(rig.cx, |root, _| root.graph_view_generation()), before,
        "the captured target does not even emit a request under a new attachment");
    assert_eq!(rig.route(), Route::World);
}

#[gpui::test]
fn graph_return_is_local_even_when_the_index_owner_stops_after_open(cx: &mut TestAppContext) {
    let (mut rig, gate) = canary_native_rig(cx, 1440.0, 1.0, facet::tokens::Appearance::Abyss);
    tab_to_graph_control(&mut rig, "Declarations");
    rig.native_press("enter");
    tab_to_graph_control(&mut rig, "Select real-rust-canary::cadence::advance_signal · function. src/cadence.rs:8");
    rig.native_press("enter");
    let page = super::tests::native_bounds(&mut rig, "Button", "Show Page view", true).expect("current selected page action");
    rig.cx.simulate_click(page.center(), gpui::Modifiers::none());
    rig.settle();
    assert!(matches!(rig.route(), Route::Symbol(ref route) if route.view == View::Page));
    gate.publish(crate::runtime::owner::OwnerState::Failed("index unavailable after navigation".into()));
    rig.repaint(); rig.settle();
    let back = super::tests::native_bounds(&mut rig, "Button", "Back to graph", true)
        .expect("the graph-origin history remains a local navigation control");
    rig.cx.simulate_click(back.center(), gpui::Modifiers::none());
    rig.settle();
    assert_eq!(rig.route(), Route::World, "Back does not require a fresh graph read to return");
}

#[gpui::test]
fn graph_painted_modes_revoke_on_owner_renewal_and_modal_cover(cx: &mut TestAppContext) {
    let (mut rig, gate) = canary_native_rig(cx, 1440.0, 1.0, facet::tokens::Appearance::Abyss);
    tab_to_graph_control(&mut rig, "Declarations");
    rig.native_press("enter");
    tab_to_graph_control(&mut rig, "Select real-rust-canary::cadence::advance_signal · function. src/cadence.rs:8");
    rig.native_press("enter");
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.mode_input_allowed(cx)), "the actual mounted graph owns a live painted receipt");
    let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    gate.publish(crate::runtime::owner::OwnerState::Starting);
    gate.publish(crate::runtime::owner::OwnerState::Ready { key: root, mode: crate::model::ServiceMode::Attached });
    // Same content root and scene, different owner attachment. Before any
    // new frame, keyboard input cannot reuse the older graph receipt.
    assert!(!rig.shell.read_with(rig.cx, |shell, cx| shell.mode_input_allowed(cx)));
    rig.cx.simulate_keystrokes("ctrl-3");
    assert_eq!(rig.route(), Route::World, "old painted input cannot open Page");
    rig.repaint();
    rig.settle();
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.mode_input_allowed(cx)), "a real repaint restores eligibility");
    rig.go(Intent::OpenSettings(SettingsPage::Index));
    assert!(!rig.shell.read_with(rig.cx, |shell, cx| shell.mode_input_allowed(cx)));
    assert!(super::tests::native_bounds(&mut rig, "Button", "Show Page view", true).is_none(), "covered graph has no native mode action");
    rig.keys("ctrl-3");
    rig.keys("secondary-.");
    assert_eq!(rig.route(), Route::World);
    rig.go(Intent::DismissOverlay);
    rig.settle();
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.mode_input_allowed(cx)));
    let action = super::tests::native_bounds(&mut rig, "Button", "Show Code view", true).expect("restored native action");
    rig.cx.simulate_click(action.center(), gpui::Modifiers::none());
    rig.settle();
    assert!(matches!(rig.route(), Route::Symbol(route) if route.view == View::Code));
}

#[gpui::test]
fn mounted_graph_recovers_from_owner_renewal_without_navigation_or_forced_redraw(cx: &mut TestAppContext) {
    use gpui::Focusable as _;
    let (mut rig, gate) = canary_native_rig(cx, 1440.0, 1.0, facet::tokens::Appearance::Abyss);
    assert!(rig.cx.update(|window, cx| rig.shell.read(cx).graph_entity(cx)
        .expect("current graph").focus_handle(cx).is_focused(window)), "the mounted predecessor owns native focus");
    let before = rig.shell.read_with(rig.cx, |shell, cx| {
        shell.graph_entity(cx).expect("current mounted graph").entity_id()
    });
    let key = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    gate.publish(crate::runtime::owner::OwnerState::Starting);
    rig.cx.run_until_parked();
    assert_eq!(rig.route(), Route::World);
    assert!(rig.graph.store.read_with(rig.cx, |store, _| store.current_owner_attachment()).is_none(),
        "withdrawal must revoke serving eligibility before recovery");

    gate.publish(crate::runtime::owner::OwnerState::Ready {
        key,
        mode: crate::model::ServiceMode::Attached,
    });
    // Drain only actual owner/subscription/notification work. A settle,
    // repaint, draw or synthetic input here would hide the missing wake.
    rig.cx.run_until_parked();
    assert_eq!(rig.route(), Route::World);
    assert!(rig.graph.store.read_with(rig.cx, |store, _| store.current_owner_attachment()).is_some());
    let mounted = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx));
    assert!(mounted.is_some(), "the same visible route must remount after admission without an external wake");
    let mounted = mounted.expect("remounted graph");
    assert_ne!(mounted.entity_id(), before,
        "the old serving scene must not survive an owner replacement");
    assert!(rig.cx.update(|window, cx| mounted.focus_handle(cx).is_focused(window)),
        "the admitted replacement inherits its predecessor's uninterrupted native ownership");
}

#[gpui::test]
fn retained_graph_replacement_cannot_steal_a_later_native_blur(cx: &mut TestAppContext) {
    let (mut rig, gate) = canary_native_rig(cx, 1440.0, 1.0, facet::tokens::Appearance::Abyss);
    let key = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    gate.publish(crate::runtime::owner::OwnerState::Starting);
    rig.cx.run_until_parked();
    rig.cx.update(|window, _| window.blur());
    gate.publish(crate::runtime::owner::OwnerState::Ready { key, mode: crate::model::ServiceMode::Attached });
    rig.cx.run_until_parked();
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).is_some(), "the retained route still mounts its current scene");
    assert!(rig.cx.update(|window, cx| window.focused(cx).is_none()), "a real later blur revokes replacement focus");
}

#[gpui::test]
fn retained_graph_waiting_for_owner_keeps_global_settings_reachable(cx: &mut TestAppContext) {
    let (mut rig, gate) = canary_native_rig(cx, 1440.0, 1.0, facet::tokens::Appearance::Abyss);
    gate.publish(crate::runtime::owner::OwnerState::Starting);
    rig.cx.run_until_parked();
    rig.native_press("secondary-,");
    rig.cx.run_until_parked();
    assert!(rig.graph.store.read_with(rig.cx, |store, _| matches!(store.snapshot().overlay(), Some(crate::navigation::Overlay::Settings(_)))),
        "a retired Graph receiver must not strand global dispatch during the read");
}

#[gpui::test]
fn retired_graph_native_control_parks_without_guessing_a_replacement_stop(cx: &mut TestAppContext) {
    use gpui::Focusable as _;
    let (mut rig, gate) = canary_native_rig(cx, 1440.0, 1.0, facet::tokens::Appearance::Abyss);
    tab_to_graph_control(&mut rig, "Declarations");
    let key = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    gate.publish(crate::runtime::owner::OwnerState::Starting);
    rig.cx.run_until_parked();
    gate.publish(crate::runtime::owner::OwnerState::Ready { key, mode: crate::model::ServiceMode::Attached });
    rig.cx.run_until_parked();
    let graph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("current replacement");
    assert!(!rig.cx.update(|window, cx| graph.focus_handle(cx).is_focused(window)), "an old control does not authorize guessing the scene's root focus");
    assert!(rig.cx.update(|window, cx| window.focused(cx).is_some_and(|focus| window.is_focus_handle_mounted(&focus))), "a current neutral receiver keeps dispatch alive");
    rig.native_press("secondary-,");
    rig.cx.run_until_parked();
    assert!(rig.graph.store.read_with(rig.cx, |store, _| matches!(store.snapshot().overlay(), Some(crate::navigation::Overlay::Settings(_)))));
}

#[gpui::test]
fn package_publication_refreshes_a_retained_graph_without_changing_the_place(cx: &mut TestAppContext) {
    let mut rig = world_rig(cx, Route::World);
    let before = rig.shell.read_with(rig.cx, |shell, cx|
        shell.graph_entity(cx).expect("mounted graph").entity_id());
    let snapshot = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot());
    let published = std::collections::BTreeSet::from([
        PackageRef::parse(PACKAGE).expect("exact package"),
    ]);
    rig.graph.store.update(rig.cx, |store, cx| store.packages_published(&published, cx));
    // Only publication/Memo/dirty-view effects may wake the graph. No
    // navigation, synthetic input, explicit draw or settle hides the boundary.
    rig.cx.run_until_parked();
    assert_eq!(rig.route(), Route::World);
    assert!(rig.graph.store.read_with(rig.cx, |store, _|
        Arc::ptr_eq(&snapshot, &store.snapshot())), "publication preserves route/history/root");
    let after = rig.shell.read_with(rig.cx, |shell, cx|
        shell.graph_entity(cx).expect("fresh graph mounts autonomously").entity_id());
    assert_ne!(after, before, "cached aggregate facts must be withdrawn at publication");
}

#[gpui::test]
fn package_publication_discards_an_older_in_flight_graph_before_rereading(cx: &mut TestAppContext) {
    use crate::runtime::indexed_world::TestProjectionGate;
    let mut rig = rig(cx, None, 1440.0, 900.0);
    rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
    let gate = Arc::new(TestProjectionGate::default());
    let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    rig.cx.update(|_, cx| super::bodies::graph::install_test_fixture_with_gate(root, Some(gate.clone()), cx));
    rig.graph.root.update(rig.cx, |root, cx| root.dispatch(Intent::Navigate(Route::World), cx));
    rig.draw_frame();
    rig.cx.run_until_parked();
    assert_eq!(gate.reads_entered(), 1, "first real Memo read is blocked");
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).is_none());
    let published = std::collections::BTreeSet::from([
        PackageRef::parse(PACKAGE).expect("exact package"),
    ]);
    rig.graph.store.update(rig.cx, |store, cx| store.packages_published(&published, cx));
    rig.cx.run_until_parked();
    assert_eq!(gate.reads_entered(), 1, "cancelled work retains its capacity slot until it exits");
    gate.release();
    rig.cx.run_until_parked();
    assert_eq!(gate.reads_entered(), 2, "the pre-publication success is discarded, then facts are reread");
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).is_some(),
        "only the fresh read may mount the same visible graph");
    assert_eq!(rig.route(), Route::World);
}

/// Exact producer locators, independent of temporary node ordering.
fn continuity_world(names: &[&str]) -> (Arc<facet::graph::World>, Arc<IdentityAdapter>) {
    use facet::graph::{Kind, Module, Node, Package, World};
    use super::bodies::graph::identity::ResolvedSymbol;
    let package = PackageRef::parse(PACKAGE).expect("exact fixture package");
    let world = Arc::new(World::new(
        vec![Package { name: "present".into(), version: "1".into(), yours: true, external: false, deps: vec![] }],
        vec![Module { pkg: 0, path: "glyph".into(), file: "glyph.rs".into() }],
        names.iter().map(|name| { let mut node = Node::new(Kind::Enum, *name, 0, 0); node.line = 138; node }).collect(), vec![],
    ).expect("valid exact world"));
    let exact = names.iter().enumerate().map(|(node, name)| (u32::try_from(node).expect("tiny fixture"),
        ResolvedSymbol { package: package.clone(), symbol: super::tests::symbol(name), line: Some(138) })).collect();
    let identities = Arc::new(IdentityAdapter::indexed(&world,
        &std::collections::BTreeMap::from([(package, 0)]), exact));
    (world, identities)
}

fn continuity_rig(cx: &mut TestAppContext, route: Route) -> Rig {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
    let (world, identities) = continuity_world(&["RelationLabel", "RelationDirection"]);
    let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    rig.cx.update(|_, cx| super::bodies::graph::install_test_world(root, world, identities, cx));
    rig.go(Intent::Navigate(route));
    rig
}

fn publish_continuity(rig: &mut Rig) {
    let packages = std::collections::BTreeSet::from([PackageRef::parse(PACKAGE).expect("exact package")]);
    rig.graph.store.update(rig.cx, |store, cx| store.packages_published(&packages, cx));
    rig.cx.run_until_parked();
}

#[gpui::test]
fn publication_preserves_selected_b_and_exact_camera_on_consumed_route_a(cx: &mut TestAppContext) {
    let route = view_route("RelationLabel", View::Graph);
    let mut rig = continuity_rig(cx, route.clone());
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
    rig.settle();
    let before = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx).expect("mounted graph"));
    let camera = facet::motion::Camera::new(13.25, -8.75, 133.5);
    before.update(rig.cx, |graph, cx| graph.fly_to(camera, cx));
    rig.settle();
    assert_eq!(before.read_with(rig.cx, |graph, _| (graph.focused(), graph.camera())), (Some(1), Some(camera)));
    let snapshot = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot());
    publish_continuity(&mut rig);
    let after = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx).expect("publication mounts autonomously"));
    assert_ne!(before.entity_id(), after.entity_id());
    assert_eq!(after.read_with(rig.cx, |graph, _| (graph.focused(), graph.camera())), (Some(1), Some(camera)),
        "consumed route A cannot override the user's actual selection B or placement");
    assert_eq!(rig.route(), route);
    assert!(rig.graph.store.read_with(rig.cx, |store, _| Arc::ptr_eq(&snapshot, &store.snapshot())), "restoration appends no history");
}

#[gpui::test]
fn covered_publication_preserves_graph_geometry_without_taking_settings_focus(cx: &mut TestAppContext) {
    let route = view_route("RelationLabel", View::Graph);
    let mut rig = continuity_rig(cx, route.clone());
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
    rig.settle();
    let before = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx).expect("mounted graph"));
    let camera = facet::motion::Camera::new(18.5, -12.25, 119.75);
    before.update(rig.cx, |graph, cx| graph.fly_to(camera, cx));
    rig.settle();
    rig.go(Intent::OpenSettings(SettingsPage::Appearance));
    rig.native_press("tab");
    rig.settle();
    let focused = rig.cx.update(|window, cx| window.focused(cx)).expect("actual Settings control focus");
    rig.cx.update(|window, cx| {
        use gpui::Focusable as _;
        assert!(!before.read(cx).focus_handle(cx).contains_focused(window, cx), "Settings owns native input while the graph is covered");
    });
    publish_continuity(&mut rig);
    assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), Some(focused),
        "publication under Settings cannot revive the hidden graph's native focus ownership");
    assert_eq!(rig.route(), route);
    rig.native_press("escape");
    rig.cx.run_until_parked();
    let after = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx).expect("graph remounts on explicit cover return"));
    assert_ne!(before.entity_id(), after.entity_id());
    assert_eq!(after.read_with(rig.cx, |graph, _| (graph.focused(), graph.camera())), (Some(1), Some(camera)),
        "the unchanged underlying reading visit retains B and exact placement across cover retirement");
    assert_eq!(rig.route(), route);
}

#[gpui::test]
fn publication_remaps_permuted_selection_and_anchor_on_first_ready_paint(cx: &mut TestAppContext) {
    use crate::runtime::indexed_world::{self, TestProjectionGate};
    let mut rig = continuity_rig(cx, view_route("RelationLabel", View::Graph));
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
    rig.settle();
    let before = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx).expect("old graph"));
    let geometry = before.read_with(rig.cx, |graph, _| graph.presentation().expect("actual selected geometry"));
    let snapshot = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot());
    let (world, identities) = continuity_world(&["RelationDirection", "RelationLabel", "KindGlyph"]);
    let scene = facet::graph::scene::Scene::new(world.clone(), facet::graph::layout::layout_of(&world));
    let expected = geometry.restore(&scene, Some(0)).expect("uniquely remapped B anchor");
    let gate = Arc::new(TestProjectionGate::default());
    rig.cx.update(|_, cx| indexed_world::install_test_projection(snapshot.key(), "permuted exact graph", world, identities, Some(gate.clone()), cx));
    publish_continuity(&mut rig);
    assert!(gate.entered());
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).is_none());
    let probe = super::bodies::graph::FirstReadyFrameProbe::new(snapshot.key());
    rig.cx.update(|_, cx| cx.set_global(probe.clone()));
    gate.release();
    rig.cx.run_until_parked();
    let after = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx).expect("first ready wake mounts"));
    assert_eq!(after.read_with(rig.cx, |graph, _| (graph.focused(), graph.camera())), (Some(0), Some(expected)),
        "raw old node 1 now names A, while exact B restores at node 0");
    let observed = probe.result.borrow();
    let first = observed.as_ref().expect("passive first ready observation");
    assert!(first.mounted_at_render && first.painted, "no navigation, input, explicit draw or second wake is needed");
    assert!(rig.graph.store.read_with(rig.cx, |store, _| Arc::ptr_eq(&snapshot, &store.snapshot())));
}

#[gpui::test]
fn later_world_navigation_cancels_selection_restoration_while_projection_is_in_flight(cx: &mut TestAppContext) {
    use crate::runtime::indexed_world::{self, TestProjectionGate};
    let mut rig = continuity_rig(cx, view_route("RelationLabel", View::Graph));
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
    rig.settle();
    let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    let (world, identities) = continuity_world(&["RelationDirection", "RelationLabel"]);
    let gate = Arc::new(TestProjectionGate::default());
    rig.cx.update(|_, cx| indexed_world::install_test_projection(root, "held replacement", world, identities, Some(gate.clone()), cx));
    publish_continuity(&mut rig);
    assert!(gate.entered());
    rig.graph.root.update(rig.cx, |root, cx| root.dispatch(Intent::Navigate(Route::World), cx));
    rig.cx.run_until_parked();
    gate.release();
    rig.cx.run_until_parked();
    assert_eq!(rig.route(), Route::World);
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx).expect("fresh world").read(cx).focused()), None,
        "later explicit World wins over retained B from route A");
}

#[gpui::test]
fn later_declaration_route_beats_retained_selection_during_projection_read(cx: &mut TestAppContext) {
    use crate::runtime::indexed_world::{self, TestProjectionGate};
    let mut rig = continuity_rig(cx, view_route("RelationLabel", View::Graph));
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
    rig.settle();
    let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    let (world, identities) = continuity_world(&["RelationDirection", "RelationLabel", "KindGlyph"]);
    let gate = Arc::new(TestProjectionGate::default());
    rig.cx.update(|_, cx| indexed_world::install_test_projection(root, "held replacement for later route", world, identities, Some(gate.clone()), cx));
    publish_continuity(&mut rig);
    assert!(gate.entered());
    let later = view_route("KindGlyph", View::Graph);
    rig.graph.root.update(rig.cx, |root, cx| root.dispatch(Intent::Navigate(later.clone()), cx));
    rig.cx.run_until_parked();
    gate.release();
    rig.cx.run_until_parked();
    assert_eq!(rig.route(), later);
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx).expect("fresh graph for later route").read(cx).focused()), Some(2),
        "route C is consumed against fresh identities instead of restoring B from the earlier visit");
}

#[gpui::test]
fn stale_graph_row_pointer_and_ax_cannot_take_current_focus_before_redraw(cx: &mut TestAppContext) {
    let (mut rig, gate) = canary_native_rig(cx, 663.0, 1.5, facet::tokens::Appearance::Abyss);
    tab_to_graph_control(&mut rig, "Declarations");
    rig.native_press("enter");
    rig.settle();
    let name = "Select real-rust-canary::cadence::advance_signal · function. src/cadence.rs:8";
    let row = super::tests::native_bounds(&mut rig, "Button", name, true).expect("rendered exact row");
    let old_node = rig.cx.update(|window, _| window.a11y_tree().expect("native tree").nodes.iter()
        .find(|(_, node)| node.role() == gpui::Role::Button && node.label() == Some(name)).map(|(id, _)| *id).expect("AX exact row"));
    tab_to_graph_control(&mut rig, "Hide declarations");
    let focused = rig.cx.update(|window, cx| window.focused(cx));
    let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    gate.publish(crate::runtime::owner::OwnerState::Starting);
    gate.publish(crate::runtime::owner::OwnerState::Ready { key: root, mode: crate::model::ServiceMode::Attached });
    rig.cx.simulate_mouse_down(row.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focused, "stale row admission runs before automatic pointer focus");
    rig.cx.simulate_mouse_up(row.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
    rig.cx.update(|window, cx| window.simulate_a11y_action(gpui::accesskit::ActionRequest {
        action: gpui::AccessibleAction::Click, target_tree: gpui::accesskit::TreeId::ROOT,
        target_node: old_node, data: None,
    }, cx));
    assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focused, "stale native AX action cannot change current focus");
    let graph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("same graph");
    assert!(graph.read_with(rig.cx, |graph, _| graph.focused()).is_none());
    rig.repaint();
    rig.settle();
    let row = super::tests::native_bounds(&mut rig, "Button", name, true).expect("fresh exact row");
    rig.cx.simulate_click(row.center(), gpui::Modifiers::none());
    rig.settle();
    assert_eq!(graph.read_with(rig.cx, |graph, _| graph.focused()), Some(1), "current owner remains operable through the shared native path");
}

#[gpui::test]
fn selected_native_landmark_and_page_code_survive_caption_no_fit(cx: &mut TestAppContext) {
    for (view, label) in [(View::Page, "Show Page view"), (View::Code, "Show Code view")] {
        let (mut rig, _) = canary_native_rig(cx, 663.0, 2.0, facet::tokens::Appearance::Abyss);
        tab_to_graph_control(&mut rig, "Declarations");
        rig.native_press("enter");
        tab_to_graph_control(&mut rig, "Select real-rust-canary::cadence::advance_signal · function. src/cadence.rs:8");
        rig.native_press("enter");
        rig.settle();
        let graph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("painted graph");
        assert_eq!(graph.read_with(rig.cx, |graph, _| graph.stats().selected_labels), 0,
            "full caption does not fit beside actual measured native chrome at this scale");
        let landmark = "Selected declaration real-rust-canary::cadence::advance_signal · function. src/cadence.rs:8";
        let bounds = super::tests::native_bounds(&mut rig, "Group", landmark, false).expect("full exact selected native landmark survives caption placement failure");
        assert!(bounds.left() >= gpui::px(0.0) && bounds.right() <= gpui::px(663.0));
        let action = super::tests::native_bounds(&mut rig, "Button", label, true).expect("fresh selected mode control");
        rig.cx.simulate_click(action.center(), gpui::Modifiers::none());
        rig.settle();
        assert!(matches!(rig.route(), Route::Symbol(route) if route.view == view && route.id.as_str().contains(&"2".repeat(64))));
    }
}


#[gpui::test]
fn indexed_projection_callback_mounts_on_its_first_announced_draw(cx: &mut TestAppContext) {
    use crate::runtime::indexed_world::TestProjectionGate;
    use super::bodies::graph::MapWorkStatus;
    let owner_root = crate::core::VersionedRoot::synthetic(backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]), 4);
    let owner = crate::runtime::owner::OwnerGate::ready(owner_root, crate::model::ServiceMode::Attached);
    let mut rig = super::tests::rig_with_engine_gate(cx, None, 1440.0, 900.0,
        crate::runtime::reads::ReadPool::start(2, |_| super::tests::Fixture).expect("fixture pool"), super::tests::RootOnly, Some(owner));
    rig.cx.update(|window, _| window.set_a11y_forced(true));
    rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
    let gate = Arc::new(TestProjectionGate::default());
    let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    let probe = super::bodies::graph::FirstReadyFrameProbe::new(root);
    rig.cx.update(|_, cx| cx.set_global(probe.clone()));
    rig.cx.update(|_, cx| super::bodies::graph::install_test_fixture_with_gate(root, Some(gate.clone()), cx));
    rig.graph.root.update(rig.cx, |root, cx| root.dispatch(Intent::Navigate(Route::World), cx));
    rig.draw_frame();
    rig.cx.run_until_parked();
    assert!(gate.entered(), "real asynchronous Memo worker is held");
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).is_none());
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_work_status(cx)), Some(MapWorkStatus::ProjectionRead));
    let before = rig.cx.update(|window, _| window.a11y_frame_number());
    gate.release();
    rig.cx.run_until_parked();
    // test-support flushes Memo's Notify and eagerly paints dirty windows.
    // The passive probe records that automatic earliest Memo-ready render,
    // then reads its actual committed AX tree at the end of that effect cycle.
    // It creates no wake, refresh, timer, explicit draw or synthetic input.
    let controls = {
        let observed = probe.result.borrow();
        let first = observed.as_ref().expect("Memo notification must cause a ready frame without external input");
        assert!(first.mounted_at_render, "the very first render observing Ready must mount the scene, without a second wake");
        assert!(first.painted && first.a11y_frame > before, "that same frame really paints the native scene");
        assert_eq!(first.work_at_paint, Some(MapWorkStatus::Discovery), "discovery has not supplied a later executor wake at this paint boundary");
        assert_eq!(first.controls.len(), 2, "both first-paint native controls are present");
        assert!(first.controls.iter().all(|(_, enabled)| *enabled), "current first-paint native controls expose real Click actions");
        first.controls.iter().map(|(id, _)| *id).collect::<Vec<_>>()
    };
    for target_node in controls {
        rig.cx.update(|window, cx| window.simulate_a11y_action(gpui::accesskit::ActionRequest {
            action: gpui::AccessibleAction::Click, target_tree: gpui::accesskit::TreeId::ROOT,
            target_node, data: None,
        }, cx));
    }
    rig.draw_frame();
    rig.cx.update(|window, _| {
        let tree = window.a11y_tree().expect("activated controls");
        for label in ["Hide declarations", "Hide coverage"] {
            assert!(tree.nodes.iter().any(|(_, node)| node.label() == Some(label)), "first-paint activation changes each native control exactly once");
        }
    });
}

#[gpui::test]
fn settings_returns_only_an_admitted_native_graph_root(cx: &mut TestAppContext) {
    use gpui::Focusable as _;
    for origin in ["root", "descendant", "shelf"] {
        let (mut rig, _) = canary_native_rig(cx, 1440.0, 1.0, facet::tokens::Appearance::Abyss);
        match origin {
            "descendant" => tab_to_graph_control(&mut rig, "Declarations"),
            "shelf" => {
                rig.cx.update(|window, cx| rig.shell.update(cx, |shell, cx| shell.take_zone(super::focus::Zone::Shelf, window, cx)));
                let shelf = rig.shell.read_with(rig.cx, |shell, _| shell.shelf_entity());
                let targets = shelf.read_with(rig.cx, |shelf, _| shelf.targets.clone());
                let target = targets.native_keys().into_iter().next().expect("a real registered Shelf control");
                rig.cx.update(|window, cx| assert!(targets.focus_native(&target, window, cx)));
            }
            _ => {}
        }
        let before = rig.cx.update(|window, cx| window.focused(cx)).expect("actual native origin");
        rig.keys("secondary-,");
        assert!(rig.graph.store.read_with(rig.cx, |store, _| matches!(store.snapshot().overlay(), Some(crate::navigation::Overlay::Settings(_)))));
        assert_ne!(rig.cx.update(|window, cx| window.focused(cx)), Some(before));
        rig.keys("escape");
        let graph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("current graph");
        assert_eq!(rig.cx.update(|window, cx| graph.focus_handle(cx).is_focused(window)), origin == "root", "only an actual root origin admits Graph-root return from {origin}");
        assert!(rig.cx.update(|window, cx| window.focused(cx).is_some_and(|focus| window.is_focus_handle_mounted(&focus))), "the uncovered receiver remains mounted");
        if origin != "root" {
            rig.keys("secondary-,");
            assert!(rig.graph.store.read_with(rig.cx, |store, _| matches!(store.snapshot().overlay(), Some(crate::navigation::Overlay::Settings(_)))));
        }
    }
}

#[gpui::test]
fn settings_graph_root_return_survives_scene_retirement_without_stealing_focus(cx: &mut TestAppContext) {
    use crate::runtime::indexed_world::TestProjectionGate;
    use gpui::Focusable as _;
    for cover_painted in [false, true] {
        for return_frame_queued in [false, true] {
            for later_blur in [false, true] {
                let (mut rig, owner) = canary_native_rig(cx, 1440.0, 1.0, facet::tokens::Appearance::Abyss);
                let predecessor = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("mounted predecessor");
                assert!(rig.cx.update(|window, cx| predecessor.focus_handle(cx).is_focused(window)), "the actual Graph root owns the departure");
                let predecessor_id = predecessor.entity_id();
                drop(predecessor);
                rig.native_press("secondary-,");
                assert!(rig.graph.store.read_with(rig.cx, |store, _| matches!(store.snapshot().overlay(), Some(crate::navigation::Overlay::Settings(_)))));
                if cover_painted { rig.settle(); }
                rig.native_press("escape");
                assert!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().overlay().is_none()), "the complete native dismissal precedes retirement");
                if return_frame_queued { rig.draw_frame(); }
                if later_blur { rig.cx.update(|window, _| window.blur()); }
                let key = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
                let projection = Arc::new(TestProjectionGate::default());
                rig.cx.update(|_, cx| super::bodies::graph::install_test_fixture_with_gate(key, Some(projection.clone()), cx));
                owner.publish(crate::runtime::owner::OwnerState::Starting);
                rig.cx.run_until_parked();
                assert!(rig.graph.store.read_with(rig.cx, |store, _| store.current_owner_attachment()).is_none());
                owner.publish(crate::runtime::owner::OwnerState::Ready { key, mode: crate::model::ServiceMode::Attached });
                rig.cx.run_until_parked();
                assert!(projection.entered(), "the replacement's real Memo worker is held");
                assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).is_none());
                projection.release();
                rig.cx.run_until_parked();
                let replacement = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("Memo completion mounts its new scene without another input or forced draw");
                assert_ne!(replacement.entity_id(), predecessor_id);
                // Complete the ordinary next platform frame. A callback
                // already queued for A may run too; B must schedule its own
                // current-frame check, without an extra redraw or gesture.
                rig.cx.update(|window, cx| { window.simulate_next_frame(cx); });
                rig.cx.run_until_parked();
                assert_eq!(rig.cx.update(|window, cx| replacement.focus_handle(cx).is_focused(window)), !later_blur,
                    "current root return obeys cover_painted={cover_painted}, return_frame_queued={return_frame_queued}, later_blur={later_blur}");
                if later_blur { assert!(rig.cx.update(|window, cx| window.focused(cx).is_none())); }
            }
        }
    }
}

#[gpui::test]
fn first_graph_paint_cannot_recreate_permission_after_later_native_input(cx: &mut TestAppContext) {
    use gpui::Focusable as _;
    for covered in [false, true] {
        for change in ["blur", "key", "inactive"] {
            let mut rig = super::tests::rig(cx, None, 1440.0, 900.0);
            rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
            if covered {
                rig.go(Intent::Navigate(Route::World));
                rig.keys("secondary-,");
                assert!(rig.graph.store.read_with(rig.cx, |store, _| matches!(store.snapshot().overlay(), Some(crate::navigation::Overlay::Settings(_)))));
                let reader = rig.shell.read_with(rig.cx, |shell, _| shell.reader_entity());
                let links = reader.read_with(rig.cx, |reader, _| reader.navigation_links());
                rig.cx.update(|_, cx| links.dispatch(Intent::DismissOverlay, cx));
            } else {
                rig.graph.root.update(rig.cx, |root, cx| root.dispatch(Intent::Navigate(Route::World), cx));
            }
            // Each update above completes ordinary store subscribers. A
            // subsequent platform event can arrive before the first paint;
            // no event is inserted into reducer/subscriber execution.
            match change {
                "blur" => rig.cx.update(|window, _| window.blur()),
                "key" => rig.native_press("left"),
                "inactive" => { rig.cx.deactivate_window(); rig.cx.update(|window, _| window.blur()); }
                _ => unreachable!(),
            }
            let later = rig.cx.update(|window, cx| window.focused(cx));
            rig.settle();
            let graph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("the requested graph still mounts");
            assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), later, "later {change} before first paint owns covered={covered}");
            assert!(!rig.cx.update(|window, cx| graph.focus_handle(cx).is_focused(window)));
        }
    }
}

#[gpui::test]
fn ask_opened_from_the_painted_loading_frame_owns_focus_after_graph_completion(cx: &mut TestAppContext) {
    use crate::runtime::indexed_world::TestProjectionGate;
    use gpui::Focusable as _;
    use gpui_component::WindowExt as _;
    let launch = crate::core::VersionedRoot::synthetic(backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]), 4);
    let owner = crate::runtime::owner::OwnerGate::ready(launch, crate::model::ServiceMode::Attached);
    let mut rig = super::tests::rig_with_engine_gate(cx, None, 1440.0, 900.0,
        crate::runtime::reads::ReadPool::start(2, |_| super::tests::Fixture).expect("fixture pool"), super::tests::RootOnly, Some(owner));
    rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
    let key = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    let gate = Arc::new(TestProjectionGate::default());
    rig.cx.update(|_, cx| super::bodies::graph::install_test_fixture_with_gate(key, Some(gate.clone()), cx));
    rig.graph.root.update(rig.cx, |root, cx| root.dispatch(Intent::Navigate(Route::World), cx));
    // The current loading frame supplies the real Shell action context; the
    // requested Graph itself has never mounted or painted.
    rig.draw();
    assert!(gate.entered(), "the actual projection is still held after its loading frame");
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).is_none());
    rig.native_press("secondary-k");
    rig.draw();
    assert!(rig.shell.read_with(rig.cx, |shell, _| shell.transients().0), "the native shortcut opens actual Ask from the current loading frame");
    rig.cx.update(|window, cx| { window.simulate_next_frame(cx); });
    rig.cx.run_until_parked();
    let ask = rig.cx.update(|window, cx| {
        assert!(window.has_focused_input(cx), "the mounted Ask editing engine owns typing");
        window.focused(cx).expect("actual Ask receiver")
    });
    gate.release();
    rig.cx.run_until_parked();
    let graph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("the projection's ordinary completion mounts the underlying graph");
    rig.cx.update(|window, cx| { window.simulate_next_frame(cx); });
    rig.cx.run_until_parked();
    assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), Some(ask.clone()));
    assert!(rig.cx.update(|window, cx| window.is_focus_handle_mounted(&ask) && window.has_focused_input(cx)));
    assert!(!rig.cx.update(|window, cx| graph.focus_handle(cx).is_focused(window)));
}

#[gpui::test]
fn delayed_graph_mount_respects_native_focus_intent(cx: &mut TestAppContext) {
    use crate::runtime::indexed_world::TestProjectionGate;
    use gpui::Focusable as _;
    for change in ["unchanged", "focus", "blur", "key", "inactive"] {
        let root = crate::core::VersionedRoot::synthetic(backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]), 4);
        let owner = crate::runtime::owner::OwnerGate::ready(root, crate::model::ServiceMode::Attached);
        let mut rig = super::tests::rig_with_engine_gate(cx, None, 1440.0, 900.0,
            crate::runtime::reads::ReadPool::start(2, |_| super::tests::Fixture).expect("fixture pool"), super::tests::RootOnly, Some(owner));
        rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
        let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
        let gate = Arc::new(TestProjectionGate::default());
        rig.cx.update(|_, cx| super::bodies::graph::install_test_fixture_with_gate(root, Some(gate.clone()), cx));
        rig.graph.root.update(rig.cx, |root, cx| root.dispatch(Intent::Navigate(Route::World), cx));
        rig.draw_frame();
        rig.cx.run_until_parked();
        assert!(gate.entered(), "the actual asynchronous projection is held");
        assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).is_none());
        match change {
            "focus" => rig.cx.update(|window, cx| window.focus_next(cx)),
            "blur" => rig.cx.update(|window, _| window.blur()),
            "key" => rig.native_press("left"),
            "inactive" => { rig.cx.deactivate_window(); rig.cx.update(|window, _| window.blur()); }
            _ => {}
        }
        let later = rig.cx.update(|window, cx| window.focused(cx));
        gate.release();
        // The Memo's ordinary notification paints its first ready frame. No
        // input, refresh or explicit draw grants it another focus opportunity.
        rig.cx.run_until_parked();
        let graph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("mounted projection");
        if change == "unchanged" {
            assert!(rig.cx.update(|window, cx| graph.focus_handle(cx).is_focused(window)), "an uninterrupted arrival owns native focus");
        } else {
            assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), later, "a later {change} must win over asynchronous mount");
            assert!(!rig.cx.update(|window, cx| graph.focus_handle(cx).is_focused(window)));
        }
    }
}

#[gpui::test]
fn native_graph_admission_does_not_reenter_its_leased_graph(cx: &mut TestAppContext) {
    let (mut rig, _) = canary_native_rig(cx, 1440.0, 1.0, facet::tokens::Appearance::Abyss);
    // Both pointer and keyboard invoke the production predicate inside a
    // leased Graph handler. It must inspect the Map's native paint receipt,
    // rather than reading that Graph entity recursively.
    let toggle = super::tests::native_bounds(&mut rig, "Button", "Declarations", true).expect("painted native scene");
    rig.cx.simulate_click(toggle.center(), gpui::Modifiers::none());
    rig.settle();
    let label = "Select real-rust-canary::cadence::advance_signal · function. src/cadence.rs:8";
    tab_to_graph_control(&mut rig, label);
    rig.native_press("enter"); rig.settle();
    let graph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("same scene");
    assert_eq!(graph.read_with(rig.cx, |graph, _| graph.focused()), Some(1));
    let page = super::tests::native_bounds(&mut rig, "Button", "Show Page view", true).expect("current selection action");
    rig.cx.simulate_click(page.center(), gpui::Modifiers::none());
    rig.settle();
    assert!(matches!(rig.route(), Route::Symbol(route) if route.view == View::Page && route.id.as_str().contains(&"2".repeat(64))), "exact native definition opened");
}


#[gpui::test]
fn stale_painted_graph_marker_cannot_take_focus_on_owner_replacement(cx: &mut TestAppContext) {
    let (mut rig, gate) = canary_native_rig(cx, 1440.0, 1.0, facet::tokens::Appearance::Abyss);
    let graph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("native graph");
    let marker = graph.read_with(rig.cx, |graph, _| graph.node_bounds(1)).expect("painted definition glyph").center();
    tab_to_graph_control(&mut rig, "Declarations");
    let focused = rig.cx.update(|window, cx| window.focused(cx));
    let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    let before = graph_native_evidence(&mut rig);
    gate.publish(crate::runtime::owner::OwnerState::Starting);
    gate.publish(crate::runtime::owner::OwnerState::Ready { key: root, mode: crate::model::ServiceMode::Attached });
    // No App update, watcher poll or frame between renewal and this Down.
    rig.cx.simulate_mouse_down(marker, gpui::MouseButton::Left, gpui::Modifiers::none());
    let after = graph_native_evidence(&mut rig);
    assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focused, "revoked paint cannot focus Graph before semantic admission; before={before}; after={after}");
    rig.cx.simulate_mouse_up(marker, gpui::MouseButton::Left, gpui::Modifiers::none());
    assert!(graph.read_with(rig.cx, |graph, _| graph.focused()).is_none());
    rig.repaint(); rig.settle();
    let marker = graph.read_with(rig.cx, |graph, _| graph.node_bounds(1)).expect("fresh marker").center();
    rig.cx.simulate_click(marker, gpui::Modifiers::none());
    assert_eq!(graph.read_with(rig.cx, |graph, _| graph.focused()), Some(1));
}


#[gpui::test]
fn actual_graph_fact_selectors_expose_unknown_reach_and_unsupported_shapes(cx: &mut TestAppContext) {
    let (mut rig, _) = canary_native_rig(cx, 1440.0, 1.0, facet::tokens::Appearance::Abyss);
    tab_to_graph_control(&mut rig, "Declarations"); rig.native_press("enter");
    tab_to_graph_control(&mut rig, "Select real-rust-canary::cadence::advance_signal · function. src/cadence.rs:8"); rig.native_press("enter"); rig.settle();
    rig.keys("r");
    let unknown = "No dependents observed in this graph; relation coverage is unknown.";
    assert!(super::tests::native_bounds(&mut rig, "Label", unknown, false).is_some(), "the actual native R result does not turn incomplete relation reads into absence");
    rig.keys("/"); rig.cx.simulate_input("crate::MorningSignal -> crate::MorningSignal"); rig.settle();
    let unsupported = "Callable argument and return type facts are unavailable in this graph. Find declarations by name.";
    assert!(super::tests::native_bounds(&mut rig, "Label", unsupported, false).is_some(), "opaque signatures cannot certify a complete negative shape search");
}
