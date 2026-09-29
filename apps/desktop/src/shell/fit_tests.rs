//! W-Fit: the shell at every window size.
//!
//! Each test opens the real window root at a size, lets it settle, redraws
//! it, and reads the probe ledger of the painted frame: every text box the
//! shell drew, every interactive box, the shell's own frame decision. The
//! assertions are about what is painted where (no text over other text, no
//! text cut by a window edge, the shelf in the state the width calls for),
//! never about how many of something there are.

#![allow(clippy::expect_used, clippy::panic, clippy::too_many_lines)]

use super::root::Shell;
use super::tests::{Rig, page_route, rig};
use crate::core::LocalProjectId;
use crate::navigation::{BrowseRoute, Intent, OrbitRoute, PackageLane, PackageRoute, Route, SettingsPage};
use facet::probe::{BoundsSample, Ledger, TextSample};
use gpui::{Modifiers, TestAppContext, point, px, size};

/// The window sizes the review sweeps (`.local/lanes/wave6/fit/sweep.py`).
pub(crate) const SIZES: [(f32, f32); 6] = [
    (800.0, 600.0),
    (1024.0, 700.0),
    (1280.0, 800.0),
    (1440.0, 900.0),
    (1920.0, 1080.0),
    (2560.0, 1440.0),
];

/// One thing wrong with a painted frame.
#[derive(Clone, Debug)]
pub(crate) struct Finding {
    pub rule: &'static str,
    pub what: String,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:<9} {}", self.rule, self.what)
    }
}

/// The part of a text box that is actually on screen: inside the window and
/// inside the clip its ancestors set.
fn visible(text: &TextSample, width: f32, height: f32) -> Option<BoundsSample> {
    let bounds = &text.bounds;
    let clip = text.paint_clip.as_ref();
    let x = bounds.x.max(clip.map_or(0.0, |b| b.x)).max(0.0);
    let y = bounds.y.max(clip.map_or(0.0, |b| b.y)).max(0.0);
    let right = (bounds.x + bounds.width).min(clip.map_or(width, |b| b.x + b.width)).min(width);
    let bottom = (bounds.y + bounds.height).min(clip.map_or(height, |b| b.y + b.height)).min(height);
    (right > x && bottom > y).then(|| BoundsSample { key: bounds.key.clone(), x, y, width: right - x, height: bottom - y })
}

/// What is wrong with the frame `ledger` recorded in a `width` x `height`
/// window: text cut mid-glyph, text hanging past an edge of the window or
/// of the clip that holds it, text over other text of its region, and
/// interactive boxes past the window's edge that nothing scrolls into reach.
pub(crate) fn findings(ledger: &Ledger, width: f32, height: f32) -> Vec<Finding> {
    let mut out = Vec::new();
    // `graph-test-*` records are the jump bar's test-only second publication of
    // a name the `text:` record already carries (`titlebar.rs`): the same
    // words at the same box, not a second text on screen.
    let texts = ledger.texts.iter().filter(|text| !text.key.starts_with("graph-test-")).collect::<Vec<_>>();
    for text in &texts {
        if text.content.trim().is_empty() {
            continue;
        }
        if text.clipped_without_ellipsis() {
            out.push(Finding {
                rule: "clip",
                what: format!(
                    "`{}` needs {:.0} px ({:?}, widest word {:.0}) in a {:.0} px box [{}]",
                    text.content, text.natural_width, text.overflow, text.min_width, text.bounds.width, text.key
                ),
            });
        }
        let Some(seen) = visible(text, width, height) else {
            continue;
        };
        let (lost_x, lost_y) = (text.bounds.width - seen.width, text.bounds.height - seen.height);
        if lost_x > 1.0 || lost_y > 1.0 {
            out.push(Finding {
                rule: "edge",
                what: format!(
                    "`{}` at ({:.0}, {:.0}) {:.0}x{:.0} shows only {:.0}x{:.0} inside the window and its clip [{}]",
                    text.content, text.bounds.x, text.bounds.y, text.bounds.width, text.bounds.height, seen.width, seen.height, text.key
                ),
            });
        }
    }
    for (index, a) in texts.iter().enumerate() {
        for b in &texts[index + 1..] {
            if a.region != b.region || a.key == b.key {
                continue;
            }
            let (Some(a_seen), Some(b_seen)) = (visible(a, width, height), visible(b, width, height)) else {
                continue;
            };
            let w = (a_seen.x + a_seen.width).min(b_seen.x + b_seen.width) - a_seen.x.max(b_seen.x);
            let h = (a_seen.y + a_seen.height).min(b_seen.y + b_seen.height) - a_seen.y.max(b_seen.y);
            if w > 1.0 && h > 1.0 {
                out.push(Finding {
                    rule: "overlap",
                    what: format!("`{}` and `{}` overlap by {w:.0}x{h:.0} [{} + {}]", a.content, b.content, a.key, b.key),
                });
            }
        }
    }
    for target in &ledger.targets {
        let b = &target.bounds;
        let reachable = ledger.scrolls.iter().any(|scroll| scroll.reaches(b));
        if target.state.focusable && !b.within(width + 0.5, height + 0.5) && !reachable {
            out.push(Finding {
                rule: "offscreen",
                what: format!("focusable [{}] at ({:.0}, {:.0}) {:.0}x{:.0} is past the {width:.0}x{height:.0} window", target.key, b.x, b.y, b.width, b.height),
            });
        }
    }
    out
}

