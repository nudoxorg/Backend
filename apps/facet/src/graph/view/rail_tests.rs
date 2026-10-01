//! Native overflow rows stay reachable, picked from their painted geometry,
//! and keep semantic identity through responsive presentation changes.
use super::{GraphView, Start};
use crate::graph::{
    layout::Layout,
    model::{Edge, Kind, Node, Rel, World},
    prism::SlotKey,
    scene::Scene,
};
use crate::{
    semantics::Word,
    theme::{Facet, set_facet},
    tokens::Appearance,
};
use gpui::{
    AppContext, Bounds, Context, Entity, Focusable, IntoElement, Modifiers, ParentElement, Pixels,
    PlatformInput, Render, ScrollDelta, ScrollWheelEvent, Styled, TestAppContext,
    VisualTestContext, Window, div, point, px, size,
};
use std::{sync::Arc, time::Duration};

fn draw(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(Duration::from_millis(16));
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.simulate_next_frame(cx);
        window.draw(cx).clear(cx);
    });
}
fn settle(cx: &mut VisualTestContext) {
    for _ in 0..100 {
        draw(cx);
    }
}
fn fixture() -> Arc<Scene> {
    let base = crate::graph::model::tests::tiny();
    let mut nodes = base.nodes.clone();
    nodes.push(Node::new(Kind::Function, "call_target", 0, 0));
    nodes.push(Node::new(Kind::Function, "reader", 0, 0));
    for node in &mut nodes {
        node.vis = Some("pub".into());
    }
    let mut edges = base.edges.clone();
    edges.extend([
        Edge {
            from: 3,
            to: 0,
            rel: Rel::TAKES,
        },
        Edge {
            from: 3,
            to: 6,
            rel: Rel::CALLS,
        },
        Edge {
            from: 7,
            to: 3,
            rel: Rel::USES,
        },
    ]);
    let world = Arc::new(
        World::new(base.packages.clone(), base.modules.clone(), nodes, edges)
            .expect("valid pinned five-group callable"),
    );
    Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))))
}
fn keys() -> [SlotKey; 5] {
    [
        SlotKey {
            node: 0,
            side: -1,
            word: Word::Takes,
        },
        SlotKey {
            node: 2,
            side: -1,
            word: Word::CalledFrom,
        },
        SlotKey {
            node: 5,
            side: 1,
            word: Word::Gives,
        },
        SlotKey {
            node: 6,
            side: 1,
            word: Word::Calls,
        },
        SlotKey {
            node: 7,
            side: 1,
            word: Word::UsedBy,
        },
    ]
}
fn visible(graph: &GraphView, key: SlotKey) -> Option<[f32; 4]> {
    let frame = graph.frame.as_ref()?;
    frame.slots.get(frame.locate(key)?)?.label
}
fn contained(inner: Bounds<Pixels>, outer: Bounds<Pixels>) -> bool {
    inner.left() >= outer.left()
        && inner.top() >= outer.top()
        && inner.right() <= outer.right()
        && inner.bottom() <= outer.bottom()
}
fn wheel(cx: &mut VisualTestContext, at: gpui::Point<Pixels>, y: f32) {
    cx.update(|window, cx| {
        window.dispatch_event(
            PlatformInput::ScrollWheel(ScrollWheelEvent {
                position: at,
                delta: ScrollDelta::Pixels(point(px(0.0), px(y))),
                modifiers: Modifiers::none(),
                ..Default::default()
            }),
            cx,
        );
    });
    draw(cx);
    draw(cx);
}
fn assert_native_text(cx: &mut VisualTestContext, key: &str, viewport: Bounds<Pixels>) {
    cx.update(|_, cx| {
        crate::probe::take(cx);
    });
    draw(cx);
    let ledger = cx.update(|_, cx| crate::probe::take(cx));
    let text = ledger
        .texts
        .iter()
        .find(|text| text.key == key)
        .expect("visible rail text really paints natively");
    assert!(!text.content.is_empty());
    let clipped = &text.bounds;
    assert!(
        clipped.x >= f32::from(viewport.left()) - 0.5
            && clipped.y >= f32::from(viewport.top()) - 0.5
            && clipped.x + clipped.width <= f32::from(viewport.right()) + 0.5
            && clipped.y + clipped.height <= f32::from(viewport.bottom()) + 0.5,
        "the revealed row's painted text must fit its actual viewport: {text:?}, {viewport:?}"
    );
}

