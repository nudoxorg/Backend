//! The hand through the real shell, on the anatomy tests' small world:
//! `relation_label` hands you a `RelationLabel`.

use super::anatomy_tests::{install, painted};
use super::tests::{Rig, page_route, rig};
use crate::navigation::{Intent, OrbitRoute, Route};
use gpui::TestAppContext;

/// Holds RelationLabel, then relation_label (⌘D on each page), and lands
/// on Orbit.
fn two_held(cx: &mut TestAppContext) -> Rig {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    install(&mut rig);
    rig.keys("cmd-d");
    rig.go(Intent::Navigate(page_route("relation_label")));
    rig.keys("cmd-d");
    rig.go(Intent::Navigate(Route::Orbit(OrbitRoute::Home)));
    rig
}

/// What the opened hand says, in paint order.
fn row(rig: &mut Rig) -> Vec<String> {
    let ledger = painted(rig);
    ledger.texts.iter().filter(|text| text.key.starts_with("hand-row:")).map(|text| text.content.clone()).collect()
}

#[gpui::test]
fn the_hand_arranges_what_you_hold_by_what_feeds_what(cx: &mut TestAppContext) {
    let mut rig = two_held(cx);
    assert!(row(&mut rig).is_empty(), "closed: only the marks");
    rig.keys("h");
    // Held RelationLabel first, but relation_label hands you one: the road
    // reads in flow order, with its one sentence.
    assert_eq!(row(&mut rig), ["relation_label", "RelationLabel", "from Link to RelationLabel, in one step"]);
    rig.keys("escape");
    assert!(row(&mut rig).is_empty(), "Esc closes the hand");
}

/// Letting go re-routes: with RelationLabel gone, relation_label still
/// reaches SemanticLinkKind, now through `kind` (RelationLabel was only ever
/// a step on the way).
#[gpui::test]
fn letting_go_re_routes_the_road(cx: &mut TestAppContext) {
    let mut rig = two_held(cx);
    rig.go(Intent::Navigate(page_route("SemanticLinkKind")));
    rig.keys("cmd-d");
    rig.keys("h");
    assert_eq!(
        row(&mut rig),
        ["relation_label", "RelationLabel", "kind", "SemanticLinkKind", "from Link to SemanticLinkKind, in two steps"]
    );
    // ← → walk the open hand; ⌫ lets go of the card it stands on.
    rig.keys("right");
    assert_eq!(rig.shell.read_with(rig.cx, |shell, _| shell.hand_at()), 1, "on RelationLabel");
    rig.keys("backspace");
    assert_eq!(row(&mut rig), ["relation_label", "kind", "SemanticLinkKind", "from Link to SemanticLinkKind, in two steps"]);
    let held = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().hand.held().len());
    assert_eq!(held, 2);
}

#[gpui::test]
fn enter_in_the_open_hand_goes_to_the_card(cx: &mut TestAppContext) {
    let mut rig = two_held(cx);
    rig.keys("h");
    rig.keys("right");
    rig.keys("enter");
    assert_eq!(rig.route(), page_route("RelationLabel"), "the second card, in shown order");
    assert!(row(&mut rig).is_empty(), "going closes the hand");
}

#[gpui::test]
fn cmd_1_and_cmd_2_go_to_the_cards_in_the_order_they_are_shown(cx: &mut TestAppContext) {
    let mut rig = two_held(cx);
    rig.keys("cmd-1");
    assert_eq!(rig.route(), page_route("relation_label"));
    rig.keys("cmd-2");
    assert_eq!(rig.route(), page_route("RelationLabel"));
    rig.keys("cmd-3");
    assert_eq!(rig.route(), page_route("RelationLabel"), "no third card: nothing moves");
}

