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
    // Landed: the driver says so once, at rest on its target (the probe
    // ends a change's tracks at rest), and is never in flight again.
    assert!(
        last(&reduced, CARRY).is_none_or(|sample| !sample.live && (sample.value - 1.0).abs() < 1e-6 && sample.value == sample.target),
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

/// Once a change lands, its driver (and its plate's edges) end at rest where
/// they were heading: the probe hears the landing, so the next change starts
/// from a rest and never from a sample still in flight (J1's motion check saw
/// `reader.carry` jump 0.988 -> 0.000 across two changes, and every plate
/// edge with it).
#[gpui::test]
fn a_landed_change_ends_its_tracks_at_rest_on_their_targets(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    let _ = rig.cx.update(|_, cx| facet::probe::take(cx));
    rig.graph.root.update(rig.cx, |root, cx| root.queue(Intent::Navigate(page_route("KindGlyph")), cx));
    let mut carry = Vec::new();
    let mut plate = Vec::new();
    for _ in 0..60 {
        rig.frame(16);
        let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
        carry.extend(ledger.tracks.iter().filter(|track| track.key == CARRY).cloned());
        plate.extend(ledger.tracks.iter().filter(|track| track.key == "reader.plate.left").cloned());
    }
    assert!(carry.iter().any(|sample| sample.live), "the change was in flight first: {carry:?}");
    for (name, samples) in [("reader.carry", &carry), ("reader.plate.left", &plate)] {
        let last = samples.last().unwrap_or_else(|| panic!("{name} was published"));
        assert!(!last.live && last.value == last.target && last.velocity == 0.0, "{name} ends at rest on its target: {last:?}");
    }
}

/// Two changes one after the other: each is born where it starts (its
/// first frame a designed start: the plate at its row, the driver at its
/// start), never stepped to from the last change's rest; the driver's
/// landing is designed too (it is never painted), while the plate's last
/// step stays a judged spring (J1 saw `reader.plate.top` "jump" 432 px from
/// one change's rest to the next change's row, and `reader.carry` 0.988 to
/// 1.000 as its page landed).
#[gpui::test]
fn each_change_is_born_where_it_starts_not_stepped_to_from_the_last(cx: &mut TestAppContext) {
    use facet::probe::TrackKind;
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    let _ = rig.cx.update(|_, cx| facet::probe::take(cx));
    let mut carry = Vec::new();
    let mut plate = Vec::new();
    for next in ["KindGlyph", "RelationDirection"] {
        rig.graph.root.update(rig.cx, |root, cx| root.queue(Intent::Navigate(page_route(next)), cx));
        for frame in 0..60 {
            rig.frame(16);
            // The shell may draw a frame twice (J1: the birth sample was
            // published, then republished as a spring at the same instant,
            // and the probe keeps the later one).
            if frame == 0 {
                rig.repaint();
            }
            let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
            carry.extend(ledger.tracks.iter().filter(|track| track.key == CARRY).cloned());
            plate.extend(ledger.tracks.iter().filter(|track| track.key == "reader.plate.top").cloned());
        }
    }
    // Of two samples at one instant, the later is the one the probe keeps.
    let kept = |samples: &mut Vec<facet::probe::TrackSample>| {
        let mut out: Vec<facet::probe::TrackSample> = Vec::new();
        for sample in samples.drain(..) {
            match out.last_mut() {
                Some(last) if (last.at_ms - sample.at_ms).abs() < 1e-6 && last.live == sample.live => *last = sample,
                _ => out.push(sample),
            }
        }
        *samples = out;
    };
    kept(&mut carry);
    kept(&mut plate);
    for (name, samples) in [("reader.carry", &carry), ("reader.plate.top", &plate)] {
        let born = samples.iter().filter(|sample| sample.live && sample.kind == TrackKind::Snap).count();
        assert_eq!(born, 2, "{name}: each change's first frame is a designed start: {samples:#?}");
        for pair in samples.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            if !a.live && b.live {
                assert_eq!(b.kind, TrackKind::Snap, "{name}: after a rest the next change is born, not stepped to: {a:?} -> {b:?}");
            }
        }
    }
    let landed = |samples: &[facet::probe::TrackSample]| samples.iter().filter(|sample| !sample.live).map(|sample| sample.kind).collect::<Vec<_>>();
    assert!(landed(&carry).iter().all(|kind| *kind == TrackKind::Snap), "the driver lands by design: {:?}", landed(&carry));
    assert!(landed(&plate).iter().all(|kind| *kind == TrackKind::Spring), "the plate's landing is judged: {:?}", landed(&plate));
}

