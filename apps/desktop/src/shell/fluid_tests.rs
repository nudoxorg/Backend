//! W-Fluid: the shell from a 2560 px window down to a 320 px phone.
//!
//! Real windows at the phone sizes (320×568, 360×640, 390×844), read from the
//! probe ledger of the painted frame: what the shelf is, where the drawer
//! opens and how it closes, that the reader's first heading is on screen in
//! full and that nothing hangs past the window's edge. Fixtures are pinned;
//! nothing here reads live data.

#![allow(clippy::expect_used, clippy::panic, clippy::too_many_lines)]

use super::fit_tests::{findings, package_route, painted, resize};
use super::focus::Zone;
use super::root::Shell;
use super::tests::{Rig, page_route, rig, view_route};
use super::ShelfMode;
use crate::navigation::{BrowseRoute, Intent, OrbitRoute, Route, SettingsPage, View};
use facet::probe::{Ledger, TextSample};
use facet::probe::StackPhase;
use facet::tokens::fluid::Dock;
use gpui::{Modifiers, TestAppContext, point, px};

/// The phone sizes the brief names.
const PHONES: [(f32, f32); 3] = [(320.0, 568.0), (360.0, 640.0), (390.0, 844.0)];

fn frame(rig: &mut Rig) -> super::Frame {
    rig.shell.read_with(rig.cx, |shell: &Shell, _| shell.frame()).expect("frame")
}

/// A shelf row's words are on screen (the shelf is open, inline or over).
fn shelf_shown(ledger: &Ledger) -> bool {
    ledger.texts.iter().any(|text| text.content == "glyph" && text.key.starts_with("shelf-row:"))
}

fn toggle(ledger: &Ledger) -> facet::probe::BoundsSample {
    ledger.targets.iter().find(|target| target.key == "tb-shelf").expect("the shelf toggle").bounds.clone()
}

fn click(rig: &mut Rig, x: f32, y: f32) {
    rig.cx.simulate_click(point(px(x), px(y)), Modifiers::default());
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
        ("settings", Route::Orbit(OrbitRoute::Home), Some(SettingsPage::Appearance)),
    ]
}

