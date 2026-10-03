//! Real installed fonts, mounted map paint, and native region bounds share
//! one immutable measurement while Reader room and text scale change.
use super::*;
use gpui::{
    AppContext as _, Context, InteractiveElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, TestAppContext, VisualTestContext, div, size,
};
use std::cell::{Cell, RefCell};

struct Board {
    modules: Rc<[ModuleFacts]>,
    width: f32,
    measured: Option<MeasuredMap>,
    observed: Rc<RefCell<Option<Rc<Layout>>>>,
    lit: bool,
    covered: bool,
    calls: Rc<Cell<usize>>,
}
impl Render for Board {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let measure = Measure::new(px(self.width), &cx.facet());
        let measured = self
            .measured
            .get_or_insert_with(|| measured(self.modules.clone(), &measure, window))
            .clone();
        let layout = measured.layout.clone();
        let modules = measured.modules.clone();
        let width = measured.measure.width();
        *self.observed.borrow_mut() = Some(layout.clone());
        let map = measured
            .shingles("measured-map")
            .rest(self.lit.then_some(Spot::Region(0)));
        // This is the host contract: the native leaves use the same Rc as
        // the painted map, with no separate text or region measurement.
        let mut body = div().relative().w(width).child(map);
        for (i, bounds) in layout.region_bounds().iter().enumerate() {
            let calls = self.calls.clone();
            let focus = cx.focus_handle();
            let target = crate::controls::button::native_button(
                div()
                    .id(("module", i))
                    .absolute()
                    .left(bounds.origin.x)
                    .top(bounds.origin.y)
                    .w(bounds.size.width)
                    .h(bounds.size.height)
                    .role(gpui::Role::Button)
                    .aria_label(modules[i].name.clone()),
                &focus,
                move |_, _| calls.set(calls.get() + 1),
            );
            body = body.child(target);
        }
        div().size_full().overflow_hidden().child(if self.covered {
            gpui::inert("covered-map", "Current modal owns input", body).into_any_element()
        } else {
            body.into_any_element()
        })
    }
}
fn modules(names: &[&str], count: usize) -> Rc<[ModuleFacts]> {
    names
        .iter()
        .map(|name| {
            ModuleFacts::new(
                name.to_string(),
                (0..count)
                    .map(|i| ShingleFacts {
                        name: format!("name_{i}").into(),
                        family: Family::Value,
                        yours: Use::Elsewhere,
                        state: None,
                    })
                    .collect(),
            )
        })
        .collect::<Vec<_>>()
        .into()
}
fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
}
fn init(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::fonts::install(cx).expect("the production bundled font cuts install");
        cx.set_global(gpui::TextTrace);
        crate::set_facet(
            crate::Facet {
                reduced_motion: true,
                ..crate::Facet::default()
            },
            cx,
        );
    });
}
// These fixture targets author four absolute lengths inside an origin-zero
// parent. GPUI snaps each metric to device pixels before layout (taffy.rs
// ToTaffy<AbsoluteLength>); their integer-device edges then survive the
// post-layout absolute-edge snap unchanged. Text room is NOT snapped.
fn native_authored_bounds(region: Bounds<Pixels>, device_scale: f32) -> Bounds<Pixels> {
    let metric = |value: Pixels| {
        let device = f32::from(value) * device_scale;
        px((device.abs() - 0.5).ceil().copysign(device) / device_scale)
    };
    Bounds::new(region.origin.map(metric), region.size.map(metric))
}

// Window::trace_text intersects the absolute bounds even when fully visible.
// Bounds::intersect -> from_corners stores (top + height) - top, rather
// than preserving the local shaped height. This is an exact f32 contract.
fn full_row_receipt_height(top: Pixels, shaped_height: Pixels) -> Pixels {
    (top + shaped_height) - top
}