/// The window narrows across the edge where the symbol page's rail moves
/// from beside the page to under it (FLUID-C: a block moved 1084 px in one
/// frame there). In no frame of the change is a word painted over another:
/// the rail does not fly across the page's words on its way down.
#[gpui::test]
fn the_rail_goes_under_the_page_without_a_word_painted_over_another(cx: &mut TestAppContext) {
    use gpui::{px, size};
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    rig.cx.update(|_, cx| cx.set_global(gpui::TextTrace));
    rig.cx.simulate_resize(size(px(1000.0), px(900.0)));
    let mut worst: Option<String> = None;
    for frame in 0..40 {
        rig.frame(16);
        rig.repaint();
        let texts: Vec<gpui::PaintedText> = rig.cx.update(|window, _| {
            window.painted_texts().iter().filter(|text| text.bounds.origin.x > px(270.0) && text.bounds.origin.y > px(60.0) && text.alpha > 0.5).cloned().collect()
        });
        for (index, a) in texts.iter().enumerate() {
            for b in &texts[index + 1..] {
                let cut = a.bounds.intersect(&b.bounds);
                if f32::from(cut.size.width) > 2.0 && f32::from(cut.size.height) > 2.0 && worst.is_none() {
                    worst = Some(format!("frame {frame}: `{}` {:?} over `{}` {:?}", a.text, a.bounds, b.text, b.bounds));
                }
            }
        }
    }
    assert!(worst.is_none(), "a word was painted over another while the rail went under the page: {worst:?}");
}

/// Cached destinations still exercise the real owner, region and native
/// layout. Back/Forward interrupts at one clock instant, then a 200% resize
/// and reduced-motion change must leave one honest page and no idle frames.
#[gpui::test]
fn repeated_route_reversal_survives_large_text_resize_and_stops_requesting_frames(cx: &mut TestAppContext) {
    use gpui::{px, size};
    let a = page_route("RelationLabel");
    let b = page_route("KindGlyph");
    let mut rig = rig(cx, Some(a.clone()), 1440.0, 900.0);
    rig.go(Intent::Navigate(b.clone()));
    rig.go(Intent::Back);
    let leave = rig.shell.read_with(rig.cx, |shell, cx| {
        shell.reader_targets(cx).placed().first().map(|(target, _)| target.id.clone())
    }).expect("the real page has a focus target");
    rig.shell.update(rig.cx, |shell, cx| {
        let targets = shell.reader_targets(cx);
        targets.focus(leave.clone());
        targets.remember_leave(a.clone(), leave.clone());
    });
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    let queue = |rig: &mut super::tests::Rig, intent| {
        rig.graph.root.update(rig.cx, |root, cx| root.queue(intent, cx));
    };
    queue(&mut rig, Intent::Forward);
    rig.frame(80);
    for intent in [Intent::Back, Intent::Forward, Intent::Back] {
        queue(&mut rig, intent);
        rig.frame(0);
    }
    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
    rig.cx.simulate_resize(size(px(640.0), px(480.0)));
    queue(&mut rig, Intent::ZoomTo { display, percent: 200 });
    queue(&mut rig, Intent::SetMotion(MotionPreference::Reduced));
    rig.settle();
    assert_eq!(rig.route(), a);
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.reader_pages(cx)), 1);
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx).1), Some(leave));
    let requested = rig.cx.update(|_, cx| facet::motion::frames_requested(cx));
    for _ in 0..4 {
        rig.frame(250);
    }
    assert_eq!(rig.cx.update(|_, cx| facet::motion::frames_requested(cx)), requested, "settled motion requests no idle frames");
}