#[gpui::test]
fn native_relation_rail_reveals_hidden_keys_without_zooming_or_ghost_picking(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::probe::enable(cx);
    });
    for reduced in [false, true] {
        cx.update(|cx| {
            set_facet(
                Facet {
                    appearance: Appearance::Glacier,
                    text_scale: 2.0,
                    reduced_motion: reduced,
                    ..Facet::default()
                },
                cx,
            )
        });
        let (graph, cx) = cx.add_window_view(|window, cx| {
            GraphView::with_scene(fixture(), Start::Focus(3), window, cx)
        });
        cx.simulate_resize(size(px(480.0), px(600.0)));
        settle(cx);
        let expected = keys();
        let (viewport, first, last_index) = graph.read_with(cx, |graph, _| {
            let prism = graph.prism.as_ref().expect("focused relation model");
            assert_eq!(
                prism.left.len() + prism.right.len(),
                5,
                "independent positive overflow precondition"
            );
            let plan = graph
                .rail_plan
                .as_ref()
                .expect("short reading room has native overflow");
            let geometry = graph
                .rail_geometry
                .as_ref()
                .expect("this frame's actual native geometry");
            let frame = graph.frame.as_ref().expect("committed picker");
            assert!(frame.rail && frame.e == 1.0);
            assert_eq!(
                frame
                    .slots
                    .iter()
                    .filter_map(|slot| slot.key)
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(
                geometry.rows.len(),
                plan.entries.len(),
                "native geometry includes all bounded children, even hidden rows"
            );
            assert!(contained(
                geometry.viewport,
                Bounds::new(point(px(0.0), px(0.0)), size(px(480.0), px(600.0)))
            ));
            assert!(
                !geometry
                    .viewport
                    .intersects(&graph.find_bounds.expect("measured native Find"))
            );
            assert!(
                !geometry
                    .viewport
                    .intersects(&graph.card_bounds.expect("measured native card"))
            );
            assert!(
                visible(graph, expected[4]).is_none(),
                "last relation starts genuinely outside the clipped rail"
            );
            let first = visible(graph, expected[0]).expect("first action really visible");
            assert_eq!(
                frame
                    .pick(first[0] + 1.0, first[1] + 1.0)
                    .and_then(|i| frame.slots[i].key),
                Some(expected[0])
            );
            assert!(
                frame.pick(first[0] - 1.0, first[1] + 1.0).is_none(),
                "rail does not expand hit targets behind its viewport"
            );
            (
                geometry.viewport,
                first,
                plan.child_index(expected[4])
                    .expect("last stable child identity"),
            )
        });
        let at = point(
            px((first[0] + first[2]) * 0.5),
            px((first[1] + first[3]) * 0.5),
        );
        cx.simulate_mouse_move(at, None, Modifiers::none());
        draw(cx);
        graph.read_with(cx, |graph, _| {
            assert_eq!(graph.hover, Some(expected[0].node));
            assert_eq!(
                graph.frame.as_ref().expect("frame").slots
                    [graph.hover_slot.expect("native row picked")]
                .key,
                Some(expected[0])
            );
        });
        let camera = graph.read_with(cx, |graph, _| graph.camera().expect("camera"));
        wheel(cx, viewport.center(), -96.0);
        graph.read_with(cx, |graph, _| {
            assert_eq!(
                graph.camera(),
                Some(camera),
                "native rail wheel must never zoom or pan the world"
            );
            assert!(
                f32::from(graph.rail_scroll.offset().y) < 0.0,
                "real native wheel moves content"
            );
        });
        // Arrow navigation walks all semantic rows, not merely visible ones.
        cx.update(|window, cx| window.focus(&graph.focus_handle(cx), cx));
        cx.simulate_keystrokes("right down down down down");
        draw(cx);
        let viewport = graph.read_with(cx, |graph, _| {
            assert_eq!(graph.state.selected, Some(expected[4]));
            assert!(
                visible(graph, expected[4]).is_some(),
                "keyboard selection must reveal the real hidden action"
            );
            assert!(
                graph.frame.as_ref().expect("frame").slots
                    [graph.state.prism_sel.expect("selected")]
                .proxy_visible
            );
            assert!(f32::from(graph.rail_scroll.offset().y) < 0.0);
            graph.rail_geometry.as_ref().expect("geometry").viewport
        });
        assert_native_text(cx, &format!("graph-prism-rail-name-{last_index}"), viewport);
        let offset = graph.read_with(cx, |graph, _| graph.rail_scroll.offset());
        wheel(cx, viewport.center(), 80.0);
        graph.read_with(cx, |graph, _| {
            assert_ne!(
                graph.rail_scroll.offset(),
                offset,
                "manual wheel remains free after keyboard reveal"
            );
            assert_eq!(graph.camera(), Some(camera));
        });
        cx.simulate_keystrokes("down");
        draw(cx); // explicit navigation reveals it again
        assert!(graph.read_with(cx, |graph, _| visible(graph, expected[4]).is_some()));
        cx.simulate_keystrokes("enter");
        settle(cx);
        assert_eq!(
            graph.read_with(cx, |graph, _| graph.focused()),
            Some(7),
            "Enter opens the exact stable relation, not a truncated slot neighbour"
        );
        cx.simulate_mouse_move(point(px(-20.0), px(-20.0)), None, Modifiers::none());
        settle(cx);
        assert_eq!(
            cx.update(|window, cx| window.simulate_next_frame(cx)),
            0,
            "rail leaves no ambient frame lease"
        );
    }
}

