//! The jump bar through the real shell: where you are as segments, no
//! history at rest, back and forward, the back menu, siblings, the address.

use super::anatomy_tests::painted;
use super::tests::{Rig, page_route, rig};
use crate::navigation::{Intent, OrbitRoute, Route};
use gpui::{Modifiers, MouseButton, TestAppContext, point, px};

/// Controlled root hydration that supplies no new producer revision. A
/// generation advance here would hide a missing same-root attachment wake.
struct StableRoot;

impl crate::runtime::actor::EngineClient for StableRoot {
    fn execute(
        &mut self,
        request: &crate::runtime::actor::EngineRequest,
    ) -> Result<crate::runtime::actor::EngineDto, crate::runtime::actor::EngineFault> {
        use crate::runtime::actor::{EngineDto, EngineFault, EngineRequest};
        let EngineRequest::Root { request, basis, .. } = request else {
            return Err(EngineFault::Cancelled);
        };
        Ok(EngineDto::Root {
            request: *request,
            basis: *basis,
            key: *basis,
            revision: basis.revision(),
            delta: None,
            project: None,
            catalog: None,
        })
    }
}

fn captured_back(rig: &mut Rig) -> super::focus::Act {
    rig.shell.read_with(rig.cx, |shell, cx| shell.titlebar_target_action("jump-back", cx))
        .expect("painted native Back action")
}

/// Read the already committed frame. This must not call `painted`,
/// `native_bounds`, `repaint`, `settle`, or another explicit draw: the
/// notification under test alone must renew the mounted header.
fn press_passively_renewed_back(rig: &mut Rig, former_renders: u64, former_frame: u64) {
    rig.cx.run_until_parked();
    assert!(rig.counts().titlebar > former_renders, "producer context movement must repaint the cached header");
    rig.cx.update(|window, _| {
        assert!(window.a11y_frame_number() > former_frame, "an ordinary dirty frame actually committed");
        let tree = window.a11y_tree().expect("renewed native header tree");
        assert!(tree.nodes.iter().any(|(_, node)| node.label() == Some("Back")
            && node.supports_action(gpui::AccessibleAction::Click)), "the committed Back control has native activation");
    });
    let bounds = rig.shell.read_with(rig.cx, |shell, cx| shell.titlebar_target_bounds("jump-back", cx))
        .expect("freshly painted Back bounds");
    rig.cx.simulate_click(bounds.center(), Modifiers::default());
    rig.settle();
    assert!(matches!(rig.route(), Route::Orbit(OrbitRoute::Home)), "the renewed pointer control returns to its real previous route");
}

#[gpui::test]
fn world_header_renews_after_authority_changes_without_chrome_resources(cx: &mut TestAppContext) {
    use crate::model::ServiceMode;
    use crate::runtime::reads::ReadPool;
    use crate::runtime::store::{Branch, StoreEvent};
    use gpui::AppContext as _;
    use std::cell::RefCell;
    use std::rc::Rc;

    let mut rig = super::tests::rig_with_engine(cx, Some(Route::World), 1440.0, 900.0,
        ReadPool::start(2, |_| super::tests::Fixture).expect("controlled page pool"), StableRoot);
    rig.cx.update(|window, _| window.set_a11y_forced(true));
    rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
    let former_root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    // Make the next OwnerReady a pure authority movement; settings must not
    // accidentally renew the header through its existing Settings watch.
    rig.go(Intent::OwnerReady { key: former_root, mode: ServiceMode::Attached });
    assert!(crate::runtime::store::RouteDependencies::chrome_keys(&Route::World).is_empty());
    let retired = captured_back(&mut rig);
    let renders = rig.counts().titlebar;
    let frame = rig.cx.update(|window, _| window.a11y_frame_number());
    let events = Rc::new(RefCell::new(Vec::new()));
    let observed = Rc::clone(&events);
    let _subscription = rig.cx.update(|_, cx| cx.subscribe(&rig.graph.store,
        move |_, event: &StoreEvent, _| observed.borrow_mut().push(event.clone())));
    let next = former_root.with_generation(former_root.generation().saturating_add(1));
    rig.cx.update(|_, cx| super::bodies::graph::install_test_fixture(next, cx));
    rig.graph.root.update(rig.cx, |root, cx| root.dispatch(Intent::OwnerReady { key: next, mode: ServiceMode::Attached }, cx));
    rig.cx.update(|window, cx| retired(window, cx));
    assert_eq!(rig.route(), Route::World, "a previously painted action cannot borrow the new root");
    rig.cx.run_until_parked();
    assert!(events.borrow().iter().any(|event| event.is_branch(Branch::Root)));
    assert!(!events.borrow().iter().any(|event| [Branch::Owner, Branch::Route, Branch::Overlay,
        Branch::Settings, Branch::GraphFocus].iter().any(|branch| event.is_branch(*branch))),
        "no unrelated branch may hide the missing Root watch");
    press_passively_renewed_back(&mut rig, renders, frame);
}

