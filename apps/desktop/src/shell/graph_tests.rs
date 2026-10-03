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

/// ⌘D on a graph focus the index has no row for holds nothing, and says so
/// through the one visit-scoped Notice (§15 ruling 1) instead of failing
/// silently. Navigating away clears it: the Notice never outlives its visit.
#[gpui::test]
fn cmd_d_on_an_unindexed_graph_focus_speaks_through_the_notice(cx: &mut TestAppContext) {
    let mut rig = world_rig(cx, Route::World);
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(RELATION_LABEL, cx));
    rig.settle();
    rig.keys("cmd-d");
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
    let root = VersionedRoot::synthetic(view_state_root(&[("shell".to_owned(), "tests".to_owned())]), 4);
    let gate = crate::runtime::owner::OwnerGate::ready(root, crate::model::ServiceMode::Attached);
    let mut rig = super::tests::rig_with_engine_gate(cx, None, width, 900.0,
        ReadPool::start(2, |_| super::tests::Fixture).expect("fixture pool"), super::tests::RootOnly, Some(gate.clone()));
    let preference = match appearance {
        facet::tokens::Appearance::Abyss => crate::model::AppearancePreference::Abyss,
        facet::tokens::Appearance::Glacier => crate::model::AppearancePreference::Glacier,
    };
    rig.go(Intent::SetAppearance(preference));
    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
    rig.go(Intent::ZoomTo { display, percent: (scale * 100.0) as u16 });
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

fn tab_to_graph_control(rig: &mut Rig, label: &str) {
    rig.cx.update(|window, _| window.set_a11y_forced(true));
    for _ in 0..64 {
        rig.keys("tab");
        rig.repaint();
        let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native tree");
        let tree: serde_json::Value = serde_json::from_str(&json).expect("tree");
        if tree["gpui_focus"].as_str().is_some_and(|id| tree["nodes"][id]["aria"]["label"].as_str() == Some(label)) { return; }
    }
    panic!("native Tab never reached {label}");
}

#[gpui::test]
fn native_tab_and_enter_select_the_exact_definition(cx: &mut TestAppContext) {
    let (mut rig, _) = canary_native_rig(cx, 663.0, 1.5, facet::tokens::Appearance::Abyss);
    tab_to_graph_control(&mut rig, "Declarations");
    rig.native_press("enter");
    tab_to_graph_control(&mut rig, "Select real-rust-canary::cadence::advance_signal · function. src/cadence.rs:8");
    rig.native_press("enter");
    let focus = rig.graph.store.read_with(rig.cx, |store, _| store.graph_focus().cloned()).expect("native selected definition");
    assert_eq!(focus.node, 1);
    assert_eq!(focus.kind, backend_library::DeclarationKind::Function);
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
    rig.keys("cmd-.");
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
    gate.publish(crate::runtime::owner::OwnerState::Starting);
    gate.publish(crate::runtime::owner::OwnerState::Ready { key: root, mode: crate::model::ServiceMode::Attached });
    rig.cx.simulate_mouse_down(marker, gpui::MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focused, "revoked paint cannot focus Graph before semantic admission");
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
