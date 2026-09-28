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
    rig.keys("secondary-d");
    rig.go(Intent::Navigate(page_route("relation_label")));
    rig.keys("secondary-d");
    rig.go(Intent::Navigate(Route::Orbit(OrbitRoute::Home)));
    rig
}

/// What the opened hand says, in paint order (everything painted after
/// the foot's `›`, which opens it).
fn row(rig: &mut Rig) -> Vec<String> {
    let ledger = painted(rig);
    let from = ledger.texts.iter().rposition(|text| text.key == "hand-open").map_or(ledger.texts.len(), |at| at + 1);
    ledger.texts[from..].iter().map(|text| text.content.clone()).collect()
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

#[gpui::test]
fn cmd_1_and_cmd_2_go_to_the_cards_in_the_order_they_are_shown(cx: &mut TestAppContext) {
    let mut rig = two_held(cx);
    rig.keys("secondary-1");
    assert_eq!(rig.route(), page_route("relation_label"));
    rig.keys("secondary-2");
    assert_eq!(rig.route(), page_route("RelationLabel"));
    rig.keys("secondary-3");
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