/// Native shell fixture coverage: hold an uncached read at the owner boundary.
/// The requested destination paints its own pending state, never the previous
/// declaration's body or targets. Resizes do not revive old motion.
/// Live owner/screenshots remain a separate product acceptance gate.
#[gpui::test]
fn pending_destination_paints_its_own_visit_and_back_recovers_the_prior_reading(cx: &mut TestAppContext) {
    use gpui::{Modifiers, px, size};
    let a = page_route("RelationLabel");
    let b = page_route("KindGlyph");
    let mut rig = rig(cx, Some(a.clone()), 1440.0, 900.0);
    let (leave, click) = rig.shell.read_with(rig.cx, |shell, cx| {
        shell.reader_targets(cx).placed().into_iter()
            .find(|(_, bounds)| bounds.center().x > px(300.0) && bounds.center().y > px(120.0) && bounds.center().y < px(800.0))
            .map(|(target, bounds)| (target.id, bounds.center()))
    }).expect("fixture page has a visible native target");
    rig.shell.update(rig.cx, |shell, cx| {
        let targets = shell.reader_targets(cx);
        targets.focus(leave.clone());
        targets.remember_leave(a.clone(), leave.clone());
    });
    rig.graph.store.update(rig.cx, |store, cx| store.owner_starting(cx));
    rig.graph.root.update(rig.cx, |root, cx| root.queue(Intent::Navigate(b.clone()), cx));
    rig.frame(0);
    assert_eq!(rig.route(), b);
    assert!(rig.said().iter().any(|line| line.contains("KindGlyph") && line.contains("on its way")));
    assert!(!rig.said().iter().any(|line| line.contains("RelationLabel")), "the old declaration cannot impersonate the destination");
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx).placed()).is_empty(), "pending destination has no stale keyboard targets");
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx).1), None);
    rig.cx.simulate_click(click, Modifiers::default());
    rig.frame(0);
    assert_eq!(rig.route(), b, "a visible prior link is natively inert");
    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
    rig.cx.simulate_resize(size(px(640.0), px(480.0)));
    for intent in [Intent::ZoomTo { display, percent: 200 }, Intent::SetMotion(MotionPreference::Reduced), Intent::SetMotion(MotionPreference::Full)] {
        rig.graph.root.update(rig.cx, |root, cx| root.queue(intent, cx));
        rig.frame(700);
    }
    for _ in 0..4 { rig.frame(700); }
    let requested = rig.cx.update(|_, cx| facet::motion::frames_requested(cx));
    rig.graph.store.update(rig.cx, |store, cx| store.owner_starting(cx));
    rig.repaint();
    for _ in 0..4 { rig.frame(250); }
    assert_eq!(rig.cx.update(|_, cx| facet::motion::frames_requested(cx)), requested, "a pending destination stays still through owner/repaint wakes");
    rig.graph.root.update(rig.cx, |root, cx| root.queue(Intent::Back, cx));
    rig.frame(0);
    rig.settle();
    assert_eq!(rig.route(), a);
    assert!(rig.said().iter().any(|line| line.contains("Earlier producer reading") || line.contains("Saved or earlier reading")));
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx).1), None, "retained destination has no semantic target focus");
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.reader_pages(cx)), 1);
    assert!(!rig.said().iter().any(|line| line.contains("previous page:")));
    rig.graph.store.update(rig.cx, |store, cx| store.owner_ready(cx));
    rig.settle();
    assert_eq!(rig.route(), a, "the obsolete B completion cannot replace Back's page");
}

/// A→B→C discards the never-painted B. A terminal destination failure releases
/// the single predecessor and exposes the current fault; late B replies cannot
/// cover it. The fixture owner boundary makes both orderings deterministic.
#[gpui::test]
fn superseded_pending_page_releases_on_failure_and_late_reads_cannot_replace_destination(cx: &mut TestAppContext) {
    let a = page_route("RelationLabel");
    let b = page_route("KindGlyph");
    let c = page_route("RelationDirection");
    let mut rig = rig(cx, Some(a), 1440.0, 900.0);
    rig.graph.store.update(rig.cx, |store, cx| store.owner_starting(cx));
    for route in [b, c.clone()] {
        rig.graph.root.update(rig.cx, |root, cx| root.queue(Intent::Navigate(route), cx));
        rig.frame(0);
    }
    assert_eq!(rig.route(), c);
    assert!(rig.said().iter().any(|line| line.contains("RelationDirection") && line.contains("on its way")));
    assert!(!rig.said().iter().any(|line| line.contains("RelationLabel")), "C must never borrow A's body");
    rig.graph.store.update(rig.cx, |store, cx| store.owner_failed(&crate::runtime::owner::OwnerFault::Lost("retention fixture failure".into()), cx));
    rig.frame(0);
    assert!(!rig.said().iter().any(|line| line.as_str() == "RelationLabel"), "terminal fault belongs to C, not A");
    assert!(rig.said().iter().any(|line| line.contains("retention fixture failure")), "the exact terminal destination is exposed");
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.reader_pages(cx)), 1);
    rig.graph.store.update(rig.cx, |store, cx| {
        store.owner_ready(cx);
        for key in super::reader::reader_keys(&store.snapshot()) { store.retry(key, cx); }
    });
    rig.settle();
    assert_eq!(rig.route(), c);
    assert!(rig.said().iter().any(|line| line.as_str() == "RelationDirection"));
    assert!(!rig.said().iter().any(|line| line.as_str() == "KindGlyph"));
}


/// A changed root cannot resurrect a different prior route as the pending
/// destination, even before its read emits a Resource event.
#[gpui::test]
fn pending_destination_stays_exact_on_root_wake_before_reads_start(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    rig.graph.store.update(rig.cx, |store, cx| store.owner_starting(cx));
    rig.graph.root.update(rig.cx, |root, cx| root.queue(Intent::Navigate(page_route("KindGlyph")), cx));
    rig.frame(0);
    assert!(rig.said().iter().any(|line| line.contains("KindGlyph") && line.contains("on its way")));
    rig.graph.store.update(rig.cx, |store, cx| {
        let root = crate::core::VersionedRoot::synthetic(
            backend_library::view_state_root(&[("retention".to_owned(), "new owner root".to_owned())]), 5,
        );
        let snapshot = std::sync::Arc::new(store.snapshot().with_key(root, None));
        store.admit_snapshot(snapshot, cx);
    });
    rig.frame(0);
    assert!(!rig.said().iter().any(|line| line.contains("previous page:")));
    assert!(!rig.said().iter().any(|line| line.as_str() == "RelationLabel"));
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.reader_pages(cx)), 1);
}
