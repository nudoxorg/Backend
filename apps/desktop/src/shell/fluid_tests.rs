//! W-Fluid: the shell from a 2560 px window down to a 320 px phone.
//!
//! Real windows at the phone sizes (320×568, 360×640, 390×844), read from the
//! probe ledger of the painted frame: what the shelf is, where the drawer
//! opens and how it closes, that the reader's first heading is on screen in
//! full and that nothing hangs past the window's edge. Fixtures are pinned;
//! nothing here reads live data.

#![allow(clippy::expect_used, clippy::panic, clippy::too_many_lines)]

use super::ShelfMode;
use super::fit_tests::{findings, package_route, painted, resize};
use super::root::Shell;
use super::tests::{Rig, page_route, rig};
use crate::navigation::{BrowseRoute, Intent, OrbitRoute, Route, SettingsPage};
use facet::probe::{Ledger, TextSample};
use facet::tokens::fluid::Dock;
use gpui::{Modifiers, TestAppContext, point, px};

/// The phone sizes the brief names.
const PHONES: [(f32, f32); 3] = [(320.0, 568.0), (360.0, 640.0), (390.0, 844.0)];

fn frame(rig: &mut Rig) -> super::Frame {
    rig.shell
        .read_with(rig.cx, |shell: &Shell, _| shell.frame())
        .expect("frame")
}

/// A shelf row's words are on screen (the shelf is open, inline or over).
fn shelf_shown(ledger: &Ledger) -> bool {
    ledger
        .texts
        .iter()
        .any(|text| text.content == "glyph" && text.key.starts_with("shelf-row:"))
}

fn toggle(ledger: &Ledger) -> facet::probe::BoundsSample {
    ledger
        .targets
        .iter()
        .find(|target| target.key == "tb-shelf")
        .expect("the shelf toggle")
        .bounds
        .clone()
}

fn click(rig: &mut Rig, x: f32, y: f32) {
    rig.cx
        .simulate_click(point(px(x), px(y)), Modifiers::default());
    rig.settle();
}

fn find_route() -> Route {
    let query = crate::model::pages::SearchQuery::new("RelationLabel", 200).expect("query");
    Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(query)))
}

