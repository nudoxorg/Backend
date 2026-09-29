//! W-Fluid: the shell from a 2560 px window down to a 320 px phone.
//!
//! Real windows at the phone sizes (320×568, 360×640, 390×844), read from the
//! probe ledger of the painted frame: what the shelf is, where the drawer
//! opens and how it closes, that the reader's first heading is on screen in
//! full and that nothing hangs past the window's edge. Fixtures are pinned;
//! nothing here reads live data.

#![allow(clippy::expect_used, clippy::panic, clippy::too_many_lines)]

use super::fit_tests::{findings, package_route, painted, resize};
use super::root::Shell;
use super::tests::{Rig, page_route, rig};
use super::ShelfMode;
use crate::navigation::{BrowseRoute, Intent, OrbitRoute, Route, SettingsPage};
use facet::probe::{Ledger, TextSample};
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

/// The six screens, by name.
fn screens() -> Vec<(&'static str, Route, Option<SettingsPage>)> {
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

/// At every phone size, on every screen the reader's first heading is on
/// screen in full, and nothing is drawn past the window's right edge.
#[gpui::test]
fn on_a_phone_every_screen_shows_its_heading_in_full_and_never_overflows(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(Route::Orbit(OrbitRoute::Home)), 1440.0, 900.0);
    for (name, route, settings) in screens() {
        rig.go(Intent::Navigate(route));
        if let Some(page) = settings {
            rig.go(Intent::OpenSettings(page));
        }
        for (width, height) in PHONES {
            resize(&mut rig, width, height);
            let titlebar = f32::from(frame(&mut rig).titlebar);
            let ledger = painted(&mut rig);
            let found = findings(&ledger, width, height);
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
            assert!(
                !found.iter().any(|finding| finding.what.contains(&heading.content) && finding.rule != "overlap"),
                "{name} at {width}x{height}: the heading `{}` is clipped: {found:?}",
                heading.content
            );
            // The pages this lane owns are clean outright.
            if matches!(name, "library" | "find" | "settings") {
                assert!(found.is_empty(), "{name} at {width}x{height}: {found:?}");
            }
        }
    }
}
