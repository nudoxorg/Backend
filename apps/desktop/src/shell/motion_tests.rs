//! Motion through the real shell, frame by frame.

use super::tests::{page_route, rig};
use crate::model::MotionPreference;
use crate::navigation::Intent;
use gpui::TestAppContext;

/// Reduced motion switched on while a page is still arriving, then off
/// again: the arrival ends where reduced motion put it and never plays
/// backwards (the storm's seed 3: `reader.pages.place-*` jumped 0.930 ->
/// 1.000 in 0 ms across a motion toggle).
#[gpui::test]
fn a_page_arriving_when_motion_is_reduced_lands_and_stays_landed(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    let queue = |rig: &mut super::tests::Rig, intent: Intent| {
        rig.graph.root.update(rig.cx, |root, cx| root.queue(intent, cx));
    };
    queue(&mut rig, Intent::Navigate(page_route("KindGlyph")));
    for _ in 0..8 {
        rig.frame(16);
    }
    let arriving = rig.cx.update(|_, cx| facet::probe::take(cx));
    let place = arrival(&arriving);
    let before = last(&arriving, &place);
    assert!(before.live && before.value < 0.99, "the page is still arriving: {before:?}");
    queue(&mut rig, Intent::SetMotion(MotionPreference::Reduced));
    rig.frame(16);
    let reduced = rig.cx.update(|_, cx| facet::probe::take(cx));
    let landed = last(&reduced, &place);
    assert!(
        !landed.live && (landed.value - 1.0).abs() < 1e-3,
        "reduced motion lands the arriving page at once: {place} is {:.4}{}",
        landed.value,
        if landed.live { " and still moving" } else { "" }
    );
    queue(&mut rig, Intent::SetMotion(MotionPreference::Full));
    for _ in 0..40 {
        rig.frame(16);
    }
    let after = rig.cx.update(|_, cx| facet::probe::take(cx));
    for sample in after.tracks.iter().filter(|track| track.key == place) {
        assert!(
            !sample.live && (sample.value - 1.0).abs() < 1e-3,
            "motion back on never replays the arrival: {place} at {:.0} ms is {:.4}",
            sample.at_ms,
            sample.value
        );
    }
}

/// The opacity track of the newest page in the reader's stack.
fn arrival(ledger: &facet::probe::Ledger) -> String {
    ledger
        .tracks
        .iter()
        .filter_map(|track| {
            let rest = track.key.strip_prefix("reader.pages.place-")?.strip_suffix(".opacity")?;
            rest.parse::<u64>().ok().map(|n| (n, track.key.clone()))
        })
        .max()
        .map(|(_, key)| key)
        .expect("an arriving page")
}

fn last<'a>(ledger: &'a facet::probe::Ledger, key: &str) -> &'a facet::probe::TrackSample {
    ledger.tracks.iter().rev().find(|track| track.key == key).expect("a sample")
}

/// The storm's seed 3: focus on a reader target, then the window shrinks
/// and the text grows. The focused target is on screen in the very frame
/// the page reflows (focus never points somewhere you cannot see).
#[gpui::test]
fn a_reflow_keeps_the_focused_target_on_screen(cx: &mut TestAppContext) {
    use gpui::{px, size};
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    rig.keys("j j j j j j");
    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
    rig.cx.simulate_resize(size(px(480.0), px(320.0)));
    rig.graph.root.update(rig.cx, |root, cx| root.queue(Intent::ZoomTo { display, percent: 150 }, cx));
    rig.cx.run_until_parked();
    let _ = rig.cx.update(|_, cx| facet::probe::take(cx));
    rig.frame(16);
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    let focused = ledger.targets.iter().filter(|target| target.state.focused).collect::<Vec<_>>();
    assert_eq!(focused.len(), 1, "one focused target: {:?}", focused.iter().map(|t| &t.key).collect::<Vec<_>>());
    let at = &focused[0].bounds;
    assert!(
        at.y >= 0.0 && at.y + at.height <= 320.0,
        "{} is at y {}..{} in a 320 px window",
        focused[0].key,
        at.y,
        at.y + at.height
    );
}