/// A screen the review names: its route and the settings page over it, if any.
type Screen = (&'static str, Route, Option<SettingsPage>);

/// The six screens, by name.
fn screens() -> Vec<Screen> {
    vec![
        ("library", Route::Orbit(OrbitRoute::Home), None),
        ("package", package_route(), None),
        ("symbol", page_route("RelationLabel"), None),
        ("graph", Route::World, None),
        ("find", find_route(), None),
        (
            "settings",
            Route::Orbit(OrbitRoute::Home),
            Some(SettingsPage::Appearance),
        ),
    ]
}

/// The largest text below the titlebar: the reader's first heading.
fn first_heading(ledger: &Ledger, titlebar: f32) -> Option<&TextSample> {
    ledger
        .texts
        .iter()
        .filter(|text| {
            text.bounds.y >= titlebar
                && !text.content.trim().is_empty()
                && !text.key.starts_with("shelf-row:")
        })
        .max_by(|a, b| {
            a.size
                .total_cmp(&b.size)
                .then(b.bounds.y.total_cmp(&a.bounds.y))
        })
}

/// On a phone the shelf is a drawer: not inline, opened by the titlebar
/// button over the page with a scrim, and put away by a click on the strip of
/// page beside it, by Escape and by moving to another page.
#[gpui::test]
fn on_a_phone_the_shelf_is_a_drawer_with_a_scrim(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    for (width, height) in PHONES {
        resize(&mut rig, width, height);
        let at = frame(&mut rig);
        assert_eq!(at.dock.mode, Dock::Drawer, "{width} px is a drawer");
        assert_eq!(at.shelf, ShelfMode::Hidden);
        let ledger = painted(&mut rig);
        assert!(
            !shelf_shown(&ledger),
            "at {width} px the shelf is closed until it is asked for"
        );
        let button = toggle(&ledger);
        assert!(
            button.within(width, height) && button.width >= 24.0 && button.height >= 24.0,
            "the toggle at {width} px is {button:?}"
        );
        let (bx, by) = (
            button.x + button.width / 2.0,
            button.y + button.height / 2.0,
        );

        // Opened by the titlebar button.
        click(&mut rig, bx, by);
        let open = painted(&mut rig);
        let row = open
            .texts
            .iter()
            .find(|text| text.content == "glyph" && text.key.starts_with("shelf-row:"))
            .unwrap_or_else(|| panic!("the toggle opened no drawer at {width} px"));
        let drawer = f32::from(frame(&mut rig).drawer);
        assert!(
            row.bounds.x >= 0.0 && row.bounds.x + row.bounds.width <= drawer,
            "the row {:?} is not inside the {drawer} px drawer at {width} px",
            row.bounds
        );
        assert!(
            drawer <= width - 47.0,
            "the drawer covers the page at {width} px: {drawer}"
        );

        // Put away by a click on the strip of page beside it (the scrim).
        click(&mut rig, width - 10.0, height / 2.0);
        assert!(
            !shelf_shown(&painted(&mut rig)),
            "a click on the scrim left the drawer open at {width} px"
        );

        // Put away by Escape.
        click(&mut rig, bx, by);
        assert!(
            shelf_shown(&painted(&mut rig)),
            "the drawer did not reopen at {width} px"
        );
        rig.keys("escape");
        assert!(
            !shelf_shown(&painted(&mut rig)),
            "Escape left the drawer open at {width} px"
        );

        // Put away by moving to another page.
        click(&mut rig, bx, by);
        assert!(shelf_shown(&painted(&mut rig)));
        rig.go(Intent::Navigate(package_route()));
        assert!(
            !shelf_shown(&painted(&mut rig)),
            "a route change left the drawer open at {width} px"
        );
        rig.go(Intent::Navigate(page_route("RelationLabel")));
    }
}

/// At every phone size, on the screens this lane owns the reader's first
/// heading is on screen in full, nothing is drawn past the window's right edge
/// and the lint findings are empty. The package and symbol pages belong to the
/// page lanes: their findings are printed (`--nocapture`) and listed in
/// `.local/lanes/wave6/fluid/ADOPT.md`, not asserted here.
#[gpui::test]
fn on_a_phone_every_screen_shows_its_heading_in_full_and_never_overflows(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(Route::Orbit(OrbitRoute::Home)), 1440.0, 900.0);
    for (name, route, settings) in screens() {
        rig.go(Intent::Navigate(route));
        if let Some(page) = settings {
            rig.go(Intent::OpenSettings(page));
        }
        let ours = matches!(name, "library" | "find" | "settings" | "graph");
        for (width, height) in PHONES {
            resize(&mut rig, width, height);
            let titlebar = f32::from(frame(&mut rig).titlebar);
            let ledger = painted(&mut rig);
            let found = findings(&ledger, width, height);
            if !ours {
                eprintln!(
                    "[fluid] {name} at {width}x{height}: {} findings (a page lane's)",
                    found.len()
                );
                for finding in &found {
                    eprintln!("[fluid]     {finding}");
                }
                continue;
            }
            // Nothing hangs past the window, whatever else a page still owes.
            let overflow: Vec<_> = found
                .iter()
                .filter(|finding| finding.rule == "edge")
                .map(ToString::to_string)
                .collect();
            assert!(
                overflow.is_empty(),
                "{name} at {width}x{height}: {overflow:?}"
            );
            let heading = first_heading(&ledger, titlebar)
                .unwrap_or_else(|| panic!("{name} at {width}x{height} draws no text"));
            let b = &heading.bounds;
            assert!(
                b.x >= 0.0 && b.x + b.width <= width + 0.5 && b.y + b.height <= height,
                "{name} at {width}x{height}: the heading `{}` is {b:?}, outside the window",
                heading.content
            );
            assert!(found.is_empty(), "{name} at {width}x{height}: {found:?}");
        }
    }
}