#[gpui::test]
fn world_header_renews_after_coalesced_same_root_owner_replacement(cx: &mut TestAppContext) {
    use crate::core::VersionedRoot;
    use crate::model::ServiceMode;
    use crate::runtime::owner::{OwnerGate, OwnerState};
    use crate::runtime::reads::ReadPool;
    use crate::runtime::store::{Branch, StoreEvent};
    use gpui::AppContext as _;
    use std::cell::RefCell;
    use std::rc::Rc;

    let root = VersionedRoot::synthetic(
        backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]), 4);
    let gate = OwnerGate::ready(root, ServiceMode::Attached);
    let mut rig = super::tests::rig_with_engine_gate(cx, Some(Route::World), 1440.0, 900.0,
        ReadPool::start(2, |_| super::tests::Fixture).expect("controlled page pool"), StableRoot, Some(gate.clone()));
    rig.cx.update(|window, _| window.set_a11y_forced(true));
    rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
    assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key()), root);
    let retired = captured_back(&mut rig);
    let former = rig.graph.store.read_with(rig.cx, |store, _| store.current_owner_attachment())
        .expect("first serving attachment");
    let renders = rig.counts().titlebar;
    let frame = rig.cx.update(|window, _| window.a11y_frame_number());
    let events = Rc::new(RefCell::new(Vec::new()));
    let observed = Rc::clone(&events);
    let _subscription = rig.cx.update(|_, cx| cx.subscribe(&rig.graph.store,
        move |_, event: &StoreEvent, _| observed.borrow_mut().push(event.clone())));
    // No executor turn between these publications: the owner watcher sees
    // only the replacement Ready, with identical settings and root.
    gate.publish(OwnerState::Starting);
    gate.publish(OwnerState::Ready { key: root, mode: ServiceMode::Attached });
    rig.cx.update(|window, cx| retired(window, cx));
    assert_eq!(rig.route(), Route::World, "the retired native action cannot use a replacement owner");
    rig.cx.run_until_parked();
    assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key()), root);
    assert!(!rig.graph.store.read_with(rig.cx, |store, _| store.admits_owner_attachment(&former)));
    assert!(events.borrow().iter().any(|event| event.is_branch(Branch::Owner)));
    assert!(!events.borrow().iter().any(|event| [Branch::Root, Branch::Route, Branch::Overlay,
        Branch::Settings, Branch::GraphFocus].iter().any(|branch| event.is_branch(*branch))),
        "the owner branch alone renews World chrome with no incidental branch wake");
    let submitted = rig.graph.store.read_with(rig.cx, |store, _| store.stats().submitted);
    rig.graph.store.update(rig.cx, |store, cx| store.owner_ready(cx));
    rig.cx.run_until_parked();
    assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.stats().submitted), submitted,
        "an unchanged owner does not submit extra reads");
    press_passively_renewed_back(&mut rig, renders, frame);
}

/// A shell on `name`'s page, recording what it paints.
fn open(cx: &mut TestAppContext, name: &str) -> Rig {
    let rig = rig(cx, Some(page_route(name)), 1440.0, 900.0);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    rig
}

fn click(rig: &mut Rig, key: &str, button: MouseButton) {
    let ledger = painted(rig);
    let at = ledger.targets.iter().find(|target| target.key == key).unwrap_or_else(|| panic!("no {key}")).bounds.clone();
    let spot = point(px(at.x + at.width / 2.0), px(at.y + at.height / 2.0));
    match button {
        MouseButton::Right => {
            rig.cx.simulate_mouse_down(spot, MouseButton::Right, Modifiers::default());
            rig.cx.simulate_mouse_up(spot, MouseButton::Right, Modifiers::default());
        }
        _ => rig.cx.simulate_click(spot, Modifiers::default()),
    }
    rig.settle();
}

fn is_open(rig: &mut Rig, key: &str) -> bool {
    let key: gpui::ElementId = key.to_owned().into();
    rig.cx.update(|window, cx| facet::overlay::float::is_open(&key, window, cx))
}