/// The largest text below the titlebar: the reader's first heading.
fn first_heading(ledger: &Ledger, titlebar: f32) -> Option<&TextSample> {
    ledger
        .texts
        .iter()
        .filter(|text| text.bounds.y >= titlebar && !text.content.trim().is_empty() && !text.key.starts_with("shelf-row:"))
        .max_by(|a, b| a.size.total_cmp(&b.size).then(b.bounds.y.total_cmp(&a.bounds.y)))
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
        assert!(!shelf_shown(&ledger), "at {width} px the shelf is closed until it is asked for");
        let button = toggle(&ledger);
        assert!(button.within(width, height) && button.width >= 24.0 && button.height >= 24.0, "the toggle at {width} px is {button:?}");
        let (bx, by) = (button.x + button.width / 2.0, button.y + button.height / 2.0);

        // Opened by the titlebar button.
        click(&mut rig, bx, by);
        let open = painted(&mut rig);
        let row = open
            .texts
            .iter()
            .find(|text| text.content == "glyph" && text.key.starts_with("shelf-row:"))
            .unwrap_or_else(|| panic!("the toggle opened no drawer at {width} px"));
        let drawer = f32::from(frame(&mut rig).drawer);
        assert!(row.bounds.x >= 0.0 && row.bounds.x + row.bounds.width <= drawer, "the row {:?} is not inside the {drawer} px drawer at {width} px", row.bounds);
        assert!(drawer <= width - 47.0, "the drawer covers the page at {width} px: {drawer}");

        // Put away by a click on the strip of page beside it (the scrim).
        click(&mut rig, width - 10.0, height / 2.0);
        assert!(!shelf_shown(&painted(&mut rig)), "a click on the scrim left the drawer open at {width} px");

        // Put away by Escape.
        click(&mut rig, bx, by);
        assert!(shelf_shown(&painted(&mut rig)), "the drawer did not reopen at {width} px");
        rig.keys("escape");
        assert!(!shelf_shown(&painted(&mut rig)), "Escape left the drawer open at {width} px");

        // Put away by moving to another page.
        click(&mut rig, bx, by);
        assert!(shelf_shown(&painted(&mut rig)));
        rig.go(Intent::Navigate(package_route()));
        assert!(!shelf_shown(&painted(&mut rig)), "a route change left the drawer open at {width} px");
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
                eprintln!("[fluid] {name} at {width}x{height}: {} findings (a page lane's)", found.len());
                for finding in &found {
                    eprintln!("[fluid]     {finding}");
                }
                continue;
            }
            // Nothing hangs past the window, whatever else a page still owes.
            let overflow: Vec<_> = found.iter().filter(|finding| finding.rule == "edge").map(ToString::to_string).collect();
            assert!(overflow.is_empty(), "{name} at {width}x{height}: {overflow:?}");
            let heading = first_heading(&ledger, titlebar).unwrap_or_else(|| panic!("{name} at {width}x{height} draws no text"));
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
    first_heading(&ledger, titlebar).expect("a heading").bounds.x
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
    assert!(before - after > 150.0, "the column narrowed from {before} to {after}: a spine is 42 px");
    let between = xs.iter().filter(|x| **x < before - 10.0 && **x > after + 10.0).count();
    assert!(between >= 5, "only {between} frames between the shelf and the spine: {xs:?}");
    let biggest = xs.windows(2).map(|pair| (pair[0] - pair[1]).abs()).fold(0.0_f32, f32::max);
    assert!(biggest < 0.5 * (before - after), "one frame moved the page {biggest} px of {}: {xs:?}", before - after);
    assert!(xs.windows(2).all(|pair| pair[1] <= pair[0] + 0.01), "the page moved back on its way: {xs:?}");
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
    assert_eq!(shelf_at(&mut rig, 890.0), ShelfMode::Shelf, "inside the band on the way down: held");
    assert_eq!(shelf_at(&mut rig, 870.0), ShelfMode::Spine, "past it: a spine");
    assert_eq!(shelf_at(&mut rig, 910.0), ShelfMode::Spine, "inside the band on the way up: held");
    for pass in 0..12 {
        let width = 900.0 + if pass % 2 == 0 { -10.0 } else { 10.0 };
        assert_eq!(shelf_at(&mut rig, width), ShelfMode::Spine, "flipped at {width}");
    }
    assert_eq!(shelf_at(&mut rig, 920.0), ShelfMode::Shelf, "past the band: a shelf");
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
    assert!(first_b.is_none(), "frame {first_b:?} painted the skipped page `{b}`: {:?}", first_b.map(|at| &frames[at]));
    assert!(frames.iter().any(|frame| shows(frame, c)), "the last route `{c}` never arrived");
    // The page it left is on screen until it has gone: the change plays from A, not from nothing.
    assert!(frames.iter().take(3).any(|frame| shows(frame, a)), "the change did not start from `{a}`: {:?}", &frames[..3]);
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
    for (width, height, plate) in [(1440.0_f32, 900.0_f32, 440.0_f32), (800.0, 600.0, 344.0), (360.0, 640.0, 360.0)] {
        resize(&mut rig, width, height);
        rig.keys("cmd-k");
        rig.keys("r e l a t i o n");
        rig.settle();
        let titlebar = f32::from(frame(&mut rig).titlebar);
        let _ = painted(&mut rig);
        let words: Vec<gpui::PaintedText> = rig.cx.update(|window, _| window.painted_texts().to_vec());
        // The query is drawn in the titlebar.
        let query = words
            .iter()
            .find(|text| text.text.as_ref() == "relation" && f32::from(text.bounds.origin.y) < titlebar)
            .unwrap_or_else(|| panic!("the typed query is not drawn in the titlebar at {width} px: {:?}", words.iter().map(|t| t.text.to_string()).collect::<Vec<_>>()));
        assert!(f32::from(query.bounds.right()) <= width, "the query overflows the window at {width} px: {:?}", query.bounds);
        // The plate is the width the token says (a sheet across the window on a phone),
        // starts under the titlebar at the window's left edge and runs to the foot.
        let bounds = rig
            .cx
            .debug_bounds("ask-plate")
            .unwrap_or_else(|| panic!("Ask draws no plate at {width} px"));
        assert!(
            (f32::from(bounds.size.width) - plate).abs() < 1.0 && f32::from(bounds.origin.x).abs() < 0.5 && (f32::from(bounds.origin.y) - titlebar).abs() < 0.5,
            "the plate at {width} px is {bounds:?}, want {plate} wide at (0, {titlebar})"
        );
        // A result row is drawn below it, over the shelf's column, inside the plate.
        let row = words
            .iter()
            .find(|text| text.text.contains("RelationLabel") && f32::from(text.bounds.origin.y) > titlebar && f32::from(text.bounds.origin.x) < plate)
            .unwrap_or_else(|| panic!("no result row for `relation` at {width} px: {:?}", words.iter().map(|t| t.text.to_string()).collect::<Vec<_>>()));
        assert!(
            f32::from(row.bounds.right()) <= plate + 0.5 && f32::from(row.bounds.right()) <= width,
            "the result row {:?} is outside the {plate} px plate at {width} px",
            row.bounds
        );
        rig.keys("escape");
        let _ = painted(&mut rig);
        let after: Vec<gpui::PaintedText> = rig.cx.update(|window, _| window.painted_texts().to_vec());
        assert!(!after.iter().any(|text| text.text.as_ref() == "relation" && f32::from(text.bounds.origin.y) < titlebar), "Escape left the query in the titlebar at {width} px");
    }
}

