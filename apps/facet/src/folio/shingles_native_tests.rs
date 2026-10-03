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
    layout: Option<Rc<Layout>>,
    observed: Rc<RefCell<Option<Rc<Layout>>>>,
    lit: bool,
    covered: bool,
    calls: Rc<Cell<usize>>,
}
impl Render for Board {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let measure = Measure::new(px(self.width), &cx.facet());
        let layout = self
            .layout
            .get_or_insert_with(|| measured(&self.modules, &measure, window))
            .clone();
        *self.observed.borrow_mut() = Some(layout.clone());
        let map = shingles("measured-map", self.modules.clone(), &measure)
            .geometry(layout.clone())
            .rest(self.lit.then_some(Spot::Region(0)));
        // This is the host contract: the native leaves use the same Rc as
        // the painted map, with no separate text or region measurement.
        let mut body = div().relative().w(px(self.width)).child(map);
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
                    .aria_label(self.modules[i].name.clone()),
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
            let label = &labels.entries[i];
            let label_bottom =
                region.origin.y + px(PAD * layout.k + labels.role.line * label.line_count() as f32);
            for run in runs {
                assert!(
                    run.bounds.left() >= region.left()
                        && run.bounds.right() <= region.right() + px(0.001),
                    "{words} escapes the shared region horizontally"
                );
                assert!(
                    run.bounds.top() >= region.top()
                        && run.bounds.bottom() <= label_bottom + px(0.001),
                    "native glyph rows disagree with the measured label band"
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
                region,
                "native target and painted region use the same geometry"
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
            layout: None,
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
                    board.layout = None;
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
            layout: None,
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
                    board.layout = None;
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
        board.layout = None;
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
