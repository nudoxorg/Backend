//! Mounted keyboard scrolling through the Reader's actual GPUI scroll handle.

use super::focus::Zone;
use super::tests::{Rig, page_route, rig, view_route};
use crate::navigation::{BrowseRoute, Intent, OrbitRoute, Route, SettingsPage, View};
use gpui::{TestAppContext, px, size};

fn scroll(rig: &mut Rig) -> (f32, f32, f32, Option<u64>) {
    let reader = rig
        .shell
        .read_with(rig.cx, |shell, _| shell.reader_entity());
    reader.read_with(rig.cx, |reader, _| {
        let (height, max, mounted) = reader.mounted_scroll_geometry();
        (
            f32::from(reader.scroll_offset().y),
            f32::from(height),
            f32::from(max),
            mounted,
        )
    })
}

fn near(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() <= 1.0,
        "scroll offset {actual} should be {expected}"
    );
}

fn keys_page(cx: &mut TestAppContext) -> Rig {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 764.0, 507.0);
    rig.go(Intent::OpenSettings(SettingsPage::Help));
    let (offset, height, max, mounted) = scroll(&mut rig);
    near(offset, 0.0);
    assert!(
        height > 0.0 && max > height,
        "Keys must actually overflow this mounted viewport: {height}, {max}"
    );
    assert!(
        mounted.is_some(),
        "the current Reader scroll container completed prepaint"
    );
    rig
}

#[gpui::test]
fn page_home_and_end_use_the_mounted_reader_extent(cx: &mut TestAppContext) {
    let mut rig = keys_page(cx);
    let (_, height, max, _) = scroll(&mut rig);
    let remainder = max % height;
    assert!(
        remainder > 1.0 && height - remainder > 1.0,
        "the 764×507 fixture must exercise a partial final page: viewport={height}, extent={max}"
    );
    rig.keys("pagedown");
    near(scroll(&mut rig).0, -height);
    rig.keys("pagedown");
    near(scroll(&mut rig).0, (-2.0 * height).max(-max));
    rig.keys("pageup");
    near(
        scroll(&mut rig).0,
        ((-2.0 * height).max(-max) + height).min(0.0),
    );
    rig.keys("home");
    near(scroll(&mut rig).0, 0.0);
    rig.keys("pageup");
    near(scroll(&mut rig).0, 0.0);
    rig.keys("end");
    near(scroll(&mut rig).0, -max);
    rig.keys("pagedown");
    near(scroll(&mut rig).0, -max);
    rig.keys("pageup");
    near(scroll(&mut rig).0, (-max + height).min(0.0));
    assert_eq!(
        rig.shell
            .read_with(rig.cx, |shell, cx| shell.focus_state(cx).0),
        Zone::Reader
    );
}

#[gpui::test]
fn viewport_commands_follow_text_zoom(cx: &mut TestAppContext) {
    let mut rig = keys_page(cx);
    rig.keys("secondary-=");
    rig.keys("secondary-=");
    let (_, height, zoomed_max, _) = scroll(&mut rig);
    assert!(zoomed_max > height);
    rig.keys("home");
    rig.keys("pagedown");
    near(scroll(&mut rig).0, -height);
    rig.keys("end");
    near(scroll(&mut rig).0, -zoomed_max);
}