/// This checks the mounted page's measured hero, not a second copy of the
/// layout formula. Ask and Reader must agree on the plate's occupied pixels
/// while the window, text scale, and inline shelf move underneath them.
#[gpui::test]
fn ask_preview_stays_beside_its_plate_through_zoom_and_resize(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    rig.keys("cmd-k");
    rig.keys("r e l a t i o n");
    rig.settle();
    rig.keys("down");
    rig.settle();
    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
    for percent in [100_u16, 150, 200] {
        rig.go(Intent::ZoomTo { display: display.clone(), percent });
        resize(&mut rig, 1440.0, 900.0);
        assert_ask_geometry(&mut rig, true);
    }
    rig.go(Intent::ZoomTo { display, percent: 100 });
    for width in [800.0, 640.0, 360.0, 1440.0] {
        rig.cx.simulate_resize(gpui::size(px(width), px(900.0)));
        rig.draw();
        assert_ask_geometry(&mut rig, false);
        rig.settle();
        assert_ask_geometry(&mut rig, true);
        let plate = rig.cx.debug_bounds("ask-plate").expect("Ask plate");
        if width <= 640.0 {
            assert!((f32::from(plate.size.width) - width).abs() < 1.0,
                "{width} px cannot fit a readable preview beside Ask: {plate:?}");
        }
    }
}

fn assert_ask_geometry(rig: &mut Rig, settled: bool) {
    let ledger = painted(rig);
    let plate = rig.cx.debug_bounds("ask-plate").expect("Ask plate");
    let viewport_width = rig.cx.update(|window, _| f32::from(window.viewport_size().width));
    if f32::from(plate.size.width) >= viewport_width - 0.5 {
        assert!((f32::from(plate.right()) - viewport_width).abs() < 1.0,
            "Ask sheet must cover the whole Reader: {plate:?}");
        return;
    }
    let hero = ledger.texts.iter()
        .find(|text| text.key.starts_with("name:0:") && text.content == "RelationLabel");
    if settled { assert!(hero.is_some(), "the settled Reader drew no symbol hero beside Ask"); }
    if let Some(hero) = hero {
        assert!(hero.bounds.x >= f32::from(plate.right()) + 8.0,
            "Reader hero {:?} crosses Ask plate {plate:?}", hero.bounds);
    }
}

