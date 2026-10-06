//! Height pressure through the mounted Shelf, including the compact 502×480
//! logical window corresponding to the live 1004×960 device capture. Native
//! wheel input uses the measured viewport, never a fabricated row-list box.

use super::ShelfHeightBudget;
use crate::core::PackageId;
use crate::model::pages::{DeclRef, Known, OutlineNode, OutlineTree, PageValue, ReadFailure, Standing, VersionEntry};
use crate::navigation::{Intent, PackageLane, PackageRoute, Route};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use crate::shell::{fit_tests, tests};
use backend_library::DeclarationKind;
use facet::probe::{BoundsSample, Ledger, ScrollSample};
use gpui::{Modifiers, TestAppContext, point, px, size};
use std::collections::HashSet;
use std::sync::Arc;

const NAME: &str = "package_with_a_deliberately_long_name_for_accessible_shelf_height";
const DECLARATIONS: usize = 32;

struct HeightFixture;

impl PageReader for HeightFixture {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        let ReadRequest::Package(package) = request else { return tests::Fixture.read(request, context); };
        let mut about = tests::registry_dossier(package);
        about.outline = Known::Known(OutlineTree {
            roots: (0..DECLARATIONS).map(|index| OutlineNode {
                decl: DeclRef::from_label(&format!("{}::lib.rs:{}::Declaration{index:02}", package.as_str(), index + 1),
                    None, Some(DeclarationKind::Struct), None).expect("fixture declaration"),
                children: Arc::from([]),
            }).collect::<Vec<_>>().into(),
            complete: true,
        });
        about.versions = Known::Known((0..8).map(|minor| {
            let version = format!("1.{minor}.0+long-build-metadata-for-the-package-context");
            VersionEntry {
                package: package.at(&version).expect("fixture release"),
                current: package.version() == Some(version.as_str()),
                version: version.into(), standing: Standing::Available,
            }
        }).collect::<Vec<_>>().into());
        Ok(PageValue::Package(about))
    }
}

fn route(name: &str) -> Route {
    Route::Package(PackageRoute {
        cargo: None, project: None,
        package: PackageId::new(&format!("pkg:cargo/{name}@1.0.0+long-build-metadata-for-the-package-context")).expect("fixture package"),
        lane: PackageLane::Overview, selected: None, at: None,
    })
}

fn open(cx: &mut TestAppContext) -> tests::Rig {
    let mut rig = tests::rig_with_reads(cx, Some(route(NAME)), 1440.0, 900.0,
        ReadPool::start(1, |_| HeightFixture).expect("fixture pool"));
    // All four Trail labels must fit or really scroll; long labels may use
    // an ellipsis while their full names remain on the native links.
    for index in 0..4 { rig.go(Intent::Navigate(route(&format!("previous_package_with_a_long_trail_label_{index}")))); }
    rig.go(Intent::Navigate(route(NAME)));
    rig.cx.update(|window, cx| { window.set_a11y_forced(true); facet::probe::enable(cx); });
    rig
}

fn resize(rig: &mut tests::Rig, width: f32, height: f32, percent: u16) {
    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
    rig.go(Intent::ZoomTo { display, percent });
    fit_tests::resize(rig, width, height);
    let (overlays, open) = rig.shell.read_with(rig.cx, |shell, cx| (
        shell.frame().expect("responsive frame").shelf_overlays,
        shell.chrome_words(cx).iter().any(|(key, value)| *key == "drawer" && value == "open"),
    ));
    if overlays && !open { rig.keys("secondary-\\"); }
}

fn scroll<'a>(ledger: &'a Ledger, key: &str) -> &'a ScrollSample {
    ledger.scrolls.iter().rev().find(|sample| sample.key == key).expect("mounted native scroll viewport")
}

fn wheel(rig: &mut tests::Rig, key: &str, delta: f32) -> Ledger {
    let ledger = fit_tests::painted(rig);
    let viewport = &scroll(&ledger, key).viewport;
    assert!(viewport.width > 0.0 && viewport.height > 0.0, "wheel needs real native bounds: {viewport:?}");
    crate::shell::tests::wheel(rig.cx, point(px(viewport.x + viewport.width * 0.5), px(viewport.y + viewport.height * 0.5)),
        point(px(0.0), px(delta)));
    rig.settle();
    fit_tests::painted(rig)
}

