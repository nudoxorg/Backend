//! Native keyboard focus through a Settings cover and its Escape return.

use super::focus::Zone;
use super::tests::{page_route, rig};
use crate::navigation::{Intent, SettingsPage};
use gpui::TestAppContext;

#[gpui::test]
fn escape_from_settings_returns_to_the_exact_reader_target(cx: &mut TestAppContext) {
    let route = page_route("RelationLabel");
    let mut rig = rig(cx, Some(route.clone()), 320.0, 900.0);
    rig.keys("j");
    let (zone, before) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert_eq!(zone, Zone::Reader);
    let before = before.expect("J focused a real reader target");
    rig.go(Intent::OpenSettings(SettingsPage::Appearance));
    let (_, covered) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert_ne!(covered.as_ref(), Some(&before), "Settings owns a separate page");
    rig.keys("escape");
    assert_eq!(rig.route(), route);
    let (zone, after) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert_eq!(zone, Zone::Reader);
    assert_eq!(after, Some(before), "Escape restores the exact reader target");
}

#[gpui::test]
fn settings_return_uses_a_registered_reader_target_when_the_old_one_is_gone(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 320.0, 900.0);
    rig.shell.update(rig.cx, |shell, cx| shell.reader_targets(cx).focus("gone-reader-target"));
    rig.go(Intent::OpenSettings(SettingsPage::Appearance));
    rig.keys("escape");
    let (zone, after) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert_eq!(zone, Zone::Reader);
    assert!(after.is_some_and(|id| id.as_ref() != "gone-reader-target"),
        "an unregistered old target must fall back to a real reader target");
}

#[gpui::test]
fn closing_a_focused_settings_radio_keeps_global_shortcuts_reachable(cx: &mut TestAppContext) {
    let route = page_route("RelationLabel");
    let mut rig = rig(cx, Some(route.clone()), 900.0, 700.0);
    rig.keys("secondary-,");
    let control = super::tests::native_bounds(&mut rig, "RadioButton", "Full", true)
        .expect("the actual Settings motion control is mounted");
    rig.cx.simulate_click(control.center(), gpui::Modifiers::none());
    rig.settle();
    assert!(rig.cx.update(|window, cx| window.focused(cx).is_some()),
        "the native radio owns the keyboard before dismissal");
    rig.keys("escape");
    assert_eq!(rig.route(), route);
    assert!(rig.cx.update(|window, cx| window.focused(cx)
        .is_some_and(|focus| window.is_focus_handle_mounted(&focus))),
        "a dismissed Settings control must not retain native dispatch ownership");
    rig.keys("secondary-,");
    assert!(rig.graph.store.read_with(rig.cx, |store, _|
        matches!(store.snapshot().overlay(), Some(crate::navigation::Overlay::Settings(_)))),
        "the global shortcut must reopen Settings without a pointer refocus");
}