fn assert_painted(cx: &mut VisualTestContext, layout: &Layout, words: &[&str]) {
    cx.update(|window, _| {
        let labels = layout.labels.as_ref().unwrap();
        for (i, words) in words.iter().enumerate() {
            let runs: Vec<_> = window
                .painted_texts()
                .iter()
                .filter(|run| run.text.as_ref() == *words)
                .collect();
            assert!(!runs.is_empty(), "actual native paint omitted {words}");
            let region = layout.region_bounds()[i];
            let native_region = native_authored_bounds(region, window.scale_factor());
            let label = &labels.entries[i];
            let painted_rows = px(labels.role.line * label.line_count() as f32);
            for run in runs {
                assert!(
                    run.bounds.left() >= region.left() && run.bounds.right() <= region.right(),
                    "{words} escapes the shared region horizontally"
                );
                assert!(
                    run.bounds.top() >= region.top()
                        && run.bounds.bottom() <= px(layout.regions[i].origin.1),
                    "native glyph rows must stay in the label band above the shingle grid"
                );
                assert_eq!(
                    run.bounds.size.height,
                    full_row_receipt_height(run.bounds.top(), painted_rows),
                    "actual native receipt must contain exactly the measured glyph rows after absolute-edge reconstruction"
                );
                assert!(
                    run.bounds.left() >= native_region.left()
                        && run.bounds.right() <= native_region.right()
                        && run.bounds.top() >= native_region.top()
                        && run.bounds.bottom() <= native_region.bottom(),
                    "visible native text must remain within the accepted device-snapped target"
                );
                assert!(run.alpha > 0.);
            }
            let node = window
                .a11y_tree()
                .unwrap()
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some(*words) && node.role() == gpui::Role::Button)
                .map(|(id, _)| *id)
                .expect("current native region");
            assert_eq!(
                window.a11y_node_bounds(node).unwrap(),
                native_region,
                "native target must exactly obey GPUI authored-metric/device snapping"
            );
        }
    });
}
#[gpui::test]
fn compact_lib_remains_one_native_row_across_shelf_room_and_text_scale(cx: &mut TestAppContext) {
    init(cx);
    let observed = Rc::new(RefCell::new(None));
    let (board, cx) = cx.add_window_view(|window, _| {
        window.set_a11y_forced(true);
        Board {
            modules: modules(&["lib", "cadence"], 1),
            width: 500.,
            measured: None,
            observed: observed.clone(),
            lit: false,
            covered: false,
            calls: Rc::new(Cell::new(0)),
        }
    });
    cx.simulate_resize(size(px(2400.), px(1400.)));
    for device_scale in [1., 2.] {
        cx.update(|window, _| window.set_scale_factor(device_scale));
        for scale in [1., 2., 2.5] {
            cx.update(|_, cx| {
                crate::set_facet(
                    crate::Facet {
                        text_scale: scale,
                        reduced_motion: true,
                        ..crate::Facet::default()
                    },
                    cx,
                )
            });
            // Include a continuous sweep: the original loss depends on f32
            // fluid sizing, not just a narrow/wide breakpoint.
            for width in [360., 500., 1200., 1863., 1440.]
                .into_iter()
                .chain((500..1400).step_by(19).map(|w| w as f32))
            {
                board.update(cx, |board, cx| {
                    board.width = width;
                    board.measured = None;
                    cx.notify();
                });
                draw(cx);
                let layout = observed.borrow().as_ref().unwrap().clone();
                assert_eq!(
                    layout.regions[0].label_lines, 1,
                    "compact lib wrapped at width {width}, scale {scale}"
                );
                assert_eq!(
                    layout.regions[1].label_lines, 1,
                    "cadence wrapped despite ample Reader room"
                );
                assert_painted(cx, &layout, &["lib", "cadence"]);
            }
        }
    }
    let unlit = observed.borrow().as_ref().unwrap().clone();
    let before = cx.update(|window, _| {
        window
            .painted_texts()
            .iter()
            .filter(|run| {
                run.text.as_ref() == "lib" && run.bounds.top() < px(unlit.regions[0].origin.1)
            })
            .map(|run| run.bounds)
            .collect::<Vec<_>>()
    });
    board.update(cx, |board, cx| {
        board.lit = true;
        cx.notify();
    });
    draw(cx);
    assert!(
        Rc::ptr_eq(&unlit, observed.borrow().as_ref().unwrap()),
        "foreground changes reuse the measured native layout"
    );
    assert_eq!(
        before,
        cx.update(|window, _| window
            .painted_texts()
            .iter()
            .filter(|run| run.text.as_ref() == "lib"
                && run.bounds.top() < px(unlit.regions[0].origin.1))
            .map(|run| run.bounds)
            .collect::<Vec<_>>()),
        "hover ink cannot change glyph geometry"
    );
}
#[gpui::test]
fn current_unicode_regions_resize_replace_and_release_native_actions_when_covered(
    cx: &mut TestAppContext,
) {
    init(cx);
    let observed = Rc::new(RefCell::new(None));
    let (board, cx) = cx.add_window_view(|window, _| {
        window.set_a11y_forced(true);
        Board {
            modules: modules(&[], 0),
            width: 360.,
            measured: None,
            observed: observed.clone(),
            lit: false,
            covered: false,
            calls: Rc::new(Cell::new(0)),
        }
    });
    cx.simulate_resize(size(px(1600.), px(2200.)));
    let long = "日本語_café_Ελληνικά_🧭_".repeat(18);
    for scale in [1., 2., 2.5] {
        cx.update(|_, cx| {
            crate::set_facet(
                crate::Facet {
                    text_scale: scale,
                    reduced_motion: true,
                    ..crate::Facet::default()
                },
                cx,
            )
        });
        for width in [360., 500., 1200., 500., 360.] {
            for (words, count) in [
                (vec![], 0),
                (vec!["lib"], 0),
                (vec!["lib"], 1),
                (vec!["lib", long.as_str(), "cadence"], 60),
            ] {
                board.update(cx, |board, cx| {
                    board.modules = modules(&words, count);
                    board.width = width;
                    board.measured = None;
                    cx.notify();
                });
                draw(cx);
                let layout = observed.borrow().as_ref().unwrap().clone();
                assert_eq!(layout.regions.len(), words.len());
                assert_painted(cx, &layout, &words);
                if words.len() == 3 {
                    assert!(
                        layout.regions[1].label_lines > 1,
                        "the full Unicode name must wrap, rather than truncate"
                    );
                }
            }
        }
    }
    board.update(cx, |board, cx| {
        board.covered = true;
        cx.notify();
    });
    draw(cx);
    let calls = board.read_with(cx, |board, _| board.calls.clone());
    let at = cx.update(|window, _| {
        let tree = window.a11y_tree().unwrap();
        let (id, node) = tree
            .nodes
            .iter()
            .find(|(_, node)| node.role() == gpui::Role::Button && node.label() == Some("lib"))
            .unwrap();
        assert!(node.is_disabled());
        assert!(
            !node.supports_action(gpui::AccessibleAction::Click),
            "covered map releases native activation"
        );
        window.a11y_node_bounds(*id).unwrap().center()
    });
    cx.simulate_click(at, gpui::Modifiers::none());
    assert_eq!(calls.get(), 0, "covered region cannot activate");
    board.update(cx, |board, cx| {
        board.covered = false;
        board.modules = modules(&["new_owner"], 1);
        board.measured = None;
        cx.notify();
    });
    draw(cx);
    assert_painted(cx, observed.borrow().as_ref().unwrap(), &["new_owner"]);
    let at = cx.update(|window, _| {
        let node = window
            .a11y_tree()
            .unwrap()
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == gpui::Role::Button && node.label() == Some("new_owner")
            })
            .map(|(id, _)| *id)
            .unwrap();
        window.a11y_node_bounds(node).unwrap().center()
    });
    cx.simulate_click(at, gpui::Modifiers::none());
    assert_eq!(
        calls.get(),
        1,
        "the replacement native region activates once"
    );
    cx.update(|window, _| {
        assert!(
            !window
                .a11y_tree()
                .unwrap()
                .nodes
                .iter()
                .any(|(_, node)| node.role() == gpui::Role::Button && node.label() == Some("lib")),
            "replacement cannot expose the old native region"
        )
    });
}

