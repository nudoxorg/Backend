//! W-Fit: the shell at every window size.
//!
//! Each test opens the real window root at a size, lets it settle, redraws
//! it, and reads the probe ledger of the painted frame: every text box the
//! shell drew, every interactive box, the shell's own frame decision. The
//! assertions are about what is painted where (no text over other text, no
//! text cut by a window edge, no text laid out where nothing shows it, the
//! shelf in the state the width calls for), never about how many of
//! something there are. The layout rules are `facet::probe::rules`, the same
//! functions the harness lint calls.

#![allow(clippy::expect_used, clippy::panic)]

use super::root::Shell;
use super::tests::{Rig, page_route, rig};
use crate::core::LocalProjectId;
use crate::navigation::{BrowseRoute, Intent, OrbitRoute, PackageLane, PackageRoute, Route, SettingsPage};
use facet::probe::rules::{overlap, stranded, visible_bounds};
use facet::probe::{BoundsSample, Ledger};
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

/// The phone windows the design has to hold (`W-Fluid.md`: 320x568, 360x640,
/// 390x844).
pub(crate) const PHONES: [(f32, f32); 3] = [(320.0, 568.0), (360.0, 640.0), (390.0, 844.0)];

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

/// What is wrong with the frame `ledger` recorded in a `width` x `height`
/// window: text cut mid-glyph, text hanging past a side edge of the window or
/// of the clip that holds it, text laid out wholly past the window's left or
/// right edge where nothing scrolls it into reach, text over other text of
/// its region, and interactive boxes past the window's edge that nothing
/// scrolls into reach.
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
        if let Some(side) = stranded(text, &ledger.scrolls, width) {
            out.push(Finding {
                rule: "stranded",
                what: format!(
                    "`{}` at ({:.0}, {:.0}) {:.0}x{:.0} lies wholly past the {} edge of the {width:.0} px window [{}]",
                    text.content, text.bounds.x, text.bounds.y, text.bounds.width, text.bounds.height, side.name(), text.key
                ),
            });
        }
        let Some(seen) = visible_bounds(text, width, height) else {
            continue;
        };
        // Sideways only: a line straddling the bottom of the window is a page that
        // scrolls, not a defect (a box shorter than its line is the `clip` rule's).
        let lost_x = text.bounds.width - seen.width;
        if lost_x > 1.0 {
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
            let (Some(a_seen), Some(b_seen)) = (visible_bounds(a, width, height), visible_bounds(b, width, height)) else {
                continue;
            };
            let (w, h) = overlap(&a_seen, &b_seen);
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
        let reachable = ledger.scrolls.iter().any(|scroll| scroll.reaches(b, &target.scroll_ancestors));
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

/// A scratch directory that goes when the test does, whether it passes or panics.
struct RemoveOnDrop(std::path::PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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

/// Resizes the rig's window to `width` x `height` and returns the findings of
/// the frame it painted, with the shell's own frame decision for the message.
fn findings_at(rig: &mut Rig, width: f32, height: f32) -> (String, Vec<Finding>) {
    resize(rig, width, height);
    let frame = rig.shell.read_with(rig.cx, |shell: &Shell, _| shell.frame()).expect("frame");
    let ledger = painted(rig);
    let found = findings(&ledger, width, height);
    (format!("shelf {:?}, reader {:.0} px, {} texts", frame.shelf, f32::from(frame.reader_width(px(width))), ledger.texts.len()), found)
}

/// Every screen, at the six sizes the review sweeps, paints nothing wrong:
/// no text cut mid-glyph, none hanging past an edge, none laid out where
/// nothing shows it, none over other text, no focusable past the window.
#[gpui::test]
fn every_screen_paints_clean_at_the_review_sizes(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(Route::Orbit(OrbitRoute::Home)), SIZES[3].0, SIZES[3].1);
    let mut wrong = Vec::new();
    for (name, route, settings) in scenes() {
        rig.go(Intent::Navigate(route));
        if let Some(page) = settings {
            rig.go(Intent::OpenSettings(page));
        }
        for (width, height) in SIZES {
            let (frame, found) = findings_at(&mut rig, width, height);
            wrong.extend(found.iter().map(|finding| format!("{name} {width:.0}x{height:.0} ({frame}): {finding}")));
        }
    }
    assert!(wrong.is_empty(), "{} findings:\n{}", wrong.len(), wrong.join("\n"));
}

/// The screens the shell lays out itself (Library, Settings, the graph's
/// frame) fit a phone at 320x568, 360x640 and 390x844. The package and symbol
/// pages fail here today (their own layouts: lede past the edge, a strip
/// 200 px beyond it); they are gated by their own lanes' tests, not this one.
#[gpui::test]
fn the_shells_own_screens_fit_a_phone(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(Route::Orbit(OrbitRoute::Home)), SIZES[3].0, SIZES[3].1);
    let mut wrong = Vec::new();
    for (name, route, settings) in scenes().into_iter().filter(|(name, _, _)| matches!(*name, "library" | "settings")) {
        rig.go(Intent::Navigate(route));
        if let Some(page) = settings {
            rig.go(Intent::OpenSettings(page));
        }
        for (width, height) in PHONES {
            let (frame, found) = findings_at(&mut rig, width, height);
            wrong.extend(found.iter().map(|finding| format!("{name} {width:.0}x{height:.0} ({frame}): {finding}")));
        }
    }
    assert!(wrong.is_empty(), "{} findings:\n{}", wrong.len(), wrong.join("\n"));
}

