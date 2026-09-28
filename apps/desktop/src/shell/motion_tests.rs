//! Motion through the real shell, frame by frame.

use super::tests::{page_route, rig};
use crate::model::MotionPreference;
use crate::navigation::Intent;
use gpui::TestAppContext;

/// Reduced motion switched on while a page is still arriving, then off
/// again: the arrival ends where reduced motion put it and never plays
/// backwards (the storm's seed 3: the arriving page jumped 0.930 -> 1.000 in
/// 0 ms across a motion toggle). The arrival is the reader's one driver,
/// `reader.carry` (the plate's openness).
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
    let before = last(&arriving, CARRY).expect("the page is arriving on the reader's driver");
    assert!(before.live && before.value < 0.99, "the page is still arriving: {before:?}");
    queue(&mut rig, Intent::SetMotion(MotionPreference::Reduced));
    rig.frame(16);
    let reduced = rig.cx.update(|_, cx| facet::probe::take(cx));
    assert!(
        last(&reduced, CARRY).is_none(),
        "reduced motion lands the arriving page at once: {:?}",
        last(&reduced, CARRY)
    );
    let pages = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_pages(cx));
    assert_eq!(pages, 1, "only the arrived page is drawn");
    queue(&mut rig, Intent::SetMotion(MotionPreference::Full));
    for _ in 0..40 {
        rig.frame(16);
    }
    let after = rig.cx.update(|_, cx| facet::probe::take(cx));
    assert!(
        last(&after, CARRY).is_none(),
        "motion back on never replays the arrival: {:?}",
        last(&after, CARRY)
    );
}

/// The reader's place-change driver.
const CARRY: &str = "reader.carry";

fn last<'a>(ledger: &'a facet::probe::Ledger, key: &str) -> Option<&'a facet::probe::TrackSample> {
    ledger.tracks.iter().rev().find(|track| track.key == key)
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