fn inside(inner: &BoundsSample, outer: &BoundsSample) -> bool {
    inner.x >= outer.x - 0.5 && inner.y >= outer.y - 0.5
        && inner.x + inner.width <= outer.x + outer.width + 0.5
        && inner.y + inner.height <= outer.y + outer.height + 0.5
}

fn expand_types(rig: &mut tests::Rig) {
    let mut fold = tests::native_bounds(rig, "Button", "Expand Types", true).expect("real Types disclosure");
    for _ in 0..4 {
        let ledger = fit_tests::painted(rig);
        let viewport = &scroll(&ledger, "shelf-rows").viewport;
        if f32::from(fold.top()) >= viewport.y && f32::from(fold.bottom()) <= viewport.y + viewport.height { break; }
        wheel(rig, "shelf-rows", -f32::from(fold.size.height));
        fold = tests::native_bounds(rig, "Button", "Expand Types", true).expect("native wheel reveals disclosure");
    }
    let ledger = fit_tests::painted(rig);
    let viewport = &scroll(&ledger, "shelf-rows").viewport;
    assert!(f32::from(fold.top()) >= viewport.y && f32::from(fold.bottom()) <= viewport.y + viewport.height,
        "the disclosure glyph/control must be fully inside the declaration viewport: {fold:?}, {viewport:?}");
    assert!(fold.size.width >= px(18.0), "the text-scaled disclosure has its own nonshrinking slot");
    rig.cx.simulate_click(fold.center(), Modifiers::none());
    rig.settle();
    let collapsed = tests::native_bounds(rig, "Button", "Collapse Types", true).expect("opened native disclosure");
    assert_eq!(collapsed.size.width, fold.size.width, "opening preserves the visible collapse slot");
}

#[test]
fn measured_budget_preserves_roomy_chrome_and_changes_continuously_under_pressure() {
    let roomy = ShelfHeightBudget::new(px(900.0), px(64.0), px(220.0), px(108.0), px(190.0));
    assert_eq!((roomy.package, roomy.controls, roomy.trail), (px(220.0), px(108.0), px(190.0)));
    let mut previous: Option<[gpui::Pixels; 4]> = None;
    for height in 1..901 {
        let budget = ShelfHeightBudget::new(px(height as f32), px(64.0), px(220.0), px(108.0), px(190.0));
        let parts = [budget.package, budget.controls, budget.rows, budget.trail];
        assert!(parts.into_iter().all(|part| part > px(0.0)), "nonempty chrome and rows each have a native viewport");
        assert!(budget.rows >= px(192.0).min(px(height as f32 * 0.5)) - px(0.01));
        assert!((f32::from(parts.into_iter().sum::<gpui::Pixels>()) - height as f32).abs() < 0.01);
        if let Some(previous) = previous {
            for (old, new) in previous.into_iter().zip(parts) {
                assert!((f32::from(new - old)).abs() <= 1.01, "one-pixel resizing has no mode jump");
            }
        }
        previous = Some(parts);
    }
}

#[gpui::test]
fn real_shelf_keeps_declarations_and_native_wheel_at_short_heights_and_all_text_scales(cx: &mut TestAppContext) {
    for percent in [100, 150, 200] {
        for (width, height) in [(502.0, 480.0), (502.0, 360.0), (1920.0, 480.0)] {
            let mut rig = open(cx);
            resize(&mut rig, width, height, percent);
            if width == 1920.0 {
                assert!(!rig.shell.read_with(rig.cx, |shell, _| shell.frame().expect("wide frame").shelf_overlays),
                    "the wide case mounts the full docked Shelf, including at 200% text");
            }
            let ledger = fit_tests::painted(&mut rig);
            let rows = &scroll(&ledger, "shelf-rows").viewport;
            let total: f32 = ["shelf-package-viewport", "shelf-controls-viewport", "shelf-row-viewport", "shelf-trail-viewport"]
                .into_iter().map(|key| ledger.bounds(key).expect("current-frame section").height).sum();
            let row = rig.cx.update(|_, cx| {
                use facet::ActiveFacet as _;
                let measure = facet::Measure::new(px(rows.width), &cx.facet());
                f32::from(measure.row() + measure.space(facet::Space::Tight))
            });
            assert!(rows.height + 0.5 >= (row * 3.0).min(total * 0.5),
                "{width}×{height}/{percent}% protects a useful declaration viewport: {rows:?}");
            expand_types(&mut rig);
            let before = fit_tests::painted(&mut rig);
            let before = scroll(&before, "shelf-rows").offset.expect("measured list offset").y;
            let after = wheel(&mut rig, "shelf-rows", -row * 2.0);
            assert!(scroll(&after, "shelf-rows").offset.expect("native wheel offset").y < before - row * 0.5,
                "actual wheel inside declaration bounds must move the list at {width}×{height}/{percent}%");
            assert!(after.texts.iter().any(|text| text.key.starts_with("shelf-row:")
                && text.content.starts_with("Declaration") && inside(&text.bounds, &scroll(&after, "shelf-rows").viewport)),
                "at least one complete declaration name really paints after native wheel");
            for lens in ["Contents", "Versions", "Rests on", "Used by"] {
                assert!(tests::native_bounds(&mut rig, "Tab", lens, true).is_some(), "full semantic tab name survives compact text: {lens}");
            }
        }
    }
}