/// The x of the reader's first heading in the frame just painted.
fn heading_x(rig: &mut Rig) -> f32 {
    let titlebar = f32::from(frame(rig).titlebar);
    let ledger = painted(rig);
    first_heading(&ledger, titlebar)
        .expect("a heading")
        .bounds
        .x
}

fn settings(cx: &mut TestAppContext, width: f32, height: f32) -> Rig {
    let mut rig = rig(cx, Some(Route::Orbit(OrbitRoute::Home)), width, height);
    rig.go(Intent::OpenSettings(SettingsPage::Appearance));
    rig
}

/// A window dragged across the shelf's edge does not swap the shelf for a
/// spine in one frame: the column glides, every frame between the two
/// placements is drawn, and no frame moves the page by more than half the
/// way.
#[gpui::test]
fn the_shelf_glides_to_a_spine_instead_of_jumping(cx: &mut TestAppContext) {
    let mut rig = settings(cx, 1000.0, 700.0);
    let before = heading_x(&mut rig);
    assert_eq!(frame(&mut rig).shelf, ShelfMode::Shelf);
    rig.cx.simulate_resize(gpui::size(px(850.0), px(700.0)));
    rig.draw();
    let mut xs = vec![heading_x(&mut rig)];
    for _ in 0..60 {
        rig.frame(16);
        xs.push(heading_x(&mut rig));
    }
    let after = *xs.last().expect("frames");
    assert_eq!(frame(&mut rig).shelf, ShelfMode::Spine);
    assert!(
        before - after > 150.0,
        "the column narrowed from {before} to {after}: a spine is 42 px"
    );
    let between = xs
        .iter()
        .filter(|x| **x < before - 10.0 && **x > after + 10.0)
        .count();
    assert!(
        between >= 5,
        "only {between} frames between the shelf and the spine: {xs:?}"
    );
    let biggest = xs
        .windows(2)
        .map(|pair| (pair[0] - pair[1]).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        biggest < 0.5 * (before - after),
        "one frame moved the page {biggest} px of {}: {xs:?}",
        before - after
    );
    assert!(
        xs.windows(2).all(|pair| pair[1] <= pair[0] + 0.01),
        "the page moved back on its way: {xs:?}"
    );
}

/// A window resting on the shelf's edge, ten px either side of it, does not
/// flip the shelf back and forth; and the shelf comes back only once the
/// window is a half band past the edge the other way.
#[gpui::test]
fn a_window_resting_on_the_shelf_edge_holds_its_mode(cx: &mut TestAppContext) {
    let mut rig = settings(cx, 1000.0, 700.0);
    let shelf_at = |rig: &mut Rig, width: f32| {
        resize(rig, width, 700.0);
        frame(rig).shelf
    };
    assert_eq!(
        shelf_at(&mut rig, 890.0),
        ShelfMode::Shelf,
        "inside the band on the way down: held"
    );
    assert_eq!(
        shelf_at(&mut rig, 870.0),
        ShelfMode::Spine,
        "past it: a spine"
    );
    assert_eq!(
        shelf_at(&mut rig, 910.0),
        ShelfMode::Spine,
        "inside the band on the way up: held"
    );
    for pass in 0..12 {
        let width = 900.0 + if pass % 2 == 0 { -10.0 } else { 10.0 };
        assert_eq!(
            shelf_at(&mut rig, width),
            ShelfMode::Spine,
            "flipped at {width}"
        );
    }
    assert_eq!(
        shelf_at(&mut rig, 920.0),
        ShelfMode::Shelf,
        "past the band: a shelf"
    );
}

