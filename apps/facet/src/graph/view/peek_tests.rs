//! Mounted GraphView plus its real per-window float layer. This covers the
//! physical-anchor lifecycle that an unanchored float-only board misses.

use super::{Camera, GraphView, Start};
use crate::graph::layout::Layout;
use crate::graph::scene::Scene;
use crate::overlay::float;
use crate::theme::{Facet, set_facet};
use gpui::{
    AppContext, Context, ElementId, Entity, IntoElement, Modifiers, MouseMoveEvent, ParentElement,
    PlatformInput, Render, Styled, TestAppContext, VisualTestContext, Window, div, point, px, size,
};
use std::sync::Arc;
use std::time::Duration;

struct MountedGraph {
    graph: Entity<GraphView>,
}

impl Render for MountedGraph {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(self.graph.clone())
            .child(float::layer(window, cx))
    }
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.simulate_next_frame(cx);
        window.draw(cx).clear(cx);
    });
}

fn advance(cx: &mut VisualTestContext, millis: u64) {
    cx.executor().advance_clock(Duration::from_millis(millis));
    cx.run_until_parked();
}

fn key(node: u32) -> ElementId {
    ElementId::NamedInteger("graph-node".into(), u64::from(node))
}

#[gpui::test]
fn mounted_graph_anchors_the_aim_protected_card_across_another_pending_symbol(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        gpui_component::init(cx);
        set_facet(
            Facet {
                reduced_motion: true,
                ..Facet::default()
            },
            cx,
        );
        crate::probe::enable(cx);
    });
    let world = Arc::new(crate::graph::model::tests::tiny());
    let mut layout = Layout::compute(&world);
    // At this fixed 20 px/unit camera, A and B are 14 px apart. B is in
    // the real 12 px transit gap between A's anchor and its right card.
    for n in 0..layout.x.len() {
        layout.x[n] = 100.0 + n as f32;
        layout.y[n] = 100.0;
    }
    layout.x[0] = 0.0;
    layout.y[0] = 0.0;
    layout.x[5] = 0.7;
    layout.y[5] = 0.0;
    let scene = Arc::new(Scene::new(world, Arc::new(layout)));
    let camera = Camera::new(0.0, 0.0, 45.0);
    let (host, cx) = cx.add_window_view(|window, cx| MountedGraph {
        graph: cx.new(|cx| GraphView::with_scene(scene.clone(), Start::Cam(camera), window, cx)),
    });
    cx.simulate_resize(size(px(900.0), px(700.0)));
    let graph = host.read_with(cx, |host, _| host.graph.clone());
    for _ in 0..4 {
        advance(cx, 16);
        draw(cx);
    }
    let a = graph.read_with(cx, |graph, _| graph.screen_position(0).expect("mounted A"));
    let b = graph.read_with(cx, |graph, _| graph.screen_position(5).expect("mounted B"));
    assert!((b.0 - a.0 - 14.0).abs() < 0.1, "pinned transit geometry");
    cx.simulate_mouse_move(point(px(a.0), px(a.1)), None, Modifiers::none());
    assert_eq!(
        graph.read_with(cx, |graph, _| graph.hover),
        Some(0),
        "actual native graph must pick A first"
    );
    advance(cx, 350);
    draw(cx);
    assert!(cx.update(|window, cx| float::is_open(&key(0), window, cx)));
    let ledger = cx.update(|_, cx| crate::probe::take(cx));
    let card = ledger
        .stacks
        .iter()
        .flat_map(|stack| &stack.entries)
        .rev()
        .find(|entry| entry.key == key(0).to_string())
        .and_then(|entry| entry.bounds.clone())
        .expect("actual painted A card");
    assert!(
        b.0 < card.x && b.1 >= card.y && b.1 <= card.y + card.height,
        "B must be in transit, outside the plate"
    );
    cx.update(|window, cx| {
        // Establish a current aim apex on A, then pick B without a frame
        // in between. These are the real mounted GraphView listeners.
        for x in [a.0 + 3.0, b.0] {
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: point(px(x), px(a.1)),
                    pressed_button: None,
                    modifiers: Modifiers::none(),
                }),
                cx,
            );
        }
    });
    assert_eq!(graph.read_with(cx, |graph, _| graph.hover), Some(5));
    assert!(
        cx.update(|window, cx| float::aimed_pending(&key(5), window, cx)),
        "actual B rest must be deferred by A's pointer aim"
    );
    draw(cx);
    assert!(
        cx.update(|window, cx| float::is_open(&key(0), window, cx)),
        "a newly anchored B must not physically unmount the still mounted, aim-protected A trigger"
    );
    assert!(cx.update(|window, cx| float::aimed_pending(&key(5), window, cx)));
    let at_card = point(
        px(card.x + card.width * 0.5),
        px(card.y + card.height * 0.5),
    );
    cx.simulate_mouse_move(at_card, None, Modifiers::none());
    advance(cx, 200);
    draw(cx);
    assert!(
        cx.update(|window, cx| float::is_open(&key(0), window, cx)),
        "pointer transit must land on the original A card"
    );
    assert!(
        !cx.update(|window, cx| float::is_open(&key(5), window, cx)),
        "an aimed-over sibling must not replace the destination card"
    );
    // A really leaves the mounted viewport. Even a card-hover hold cannot
    // keep its old anchor alive after the original glyph is gone.
    graph.update(cx, |graph, cx| {
        graph
            .rig
            .as_mut()
            .expect("camera")
            .set(Camera::new(100.0, 0.0, 45.0));
        cx.notify();
    });
    draw(cx);
    assert!(
        !cx.update(|window, cx| float::is_open(&key(0), window, cx)),
        "physical trigger disappearance must still close the owned card"
    );
}