struct Region {
    graph: Entity<GraphView>,
    bounds: Bounds<Pixels>,
}
impl Render for Region {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().relative().size_full().child(
            div()
                .absolute()
                .left(self.bounds.origin.x)
                .top(self.bounds.origin.y)
                .w(self.bounds.size.width)
                .h(self.bounds.size.height)
                .child(self.graph.clone()),
        )
    }
}
#[gpui::test]
fn first_embedded_resize_switches_columns_and_rail_without_changing_selected_relation(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::probe::enable(cx);
    });
    for reduced in [false, true] {
        cx.update(|cx| {
            set_facet(
                Facet {
                    appearance: Appearance::Glacier,
                    text_scale: 2.0,
                    reduced_motion: reduced,
                    ..Facet::default()
                },
                cx,
            )
        });
        let (host, cx) = cx.add_window_view(|window, cx| Region {
            graph: cx.new(|cx| GraphView::with_scene(fixture(), Start::Focus(3), window, cx)),
            bounds: Bounds::new(point(px(72.0), px(48.0)), size(px(1440.0), px(600.0))),
        });
        cx.simulate_resize(size(px(1600.0), px(900.0)));
        let graph = host.read_with(cx, |host, _| host.graph.clone());
        settle(cx);
        assert!(graph.read_with(cx, |graph, _| {
            !graph.frame.as_ref().expect("comfortable columns").rail && graph.rail_plan.is_none()
        }));
        cx.update(|window, cx| window.focus(&graph.focus_handle(cx), cx));
        cx.simulate_keystrokes("right down down");
        draw(cx);
        let selected = keys()[4];
        assert_eq!(
            graph.read_with(cx, |graph, _| graph.state.selected),
            Some(selected),
            "positive ordinary-column identity precondition"
        );
        for width in [480.0, 1440.0, 480.0] {
            let region = Bounds::new(point(px(104.0), px(76.0)), size(px(width), px(600.0)));
            host.update(cx, |host, cx| {
                host.bounds = region;
                cx.notify();
            });
            draw(cx);
            graph.read_with(cx, |graph, _| {
                assert_eq!(
                    graph.state.selected,
                    Some(selected),
                    "same key survives the very first responsive draw"
                );
                let frame = graph.frame.as_ref().expect("current frame");
                let slot = &frame.slots[frame.locate(selected).expect("same semantic row")];
                assert_eq!(frame.rail, width < 640.0);
                assert!(
                    slot.label.is_some(),
                    "selected row is revealed in the first current viewport"
                );
                if let Some(geometry) = &graph.rail_geometry {
                    assert!(contained(geometry.viewport, region));
                    assert!(
                        !geometry
                            .viewport
                            .intersects(&graph.find_bounds.expect("actual embedded Find"))
                    );
                    assert!(
                        !geometry
                            .viewport
                            .intersects(&graph.card_bounds.expect("actual embedded card"))
                    );
                }
            });
            settle(cx);
            assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
        }
        for text_scale in [1.0, 2.0] {
            cx.update(|_, cx| {
                set_facet(
                    Facet {
                        appearance: Appearance::Glacier,
                        text_scale,
                        reduced_motion: reduced,
                        ..Facet::default()
                    },
                    cx,
                )
            });
            draw(cx);
            graph.read_with(cx, |graph, _| {
                assert_eq!(graph.state.selected, Some(selected));
                assert!(visible(graph, selected).is_some(), "actual native row extent changes must reveal the selected key in their first draw");
            });
            settle(cx);
            assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
        }
        cx.simulate_keystrokes("enter");
        settle(cx);
        assert_eq!(
            graph.read_with(cx, |graph, _| graph.focused()),
            Some(selected.node)
        );
    }
}