#[gpui::test]
fn a_sheet_removes_a_previously_mounted_reader_action_from_accesskit(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(view_route("RelationLabel", View::Code)), 360.0, 640.0);
    rig.settle();
    let native = |rig: &mut Rig| {
        rig.repaint();
        let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native AccessKit tree");
        serde_json::from_str::<serde_json::Value>(&json).expect("native tree JSON")
    };
    let has_jump = |tree: &serde_json::Value| tree["nodes"].as_object().expect("native nodes")
        .values().any(|node| node["aria"]["role"].as_str() == Some("Button")
            && node["aria"]["label"].as_str() == Some("Go to line")
            && node["aria"]["on_action"].as_array().is_some_and(|actions|
                actions.iter().any(|action| action.as_str() == Some("Click"))));
    assert!(has_jump(&native(&mut rig)), "the Code Reader must positively mount a native action before Ask");
    rig.keys("cmd-k");
    rig.keys("r e l a t i o n");
    rig.settle();
    let plate = rig.cx.debug_bounds("ask-plate").expect("Ask sheet");
    assert!((f32::from(plate.size.width) - 360.0).abs() < 1.0);
    assert!(!has_jump(&native(&mut rig)), "the covered Reader action remains in the native modal tree");
    rig.cx.simulate_keystrokes("escape");
    rig.frame(0);
    assert!(rig.cx.debug_bounds("ask-plate").is_some(), "the sheet should have painted exit pixels");
    assert!(!has_jump(&native(&mut rig)), "the Reader action returned beneath a departing sheet");
    rig.settle();
    assert!(has_jump(&native(&mut rig)), "the Reader action did not return after the sheet cleared");
}

#[gpui::test]
fn find_claims_native_query_focus_only_after_ask_exit_and_its_page_settle(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(Route::Orbit(OrbitRoute::Home)), 1440.0, 900.0);
    let query = crate::model::pages::SearchQuery::new("RelationLabel", 200).expect("query");
    let destination = Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(query)));
    let native_focus = |rig: &mut Rig| {
        rig.cx.update(|window, _| window.set_a11y_forced(true));
        rig.repaint();
        let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native AccessKit tree");
        let tree: serde_json::Value = serde_json::from_str(&json).expect("native tree JSON");
        let id = tree["accesskit_focus"].as_str().expect("native focus id");
        (id.to_owned(), tree["nodes"][id]["aria"]["label"].as_str().map(str::to_owned))
    };

    rig.keys("cmd-k");
    rig.graph.root.update(rig.cx, |root, cx| root.queue(Intent::Navigate(destination.clone()), cx));
    rig.frame(0);
    assert_eq!(rig.route(), destination);
    assert_eq!(ask_phase(&painted(&mut rig)), Some(StackPhase::Leaving), "the Ask exit must still own painted pixels");
    assert_ne!(native_focus(&mut rig).1.as_deref(), Some("Find query"),
        "the arriving Find field claimed native focus behind the leaving Ask");

    rig.settle();
    assert_eq!(ask_phase(&painted(&mut rig)), None);
    assert_eq!(native_focus(&mut rig).1.as_deref(), Some("Find query"),
        "the query never claimed focus after Ask and the Reader both settled");
    rig.cx.update(|window, cx| window.focus_next(cx));
    let away = native_focus(&mut rig);
    assert_ne!(away.1.as_deref(), Some("Find query"), "native focus did not leave the query");
    rig.repaint();
    assert_eq!(native_focus(&mut rig), away, "a Find redraw stole native focus back to its query");
}

#[gpui::test]
fn find_waits_for_its_own_arrival_and_releases_focus_on_departure(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(Route::Orbit(OrbitRoute::Home)), 1440.0, 900.0);
    let query = crate::model::pages::SearchQuery::new("RelationLabel", 200).expect("query");
    let destination = Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(query)));
    let native_focus = |rig: &mut Rig| {
        rig.cx.update(|window, _| window.set_a11y_forced(true));
        rig.repaint();
        let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native AccessKit tree");
        let tree: serde_json::Value = serde_json::from_str(&json).expect("native tree JSON");
        let id = tree["accesskit_focus"].as_str().expect("native focus id");
        tree["nodes"][id]["aria"]["label"].as_str().map(str::to_owned)
    };
    let settled = |rig: &mut Rig| rig.shell.read_with(rig.cx, |shell, cx|
        shell.reader_entity().read(cx).native_motion_settled());

    rig.graph.root.update(rig.cx, |root, cx| root.queue(Intent::Navigate(destination.clone()), cx));
    rig.frame(0);
    assert_eq!(rig.route(), destination);
    assert!(!settled(&mut rig), "the immediate arrival must still have a pending or painted Reader transition");
    assert_ne!(native_focus(&mut rig).as_deref(), Some("Find query"),
        "Find took native focus before its own page settled");
    rig.settle();
    assert!(settled(&mut rig));
    assert_eq!(native_focus(&mut rig).as_deref(), Some("Find query"),
        "the settled page failed to transfer native focus to its query");

    rig.graph.root.update(rig.cx, |root, cx| root.queue(Intent::Navigate(Route::Orbit(OrbitRoute::Home)), cx));
    rig.frame(0);
    assert!(!settled(&mut rig), "the departing Find must retain a painted Reader transition");
    assert_ne!(native_focus(&mut rig).as_deref(), Some("Find query"),
        "a departing Find retained native keyboard focus");
}