#[gpui::test]
fn the_jump_bar_says_where_you_are_and_nothing_about_history_at_rest(cx: &mut TestAppContext) {
    let mut rig = open(cx, "RelationLabel");
    rig.go(Intent::Navigate(page_route("KindGlyph")));
    let ledger = painted(&mut rig);
    let texts: Vec<&str> = ledger.texts.iter().map(|text| text.content.as_str()).collect();
    let here = ledger.texts.iter().position(|text| text.key == "graph-test-here-name").expect("the name");
    assert_eq!(ledger.texts[here].content, "KindGlyph");
    assert_eq!(texts[here - 4..here], ["present", "›", "glyph", "›"], "package › module › the name");
    assert!(!ledger.targets.iter().any(|target| target.key.starts_with("bead")), "no beads");
    assert!(!ledger.targets.iter().any(|target| target.key == "tb-trail"), "no trail button");
    assert!(!ledger.targets.iter().any(|target| target.key == "jump-forward"), "no forward without forward history");
}

#[gpui::test]
fn back_steps_back_forward_appears_and_a_right_click_lists_the_last_places(cx: &mut TestAppContext) {
    let mut rig = open(cx, "RelationLabel");
    rig.go(Intent::Navigate(page_route("KindGlyph")));
    click(&mut rig, "jump-back", MouseButton::Left);
    assert_eq!(rig.route(), page_route("RelationLabel"));
    let ledger = painted(&mut rig);
    assert!(ledger.targets.iter().any(|target| target.key == "jump-forward"), "forward, now that there is somewhere to go");
    click(&mut rig, "jump-forward", MouseButton::Left);
    assert_eq!(rig.route(), page_route("KindGlyph"));
    click(&mut rig, "jump-back", MouseButton::Right);
    assert!(is_open(&mut rig, "jump-back-menu"), "a right click lists the last places");
    assert_eq!(rig.route(), page_route("KindGlyph"), "listing is not stepping");
}

#[gpui::test]
fn a_module_segment_opens_its_siblings_from_the_outline(cx: &mut TestAppContext) {
    let mut rig = open(cx, "RelationLabel");
    click(&mut rig, "jump-seg-1", MouseButton::Left);
    assert!(is_open(&mut rig, "jump-siblings-1"), "glyph's menu");
    let names: Vec<String> = rig.graph.store.read_with(rig.cx, |store, _| {
        super::jump::siblings(&page_route("RelationLabel"), 1, store).real.into_iter().map(|s| s.name.to_string()).collect()
    });
    assert_eq!(names, ["identity", "glyph", "outline"], "the modules beside glyph");
    let members: Vec<String> = rig.graph.store.read_with(rig.cx, |store, _| {
        super::jump::siblings(&page_route("RelationLabel"), 2, store).real.into_iter().map(|s| s.name.to_string()).collect()
    });
    assert_eq!(members, ["RelationLabel", "RelationDirection", "KindGlyph", "relation_label"], "what glyph holds");
}

/// A `#[cfg(test)]` module never sits among the real ones: the menu folds
/// it into one trailing "tests" row, and choosing that row unfolds it.
#[gpui::test]
fn test_only_modules_fold_into_one_trailing_tests_row(cx: &mut TestAppContext) {
    let mut rig = open(cx, "RelationLabel");
    let tests: Vec<String> = rig.graph.store.read_with(rig.cx, |store, _| {
        super::jump::siblings(&page_route("RelationLabel"), 1, store).tests.into_iter().map(|s| s.name.to_string()).collect()
    });
    assert_eq!(tests, ["glyph_tests"], "the test module, apart");
    click(&mut rig, "jump-seg-1", MouseButton::Left);
    // The menu opens on identity; glyph, outline, then the fold.
    rig.keys("down down down enter");
    assert!(is_open(&mut rig, "jump-siblings-1-tests"), "choosing the fold reopens the menu, unfolded");
    assert_eq!(rig.route(), page_route("RelationLabel"), "unfolding is not going");
    rig.keys("down down down enter");
    assert!(matches!(rig.route(), Route::Symbol(ref symbol) if symbol.id.as_str().ends_with("::glyph_tests")), "the test module's page: {:?}", rig.route());
}