/// Three routes in one instant (A, B, C) are one change from A to C. B never
/// reaches the screen: the reader draws A leaving and C arriving, and no
/// frame of the change shows B's title or its "on its way" skeleton (the leaving
/// page had been the previous *place*, not the last page painted).
#[gpui::test]
fn a_burst_of_routes_paints_no_page_for_the_ones_it_skipped(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    let (a, b, c) = ("RelationLabel", "RelationDirection", "KindGlyph");
    rig.graph.root.update(rig.cx, |root, cx| {
        root.queue(Intent::Navigate(page_route(b)), cx);
        root.queue(Intent::Navigate(page_route(c)), cx);
    });
    let mut frames = Vec::new();
    for _ in 0..50 {
        rig.frame(16);
        let at = frame(&mut rig);
        let (left, top) = (f32::from(at.shelf_width), f32::from(at.titlebar));
        let ledger = painted(&mut rig);
        // The reader's own words: not the shelf's rows or the titlebar's bar.
        frames.push(
            ledger
                .texts
                .iter()
                .filter(|text| text.bounds.x >= left && text.bounds.y >= top)
                .map(|text| text.content.clone())
                .collect::<Vec<_>>(),
        );
    }
    let shows = |frame: &[String], name: &str| frame.iter().any(|content| content == name);
    let first_b = frames.iter().position(|frame| shows(frame, b));
    assert!(
        first_b.is_none(),
        "frame {first_b:?} painted the skipped page `{b}`: {:?}",
        first_b.map(|at| &frames[at])
    );
    assert!(
        frames.iter().any(|frame| shows(frame, c)),
        "the last route `{c}` never arrived"
    );
    // The page it left is on screen until it has gone: the change plays from A, not from nothing.
    assert!(
        frames.iter().take(3).any(|frame| shows(frame, a)),
        "the change did not start from `{a}`: {:?}",
        &frames[..3]
    );
}

/// ⌘K at every width: the query is drawn in the titlebar as it is typed, and
/// its results are drawn as a plate over the shelf's column below it: a panel
/// no wider than the token says in a roomy window, a sheet across the window
/// on a phone. Nothing is drawn past the window's edge, and Escape puts it all
/// away.
#[gpui::test]
fn typing_into_ask_draws_the_query_and_a_result_row(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    rig.cx.update(|_, cx| cx.set_global(gpui::TextTrace));
    for (width, height, plate) in [
        (1440.0_f32, 900.0_f32, 440.0_f32),
        (800.0, 600.0, 344.0),
        (360.0, 640.0, 360.0),
    ] {
        resize(&mut rig, width, height);
        rig.keys("cmd-k");
        rig.keys("r e l a t i o n");
        rig.settle();
        let titlebar = f32::from(frame(&mut rig).titlebar);
        let _ = painted(&mut rig);
        let words: Vec<gpui::PaintedText> =
            rig.cx.update(|window, _| window.painted_texts().to_vec());
        // The query is drawn in the titlebar.
        let query = words
            .iter()
            .find(|text| {
                text.text.as_ref() == "relation" && f32::from(text.bounds.origin.y) < titlebar
            })
            .unwrap_or_else(|| {
                panic!(
                    "the typed query is not drawn in the titlebar at {width} px: {:?}",
                    words.iter().map(|t| t.text.to_string()).collect::<Vec<_>>()
                )
            });
        assert!(
            f32::from(query.bounds.right()) <= width,
            "the query overflows the window at {width} px: {:?}",
            query.bounds
        );
        // The plate is the width the token says (a sheet across the window on a phone),
        // starts under the titlebar at the window's left edge and runs to the foot.
        let bounds = rig
            .cx
            .debug_bounds("ask-plate")
            .unwrap_or_else(|| panic!("Ask draws no plate at {width} px"));
        assert!(
            (f32::from(bounds.size.width) - plate).abs() < 1.0
                && f32::from(bounds.origin.x).abs() < 0.5
                && (f32::from(bounds.origin.y) - titlebar).abs() < 0.5,
            "the plate at {width} px is {bounds:?}, want {plate} wide at (0, {titlebar})"
        );
        // A result row is drawn below it, over the shelf's column, inside the plate.
        let row = words
            .iter()
            .find(|text| {
                text.text.contains("RelationLabel")
                    && f32::from(text.bounds.origin.y) > titlebar
                    && f32::from(text.bounds.origin.x) < plate
            })
            .unwrap_or_else(|| {
                panic!(
                    "no result row for `relation` at {width} px: {:?}",
                    words.iter().map(|t| t.text.to_string()).collect::<Vec<_>>()
                )
            });
        assert!(
            f32::from(row.bounds.right()) <= plate + 0.5 && f32::from(row.bounds.right()) <= width,
            "the result row {:?} is outside the {plate} px plate at {width} px",
            row.bounds
        );
        rig.keys("escape");
        let _ = painted(&mut rig);
        let after: Vec<gpui::PaintedText> =
            rig.cx.update(|window, _| window.painted_texts().to_vec());
        assert!(
            !after.iter().any(|text| text.text.as_ref() == "relation"
                && f32::from(text.bounds.origin.y) < titlebar),
            "Escape left the query in the titlebar at {width} px"
        );
    }
}