/// Real resize events hold one placement around the readable-width edge,
/// then change it once in each direction. The sampled plate may still be
/// moving; this checks its settled, painted rectangle at each stop.
#[gpui::test]
fn ask_panel_and_sheet_hold_through_oscillating_resizes(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 800.0, 700.0);
    rig.keys("cmd-k");
    rig.keys("r e l a t i o n");
    rig.settle();
    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
    for percent in [100_u16, 150, 200] {
        let scale = f32::from(percent) / 100.0;
        rig.go(Intent::ZoomTo { display: display.clone(), percent });
        let mut sheets = Vec::new();
        for design_width in [800.0, 700.0, 720.0, 670.0, 685.0, 700.0, 740.0] {
            let width = design_width * scale;
            resize(&mut rig, width, 700.0);
            let plate = rig.cx.debug_bounds("ask-plate").expect("Ask plate");
            sheets.push((f32::from(plate.size.width) - width).abs() < 1.0);
        }
        assert_eq!(sheets, [false, false, false, true, true, true, false],
            "{percent}% text alternated panel and sheet across the resize band");
        assert_eq!(sheets.windows(2).filter(|pair| pair[0] != pair[1]).count(), 2,
            "{percent}% text changed placement more than once in each direction");
    }
    rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
    rig.cx.simulate_resize(gpui::size(px(670.0 * 2.0), px(700.0)));
    rig.draw();
    let plate = rig.cx.debug_bounds("ask-plate").expect("reduced-motion sheet");
    assert!((f32::from(plate.size.width) - 1340.0).abs() < 1.0,
        "reduced motion settles the occupied rectangle in the resize frame");
}

fn ask_phase(ledger: &Ledger) -> Option<StackPhase> {
    ledger.stacks.iter().rev().flat_map(|stack| stack.entries.iter())
        .find(|entry| entry.key == "ask-plate" || entry.key == "ask-field" || entry.key == "ask-veil")
        .map(|entry| entry.phase)
}

fn has_native_ask_results(rig: &mut Rig) -> bool {
    rig.repaint();
    let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native AccessKit tree");
    let tree: serde_json::Value = serde_json::from_str(&json).expect("native tree JSON");
    tree["nodes"].as_object().expect("native nodes").values().any(|node|
        node["aria"]["label"].as_str() == Some("Search results"))
}