/// Repaints every view and takes the frame's ledger.
pub(crate) fn painted(rig: &mut Rig) -> Ledger {
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    let _ = rig.cx.update(|_, cx| facet::probe::take(cx));
    rig.repaint();
    rig.cx.update(|_, cx| facet::probe::take(cx))
}

/// Resizes the window as a person dragging its edge does, and settles.
pub(crate) fn resize(rig: &mut Rig, width: f32, height: f32) {
    rig.cx.simulate_resize(size(px(width), px(height)));
    rig.settle();
}

pub(crate) fn package_route() -> Route {
    Route::Package(PackageRoute {
        project: None,
        at: None,
        package: crate::core::PackageId::new(super::tests::PACKAGE).expect("package"),
        lane: PackageLane::Overview,
        selected: None,
    })
}

fn find_route(text: &str) -> Route {
    let query = crate::model::pages::SearchQuery::new(text, 200).expect("query");
    Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(query)))
}

/// What a route is called in a report.
fn scenes() -> Vec<(&'static str, Route, Option<SettingsPage>)> {
    vec![
        ("library", Route::Orbit(OrbitRoute::Home), None),
        ("package", package_route(), None),
        ("symbol", page_route("RelationLabel"), None),
        ("graph", Route::World, None),
        ("find", find_route("RelationLabel"), None),
        ("settings", Route::Orbit(OrbitRoute::Home), Some(SettingsPage::Appearance)),
    ]
}

/// Survey, not a gate: prints what the frame at every size says for every
/// scene. Run with `--nocapture` to read it.
#[gpui::test]
fn survey_every_scene_at_every_size(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(Route::Orbit(OrbitRoute::Home)), SIZES[3].0, SIZES[3].1);
    for (name, route, settings) in scenes() {
        rig.go(Intent::Navigate(route));
        if let Some(page) = settings {
            rig.go(Intent::OpenSettings(page));
        }
        for (width, height) in SIZES {
            resize(&mut rig, width, height);
            let frame = rig.shell.read_with(rig.cx, |shell: &Shell, _| shell.frame()).expect("frame");
            let ledger = painted(&mut rig);
            let found = findings(&ledger, width, height);
            eprintln!(
                "[fit] {name:<8} {width:>4.0}x{height:<4.0} shelf {:?} reader {:>5.0} px  texts {:>3}  findings {}",
                frame.shelf,
                f32::from(frame.reader_width(px(width))),
                ledger.texts.len(),
                found.len()
            );
            for finding in &found {
                eprintln!("[fit]     {finding}");
            }
        }
    }
}

/// The Library's project tile stores an action in the reader's target list.
/// An action that captured the list itself (`ctx.targets.clone()`) is a
/// cycle: the list holds the action and the action holds the list, and with
/// them everything the action captured, the shell's links and the data
/// store among it. The harness caught it as a leaked `DataStore` handle
/// when the window closed, on the Library scene only (the one scene that
/// draws a project tile), and lost that scene's capture.
#[gpui::test]
fn the_librarys_project_tile_does_not_keep_its_own_target_list_alive(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(Route::Orbit(OrbitRoute::Home)), 1440.0, 900.0);
    let folder = std::env::temp_dir().join(format!("nudox-fit-project-{}", std::process::id()));
    std::fs::create_dir_all(&folder).expect("project folder");
    let project = LocalProjectId::from_path(&folder).expect("project identity");
    rig.go(Intent::AddProject { project });
    rig.go(Intent::Navigate(Route::Orbit(OrbitRoute::Home)));
    rig.repaint();
    let said = rig.said();
    let tile = folder.file_name().and_then(|name| name.to_str()).expect("folder name").to_owned();
    assert!(said.iter().any(|line| *line == tile), "the Library draws the project's tile {tile:?}: {said:#?}");
    let probe = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx).list_probe());
    assert!(probe.upgrade().is_some(), "the probe sees the live list");
    // Close the window and let go of every strong handle the rig keeps.
    let Rig { shell, graph, cx, .. } = rig;
    cx.update(|window, _| window.remove_window());
    drop(shell);
    drop(graph);
    // The window is gone, so only the app context can still be driven.
    for _ in 0..4 {
        cx.cx.update(|_| {});
        cx.cx.run_until_parked();
    }
    assert!(
        probe.upgrade().is_none(),
        "the reader's target list outlived its window: an action stored in it holds the list itself"
    );
}