/// A fast shrink (a maximise-then-restore, an edge snap: 1440 to 360 at once)
/// leaves the shelf on its way to its new width for a few frames. The reader
/// must not be squeezed to a sliver meanwhile: the column may take at most the
/// share of the window `COLUMNS_SHARE` gives it, so the heading has room in
/// every frame.
#[gpui::test]
fn a_fast_shrink_never_squeezes_the_reader_to_a_sliver(cx: &mut TestAppContext) {
    let mut rig = settings(cx, 1440.0, 900.0);
    assert!(heading_x(&mut rig) > 250.0, "at 1440 the shelf is inline");
    rig.cx.simulate_resize(gpui::size(px(360.0), px(640.0)));
    rig.draw();
    let mut xs = vec![heading_x(&mut rig)];
    for _ in 0..40 {
        rig.frame(16);
        xs.push(heading_x(&mut rig));
    }
    // The shelf column takes at most 42 % of 360 px (151 px); the heading sits a 16 px gutter inside the reader.
    let worst = xs.iter().copied().fold(0.0_f32, f32::max);
    assert!(
        worst <= 0.42 * 360.0 + 16.5,
        "the reader was squeezed: the heading sat at x = {worst} in a 360 px window: {xs:?}"
    );
    assert!(
        (xs[xs.len() - 1] - 16.0).abs() <= 2.0,
        "it settles at the phone's gutter, not {}",
        xs[xs.len() - 1]
    );
}