#[gpui::test]
fn retained_bundle_keeps_its_facts_and_measure_until_fresh_native_replacement(
    cx: &mut TestAppContext,
) {
    init(cx);
    let observed = Rc::new(RefCell::new(None));
    let (board, cx) = cx.add_window_view(|window, _| {
        window.set_a11y_forced(true);
        Board {
            modules: modules(&["lib"], 1),
            width: 360.,
            measured: None,
            observed: observed.clone(),
            lit: false,
            covered: false,
            calls: Rc::new(Cell::new(0)),
        }
    });
    cx.simulate_resize(size(px(1000.), px(1200.)));
    draw(cx);
    let retained = observed.borrow().as_ref().unwrap().clone();
    assert_painted(cx, &retained, &["lib"]);
    // Changing pending inputs does not provide a way to rebind facts or
    // font/container measure inside an already measured immutable bundle.
    // This checks coherence only; the host still owns current-page admission.
    board.update(cx, |board, cx| {
        board.modules = modules(&["new_café_日本語"], 60);
        board.width = 500.;
        cx.notify();
    });
    cx.update(|_, cx| {
        crate::set_facet(
            crate::Facet {
                text_scale: 2.,
                reduced_motion: true,
                ..crate::Facet::default()
            },
            cx,
        )
    });
    draw(cx);
    assert!(Rc::ptr_eq(&retained, observed.borrow().as_ref().unwrap()));
    assert_painted(cx, &retained, &["lib"]);
    cx.update(|window, _| {
        assert!(
            !window
                .painted_texts()
                .iter()
                .any(|run| run.text.as_ref() == "new_café_日本語")
        )
    });
    // The only replacement path measures the complete new input together.
    board.update(cx, |board, cx| {
        board.measured = None;
        cx.notify();
    });
    draw(cx);
    let fresh = observed.borrow().as_ref().unwrap().clone();
    assert!(!Rc::ptr_eq(&retained, &fresh));
    assert_eq!(fresh.width, 500.);
    assert!(fresh.labels.as_ref().unwrap().role.size > retained.labels.as_ref().unwrap().role.size);
    assert_painted(cx, &fresh, &["new_café_日本語"]);
    cx.update(|window, _| {
        assert!(
            !window
                .painted_texts()
                .iter()
                .any(|run| run.text.as_ref() == "lib")
        )
    });
}

