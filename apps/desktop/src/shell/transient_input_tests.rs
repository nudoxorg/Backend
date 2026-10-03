//! Real key and pointer delivery through the painted Shell. These regressions
//! do not invoke control callbacks or seed transient state.
#![allow(clippy::expect_used, clippy::panic)]
use super::anatomy_tests::painted;
use super::tests::{Rig, page_route, rig};
use crate::navigation::Overlay;
use gpui::{Modifiers, TestAppContext, point, px};

fn overlay(rig: &mut Rig) -> Option<Overlay> {
    rig.graph
        .store
        .read_with(rig.cx, |store, _| store.snapshot().overlay())
}
fn click_target(rig: &mut Rig, part: &str) {
    let ledger = painted(rig);
    let target = ledger
        .targets
        .iter()
        .find(|target| target.key.contains(part))
        .unwrap_or_else(|| panic!("painted target {part:?} was absent"));
    let at = point(
        px(target.bounds.x + target.bounds.width / 2.0),
        px(target.bounds.y + target.bounds.height / 2.0),
    );
    rig.cx.simulate_mouse_move(at, None, Modifiers::none());
    rig.draw();
    rig.cx.simulate_click(at, Modifiers::none());
    rig.settle();
}
fn click_text(rig: &mut Rig, words: &str) {
    let ledger = painted(rig);
    let text = ledger
        .texts
        .iter()
        .find(|text| text.content.as_ref() == words)
        .unwrap_or_else(|| panic!("painted text {words:?} was absent"));
    let at = point(
        px(text.bounds.x + text.bounds.width / 2.0),
        px(text.bounds.y + text.bounds.height / 2.0),
    );
    rig.cx.simulate_click(at, Modifiers::none());
    rig.settle();
}
fn enable(rig: &mut Rig) {
    let shell = rig.shell.clone();
    rig.cx.update(|window, cx| {
        window.replace_root(cx, |window, cx| {
            gpui_component::Root::new(shell, window, cx).bordered(false)
        });
        window.set_a11y_forced(true);
        facet::probe::enable(cx);
    });
    rig.settle();
}
fn assert_dispatch_alive(rig: &mut Rig) {
    assert!(
        rig.cx.update(|window, cx| window.focused(cx).is_some()),
        "a mounted owner receives subsequent keys"
    );
    rig.keys("cmd-k");
    assert_eq!(overlay(rig), Some(Overlay::CommandPalette));
}

#[gpui::test]
fn actual_tab_enter_and_space_match_pointer_shelf_activation(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    enable(&mut rig);
    let route = rig.route();
    for key in ["enter", "space"] {
        let before = rig
            .graph
            .store
            .read_with(rig.cx, |store, _| store.snapshot().settings().shelf_open);
        let mut reached = false;
        for _ in 0..16 {
            rig.keys("tab");
            if painted(&mut rig)
                .targets
                .iter()
                .any(|target| target.key.contains("tb-shelf") && target.state.focused)
            {
                reached = true;
                break;
            }
        }
        assert!(reached, "real Tab must reach Toggle the shelf");
        rig.keys(key);
        assert_eq!(
            rig.graph
                .store
                .read_with(rig.cx, |store, _| store.snapshot().settings().shelf_open),
            !before,
            "one key gesture toggles exactly once"
        );
        click_target(&mut rig, "tb-shelf");
        assert_eq!(
            rig.graph
                .store
                .read_with(rig.cx, |store, _| store.snapshot().settings().shelf_open),
            before,
            "pointer invokes the same action"
        );
        assert_eq!(rig.route(), route);
    }
}

#[gpui::test]
fn settings_add_escape_and_pointer_cancel_restore_the_same_live_underlay(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    enable(&mut rig);
    let route = rig.route();
    rig.keys("cmd-,");
    let settings = overlay(&mut rig);
    assert!(matches!(settings, Some(Overlay::Settings(_))));
    for pointer in [false, true] {
        rig.keys("cmd-o");
        assert_eq!(overlay(&mut rig), Some(Overlay::AddProject));
        if pointer {
            click_target(&mut rig, "add-folder-cancel");
        } else {
            rig.keys("escape");
        }
        assert_eq!(overlay(&mut rig), settings);
        assert_eq!(rig.route(), route);
        assert_dispatch_alive(&mut rig);
        rig.keys("escape");
        assert_eq!(overlay(&mut rig), settings);
    }
}

