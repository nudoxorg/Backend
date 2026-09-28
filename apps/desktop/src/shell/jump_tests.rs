//! The jump bar through the real shell: where you are as segments, no
//! history at rest, back and forward, the back menu, siblings, the address.

use super::anatomy_tests::painted;
use super::tests::{Rig, page_route, rig};
use crate::navigation::{Intent, OrbitRoute, Route};
use gpui::{Modifiers, MouseButton, TestAppContext, point, px};

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
        super::jump::siblings(&page_route("RelationLabel"), 1, store).into_iter().map(|s| s.name.to_string()).collect()
    });
    assert_eq!(names, ["identity", "glyph", "outline"], "the modules beside glyph");
    let members: Vec<String> = rig.graph.store.read_with(rig.cx, |store, _| {
        super::jump::siblings(&page_route("RelationLabel"), 2, store).into_iter().map(|s| s.name.to_string()).collect()
    });
    assert_eq!(members, ["RelationLabel", "RelationDirection", "KindGlyph", "relation_label"], "what glyph holds");
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