struct NativeMetrics;
impl Render for NativeMetrics {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().relative().size_full().child(
            div()
                .id("native-metric")
                .absolute()
                .left(px(0.75))
                .top(px(0.75))
                .w(px(34.498))
                .h(px(43.239998))
                .role(gpui::Role::Group)
                .aria_label("native metric"),
        )
    }
}
#[gpui::test]
fn native_authored_metrics_snap_exactly_at_one_and_two_device_pixels(cx: &mut TestAppContext) {
    let (metrics, cx) = cx.add_window_view(|window, _| {
        window.set_a11y_forced(true);
        NativeMetrics
    });
    cx.simulate_resize(size(px(100.), px(100.)));
    // Handwritten expected bounds include half-ties in authored insets:
    // 0.75 logical at 2x is 1.5 device pixels, which rounds toward zero to 1.
    for (scale, expected) in [
        (
            1.,
            Bounds::new(gpui::point(px(1.), px(1.)), size(px(34.), px(43.))),
        ),
        (
            2.,
            Bounds::new(gpui::point(px(0.5), px(0.5)), size(px(34.5), px(43.))),
        ),
    ] {
        cx.update(|window, _| window.set_scale_factor(scale));
        metrics.update(cx, |_, cx| cx.notify());
        draw(cx);
        cx.update(|window, _| {
            let node = window
                .a11y_tree()
                .unwrap()
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some("native metric"))
                .map(|(id, _)| *id)
                .unwrap();
            assert_eq!(window.a11y_node_bounds(node).unwrap(), expected);
            assert_eq!(
                native_authored_bounds(
                    Bounds::new(
                        gpui::point(px(0.75), px(0.75)),
                        size(px(34.498), px(43.239998))
                    ),
                    scale
                ),
                expected
            );
        });
    }
}

#[test]
fn fully_visible_bounds_reconstruct_exact_float_edges() {
    let clip = Bounds::new(gpui::point(px(0.), px(0.)), size(px(2000.), px(2000.)));
    // Handwritten IEEE-754 results, independently of the helper. The first
    // case is the wide compact label receipt from the real CDF failure.
    for (top, height, expected_bits) in [
        (0., 20.4, 0x41a33333),
        (10.88, 20.4, 0x41a33332),
        (512., 14.1, 0x41619980),
    ] {
        let shaped = Bounds::new(gpui::point(px(0.), px(top)), size(px(100.), px(height)));
        let receipt = shaped.intersect(&clip);
        let expected = px(f32::from_bits(expected_bits));
        assert_eq!(receipt.size.height, expected);
        assert_eq!(full_row_receipt_height(px(top), px(height)), expected);
        assert_eq!(
            receipt.bottom(),
            shaped.bottom(),
            "the full visible row edge is preserved"
        );
        assert_ne!(
            full_row_receipt_height(px(top), px(height * 2.)),
            expected,
            "an extra row must fail this exact oracle"
        );
    }
}

#[gpui::test]
fn wide_native_row_preserves_shaped_height_and_records_exact_visible_edges(
    cx: &mut TestAppContext,
) {
    init(cx);
    let observed = Rc::new(RefCell::new(None));
    let (_, cx) = cx.add_window_view(|window, _| {
        window.set_a11y_forced(true);
        Board {
            modules: modules(&["lib"], 1),
            width: 1600.,
            measured: None,
            observed: observed.clone(),
            lit: false,
            covered: false,
            calls: Rc::new(Cell::new(0)),
        }
    });
    cx.simulate_resize(size(px(1800.), px(700.)));
    draw(cx);
    let layout = observed.borrow().as_ref().unwrap().clone();
    assert_eq!(layout.regions[0].label_lines, 1);
    // At 1600 room and 100% text the role is 15 * 1.36 = 20.4,
    // starting at 8 * 1.36 = 10.88. These are unsnapped native metrics.
    assert_eq!(layout.labels.as_ref().unwrap().role.line, 20.4);
    cx.update(|window, _| {
        let row = window
            .painted_texts()
            .iter()
            .find(|run| run.text.as_ref() == "lib")
            .unwrap();
        assert_eq!(row.bounds.top(), px(10.88));
        assert_eq!(row.bounds.size.height, px(f32::from_bits(0x41a33332)));
        assert_eq!(row.bounds.bottom(), px(f32::from_bits(0x41fa3d70)));
    });
    assert_painted(cx, &layout, &["lib"]);
}
