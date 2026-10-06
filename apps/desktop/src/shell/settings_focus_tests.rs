//! Native keyboard focus through a Settings cover and its Escape return.

use super::focus::Zone;
use super::tests::{page_route, rig};
use crate::navigation::{Intent, SettingsPage};
use gpui::{AppContext as _, InteractiveElement as _, ParentElement as _, StatefulInteractiveElement as _, Styled as _, TestAppContext};

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
fn settings_return_does_not_guess_a_target_from_unmatched_native_focus(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 320.0, 900.0);
    rig.shell.update(rig.cx, |shell, cx| shell.reader_targets(cx).focus("gone-reader-target"));
    rig.go(Intent::OpenSettings(SettingsPage::Appearance));
    rig.keys("escape");
    let (zone, after) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert_eq!(zone, Zone::Reader);
    assert!(after.is_none(), "an unmatched native origin cannot authorize a logical fallback");
    assert!(rig.cx.update(|window, cx| window.focused(cx)
        .is_some_and(|focus| window.is_focus_handle_mounted(&focus))));
}

struct WithMarkdown {
    shell: gpui::Entity<super::root::Shell>,
}

impl gpui::Render for WithMarkdown {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        gpui::div().relative().size_full().child(self.shell.clone()).child(
            gpui::div().absolute().left(gpui::px(300.0)).top(gpui::px(200.0)).w(gpui::px(350.0)).h(gpui::px(80.0))
                .id("unmatched-markdown").debug_selector(|| "unmatched-markdown".into())
                .child(super::markdown::view("native-document", "Actual selectable Markdown native origin")),
        )
    }
}

#[gpui::test]
fn settings_unmatched_markdown_origin_never_restores_a_stale_logical_row(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 900.0, 700.0);
    rig.keys("j");
    let shell = rig.shell.clone();
    rig.cx.update(|window, cx| {
        window.replace_root(cx, |window, cx| {
            let view = cx.new(|_| WithMarkdown { shell });
            cx.new(|cx| gpui_component::Root::new(view, window, cx).bordered(false))
        });
    });
    rig.repaint();
    let document = rig.cx.debug_bounds("unmatched-markdown").expect("actual mounted Markdown");
    let before_click = rig.cx.update(|window, cx| window.focused(cx));
    rig.cx.simulate_click(document.origin + gpui::point(gpui::px(8.0), gpui::px(8.0)), gpui::Modifiers::none());
    let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
    let origin = rig.cx.update(|window, cx| window.focused(cx)).expect("native text selection owns focus");
    assert_ne!(Some(origin.clone()), before_click, "the actual Markdown click moved native focus");
    assert!(rig.cx.update(|window, _| window.is_focus_handle_mounted(&origin)));
    assert!(targets.target_for_native_handle(&origin).is_none(), "Markdown is an unmatched native receiver");
    assert!(targets.focused().is_some(), "the adversarial stale logical selection exists");
    // The test document is a real native sibling of the Shell. Opening via
    // the same local intent exercises its captured receiver without lending
    // that sibling the Shell's keyboard action context.
    rig.go(Intent::OpenSettings(SettingsPage::Appearance));
    rig.keys("escape");
    assert!(targets.focused().is_none(), "the native receipt must retire the stale logical row");
    assert_ne!(rig.cx.update(|window, cx| window.focused(cx)), Some(origin));
    rig.keys("secondary-,");
    assert!(rig.graph.store.read_with(rig.cx, |store, _| matches!(store.snapshot().overlay(), Some(crate::navigation::Overlay::Settings(_)))),
        "the neutral mounted Shell still receives global shortcuts");
}

fn focus_find(rig: &mut super::tests::Rig) -> gpui::FocusHandle {
    rig.cx.update(|window, _| window.set_a11y_forced(true));
    rig.repaint();
    let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("Find tree");
    let tree: serde_json::Value = serde_json::from_str(&json).expect("native tree");
    let role = tree["nodes"].as_object().expect("nodes").values()
        .find(|node| node["aria"]["label"] == "Find query")
        .and_then(|node| node["aria"]["role"].as_str()).expect("query role").to_owned();
    let query = super::tests::native_bounds(rig, &role, "Find query", true).expect("actual query");
    rig.cx.simulate_click(query.center(), gpui::Modifiers::none());
    rig.cx.update(|window, cx| window.focused(cx)).expect("query receiver")
}