#[gpui::test]
fn an_empty_hand_shows_nothing_in_the_foot(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    install(&mut rig);
    let ledger = painted(&mut rig);
    assert!(!ledger.texts.iter().any(|text| text.key == "hand-open"), "no marks, no chevron");
    rig.keys("h");
    assert!(row(&mut rig).is_empty(), "H on an empty hand opens nothing");
}

#[gpui::test]
fn the_hand_is_what_a_restart_restores(cx: &mut TestAppContext) {
    let rig = two_held(cx);
    let snapshot = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot());
    let persisted = crate::model::PersistentState::project(&snapshot);
    assert_eq!(persisted.hand.len(), 2);
    let restored = crate::model::PersistentState::at(std::env::temp_dir().join("nudox-hand-roundtrip.json")).cold_reload(&persisted);
    assert_eq!(restored.hand, snapshot.session().hand, "what you held comes back, touched when it was");
}

/// On Orbit the resume line is the hand: its road, the sentence, and where
/// you left; clicking it goes back there.
#[gpui::test]
fn orbit_resumes_from_the_hand(cx: &mut TestAppContext) {
    let mut rig = two_held(cx);
    let said = rig.said();
    assert!(
        said.iter().any(|line| line
            == "Continue relation_label → RelationLabel · from Link to RelationLabel, in one step · you left at relation_label just now"),
        "{said:#?}"
    );
    let ledger = painted(&mut rig);
    let at = ledger.targets.iter().find(|target| target.key == "resume").expect("the resume line").bounds.clone();
    rig.cx.simulate_click(gpui::point(gpui::px(at.x + 20.0), gpui::px(at.y + at.height / 2.0)), gpui::Modifiers::default());
    rig.settle();
    assert_eq!(rig.route(), page_route("relation_label"), "where you left");
}

/// With an empty hand, Orbit continues at the last page behind you.
#[gpui::test]
fn orbit_with_an_empty_hand_continues_at_the_last_page(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    install(&mut rig);
    rig.go(Intent::Navigate(Route::Orbit(OrbitRoute::Home)));
    let said = rig.said();
    assert!(said.iter().any(|line| line == "Continue at RelationLabel"), "{said:#?}");
}

/// The first card ever held is whispered beside the foot ("RelationLabel
/// *in hand*"), once per install: not for the second, not after a restart.
#[gpui::test]
fn the_first_card_is_whispered_once(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    install(&mut rig);
    let whisper = |rig: &mut Rig| {
        let ledger = painted(rig);
        let texts: Vec<String> = ledger.texts.iter().map(|text| text.content.clone()).collect();
        texts.windows(2).find(|pair| pair[1] == "in hand").map(|pair| pair[0].clone())
    };
    assert_eq!(whisper(&mut rig), None, "nothing held, nothing said");
    rig.keys("cmd-d");
    assert_eq!(whisper(&mut rig).as_deref(), Some("RelationLabel"), "the first card");
    rig.go(Intent::Navigate(page_route("relation_label")));
    rig.keys("cmd-d");
    assert_ne!(whisper(&mut rig).as_deref(), Some("relation_label"), "the second card is never whispered");
    for _ in 0..160 {
        rig.frame(16);
    }
    assert_eq!(whisper(&mut rig), None, "gone after its 2.4 s");
    let snapshot = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot());
    let persisted = crate::model::PersistentState::project(&snapshot);
    assert!(persisted.hand_whispered);
    let restored = crate::model::PersistentState::at(std::env::temp_dir().join("nudox-whisper.json")).cold_reload(&persisted);
    assert!(restored.whispered && restored.whisper.is_none(), "a restart says nothing again");
}