/// The shelf's rows swap for the spine's while the column narrows. The
/// legibility law (`facet::gallery::legible`): a text painted translucent
/// (alpha under 0.95) must not linger there (at most 2 frames under 0.8), and
/// must not be cut mid-glyph while it is: the words are either whole or gone.
/// At 100 % and at 200 % text (a window twice as wide is the same room).
#[gpui::test]
fn the_shelf_swaps_for_the_spine_without_a_lingering_fade_or_a_cut_word(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1000.0, 900.0);
    rig.cx.update(|_, cx| cx.set_global(gpui::TextTrace));
    for percent in [100_u16, 200] {
        let scale = f32::from(percent) / 100.0;
        let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
        rig.go(Intent::ZoomTo { display, percent });
        resize(&mut rig, 1000.0 * scale, 900.0);
        assert_eq!(
            frame(&mut rig).shelf,
            ShelfMode::Shelf,
            "{percent} %: 1000 design px is a shelf"
        );
        let column = f32::from(frame(&mut rig).shelf_width);
        let titlebar = f32::from(frame(&mut rig).titlebar);
        // Every frame of the change, from the shelf at 1000 to the spine at 850 design px.
        rig.cx
            .simulate_resize(gpui::size(px(850.0 * scale), px(900.0)));
        rig.draw();
        let mut film: Vec<Vec<gpui::PaintedText>> = Vec::new();
        for _ in 0..60 {
            rig.frame(16);
            rig.repaint();
            film.push(rig.cx.update(|window, _| window.painted_texts().to_vec()));
        }
        assert_eq!(frame(&mut rig).shelf, ShelfMode::Spine);
        // The shelf's words: painted inside the shelf column, below the titlebar, whole at the start.
        let in_column = |text: &gpui::PaintedText| {
            f32::from(text.bounds.right()) <= column + 1.0
                && f32::from(text.bounds.origin.y) > titlebar
        };
        let mut natural: std::collections::BTreeMap<(String, i32), f32> =
            std::collections::BTreeMap::new();
        for text in film.iter().flatten().filter(|text| in_column(text)) {
            let key = (
                text.text.to_string(),
                f32::from(text.bounds.origin.y).round() as i32,
            );
            let wide = natural.entry(key).or_insert(0.0);
            *wide = wide.max(f32::from(text.bounds.size.width));
        }
        assert!(
            !natural.is_empty(),
            "{percent} %: the shelf drew no words in its column"
        );
        for (key, whole) in &natural {
            let mut lingering = 0;
            for (frame_at, texts) in film.iter().enumerate() {
                let Some(text) = texts.iter().find(|text| {
                    in_column(text)
                        && text.text.as_ref() == key.0
                        && f32::from(text.bounds.origin.y).round() as i32 == key.1
                }) else {
                    lingering = 0;
                    continue;
                };
                if text.alpha < 0.95 {
                    assert!(
                        f32::from(text.bounds.size.width) >= whole - 1.5,
                        "{percent} %: `{}` is cut mid-glyph ({:.0} of {whole:.0} px) while it fades (alpha {:.2}) at frame {frame_at}",
                        key.0,
                        f32::from(text.bounds.size.width),
                        text.alpha
                    );
                }
                lingering = if (0.02..0.8).contains(&text.alpha) {
                    lingering + 1
                } else {
                    0
                };
                assert!(
                    lingering <= 2,
                    "{percent} %: `{}` lingers in a fade ({lingering} frames under 0.8, alpha {:.2}) at frame {frame_at}",
                    key.0,
                    text.alpha
                );
            }
        }
    }
}

/// At 200 % text a 360 px phone is 180 design px of room. Nothing on the
/// screens this lane owns hangs past the window's edge there (a segmented
/// control stands its choices in a column when its row is wider than the
/// room; the graph's where-line wraps), and a caption
/// that has more words than the room holds wraps instead of being cut on both
/// sides (a text in a flex row is as wide as its one line unless it may
/// shrink). The rest of the findings are printed (`--nocapture`).
#[gpui::test]
fn at_200_percent_text_the_owned_screens_never_hang_past_a_phone(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(Route::Orbit(OrbitRoute::Home)), 1440.0, 900.0);
    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
    rig.go(Intent::ZoomTo {
        display,
        percent: 200,
    });
    for (name, route, settings) in screens() {
        if !matches!(name, "library" | "find" | "settings" | "graph") {
            continue;
        }
        rig.go(Intent::Navigate(route));
        if let Some(page) = settings {
            rig.go(Intent::OpenSettings(page));
        }
        for (width, height) in [(360.0, 900.0), (390.0, 844.0)] {
            resize(&mut rig, width, height);
            let ledger = painted(&mut rig);
            let found = findings(&ledger, width, height);
            for finding in &found {
                eprintln!("[fluid] {name} at {width}x{height}, 200 %: {finding}");
            }
            let hanging: Vec<_> = found
                .iter()
                .filter(|finding| matches!(finding.rule, "edge" | "stranded" | "offscreen"))
                .map(ToString::to_string)
                .collect();
            assert!(
                hanging.is_empty(),
                "{name} at {width}x{height}, 200 % text: {hanging:?}"
            );
            for text in ledger
                .texts
                .iter()
                .filter(|text| text.content.contains("declarations from"))
            {
                let b = &text.bounds;
                assert!(
                    b.x >= 0.0 && b.x + b.width <= width + 0.5,
                    "{name} at {width}x{height}, 200 %: the caption `{}` is {b:?}, outside the window",
                    text.content
                );
            }
        }
    }
}