/// A result can be painted through the moving plate, but its clipped rows
/// cannot become native or keyboard actions until the full row is revealed.
#[gpui::test]
fn ask_entering_results_remain_inert_until_the_plate_exposes_them(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    rig.cx.simulate_keystrokes("cmd-k");
    rig.frame(16);
    rig.cx.simulate_keystrokes("r e l a t i o n");
    rig.frame(32);
    let plate = rig.cx.debug_bounds("ask-plate").expect("entering plate");
    assert!(f32::from(plate.size.width) > 1.0 && f32::from(plate.size.width) < 439.0,
        "the test must sample an actually clipped entry: {plate:?}");
    rig.frame(168);
    let plate = rig.cx.debug_bounds("ask-plate").expect("spring plate after veil tween");
    assert!(f32::from(plate.size.width) < 439.0, "the plate unexpectedly settled with the veil: {plate:?}");
    assert_eq!(ask_phase(&painted(&mut rig)), Some(StackPhase::Entering),
        "a settled veil mislabeled the still-moving plate Open");
    let native = |rig: &mut Rig| {
        rig.repaint();
        let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native Ask tree");
        serde_json::from_str::<serde_json::Value>(&json).expect("native tree JSON")
    };
    let actionable_links = |tree: &serde_json::Value| tree["nodes"].as_object().expect("native nodes")
        .values().filter(|node| node["aria"]["role"].as_str() == Some("Link")
            && node["aria"]["on_action"].as_array().is_some_and(|actions|
                actions.iter().any(|action| action.as_str() == Some("Click")))).count();
    assert_eq!(actionable_links(&native(&mut rig)), 0, "a clipped entering row registered a native action");
    rig.cx.simulate_keystrokes("tab");
    rig.frame(0);
    let tree = native(&mut rig);
    let focus = tree["accesskit_focus"].as_str().expect("native focus");
    assert_eq!(tree["nodes"][focus]["aria"]["role"].as_str(), Some("TextInput"),
        "Tab escaped the editor into a clipped result");
    rig.settle();
    assert_eq!(ask_phase(&painted(&mut rig)), Some(StackPhase::Open));
    assert!(actionable_links(&native(&mut rig)) > 0, "settled results never became actionable");
}

/// The modal's painted shell reverses from its current width. Results and
/// their native actions leave in the first closing frame, even while plate
/// pixels continue out, and a quick reopen starts at that painted width.
#[gpui::test]
fn ask_exit_is_inert_and_reopen_reverses_its_painted_plate(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    rig.keys("cmd-k");
    rig.keys("r e l a t i o n");
    assert!(has_native_ask_results(&mut rig), "the live Ask results must first exist in AccessKit");
    let opened = rig.cx.debug_bounds("ask-plate").expect("open plate");
    assert!((f32::from(opened.size.width) - 440.0).abs() < 1.0);

    rig.cx.simulate_keystrokes("escape");
    rig.frame(0);
    let leaving = painted(&mut rig);
    let first = rig.cx.debug_bounds("ask-plate").expect("painted exit plate");
    assert_eq!(ask_phase(&leaving), Some(StackPhase::Leaving));
    assert!(!has_native_ask_results(&mut rig), "exiting pixels retained stale native result actions");
    rig.frame(80);
    let narrowing = rig.cx.debug_bounds("ask-plate").expect("mid-exit plate");
    assert!(f32::from(narrowing.size.width) < f32::from(first.size.width) - 2.0);
    assert!(f32::from(narrowing.size.width) > 1.0);

    rig.cx.simulate_keystrokes("cmd-k");
    rig.frame(0);
    rig.cx.simulate_keystrokes("r e l a t i o n");
    rig.frame(0);
    let reversed = rig.cx.debug_bounds("ask-plate").expect("reopened plate");
    assert!((f32::from(reversed.size.width) - f32::from(narrowing.size.width)).abs() < 3.0,
        "reopen jumped rather than retargeted the painted plate: {narrowing:?} -> {reversed:?}");
    let mut widths = Vec::new();
    for _ in 0..10 {
        rig.frame(32);
        widths.push(f32::from(rig.cx.debug_bounds("ask-plate").expect("reopening plate").size.width));
    }
    assert!(widths.last().copied().unwrap_or_default() > f32::from(reversed.size.width) + 15.0,
        "reopened plate did not turn toward its target: {widths:?}");
    rig.cx.simulate_resize(gpui::size(px(800.0), px(900.0)));
    rig.frame(16);
    assert_ask_geometry(&mut rig, false);
    rig.settle();
    assert_ask_geometry(&mut rig, true);
    assert!(has_native_ask_results(&mut rig), "the reopened modal did not restore its live results");

    rig.cx.simulate_keystrokes("escape");
    rig.frame(0);
    rig.settle();
    let gone = painted(&mut rig);
    assert_eq!(ask_phase(&gone), None, "the exit kept scheduling or left a painted stack entry");
    assert!(rig.cx.debug_bounds("ask-plate").is_none(), "Ask left plate pixels after settling");
    assert!(!has_native_ask_results(&mut rig));

    rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
    rig.cx.simulate_keystrokes("cmd-k");
    rig.frame(0);
    rig.cx.simulate_keystrokes("r e l a t i o n");
    rig.frame(0);
    let reduced = rig.cx.debug_bounds("ask-plate").expect("reduced-motion Ask plate");
    assert!((f32::from(reduced.size.width) - 344.0).abs() < 1.0,
        "reduced motion did not place the whole panel in its first query frame");
    rig.cx.simulate_keystrokes("escape");
    rig.frame(0);
    let gone = painted(&mut rig);
    assert_eq!(ask_phase(&gone), None, "reduced motion kept an exit animation");
    assert!(rig.cx.debug_bounds("ask-plate").is_none());
}