/// The whisper ends by its OWN timer, whatever the motion clock says: the
/// executor's clock alone passing 2.4 s spends it (a timer that fired and
/// left the whisper standing was re-armed from an unmoved motion clock, for
/// ever: the rig never settled and the foot spun a core).
#[gpui::test]
fn the_whisper_is_spent_when_its_timer_fires(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    install(&mut rig);
    let whisper = |rig: &mut Rig| {
        let ledger = painted(rig);
        let texts: Vec<String> = ledger.texts.iter().map(|text| text.content.clone()).collect();
        texts.windows(2).any(|pair| pair[1] == "in hand")
    };
    rig.keys("cmd-d");
    assert!(whisper(&mut rig), "the first card is whispered");
    rig.cx.executor().advance_clock(std::time::Duration::from_millis(3_000));
    rig.cx.run_until_parked();
    rig.draw();
    assert!(!whisper(&mut rig), "its timer fired: it is spent");
}

/// Take → hand: the held card's stone starts over the hero stone it was
/// held from and travels to its place in the foot.
#[gpui::test]
fn a_taken_card_travels_from_the_hero_stone_to_the_foot(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    install(&mut rig);
    rig.repaint();
    let hero = rig.cx.debug_bounds("decl:/fixture/present::glyph.rs:138::RelationLabel").expect("the hero stone");
    let _ = rig.cx.update(|_, cx| facet::probe::take(cx));
    rig.cx.simulate_keystrokes("cmd-d");
    for _ in 0..40 {
        rig.frame(16);
    }
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    let y: Vec<&facet::probe::TrackSample> = ledger
        .tracks
        .iter()
        .filter(|track| track.key == "shared.hand:/fixture/present::glyph.rs:138::RelationLabel.y")
        .collect();
    let (first, last) = (y.first().expect("the stone travelled"), y.last().expect("and landed"));
    let hero_y = f32::from(hero.center().y);
    assert!((first.value - hero_y).abs() < 2.0, "it starts over the hero stone ({hero_y}): {}", first.value);
    assert!(last.value > 860.0 && !last.live, "it lands in the foot: {} (live {})", last.value, last.live);
}

/// Let go: the stone sinks through the foot's floor (no fade), its room
/// closes, and then it is gone.
#[gpui::test]
fn a_card_let_go_sinks_and_its_room_closes(cx: &mut TestAppContext) {
    let mut rig = two_held(cx);
    rig.keys("h");
    let _ = rig.cx.update(|_, cx| facet::probe::take(cx));
    rig.cx.simulate_keystrokes("backspace");
    for _ in 0..40 {
        rig.frame(16);
    }
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    let leaving = |axis: &str| -> Vec<&facet::probe::TrackSample> {
        ledger
            .tracks
            .iter()
            .filter(|track| track.key.starts_with("hand.marks.") && track.key.contains("relation_label") && track.key.ends_with(axis))
            .collect()
    };
    let y = leaving(".y");
    let deepest = y.iter().map(|sample| sample.value).fold(0.0_f32, f32::max);
    assert!(deepest > 17.0, "it sank through the floor: {deepest}");
    assert!(leaving(".opacity").iter().all(|sample| (sample.value - 1.0).abs() < 1e-3), "no fade");
    let marks = rig.shell.read_with(rig.cx, |shell, cx| shell.status_marks(cx));
    assert_eq!(marks, 1, "one card left in the foot");
}

/// ⌘ held: each card shows the digit that goes to it, in shown order.
#[gpui::test]
fn holding_cmd_shows_each_cards_digit(cx: &mut TestAppContext) {
    let mut rig = two_held(cx);
    let digits = |rig: &mut Rig| -> Vec<String> {
        let ledger = painted(rig);
        ledger.texts.iter().filter(|text| text.key.starts_with("hand-digit:")).map(|text| text.content.clone()).collect()
    };
    assert!(digits(&mut rig).is_empty(), "no digits at rest");
    rig.cx.simulate_modifiers_change(gpui::Modifiers { platform: true, ..gpui::Modifiers::default() });
    rig.cx.executor().advance_clock(super::reveal::HOLD + std::time::Duration::from_millis(10));
    rig.cx.run_until_parked();
    rig.draw();
    assert_eq!(digits(&mut rig), ["1", "2"]);
}