#[gpui::test]
fn focused_native_link_keeps_focus_while_reader_scrolls(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 320.0, 510.0);
    rig.keys("j");
    let before_focus = rig
        .shell
        .read_with(rig.cx, |shell, cx| shell.focus_state(cx).1);
    assert!(before_focus.is_some(), "J focused a mounted Reader control");
    let native = rig
        .cx
        .update(|window, cx| (window.focused(cx), window.focus_epoch()));
    assert!(native.0.is_some(), "a native link owns the keyboard");
    let reader = rig
        .shell
        .read_with(rig.cx, |shell, _| shell.reader_entity());
    let owns_native = rig
        .cx
        .update(|window, cx| reader.read(cx).owns_keyboard_scroll_focus(window, cx));
    assert!(
        owns_native,
        "the focused native Reader child owns the page keys"
    );
    rig.cx.simulate_resize(size(px(390.0), px(507.0)));
    rig.settle();
    rig.keys("secondary-=");
    rig.keys("secondary-=");
    assert_eq!(
        rig.cx
            .update(|window, cx| (window.focused(cx), window.focus_epoch())),
        native,
        "resize and text reflow retain the exact native focus handle without another focus request"
    );
    let (from, height, max, _) = scroll(&mut rig);
    assert!(max > 0.0, "the narrow declaration page overflows");
    rig.keys("pagedown");
    near(scroll(&mut rig).0, (from - height).max(-max));
    assert_eq!(
        rig.shell
            .read_with(rig.cx, |shell, cx| shell.focus_state(cx).1),
        before_focus,
        "viewport movement must preserve the focused link"
    );
    assert_eq!(
        rig.cx
            .update(|window, cx| (window.focused(cx), window.focus_epoch())),
        native,
        "page movement retains the exact native link handle"
    );
    rig.keys("end");
    near(scroll(&mut rig).0, -max);
    assert_eq!(
        rig.cx
            .update(|window, cx| (window.focused(cx), window.focus_epoch())),
        native
    );
}

#[gpui::test]
fn home_at_top_cancels_a_queued_offscreen_target_reveal(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 320.0, 320.0);
    rig.repaint();
    let viewport = rig
        .cx
        .debug_bounds("reader-scroll")
        .expect("mounted Reader viewport");
    let targets = rig
        .shell
        .read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
    let target = targets
        .native_keys()
        .into_iter()
        .find(|id| {
            targets
                .bounds_of(id)
                .is_some_and(|bounds| bounds.top() > viewport.bottom() + px(24.0))
        })
        .expect("the narrow page has a mounted native target below the viewport");
    near(scroll(&mut rig).0, 0.0);
    let reader = rig
        .shell
        .read_with(rig.cx, |shell, _| shell.reader_entity());
    rig.cx.update(|window, cx| {
        targets.focus(target.clone());
        assert!(
            targets.focus_native(&target, window, cx),
            "offscreen native target is mounted"
        );
        reader.read(cx).reveal_focused();
    });
    let native = rig
        .cx
        .update(|window, cx| (window.focused(cx), window.focus_epoch()));
    assert!(native.0.is_some());
    // Deliver Home before the pending reveal receives a prepaint. At the top
    // the scroll offset is unchanged, but this explicit command still wins.
    rig.cx.simulate_keystrokes("home");
    rig.settle();
    near(scroll(&mut rig).0, 0.0);
    assert_eq!(
        rig.cx
            .update(|window, cx| (window.focused(cx), window.focus_epoch())),
        native,
        "Home neither moves nor refocuses the offscreen native target"
    );
    assert_eq!(
        rig.shell
            .read_with(rig.cx, |shell, cx| shell.focus_state(cx).1),
        Some(target)
    );

    // The same pending reveal really would move the viewport if Home had not
    // superseded it; this validates that the target is a useful oracle.
    reader.update(rig.cx, |reader, cx| {
        reader.reveal_focused();
        cx.notify();
    });
    rig.settle();
    assert!(
        scroll(&mut rig).0 < -8.0,
        "the offscreen target requires a real scroll reveal"
    );
}