#[gpui::test]
fn native_relation_rail_waits_for_the_same_gather_readiness_as_the_canvas(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        set_facet(
            Facet {
                appearance: Appearance::Glacier,
                text_scale: 2.0,
                ..Facet::default()
            },
            cx,
        );
    });
    let (graph, cx) = cx.add_window_view(|window, cx| {
        GraphView::with_scene(fixture(), Start::Focus(3), window, cx)
    });
    cx.simulate_resize(size(px(480.0), px(600.0)));
    settle(cx);
    graph.update(cx, |graph, cx| {
        let prism = graph.prism.as_mut().expect("pinned focused relation model");
        prism.g = 0.25;
        prism.target = 1.0;
        graph.motion.set(super::PRISM_KEY, 0.25);
        cx.notify();
    });
    draw(cx);
    let at = graph.read_with(cx, |graph, _| {
        let frame = graph.frame.as_ref().expect("native growing rail");
        assert!(
            frame.rail && frame.e < 0.8,
            "positive gather-not-ready precondition: {}",
            frame.e
        );
        let row =
            visible(graph, keys()[0]).expect("row geometry exists before it becomes interactive");
        assert!(
            frame
                .pick((row[0] + row[2]) * 0.5, (row[1] + row[3]) * 0.5)
                .is_none()
        );
        point(px((row[0] + row[2]) * 0.5), px((row[1] + row[3]) * 0.5))
    });
    cx.simulate_mouse_move(at, None, Modifiers::none());
    cx.simulate_mouse_down(at, gpui::MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(at, gpui::MouseButton::Left, Modifiers::none());
    graph.read_with(cx, |graph, _| {
        assert_eq!(graph.focused(), Some(3));
        assert!(
            graph.hover.is_none() && graph.state.selected.is_none() && graph.drag.is_none(),
            "an early native row cannot acquire pointer or graph navigation ownership"
        );
    });
    cx.simulate_mouse_move(point(px(-20.0), px(-20.0)), None, Modifiers::none());
    settle(cx);
    assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
}