/// Below 640 effective px the shelf is not inline at all, and below 520 the
/// titlebar used to drop its shelf toggle with the inbox button: the pointer
/// then had no way to the shelf or to Settings' pages (⌘\ still did). The
/// toggle stays at every width, is a real hit target inside the window, has
/// the back chevron to its right rather than under it, and opens the shelf
/// over the reader.
#[gpui::test]
fn the_shelf_can_be_opened_with_the_pointer_at_every_width(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    for (width, height) in [(1440.0, 900.0), (900.0, 700.0), (700.0, 600.0), (600.0, 600.0), (520.0, 640.0), (480.0, 640.0), (360.0, 640.0)] {
        resize(&mut rig, width, height);
        let frame = rig.shell.read_with(rig.cx, |shell: &Shell, _| shell.frame()).expect("frame");
        let ledger = painted(&mut rig);
        let toggle = ledger
            .targets
            .iter()
            .find(|target| target.key == "tb-shelf")
            .unwrap_or_else(|| panic!("no shelf toggle at {width} px: {:?}", ledger.targets.iter().map(|t| &t.key).collect::<Vec<_>>()));
        let at = &toggle.bounds;
        assert!(at.within(width, height), "the toggle at {width} px is {at:?}, outside the window");
        assert!(at.width >= 24.0 && at.height >= 24.0, "the toggle at {width} px is {:.0}x{:.0}", at.width, at.height);
        if let Some(back) = ledger.targets.iter().find(|target| target.key == "jump-back") {
            assert!(
                back.bounds.x >= at.x + at.width - 0.5,
                "at {width} px the back chevron ({:.0}) starts under the toggle ({:.0}..{:.0})",
                back.bounds.x,
                at.x,
                at.x + at.width
            );
        }
        if frame.shelf == super::ShelfMode::Shelf {
            continue;
        }
        // Not inline: a shelf row's words are not on screen until the toggle opens it.
        let words = |ledger: &Ledger| ledger.texts.iter().any(|text| text.content == "glyph" && text.key.starts_with("shelf-row:"));
        assert!(!words(&ledger), "at {width} px the shelf is already open");
        rig.cx.simulate_click(point(px(at.x + at.width / 2.0), px(at.y + at.height / 2.0)), Modifiers::default());
        rig.settle();
        let opened = painted(&mut rig);
        let row = opened
            .texts
            .iter()
            .find(|text| text.content == "glyph" && text.key.starts_with("shelf-row:"))
            .unwrap_or_else(|| panic!("clicking the toggle at {width} px opened no shelf"));
        assert!(
            row.bounds.x >= 0.0 && row.bounds.x + row.bounds.width <= width && row.bounds.x < f32::from(frame.shelf_body),
            "the opened shelf's row is at {:?} in a {width} px window",
            row.bounds
        );
        // Put it away again for the next width.
        rig.cx.simulate_click(point(px(at.x + at.width / 2.0), px(at.y + at.height / 2.0)), Modifiers::default());
        rig.settle();
    }
}

/// The shelf opened over the reader in a narrow window is a thing that was
/// asked for at that width. Widening the window to hold the shelf inline
/// answers it; narrowing again must not bring the overlay back on its own.
#[gpui::test]
fn a_shelf_opened_over_a_narrow_window_does_not_reopen_when_the_window_narrows_again(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    let row = |ledger: &Ledger| ledger.texts.iter().any(|text| text.content == "glyph" && text.key.starts_with("shelf-row:"));
    resize(&mut rig, 480.0, 640.0);
    let ledger = painted(&mut rig);
    assert!(!row(&ledger), "at 480 px the shelf is closed until it is asked for");
    let toggle = ledger.targets.iter().find(|target| target.key == "tb-shelf").expect("the toggle").bounds.clone();
    rig.cx.simulate_click(point(px(toggle.x + toggle.width / 2.0), px(toggle.y + toggle.height / 2.0)), Modifiers::default());
    rig.settle();
    assert!(row(&painted(&mut rig)), "the toggle opened the shelf over the reader");
    resize(&mut rig, 1440.0, 900.0);
    assert!(row(&painted(&mut rig)), "at 1440 px the shelf is inline");
    resize(&mut rig, 480.0, 640.0);
    assert!(!row(&painted(&mut rig)), "narrowing again did not ask for the shelf");
}