#[gpui::test]
fn compact_two_hundred_percent_reaches_every_declaration_and_scrolls_chrome_independently(cx: &mut TestAppContext) {
    let mut rig = open(cx);
    resize(&mut rig, 502.0, 480.0, 200);
    expand_types(&mut rig);
    let mut seen = HashSet::new();
    for _ in 0..80 {
        let ledger = fit_tests::painted(&mut rig);
        let viewport = &scroll(&ledger, "shelf-rows").viewport;
        let unobscured_top = rig.cx.debug_bounds("shelf-sticky-clip")
            .map_or(viewport.y, |clip| f32::from(clip.bottom()));
        for text in &ledger.texts {
            if text.key.starts_with("shelf-row:") && text.content.starts_with("Declaration")
                && inside(&text.bounds, viewport) && text.bounds.y >= unobscured_top - 0.5 {
                seen.insert(text.content.to_string());
                let native = tests::native_bounds(&mut rig, "Button", &format!("{}, lib", text.content), true)
                    .or_else(|| tests::native_bounds(&mut rig, "Button", &text.content, true));
                assert!(native.is_some(), "a painted declaration has its real native action");
            }
        }
        if seen.len() == DECLARATIONS { break; }
        wheel(&mut rig, "shelf-rows", -36.0);
    }
    assert_eq!(seen.len(), DECLARATIONS, "every declaration must be paintable by native wheel in the actual list viewport: {seen:?}");
    let baseline = fit_tests::painted(&mut rig);
    let rows_offset = scroll(&baseline, "shelf-rows").offset.expect("row offset").y;
    for key in ["shelf-package", "shelf-trail"] {
        let before = fit_tests::painted(&mut rig);
        let chrome = scroll(&before, key);
        assert!(chrome.content.height > chrome.viewport.height + 1.0, "long {key} must have a real bounded scroller: {chrome:?}");
        let old = chrome.offset.expect("native chrome offset").y;
        let after = wheel(&mut rig, key, -120.0);
        assert!(scroll(&after, key).offset.expect("chrome moved").y < old - 1.0, "native wheel really scrolls {key}");
        assert!((scroll(&after, "shelf-rows").offset.expect("row owner").y - rows_offset).abs() < 0.5,
            "scrolling surrounding chrome cannot move the declarations");
    }
    let controls = wheel(&mut rig, "shelf-controls", -600.0);
    assert!(controls.texts.iter().any(|text| text.content == "Type to narrow"
        && text.scroll_ancestors.iter().any(|key| key == "shelf-controls")
        && inside(&text.bounds, &scroll(&controls, "shelf-controls").viewport)),
        "the full text-scaled filter line is actually reachable in its bounded native viewport");
    wheel(&mut rig, "shelf-trail", 600.0);
    let mut trail_seen = HashSet::new();
    for _ in 0..80 {
        let ledger = fit_tests::painted(&mut rig);
        let viewport = &scroll(&ledger, "shelf-trail").viewport;
        for text in &ledger.texts {
            if text.content.starts_with("previous_package_with_a_long_trail_label_")
                && text.scroll_ancestors.iter().any(|key| key == "shelf-trail") && inside(&text.bounds, viewport) {
                let link = tests::native_bounds(&mut rig, "Link", &text.content, true)
                    .expect("each reachable Trail label retains its full native link name and action");
                assert!(f32::from(link.top()) >= viewport.y - 0.5
                    && f32::from(link.bottom()) <= viewport.y + viewport.height + 0.5,
                    "the complete native Trail hitbox is reachable through its own scroller: {link:?}, {viewport:?}");
                assert!(text.paint_clip.as_ref().is_some_and(|clip| inside(&text.bounds, clip)),
                    "the label's actual renderer mask admits its full measured ellipsis box");
                assert_eq!(text.overflow, facet::probe::TextOverflow::Ellipsis,
                    "long full accessible names have explicit visible ellipsis");
                trail_seen.insert(text.content.clone());
            }
        }
        if trail_seen.len() == 4 { break; }
        wheel(&mut rig, "shelf-trail", -4.0);
    }
    assert_eq!(trail_seen.len(), 4, "all four long Trail links must really paint by native wheel: {trail_seen:?}");
    let after = fit_tests::painted(&mut rig);
    assert!((scroll(&after, "shelf-rows").offset.expect("row owner").y - rows_offset).abs() < 0.5,
        "reaching all view controls and Trail links preserves the declaration scroll owner");
    // Return through native wheel to the real disclosure, then collapse.
    // Descendants must disappear, rather than staying in an old list cache.
    wheel(&mut rig, "shelf-rows", 10_000.0);
    let collapse = tests::native_bounds(&mut rig, "Button", "Collapse Types", true)
        .expect("native wheel reaches the collapse control");
    let ledger = fit_tests::painted(&mut rig);
    let viewport = &scroll(&ledger, "shelf-rows").viewport;
    assert!(f32::from(collapse.top()) >= viewport.y - 0.5
        && f32::from(collapse.bottom()) <= viewport.y + viewport.height + 0.5);
    rig.cx.simulate_click(collapse.center(), Modifiers::none());
    rig.settle();
    assert!(tests::native_bounds(&mut rig, "Button", "Expand Types", true).is_some());
    assert!(!fit_tests::painted(&mut rig).texts.iter().any(|text|
        text.key.starts_with("shelf-row:") && text.content.starts_with("Declaration")),
        "collapsing removes all old descendants from the painted native list");
}

