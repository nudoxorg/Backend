//! The Library page while an install adds packages: the ring of names grows
//! under a person's eyes, frame by frame.

#![allow(clippy::expect_used, clippy::panic)]

use super::tests::{Fixture, rig_with_reads};
use crate::model::pages::{GapReason, IndexedPackage, Known, OrbitModel, PackageRef, PageKey, PageValue, ReadFailure, Readiness};
use crate::navigation::{OrbitRoute, Route};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use gpui::TestAppContext;
use std::sync::{Arc, Mutex};

/// The fixture's pages, with a Library whose packages are what `packages`
/// holds when it is read (an install adds to it between reads).
struct Growing {
    packages: Arc<Mutex<Vec<&'static str>>>,
}

impl PageReader for Growing {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        if matches!(request, ReadRequest::Orbit) {
            let names = self.packages.lock().expect("the list").clone();
            let indexed = names
                .iter()
                .map(|name| IndexedPackage {
                    package: PackageRef::parse(&format!("/cache/src/{name}")).expect("a package"),
                    name: Arc::from(*name),
                    readiness: Readiness::Ready,
                })
                .collect::<Vec<_>>();
            return Ok(PageValue::Orbit(OrbitModel {
                indexed: Known::Known(indexed.into()),
                projects: Known::Known(Arc::from([])),
                explore: Known::Unknown(crate::model::pages::Gap::new(GapReason::NotServed, "")),
                tree: Known::Unknown(crate::model::pages::Gap::new(GapReason::NotServed, "")),
            }));
        }
        Fixture.read(request, context)
    }
}

/// Where each chip of the ring was drawn, from the flow's own probe tracks,
/// and whether that was a designed snap (a line change).
fn chips(ledger: &facet::probe::Ledger) -> Vec<(String, f32, bool)> {
    ledger
        .tracks
        .iter()
        .filter(|track| track.key.starts_with("orbit-ring.") && (track.key.ends_with(".x") || track.key.ends_with(".y")))
        .map(|track| (track.key.clone(), track.value, track.kind == facet::probe::TrackKind::Snap))
        .collect()
}

/// Packages arrive one by one, the way an install adds them (the library is
/// in name order, so each lands between two names already there, and the
/// ring re-wraps). In every frame: no two names of the ring are painted over
/// each other, and no name flies further than a glide's step (one that must
/// go to another line lands there, as a designed snap).
#[gpui::test]
fn the_ring_grows_as_packages_arrive_and_no_name_is_ever_painted_over_another(cx: &mut TestAppContext) {
    let packages = Arc::new(Mutex::new(vec!["equivalent", "indexmap", "serde", "toml", "winnow"]));
    let shared = Arc::clone(&packages);
    let pool = ReadPool::start(2, move |_| Growing { packages: Arc::clone(&shared) }).expect("read pool");
    let mut rig = rig_with_reads(cx, Some(Route::Orbit(OrbitRoute::Home)), 1440.0, 900.0, pool);
    rig.cx.update(|_, cx| {
        facet::probe::enable(cx);
        cx.set_global(gpui::TextTrace);
    });
    let arrivals = ["serde_core", "hashbrown", "serde_spanned", "toml_datetime", "toml_edit", "toml_write", "serde_derive", "unicode-ident", "proc-macro2", "quote", "memchr", "syn"];
    let mut snaps = 0;
    for arriving in arrivals {
        {
            let mut list = packages.lock().expect("the list");
            list.push(arriving);
            list.sort_unstable();
        }
        let names: Vec<&str> = packages.lock().expect("the list").clone();
        let _ = rig.cx.update(|_, cx| facet::probe::take(cx));
        rig.graph.store.update(rig.cx, |store, cx| store.retry(PageKey::Orbit, cx));
        let mut last: Option<Vec<(String, f32, bool)>> = None;
        for frame in 0..40 {
            rig.frame(16);
            rig.repaint();
            let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
            // The ring's names, where gpui painted them (the sidebar, left of
            // the reader, lists the same names).
            let painted: Vec<gpui::PaintedText> = rig.cx.update(|window, _| {
                window.painted_texts().iter().filter(|text| names.contains(&text.text.as_ref()) && text.bounds.origin.x > gpui::px(300.0)).cloned().collect()
            });
            for (index, a) in painted.iter().enumerate() {
                for b in &painted[index + 1..] {
                    let cut = a.bounds.intersect(&b.bounds);
                    assert!(
                        f32::from(cut.size.width) * f32::from(cut.size.height) < 1.0,
                        "after {arriving} arrived, frame {frame}: `{}` is painted over `{}` ({:?} and {:?})",
                        a.text,
                        b.text,
                        a.bounds,
                        b.bounds
                    );
                }
            }
            let now = chips(&ledger);
            snaps += now.iter().filter(|(_, _, snap)| *snap).count();
            if let Some(before) = &last {
                for (key, value, snap) in &now {
                    if let Some((_, was, _)) = before.iter().find(|(other, _, _)| other == key)
                        && !snap
                    {
                        assert!((value - was).abs() <= 48.0, "after {arriving} arrived, frame {frame}: `{key}` flew {:.1} px in one frame", value - was);
                    }
                }
            }
            last = Some(now);
        }
        rig.settle();
        let drawn: Vec<String> = rig.cx.update(|window, _| {
            window.painted_texts().iter().filter(|text| text.bounds.origin.x > gpui::px(300.0)).map(|text| text.text.to_string()).collect()
        });
        for name in &names {
            assert!(drawn.iter().any(|text| text == name), "`{name}` is in the ring once {arriving} arrived: {drawn:?}");
        }
    }
    assert!(snaps > 0, "the ring re-wrapped at least once (a name changed line) while it grew");
}