#[gpui::test]
fn settings_returns_the_retained_find_input_and_unsent_draft(cx: &mut TestAppContext) {
    use crate::navigation::{BrowseRoute, OrbitRoute, Route};
    for percent in [100_u16, 200] {
        let mut rig = rig(cx, None, 1440.0, 1400.0);
        let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
        rig.go(Intent::ZoomTo { display, percent });
        rig.go(Intent::Navigate(Route::Orbit(OrbitRoute::Browse(BrowseRoute::FindHome))));
        let query = focus_find(&mut rig);
        rig.cx.simulate_input("unsubmitted draft");
        rig.keys("secondary-, escape");
        assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), Some(query), "the same retained native input returns at {percent}%");
        assert!(rig.cx.update(|window, cx| window.has_focused_input(cx)));
        rig.keys("x enter");
        assert!(matches!(rig.route(), Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(query))) if query.text.as_ref() == "unsubmitted draftx"),
            "actual typing and Return preserve and submit the pre-cover draft");
    }
}

#[gpui::test]
fn settings_find_return_respects_later_native_focus_blur_and_inactive_window(cx: &mut TestAppContext) {
    use crate::navigation::{BrowseRoute, OrbitRoute, Route};
    for change in ["focus", "blur", "inactive"] {
        let mut rig = rig(cx, None, 1440.0, 900.0);
        rig.go(Intent::Navigate(Route::Orbit(OrbitRoute::Browse(BrowseRoute::FindHome))));
        let query = focus_find(&mut rig);
        rig.keys("secondary-,");
        let reader = rig.shell.read_with(rig.cx, |shell, _| shell.reader_entity());
        let links = reader.read_with(rig.cx, |reader, _| reader.navigation_links());
        if change == "inactive" { rig.cx.deactivate_window(); }
        rig.cx.update(|_, cx| links.dispatch(Intent::DismissOverlay, cx));
        let later = rig.cx.update(|window, cx| {
            if change == "focus" { window.focus_next(cx); } else { window.blur(); }
            window.focused(cx)
        });
        assert_ne!(later, Some(query));
        rig.settle();
        assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), later, "a later {change} owns the uncovered visit");
    }
}

#[gpui::test]
fn settings_find_native_origin_survives_an_ask_cover_of_settings(cx: &mut TestAppContext) {
    use crate::navigation::{BrowseRoute, OrbitRoute, Route};
    let mut rig = rig(cx, None, 1440.0, 900.0);
    rig.go(Intent::Navigate(Route::Orbit(OrbitRoute::Browse(BrowseRoute::FindHome))));
    let query = focus_find(&mut rig);
    rig.cx.simulate_input("covered draft");
    rig.keys("secondary-, secondary-k escape escape");
    assert!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().overlay().is_none()));
    assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), Some(query), "Ask returns to Settings, whose original Reader native origin still owns the final return");
    rig.keys("x enter");
    assert!(matches!(rig.route(), Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(query))) if query.text.as_ref() == "covered draftx"));
}

#[gpui::test]
fn current_find_edit_survives_producer_authority_replacement(cx: &mut TestAppContext) {
    use crate::navigation::{BrowseRoute, OrbitRoute, Route};
    let mut rig = rig(cx, None, 1440.0, 900.0);
    rig.go(Intent::Navigate(Route::Orbit(OrbitRoute::Browse(BrowseRoute::FindHome))));
    let query = focus_find(&mut rig);
    rig.cx.simulate_input("authority draft");
    let original = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot());
    let next = crate::core::VersionedRoot::synthetic(backend_library::view_state_root(&[("find".into(), "replacement".into())]), 8);
    rig.graph.store.update(rig.cx, |store, cx| store.admit_snapshot(std::sync::Arc::new(original.with_key(next, None)), cx));
    rig.cx.run_until_parked();
    assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), Some(query), "producer replacement does not replace the current local editing engine");
    rig.keys("x enter");
    assert!(matches!(rig.route(), Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(query))) if query.text.as_ref() == "authority draftx"), "actual local typing and Return retain the unsent edit across new producer authority");
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