/// The Code Reader's visible copy target stays selected across Ask, but its
/// keys cannot walk, activate, or change routes beneath a departing plate.
/// Once the final pixel clears, the same target and shortcuts work again.
#[gpui::test]
fn ask_exit_blocks_background_keyboard_until_its_sampled_scene_clears(cx: &mut TestAppContext) {
    let code = view_route("RelationLabel", View::Code);
    let mut rig = rig(cx, Some(code.clone()), 1440.0, 900.0);
    let visible = painted(&mut rig);
    assert!(visible.targets.iter().any(|target| target.key == "source-copy-excerpt"),
        "the Code Reader must positively paint the control used in this test");
    rig.keys("j");
    rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx).focus("source-copy-excerpt"));
    let selected = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert_eq!(selected, (Zone::Reader, Some("source-copy-excerpt".into())),
        "the keyboard did not stand on the visible Code control");
    rig.cx.write_to_clipboard(gpui::ClipboardItem::new_string("ask-exit-sentinel"));

    rig.keys("cmd-k");
    rig.keys("r e l a t i o n");
    rig.cx.simulate_keystrokes("escape");
    rig.frame(0);
    assert_eq!(ask_phase(&painted(&mut rig)), Some(StackPhase::Leaving));
    rig.cx.simulate_keystrokes("tab shift-tab j enter cmd-. ctrl-1");
    rig.frame(0);
    assert_eq!(rig.route(), code, "a background navigation key fired under the painted Ask exit");
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx)), selected,
        "Tab or J moved focus to a covered control");
    assert_eq!(rig.cx.read_from_clipboard().and_then(|item| item.text()).as_deref(), Some("ask-exit-sentinel"),
        "Enter activated the selected Code control beneath Ask's exit");

    rig.cx.simulate_keystrokes("cmd-k");
    rig.frame(0);
    assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().overlay()),
        Some(crate::navigation::Overlay::CommandPalette), "⌘K could not interrupt Ask's exit");
    rig.cx.simulate_keystrokes("escape");
    rig.frame(0);
    rig.settle();
    assert_eq!(ask_phase(&painted(&mut rig)), None);
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx)), selected,
        "the Code control lost its selected focus after the exit settled");
    rig.keys("enter");
    assert_ne!(rig.cx.read_from_clipboard().and_then(|item| item.text()).as_deref(), Some("ask-exit-sentinel"),
        "the Code control did not activate after Ask's plate cleared");
    rig.keys("cmd-.");
    assert_eq!(rig.route(), view_route("RelationLabel", View::Page),
        "source navigation did not resume after Ask's plate cleared");
}