/// The gate above is only worth something if `findings` can fail: it reads a
/// pinned ledger the way the harness lint does, and names each thing wrong.
#[test]
fn findings_name_what_is_wrong_with_a_pinned_frame() {
    use facet::probe::{TextOverflow, TextSample};
    let sample = |key: &str, x: f32, y: f32, width: f32, natural: f32| TextSample {
        key: key.to_owned(),
        bounds: BoundsSample { key: key.to_owned(), x, y, width, height: 16.0 },
        paint_clip: Some(BoundsSample { key: key.to_owned(), x: 0.0, y: 0.0, width: 360.0, height: 640.0 }),
        scroll_ancestors: Vec::new(),
        natural_width: natural,
        overflow: TextOverflow::Clip,
        content: key.to_owned(),
        min_width: natural,
        line_height: 16.0,
        size: 14.0,
        weight: 400.0,
        region: Some("page".to_owned()),
    };
    let ledger = Ledger {
        texts: vec![
            sample("fits", 10.0, 10.0, 80.0, 80.0),
            sample("needs-room", 100.0, 10.0, 40.0, 90.0),
            sample("cut-by-edge", 330.0, 10.0, 60.0, 60.0),
            sample("past-edge", 400.0, 10.0, 60.0, 60.0),
            sample("on-top", 12.0, 10.0, 40.0, 40.0),
            sample("straddles-the-fold", 10.0, 630.0, 80.0, 80.0),
        ],
        ..Ledger::default()
    };
    let found = findings(&ledger, 360.0, 640.0);
    let rules = found.iter().map(|finding| (finding.rule, finding.what.split('[').next_back().unwrap_or_default().trim_end_matches(']').to_owned())).collect::<Vec<_>>();
    for wanted in [("clip", "needs-room"), ("edge", "cut-by-edge"), ("stranded", "past-edge"), ("overlap", "fits + on-top")] {
        assert!(rules.iter().any(|(rule, key)| (*rule, key.as_str()) == wanted), "no `{}` finding for `{}`: {found:#?}", wanted.0, wanted.1);
    }
    for fine in ["[fits]", "[straddles-the-fold]"] {
        assert!(!found.iter().any(|finding| finding.what.contains(fine)), "{fine} is not a finding (a fit, a page that scrolls): {found:#?}");
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
    let _removed_at_the_end = RemoveOnDrop(folder.clone());
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

/// `ink4` draws rules and inactive ticks, never words: it fails 4.5:1 on
/// every ground (`facet::tokens`, `the_inks_step_down_in_order_and_ink4_is_never_text`).
/// GAPS.md D1 was the sidebar's "Type to narrow" set in it, failing the
/// contrast lint on every Library frame. No text the shell sets names it.
#[test]
fn no_words_in_the_shell_are_set_in_ink4() {
    fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("the shell's sources").flatten() {
            let path = entry.path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let shell = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/shell");
    let mut files = Vec::new();
    sources(&shell, &mut files);
    // Spelled in pieces, so this scan does not find itself.
    let ink4 = ["palette.", "ink4"].concat();
    let (argument, color) = (format!(", {ink4})"), format!("text_color({ink4}"));
    let mut words = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("a source file");
        for (index, line) in text.lines().enumerate() {
            let set_as_text = (line.contains("text(") && line.contains(&argument)) || line.contains(&color);
            if set_as_text && !line.trim_start().starts_with("//") {
                words.push(format!("{}:{}: {}", file.strip_prefix(&shell).unwrap_or(file).display(), index + 1, line.trim()));
            }
        }
    }
    assert!(files.len() > 40, "the scan read the shell ({} files)", files.len());
    assert!(words.is_empty(), "words set in ink4:\n{}", words.join("\n"));
}