/// The Library leaves for a page and comes back: its ring of names does not
/// fly while it is the page going away (drawn under the plate, it is a still
/// copy), and back again every name stands where it lays out. J9 saw three
/// chips fly 700 px as the Library left, and come back mid-flight.
#[gpui::test]
fn the_ring_stands_still_while_the_library_leaves_and_when_it_comes_back(cx: &mut TestAppContext) {
    let packages = Arc::new(Mutex::new(vec!["equivalent", "indexmap", "serde", "serde_core", "toml", "toml_datetime", "toml_edit", "winnow"]));
    let shared = Arc::clone(&packages);
    let pool = ReadPool::start(2, move |_| Growing { packages: Arc::clone(&shared) }).expect("read pool");
    let mut rig = rig_with_reads(cx, Some(Route::Orbit(OrbitRoute::Home)), 1440.0, 900.0, pool);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    for _ in 0..40 {
        rig.frame(16);
    }
    let _ = rig.cx.update(|_, cx| facet::probe::take(cx));
    let mut flying = Vec::new();
    let mut watch = |rig: &mut super::tests::Rig, what: &str| {
        for frame in 0..40 {
            rig.frame(16);
            let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
            flying.extend(ledger.tracks.iter().filter(|track| track.key.starts_with("orbit-ring.") && track.live).map(|track| format!("{what} frame {frame}: {} at {:.1} toward {:.1}", track.key, track.value, track.target)));
        }
    };
    rig.graph.root.update(rig.cx, |root, cx| root.queue(crate::navigation::Intent::Navigate(super::tests::page_route("RelationLabel")), cx));
    watch(&mut rig, "leaving");
    // While the Library is away, the window narrows: the ring it comes back
    // to wraps otherwise than the one it left.
    rig.cx.simulate_resize(gpui::size(gpui::px(1000.0), gpui::px(900.0)));
    watch(&mut rig, "away");
    rig.cx.simulate_keystrokes("cmd-[");
    watch(&mut rig, "back");
    assert!(matches!(rig.route(), Route::Orbit(_)), "back on the Library: {:?}", rig.route());
    assert!(flying.is_empty(), "no name of the ring flies: {flying:#?}");
}