#[gpui::test]
fn inbox_is_the_painted_underlay_and_escape_returns_to_it(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    enable(&mut rig);
    click_target(&mut rig, "tb-inbox");
    let route = rig.route();
    rig.keys("cmd-k");
    assert_eq!(
        rig.graph
            .store
            .read_with(rig.cx, |store, _| store.snapshot().page_overlay()),
        Some(Overlay::Inbox)
    );
    rig.keys("escape");
    assert_eq!(overlay(&mut rig), Some(Overlay::Inbox));
    assert_eq!(rig.route(), route);
    assert_dispatch_alive(&mut rig);
}

#[gpui::test]
fn drawer_click_handoff_and_escape_do_not_dismiss_settings(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 663.0, 900.0);
    enable(&mut rig);
    rig.keys("cmd-,");
    let settings = overlay(&mut rig);
    let route = rig.route();
    rig.keys("cmd-\\");
    assert!(
        rig.shell
            .read_with(rig.cx, |shell, _| shell.shelf_input_owner(true))
    );
    click_text(&mut rig, "Contents");
    assert!(
        rig.shell
            .read_with(rig.cx, |shell, _| shell.shelf_input_owner(true)),
        "a drawer row click cannot dismiss its own layer"
    );
    rig.keys("escape");
    assert!(
        !rig.shell
            .read_with(rig.cx, |shell, _| shell.shelf_input_owner(true))
    );
    assert_eq!(overlay(&mut rig), settings);
    assert_eq!(rig.route(), route);
    assert_dispatch_alive(&mut rig);
}

#[gpui::test]
fn add_query_survives_ask_cover_and_returns_with_live_focus(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    enable(&mut rig);
    rig.keys("cmd-o");
    rig.cx.simulate_input("/not-a-real-project");
    rig.settle();
    rig.keys("cmd-k");
    assert_eq!(overlay(&mut rig), Some(Overlay::CommandPalette));
    rig.keys("escape");
    assert_eq!(overlay(&mut rig), Some(Overlay::AddProject));
    let field = rig
        .cx
        .update(|window, cx| super::onboard::field(window, cx))
        .expect("retained Add field");
    assert_eq!(
        field.read_with(rig.cx, |input, _| input.value().to_string()),
        "/not-a-real-project"
    );
    assert!(
        rig.cx
            .update(|window, cx| field.read(cx).focus_handle(cx).is_focused(window)),
        "the uncovered Add editor receives native keyboard focus"
    );
    rig.keys("escape");
    assert_eq!(overlay(&mut rig), None);
    assert_dispatch_alive(&mut rig);
}

#[gpui::test]
fn drawer_narrowing_and_lens_clicks_keep_the_same_live_surface(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 663.0, 900.0);
    enable(&mut rig);
    let route = rig.route();
    rig.keys("cmd-\\");
    for words in ["Versions", "Rests on", "Used by", "Contents"] {
        click_text(&mut rig, words);
        assert!(
            rig.shell
                .read_with(rig.cx, |shell, _| shell.shelf_input_owner(true)),
            "{words} keeps the drawer input owner"
        );
        assert_eq!(rig.route(), route);
    }
    for key in ["cmd-up", "cmd-]", "ctrl-1"] {
        rig.keys(key);
        assert_eq!(rig.route(), route, "{key} cannot navigate the covered page");
        assert!(
            rig.shell
                .read_with(rig.cx, |shell, _| shell.shelf_input_owner(true))
        );
    }
    click_text(&mut rig, "Type to narrow");
    rig.keys("b a s e");
    assert!(
        painted(&mut rig)
            .texts
            .iter()
            .any(|text| text.content.as_ref() == "base")
    );
    assert!(
        rig.shell
            .read_with(rig.cx, |shell, _| shell.shelf_input_owner(true))
    );
    rig.keys("escape");
    rig.keys("escape");
    assert!(
        !rig.shell
            .read_with(rig.cx, |shell, _| shell.shelf_input_owner(true))
    );
    assert_eq!(rig.route(), route);
}