#[gpui::test]
fn resizing_the_mounted_wide_shelf_preserves_the_selected_key_native_handle_and_anchor(cx: &mut TestAppContext) {
    let mut rig = open(cx);
    resize(&mut rig, 1920.0, 600.0, 200);
    // The parent row takes Shelf focus without navigating away from the
    // fixture package. Keyboard walking selects declarations in the list.
    let types = tests::native_bounds(&mut rig, "Button", "Types", true).expect("Types row");
    rig.cx.simulate_click(types.center(), Modifiers::none());
    rig.settle();
    rig.keys("down down down down down");
    let shelf = rig.shell.read_with(rig.cx, |shell, _| shell.shelf_entity());
    let selected = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    let focused = shelf.read_with(rig.cx, |shelf, _| shelf.layout.focused_in_list.clone()).expect("native selected row handle");
    let top = shelf.read_with(rig.cx, |shelf, _| shelf.layout.scroll.logical_scroll_top());
    for height in (360..=600).rev().step_by(7) {
        rig.cx.simulate_resize(size(px(1920.0), px(height as f32)));
        rig.repaint(); // inspect the first frame, without settling an old geometry
        assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx)), selected);
        let current = shelf.read_with(rig.cx, |shelf, _| shelf.layout.focused_in_list.clone()).expect("selected native handle remains mounted");
        assert_eq!(current, focused, "geometry changes cannot replace the native focus identity");
        let current_top = shelf.read_with(rig.cx, |shelf, _| shelf.layout.scroll.logical_scroll_top());
        assert_eq!(current_top.item_ix, top.item_ix, "continuous height changes retain the logical row anchor");
        assert!((f32::from(current_top.offset_in_item - top.offset_in_item)).abs() < 0.5,
            "continuous height changes retain the in-row fraction");
        let viewport = shelf.read_with(rig.cx, |shelf, _| shelf.viewport());
        let covered = shelf.read_with(rig.cx, |shelf, _| shelf.diagnostic_sticky_covered());
        let row_height = shelf.read_with(rig.cx, |shelf, _| shelf.diagnostic_row_height());
        assert!(covered <= viewport.size.height - row_height, "pinned ancestors leave an ordinary row usable");
    }
}