#[gpui::test]
fn reader_clearance_follows_the_plate_that_was_painted_mid_flight(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 670.0, 700.0);
    rig.keys("cmd-k");
    rig.keys("r e l a t i o n");
    rig.settle();
    rig.keys("down");
    rig.settle();
    assert!((f32::from(rig.cx.debug_bounds("ask-plate").expect("sheet").size.width) - 670.0).abs() < 1.0);
    rig.cx.simulate_resize(gpui::size(px(800.0), px(700.0)));
    rig.draw();
    let mut widths = Vec::new();
    for _ in 0..40 {
        rig.frame(16);
        let ledger = painted(&mut rig);
        let plate = rig.cx.debug_bounds("ask-plate").expect("painted Ask plate");
        let width = f32::from(plate.size.width);
        widths.push(width);
        if let Some(hero) = ledger.texts.iter().find(|text|
            text.key.starts_with("name:0:") && text.content == "RelationLabel") {
            assert!(hero.bounds.x >= f32::from(plate.right()) + 8.0,
                "frame hero {:?} crossed its actually painted plate {plate:?}", hero.bounds);
        }
    }
    let between = widths.iter().filter(|width| **width > 350.0 && **width < 660.0).count();
    assert!(between >= 3, "plate skipped its moving widths: {widths:?}");
    rig.settle();
    let settled = rig.cx.debug_bounds("ask-plate").expect("settled Ask panel");
    assert!((f32::from(settled.size.width) - 344.0).abs() < 2.0,
        "panel did not settle at its FACET width after {widths:?}");
    assert_ask_geometry(&mut rig, true);
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
    assert!(worst <= 0.42 * 360.0 + 16.5, "the reader was squeezed: the heading sat at x = {worst} in a 360 px window: {xs:?}");
    assert!((xs[xs.len() - 1] - 16.0).abs() <= 2.0, "it settles at the phone's gutter, not {}", xs[xs.len() - 1]);
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
        assert_eq!(frame(&mut rig).shelf, ShelfMode::Shelf, "{percent} %: 1000 design px is a shelf");
        let column = f32::from(frame(&mut rig).shelf_width);
        let titlebar = f32::from(frame(&mut rig).titlebar);
        // Every frame of the change, from the shelf at 1000 to the spine at 850 design px.
        rig.cx.simulate_resize(gpui::size(px(850.0 * scale), px(900.0)));
        rig.draw();
        let mut film: Vec<Vec<gpui::PaintedText>> = Vec::new();
        for _ in 0..60 {
            rig.frame(16);
            rig.repaint();
            film.push(rig.cx.update(|window, _| window.painted_texts().to_vec()));
        }
        assert_eq!(frame(&mut rig).shelf, ShelfMode::Spine);
        // The shelf's words: painted inside the shelf column, below the titlebar, whole at the start.
        let in_column = |text: &gpui::PaintedText| f32::from(text.bounds.right()) <= column + 1.0 && f32::from(text.bounds.origin.y) > titlebar;
        let mut natural: std::collections::BTreeMap<(String, i32), f32> = std::collections::BTreeMap::new();
        for text in film.iter().flatten().filter(|text| in_column(text)) {
            let key = (text.text.to_string(), f32::from(text.bounds.origin.y).round() as i32);
            let wide = natural.entry(key).or_insert(0.0);
            *wide = wide.max(f32::from(text.bounds.size.width));
        }
        assert!(!natural.is_empty(), "{percent} %: the shelf drew no words in its column");
        for (key, whole) in &natural {
            let mut lingering = 0;
            for (frame_at, texts) in film.iter().enumerate() {
                let Some(text) = texts.iter().find(|text| in_column(text) && text.text.as_ref() == key.0 && f32::from(text.bounds.origin.y).round() as i32 == key.1) else {
                    lingering = 0;
                    continue;
                };
                if text.alpha < 0.95 {
                    assert!(f32::from(text.bounds.size.width) >= whole - 1.5, "{percent} %: `{}` is cut mid-glyph ({:.0} of {whole:.0} px) while it fades (alpha {:.2}) at frame {frame_at}", key.0, f32::from(text.bounds.size.width), text.alpha);
                }
                lingering = if (0.02..0.8).contains(&text.alpha) { lingering + 1 } else { 0 };
                assert!(lingering <= 2, "{percent} %: `{}` lingers in a fade ({lingering} frames under 0.8, alpha {:.2}) at frame {frame_at}", key.0, text.alpha);
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
    rig.go(Intent::ZoomTo { display, percent: 200 });
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
            let hanging: Vec<_> = found.iter().filter(|finding| matches!(finding.rule, "edge" | "stranded" | "offscreen")).map(ToString::to_string).collect();
            assert!(hanging.is_empty(), "{name} at {width}x{height}, 200 % text: {hanging:?}");
            for text in ledger.texts.iter().filter(|text| text.content.contains("declarations from")) {
                let b = &text.bounds;
                assert!(b.x >= 0.0 && b.x + b.width <= width + 0.5, "{name} at {width}x{height}, 200 %: the caption `{}` is {b:?}, outside the window", text.content);
            }
        }
    }
}