#[gpui::test]
fn rapid_ask_add_escape_does_not_revive_a_departed_visit(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 663.0, 900.0);
    enable(&mut rig);
    rig.cx.simulate_keystrokes("cmd-k");
    rig.frame(16);
    rig.cx.simulate_input("RelationLabel");
    rig.frame(16);
    rig.cx.simulate_keystrokes("cmd-o");
    rig.frame(16);
    assert_eq!(overlay(&mut rig), Some(Overlay::AddProject));
    rig.cx.simulate_keystrokes("escape");
    rig.frame(16);
    assert_eq!(overlay(&mut rig), Some(Overlay::CommandPalette));
    rig.cx.simulate_keystrokes("escape");
    rig.settle();
    assert_eq!(overlay(&mut rig), None);
    let old = rig.route();
    rig.keys("ctrl-1");
    assert_ne!(
        rig.route(),
        old,
        "committed navigation establishes a new visit"
    );
    assert_dispatch_alive(&mut rig);
}

/// Read the actually reported native focus after real input delivery. No
/// direct focus assignment or seeded card-selection state participates.
fn focused_label(rig: &mut Rig) -> Option<String> {
    rig.repaint();
    let json = rig
        .cx
        .update(|window, _| window.debug_a11y_tree_json())
        .expect("forced native tree");
    let tree: serde_json::Value = serde_json::from_str(&json).expect("native tree JSON");
    let id = tree["gpui_focus"].as_str()?;
    tree["nodes"][id]["aria"]["label"]
        .as_str()
        .map(str::to_owned)
}

#[gpui::test]
fn native_hand_tab_and_shift_tab_follow_card_order_and_escape_keeps_dispatch(
    cx: &mut TestAppContext,
) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    super::anatomy_tests::install(&mut rig);
    enable(&mut rig);
    rig.keys("cmd-d");
    rig.go(crate::navigation::Intent::Navigate(page_route(
        "relation_label",
    )));
    rig.keys("cmd-d");
    rig.go(crate::navigation::Intent::Navigate(
        crate::navigation::Route::Orbit(crate::navigation::OrbitRoute::Home),
    ));
    let open = rig
        .cx
        .debug_bounds("hand-open")
        .expect("painted native hand opener");
    rig.cx.simulate_click(open.center(), Modifiers::none());
    rig.settle();
    let mut reached = false;
    for _ in 0..32 {
        rig.keys("tab");
        if focused_label(&mut rig).as_deref() == Some("Open relation_label from hand") {
            reached = true;
            break;
        }
    }
    assert!(reached, "real Tab reaches the first native card");
    rig.keys("tab");
    assert_eq!(
        focused_label(&mut rig).as_deref(),
        Some("Open RelationLabel from hand")
    );
    rig.keys("shift-tab");
    assert_eq!(
        focused_label(&mut rig).as_deref(),
        Some("Open relation_label from hand")
    );
    rig.keys("enter");
    assert_eq!(
        rig.route(),
        page_route("relation_label"),
        "the focused native card activates, regardless of the shell's old logical zone"
    );
    rig.keys("h");
    rig.keys("escape");
    assert_dispatch_alive(&mut rig);
}

#[gpui::test]
fn actual_conflicting_hint_letters_narrow_the_current_hint_owner(cx: &mut TestAppContext) {
    for letter in ["f", "g", "h", "s"] {
        let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
        super::anatomy_tests::install(&mut rig);
        enable(&mut rig);
        let route = rig.route();
        rig.keys("f");
        let before = rig.shell.read_with(rig.cx, |shell, _| shell.hint_codes());
        assert!(
            before
                .iter()
                .any(|code| code.starts_with(letter) && code.len() == 2),
            "painted controls supply the two-key {letter} hint family"
        );
        rig.keys(letter);
        let after = rig.shell.read_with(rig.cx, |shell, _| shell.hint_codes());
        assert!(
            !after.is_empty() && after.len() < before.len(),
            "{letter} narrows hints instead of invoking a competing page action"
        );
        assert!(after.iter().all(|code| code.starts_with(letter)));
        assert_eq!(rig.route(), route);
        assert!(rig.shell.read_with(rig.cx, |shell, cx| {
            shell
                .chrome_words(cx)
                .iter()
                .any(|(key, value)| *key == "hand" && value == "closed")
        }));
        rig.keys("escape");
    }
}