#[gpui::test]
fn graph_camera_keeps_its_keys_and_settings_keys_scrolls_with_default_focus(
    cx: &mut TestAppContext,
) {
    let mut rig = super::graph_tests::world_rig(cx, Route::World);
    rig.cx.simulate_resize(size(px(764.0), px(507.0)));
    rig.settle();
    let graph = rig
        .shell
        .read_with(rig.cx, |shell, cx| shell.graph_entity(cx))
        .expect("mounted Graph");
    let camera = graph.read_with(rig.cx, |graph, _| graph.camera().expect("laid out camera"));
    let graph_offset = scroll(&mut rig).0;
    rig.keys("pagedown");
    rig.keys("home");
    near(scroll(&mut rig).0, graph_offset);
    assert_eq!(
        graph.read_with(rig.cx, |graph, _| graph.camera()),
        Some(camera),
        "Graph retains its camera without borrowing the Reader's scroll keys"
    );

    rig.go(Intent::OpenSettings(SettingsPage::Help));
    let shell = rig.shell.clone();
    assert!(
        rig.cx
            .update(|window, cx| shell.read(cx).allows_reader_native_return(window)),
        "the Shell's default Reader-zone focus owns Settings Keys"
    );
    let (before, height, max, mounted) = scroll(&mut rig);
    near(before, 0.0);
    assert!(mounted.is_some() && max > height);
    rig.keys("pagedown");
    near(scroll(&mut rig).0, -height);
    rig.keys("home");
    near(scroll(&mut rig).0, 0.0);
    assert_eq!(
        rig.route(),
        Route::World,
        "Settings does not change the Graph route"
    );
}

#[gpui::test]
fn shelf_modal_and_next_page_cannot_scroll_the_old_reader_visit(cx: &mut TestAppContext) {
    let mut rig = keys_page(cx);
    rig.keys("pagedown");
    let before = scroll(&mut rig).0;
    rig.keys("secondary-k");
    assert!(
        rig.shell.read_with(rig.cx, |shell, _| shell.transients().0),
        "Ask is mounted"
    );
    rig.keys("pagedown");
    near(scroll(&mut rig).0, before);
    rig.keys("escape");
    rig.settle();

    let shell = rig.shell.clone();
    rig.cx.update(|window, cx| {
        shell.update(cx, |shell, cx| shell.take_zone(Zone::Shelf, window, cx))
    });
    rig.draw();
    assert_eq!(
        rig.shell
            .read_with(rig.cx, |shell, cx| shell.focus_state(cx).0),
        Zone::Shelf
    );
    let before_shelf = scroll(&mut rig).0;
    rig.keys("pagedown");
    near(scroll(&mut rig).0, before_shelf);

    rig.go(Intent::OpenSettings(SettingsPage::Appearance));
    let (_, _, next_max, next_mount) = scroll(&mut rig);
    assert!(
        next_mount.is_some(),
        "the destination has its own mounted extent"
    );
    let shell = rig.shell.clone();
    rig.cx.update(|window, cx| {
        shell.update(cx, |shell, cx| shell.take_zone(Zone::Reader, window, cx))
    });
    rig.draw();
    rig.keys("end");
    near(scroll(&mut rig).0, -next_max);
}

#[gpui::test]
fn source_body_uses_the_same_reader_viewport_commands(cx: &mut TestAppContext) {
    let mut rig = rig(
        cx,
        Some(view_route("RelationLabel", View::Code)),
        320.0,
        320.0,
    );
    let (_, height, max, mounted) = scroll(&mut rig);
    assert!(mounted.is_some() && height > 0.0 && max > 0.0);
    rig.keys("end");
    near(scroll(&mut rig).0, -max);
    rig.keys("home");
    near(scroll(&mut rig).0, 0.0);
}

#[gpui::test]
fn focused_find_editor_keeps_its_page_keys(cx: &mut TestAppContext) {
    let route = Route::Orbit(OrbitRoute::Browse(BrowseRoute::FindHome));
    let mut rig = rig(cx, Some(route.clone()), 764.0, 510.0);
    assert!(
        rig.cx.update(|window, _| window.has_input_handler()),
        "the native Find query owns text input"
    );
    let before = scroll(&mut rig).0;
    rig.keys("pagedown");
    near(scroll(&mut rig).0, before);
    assert!(
        rig.cx.update(|window, _| window.has_input_handler()),
        "Page Down must remain with the editor"
    );
    assert_eq!(rig.route(), route);
}