/// The shelf folds test-only modules the same way, after the real ones.
#[gpui::test]
fn the_shelf_folds_test_only_modules_into_a_trailing_tests_row(cx: &mut TestAppContext) {
    let mut rig = open(cx, "Identity");
    let rows = |rig: &mut Rig| -> Vec<String> {
        let ledger = painted(rig);
        ledger.texts.iter().filter(|text| text.key.starts_with("shelf-row:")).map(|text| text.content.clone()).collect()
    };
    assert_eq!(rows(&mut rig), ["identity", "Identity", "glyph", "outline", "tests"], "real modules, then one fold");
    let ledger = painted(&mut rig);
    let fold = ledger.targets.iter().find(|target| target.key == super::shelf::TESTS_ROW).expect("the fold is a target").bounds.clone();
    rig.cx.simulate_click(point(px(fold.x + fold.width / 2.0), px(fold.y + fold.height / 2.0)), Modifiers::default());
    rig.settle();
    assert_eq!(rows(&mut rig), ["identity", "Identity", "glyph", "outline", "tests", "glyph_tests"], "opened, it lists them under itself");
}

#[gpui::test]
fn cmd_shift_c_copies_the_address(cx: &mut TestAppContext) {
    let mut rig = open(cx, "RelationLabel");
    rig.keys("secondary-shift-c");
    let copied = rig.cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some("nudox://present/glyph/RelationLabel"));
    rig.go(Intent::Navigate(Route::Orbit(OrbitRoute::Home)));
    rig.keys("secondary-shift-c");
    assert_eq!(rig.cx.read_from_clipboard().and_then(|item| item.text()).as_deref(), Some("nudox://orbit"));
}

#[gpui::test]
fn titlebar_find_is_a_local_door_to_the_real_package_index_reader(cx: &mut TestAppContext) {
    let mut rig = open(cx, "RelationLabel");
    let visible_add = crate::shell::tests::native_bounds(&mut rig, "Button", "Add package", true)
        .expect("the quick package Add button is a real enabled native control");
    assert!(visible_add.size.width > gpui::px(0.0));
    click(&mut rig, "tb-find-packages", MouseButton::Left);
    assert_eq!(rig.route(), Route::Orbit(OrbitRoute::Browse(crate::navigation::BrowseRoute::FindHome)));
    let native = rig.cx.update(|window, _| window.a11y_tree().expect("Find native tree")
        .nodes.iter().any(|(_, node)| node.label() == Some("Find query")));
    assert!(native, "quick Find mounts the existing typed query editor");
    rig.keys("secondary-[");
    assert_eq!(rig.route(), page_route("RelationLabel"), "Find is a history stop with a clear return");
}

/// Depth moved to ⌃1–⌃4 when the hand took ⌘1–⌘5.
#[gpui::test]
fn ctrl_1_to_4_move_through_the_depths(cx: &mut TestAppContext) {
    let mut rig = open(cx, "RelationLabel");
    rig.keys("ctrl-4");
    assert!(matches!(rig.route(), Route::Symbol(ref symbol) if symbol.view == crate::navigation::View::Code), "⌃4: the code: {:?}", rig.route());
    rig.keys("ctrl-3");
    assert_eq!(rig.route(), page_route("RelationLabel"), "⌃3: the page");
    rig.keys("ctrl-2");
    assert!(matches!(rig.route(), Route::Package(_)), "⌃2: the package: {:?}", rig.route());
    rig.keys("ctrl-1");
    assert!(matches!(rig.route(), Route::Orbit(_)), "⌃1: Orbit: {:?}", rig.route());
}

/// Every jump-bar target is at least 24 × 24 px to hit (gui-plan.md:213),
/// while the drawn words keep their own size.
#[gpui::test]
fn every_jump_bar_target_is_at_least_24_px_square(cx: &mut TestAppContext) {
    let mut rig = open(cx, "RelationLabel");
    rig.go(Intent::Navigate(page_route("KindGlyph")));
    click(&mut rig, "jump-back", MouseButton::Left);
    let ledger = painted(&mut rig);
    let targets: Vec<_> = ledger.targets.iter().filter(|target| target.key.starts_with("jump-")).collect();
    let keys: Vec<&str> = targets.iter().map(|target| target.key.as_str()).collect();
    for key in ["jump-back", "jump-forward", "jump-seg-0", "jump-seg-1", "jump-seg-2"] {
        assert!(keys.contains(&key), "{key} is measured: {keys:?}");
    }
    for target in targets {
        assert!(target.bounds.width >= 24.0 && target.bounds.height >= 24.0, "{} is {}×{}", target.key, target.bounds.width, target.bounds.height);
    }
    let glyph = ledger.texts.iter().find(|text| text.content == "glyph").expect("the module's word");
    assert!(glyph.bounds.height < 20.0, "the word itself is not enlarged: {}", glyph.bounds.height);
}
