//! Native first-draw constraints and explicitly held worker delivery. These
//! tests do not wait for settlement before asserting responsive chrome.
use super::{GraphView, Start, View};
use crate::graph::{discovery::Discovery, layout::Layout, scene::Scene};
use crate::theme::{Facet, set_facet};
use gpui::{AppContext, Bounds, Context, Entity, Focusable, IntoElement, ParentElement, Pixels,
    Render, Styled, TestAppContext, VisualTestContext, Window, div, point, px, size};
use std::{sync::Arc, time::Duration};

fn draw(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(Duration::from_millis(16));
    cx.run_until_parked();
    cx.update(|window, cx| { window.simulate_next_frame(cx); window.draw(cx).clear(cx); });
}
fn settle(cx: &mut VisualTestContext) { for _ in 0..100 { draw(cx); } }
fn scene() -> Arc<Scene> {
    let mut world = crate::graph::model::tests::tiny();
    for node in &mut world.nodes { node.vis = Some("pub".into()); }
    world.nodes[0].doc = Some("Pinned readable documentation.".into());
    let world = Arc::new(world);
    Arc::new(Scene::new(world.clone(), Arc::new(Layout::compute(&world))))
}
struct Region { graph: Entity<GraphView>, bounds: Bounds<Pixels> }
impl Render for Region {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().relative().size_full().child(div().absolute().left(self.bounds.origin.x).top(self.bounds.origin.y)
            .w(self.bounds.size.width).h(self.bounds.size.height).child(self.graph.clone()))
    }
}
fn assert_content(ledger: &crate::probe::Ledger, card: Bounds<Pixels>) {
    for key in ["graph-focus-title", "graph-focus-qual", "graph-focus-doc"] {
        let text = ledger.texts.iter().find(|text| text.key == key).expect("actual native focus content must paint");
        assert!(!text.content.is_empty());
        assert!(text.bounds.x >= f32::from(card.left()) - 0.5 && text.bounds.y >= f32::from(card.top()) - 0.5
            && text.bounds.x + text.bounds.width <= f32::from(card.right()) + 0.5
            && text.bounds.y + text.bounds.height <= f32::from(card.bottom()) + 0.5,
            "painted content must lie inside the actual card: {text:?}, card {card:?}");
    }
}

#[gpui::test]
fn first_draw_resize_uses_actual_embedded_region_and_preserves_native_find_focus(cx: &mut TestAppContext) {
    cx.update(|cx| { gpui_component::init(cx); crate::probe::enable(cx); });
    for (reduced, text_scale) in [(false, 1.0), (true, 1.0), (false, 2.0), (true, 2.0)] {
        cx.update(|cx| set_facet(Facet { reduced_motion: reduced, text_scale, ..Facet::default() }, cx));
        let scene = scene();
        let initial = Bounds::new(point(px(80.0), px(50.0)), size(px(900.0), px(640.0)));
        let (host, cx) = cx.add_window_view(|window, cx| Region {
            graph: cx.new(|cx| GraphView::with_scene(scene, Start::Focus(0), window, cx)), bounds: initial });
        let graph = host.read_with(cx, |host, _| host.graph.clone());
        settle(cx);
        for width in [480.0, 900.0, 480.0] {
            let region = Bounds::new(point(px(120.0), px(60.0)), size(px(width), px(620.0)));
            cx.update(|_, cx| { crate::probe::take(cx); });
            host.update(cx, |host, cx| { host.bounds = region; cx.notify(); });
            draw(cx); // Exactly the first dirty draw, before any correction settles.
            let card = graph.read_with(cx, |graph, _| {
                assert_eq!(graph.view, Some(View { x: 120.0, y: 60.0, w: width, h: 620.0 }));
                assert_eq!(graph.focused(), Some(0));
                let glyph = graph.node_bounds(0).expect("focused glyph remains visible in the actual viewport");
                assert!(glyph.left() >= region.left() && glyph.top() >= region.top() && glyph.right() <= region.right() && glyph.bottom() <= region.bottom());
                let card = graph.card_bounds.expect("actual measured card on first draw");
                // The card is a sheet under the map below 640 design px (width ÷ text scale).
                if width / text_scale < 640.0 {
                    assert_eq!(card.left(), region.left() + px(12.0));
                    assert_eq!(card.right(), region.right() - px(12.0));
                    assert_eq!(card.bottom(), region.bottom() - px(40.0));
                } else {
                    assert_eq!(card.top(), region.top() + px(14.0));
                    assert_eq!(card.right(), region.right() - px(16.0));
                    assert_eq!(card.size.width, px(340.0));
                }
                card
            });
            let ledger = cx.update(|_, cx| crate::probe::take(cx));
            if reduced {
                assert_content(&ledger, card);
            }
            settle(cx);
            if !reduced {
                // Carried to its place, not jumped there: once it has arrived its words lie inside it.
                cx.update(|_, cx| { crate::probe::take(cx); });
                draw(cx);
                let ledger = cx.update(|_, cx| crate::probe::take(cx));
                assert_content(&ledger, card);
            }
            graph.read_with(cx, |graph, _| assert!(!graph.node_bounds(0).expect("settled visible focus").intersects(&graph.card_bounds.expect("card"))));
            assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
        }
        cx.update(|window, cx| window.focus(&graph.focus_handle(cx), cx));
        cx.simulate_keystrokes("/"); draw(cx);
        cx.simulate_input("Page"); settle(cx);
        let field = graph.read_with(cx, |graph, _| graph.find.entity_id());
        for width in [900.0, 480.0] {
            host.update(cx, |host, cx| { host.bounds.size.width = px(width); cx.notify(); });
            draw(cx);
            cx.update(|window, cx| {
                let graph = graph.read(cx);
                assert!(graph.find_open() && graph.find_focused(window, cx));
                assert_eq!(graph.find.entity_id(), field, "actual editor identity survives first-draw constraints");
                assert_eq!(graph.find.read(cx).value().as_ref(), "Page");
                assert_eq!(graph.find_bounds.expect("real native field").left(), px(136.0));
            });
        }
        cx.simulate_keystrokes("escape"); settle(cx);
        assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
    }
}

#[gpui::test]
fn cold_t_before_scene_delivery_uses_the_intended_route_and_starts_exactly_once(cx: &mut TestAppContext) {
    cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); crate::probe::enable(cx); });
    let scene = scene();
    let prepared = Discovery::prepare(&scene.world);
    let expected = Discovery::from_prepared(prepared.clone()).package_tour(1).expect("pinned package tour").clone();
    assert!(expected.shown(), "pinned package has a real reading path");
    let (graph, cx) = cx.add_window_view(|window, cx| GraphView::empty(scene.world.clone(), Start::Focus(3), window, cx));
    draw(cx); // Scene and index are intentionally held, while native chrome exists.
    cx.update(|window, cx| window.focus(&graph.focus_handle(cx), cx));
    cx.simulate_keystrokes("t"); draw(cx);
    graph.read_with(cx, |graph, _| {
        assert!(graph.scene.is_none() && graph.discovery.is_none() && graph.rig.is_none());
        assert_eq!(graph.state.exploration.preparing_tour().expect("accepted native T").package, 1);
        assert!(!graph.ready());
    });
    let ledger = cx.update(|_, cx| crate::probe::take(cx));
    let status = ledger.texts.iter().find(|text| text.key == "graph-tour-status" && text.content.contains("Preparing")).expect("pending native input must have truthful painted feedback");
    let viewport = graph.read_with(cx, |graph, _| graph.view.expect("measured cold viewport"));
    assert!(status.bounds.x >= viewport.x && status.bounds.y >= viewport.y && status.bounds.x + status.bounds.width <= viewport.x + viewport.w && status.bounds.y + status.bounds.height <= viewport.y + viewport.h, "pending status must actually fit the native viewport: {status:?}");
    graph.update(cx, |graph, cx| { graph.scene = Some(scene.clone()); graph.install_discovery(prepared.clone(), cx); });
    let epoch = graph.read_with(cx, |graph, _| {
        assert_eq!(graph.tour_stop(), Some(expected.stops[0].node));
        assert!(graph.state.exploration.preparing_tour().is_none());
        assert_eq!(graph.focused(), None, "old initial focus cannot own a delivered tour");
        graph.state.generation
    });
    graph.update(cx, |graph, cx| graph.install_discovery(prepared, cx));
    assert_eq!(graph.read_with(cx, |graph, _| graph.state.generation), epoch, "duplicate delivery cannot restart navigation");
    draw(cx);
    graph.read_with(cx, |graph, _| { assert_eq!(graph.focused(), None); assert_eq!(graph.tour_stop(), Some(expected.stops[0].node)); assert!(graph.rig.is_some()); });
    settle(cx);
    assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
}

#[gpui::test]
fn cold_t_delivery_cannot_replay_after_native_find_escape_focus_or_suspension(cx: &mut TestAppContext) {
    cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); });
    let scene = scene();
    let prepared = Discovery::prepare(&scene.world);
    assert!(Discovery::from_prepared(prepared.clone()).package_tour(1).is_some_and(crate::semantics::tour::Tour::shown));
    let (graph, cx) = cx.add_window_view(|window, cx| { let mut graph = GraphView::empty(scene.world.clone(), Start::Focus(3), window, cx); graph.scene = Some(scene.clone()); graph });
    draw(cx);
    for cancel in ["find", "escape", "focus", "suspend"] {
        graph.update(cx, |graph, cx| { graph.discovery = None; graph.set_focus(Some(3), false, cx); });
        cx.update(|window, cx| window.focus(&graph.focus_handle(cx), cx));
        cx.simulate_keystrokes("t"); draw(cx);
        assert!(graph.read_with(cx, |graph, _| graph.state.exploration.preparing_tour().is_some()), "{cancel}: positive cold native input precondition");
        match cancel {
            "find" => { cx.simulate_keystrokes("/"); draw(cx); cx.update(|window, cx| assert!(graph.read(cx).find_focused(window, cx))); }
            "escape" => { cx.simulate_keystrokes("escape"); draw(cx); }
            "focus" => graph.update(cx, |graph, cx| graph.set_focus(Some(0), false, cx)),
            "suspend" => cx.update(|window, cx| graph.update(cx, |graph, cx| graph.suspend(window, cx))),
            _ => unreachable!(),
        }
        graph.update(cx, |graph, cx| graph.install_discovery(prepared.clone(), cx));
        draw(cx);
        graph.read_with(cx, |graph, _| {
            assert!(graph.state.exploration.preparing_tour().is_none() && graph.tour_stop().is_none(), "{cancel}: stale worker delivery must not revive the tour");
            assert!(graph.ready());
            assert_eq!(graph.focused(), Some(if cancel == "focus" { 0 } else { 3 }));
        });
        if cancel == "find" { cx.simulate_keystrokes("escape"); }
        settle(cx);
        assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
    }
}

#[gpui::test]
fn painted_package_names_own_their_clicks_without_stealing_visible_glyphs(cx: &mut TestAppContext) {
    cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); });
    let initial = scene();
    let (graph, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(initial.clone(), Start::World, window, cx));
    settle(cx);
    let (view, camera, territory, at) = graph.read_with(cx, |graph, _| {
        let (view, camera, labels) = graph.painted_labels.as_ref().expect("native labels painted");
        let label = labels.iter().find(|label| label.territory.module.is_none()).expect("visible package name");
        (*view, *camera, label.territory, label.bounds.center())
    });
    let mut layout = (*initial.layout).clone();
    for i in 0..layout.x.len() { layout.x[i] = -10_000.0; layout.y[i] = -10_000.0; }
    let (x, y) = view.to_world(&camera, f32::from(at.x) + 9.0, f32::from(at.y));
    layout.x[0] = x as f32; layout.y[0] = y as f32;
    let glyph_at = point(at.x + px(50.0), at.y + px(80.0));
    let (x, y) = view.to_world(&camera, f32::from(glyph_at.x), f32::from(glyph_at.y));
    layout.x[3] = x as f32; layout.y[3] = y as f32;
    let pinned = Arc::new(Scene::new(initial.world.clone(), Arc::new(layout)));
    assert_eq!(pinned.pick(&view, &camera, f32::from(at.x), f32::from(at.y)), Some(0), "expanded glyph would steal this text click without semantic paint targets");
    assert!(!Scene::glyph(pinned.world.node(0).kind, view.k(&camera)).contains(9.0, 0.0), "text overlaps expanded picking, not a genuine painted glyph");
    graph.update(cx, |graph, cx| { graph.scene = Some(pinned.clone()); cx.notify(); });
    draw(cx);
    graph.read_with(cx, |graph, _| {
        assert_eq!(graph.painted_territory(&view, &camera, f32::from(at.x), f32::from(at.y)), Some(territory));
        let mut other = camera; other.x += 1.0;
        assert_eq!(graph.painted_territory(&view, &other, f32::from(at.x), f32::from(at.y)), None, "stale paint regions cannot route a different projection");
        assert_eq!(graph.painted_territory(&view, &camera, f32::from(glyph_at.x), f32::from(glyph_at.y)), None);
    });
    cx.simulate_mouse_move(at, None, gpui::Modifiers::none());
    assert_eq!(graph.read_with(cx, |graph, _| graph.hover), None, "package text never opens the nearby expanded glyph's card");
    cx.simulate_mouse_down(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    settle(cx);
    graph.read_with(cx, |graph, _| {
        assert_eq!(graph.focused(), None, "real text click enters the package");
        assert_eq!(graph.camera(), Some(view.frame(pinned.layout.packages[territory.pkg as usize].bounds, 1.25)));
    });
    // Reset only the camera to the pinned overview, then exercise a real glyph
    // outside all accepted text. Product focus routing stays native.
    graph.update(cx, |graph, cx| { graph.rig.as_mut().expect("rig").set(camera); cx.notify(); });
    draw(cx);
    assert_eq!(graph.read_with(cx, |graph, _| graph.painted_territory(&view, &camera, f32::from(glyph_at.x), f32::from(glyph_at.y))), None);
    cx.simulate_mouse_move(glyph_at, None, gpui::Modifiers::none());
    cx.simulate_mouse_down(glyph_at, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(glyph_at, gpui::MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(graph.read_with(cx, |graph, _| graph.focused()), Some(3), "genuine glyphs outside accepted names keep direct selection");
    settle(cx);
    graph.update(cx, |graph, cx| { graph.rig.as_mut().expect("retained rig").set(camera); cx.notify(); });
    draw(cx);
    let name = graph.read_with(cx, |graph, _| {
        assert_eq!(graph.focused(), Some(3), "focus persists while inspecting the overview");
        let (_, _, labels) = graph.painted_labels.as_ref().expect("overview labels");
        labels.iter().filter(|label| label.territory.module.is_none())
            .map(|label| label.bounds.center()).find(|at| !graph.over_chrome(f32::from(at.x), f32::from(at.y)))
            .expect("accepted package text outside the focused card")
    });
    cx.simulate_mouse_move(name, None, gpui::Modifiers::none());
    cx.simulate_mouse_down(name, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(name, gpui::MouseButton::Left, gpui::Modifiers::none());
    graph.read_with(cx, |graph, _| {
        assert_eq!(graph.focused(), None, "package navigation releases prior focus");
        assert!(matches!(graph.state.exploration, super::Exploration::Free));
    });
    settle(cx);
}

#[gpui::test]
fn native_enter_waits_for_the_same_cold_query_and_accepts_exactly_once(cx: &mut TestAppContext) {
    cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); });
    let scene = scene();
    let prepared = Discovery::prepare(&scene.world);
    let completed = Discovery::query_prepared(prepared.clone(), &scene.world, "Error");
    assert_eq!(completed.rows.iter().map(|row| row.node).collect::<Vec<_>>(), vec![5]);
    let previous = Discovery::query_prepared(prepared.clone(), &scene.world, "Page");
    assert_eq!(previous.rows.iter().map(|row| row.node).collect::<Vec<_>>(), vec![0]);
    let (graph, cx) = cx.add_window_view(|window, cx| {
        let mut graph = GraphView::empty(scene.world.clone(), Start::World, window, cx);
        graph.scene = Some(scene.clone()); graph
    });
    draw(cx);
    cx.update(|window, cx| window.focus(&graph.focus_handle(cx), cx));
    // No draw between slash, typing and Enter: the native Focus observer is
    // intentionally still pending when Enter claims this query generation.
    cx.simulate_keystrokes("/");
    cx.simulate_input("Error");
    cx.update(|window, cx| assert!(graph.read(cx).find_focused(window, cx)));
    // Pin the previous result packet at the held worker boundary. Enter must
    // wait for Error even when Page remains selectable in retained state.
    graph.update(cx, |graph, _| {
        graph.results = vec![0]; graph.search = std::rc::Rc::new(previous);
        assert!(graph.searching);
    });
    cx.simulate_keystrokes("enter");
    let generation = graph.read_with(cx, |graph, _| {
        assert!(graph.searching && graph.discovery.is_none());
        assert_eq!(graph.results, vec![0], "a stale row is genuinely present when Enter arrives");
        assert_eq!(graph.state.pending_accept, Some(graph.state.generation));
        assert_eq!(graph.focused(), None);
        graph.state.generation
    });
    draw(cx); // The delayed native Focus observer now sees the queued Enter.
    assert_eq!(graph.read_with(cx, |graph, _| graph.state.pending_accept), Some(generation));
    graph.update(cx, |graph, cx| {
        graph.install_discovery(prepared, cx);
        graph._search_task = None; // Hold the actual query-delivery boundary.
        graph.searching = true;
        assert_eq!(graph.state.generation, generation, "index attachment resumes the exact query, not a new one");
        assert_eq!(graph.state.pending_accept, Some(generation));
        graph.deliver_search(generation, "Error", completed.clone(), cx);
        assert_eq!(graph.focused(), None, "worker delivery does not mutate window focus");
    });
    draw(cx); // Acceptance and chrome share this first native draw.
    cx.update(|window, cx| {
        let graph = graph.read(cx);
        assert_eq!(graph.focused(), Some(5));
        assert!(!graph.find_open() && !graph.find_focused(window, cx));
        assert_eq!(graph.state.pending_accept, None);
        assert_eq!(graph.find.read(cx).value().as_ref(), "Error");
    });
    let accepted = graph.read_with(cx, |graph, _| graph.state.generation);
    graph.update(cx, |graph, cx| graph.deliver_search(generation, "Error", completed, cx));
    settle(cx);
    assert_eq!(graph.read_with(cx, |graph, _| graph.state.generation), accepted, "duplicate stale delivery cannot replay Enter");
    assert_eq!(cx.update(|window, cx| window.simulate_next_frame(cx)), 0);
}

#[gpui::test]
fn native_pending_enter_cancels_on_edit_escape_blur_and_suspend(cx: &mut TestAppContext) {
    cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); });
    let scene = scene();
    let prepared = Discovery::prepare(&scene.world);
    let completed = Discovery::query_prepared(prepared.clone(), &scene.world, "Page");
    assert_eq!(completed.rows.iter().map(|row| row.node).collect::<Vec<_>>(), vec![0]);
    for cancel in ["edit", "escape", "blur", "suspend"] {
        let (graph, cx) = cx.add_window_view(|window, cx| {
            let mut graph = GraphView::empty(scene.world.clone(), Start::World, window, cx);
            graph.scene = Some(scene.clone());
            graph.discovery = Some(std::rc::Rc::new(Discovery::from_prepared(prepared.clone()))); graph
        });
        draw(cx);
        cx.update(|window, cx| window.focus(&graph.focus_handle(cx), cx));
        cx.simulate_keystrokes("/"); draw(cx); cx.simulate_input("Page");
        graph.update(cx, |graph, _| { graph._search_task = None; graph.searching = true; graph.results.clear(); graph.search = super::empty_search(); });
        cx.simulate_keystrokes("enter");
        let generation = graph.read_with(cx, |graph, _| graph.state.pending_accept.expect("native Enter waits for held worker"));
        match cancel {
            "edit" => cx.simulate_input("x"),
            "escape" => cx.simulate_keystrokes("escape"),
            "blur" => cx.update(|window, cx| window.focus(&graph.focus_handle(cx), cx)),
            "suspend" => cx.update(|window, cx| graph.update(cx, |graph, cx| graph.suspend(window, cx))),
            _ => unreachable!(),
        }
        // Deliver before another draw: real native focus must beat an Input
        // Blur observer that has not yet seen its previous/current focus path.
        graph.update(cx, |graph, cx| { graph._search_task = None; graph.deliver_search(generation, "Page", completed.clone(), cx); });
        draw(cx);
        graph.read_with(cx, |graph, _| {
            assert_eq!(graph.focused(), None, "cancel={cancel}: delayed Enter cannot navigate");
            assert_eq!(graph.state.pending_accept, None, "cancel={cancel}");
        });
        cx.simulate_keystrokes("escape"); settle(cx);
    }
}

#[gpui::test]
fn native_pending_enter_with_no_matches_keeps_the_query_editable(cx: &mut TestAppContext) {
    cx.update(|cx| { gpui_component::init(cx); set_facet(Facet::default(), cx); });
    let scene = scene(); let prepared = Discovery::prepare(&scene.world);
    let completed = Discovery::query_prepared(prepared.clone(), &scene.world, "MissingSymbol");
    assert!(completed.rows.is_empty() && completed.chains.is_empty());
    let (graph, cx) = cx.add_window_view(|window, cx| {
        let mut graph = GraphView::empty(scene.world.clone(), Start::World, window, cx);
        graph.scene = Some(scene.clone()); graph.discovery = Some(std::rc::Rc::new(Discovery::from_prepared(prepared))); graph
    });
    draw(cx); cx.update(|window, cx| window.focus(&graph.focus_handle(cx), cx));
    cx.simulate_keystrokes("/"); draw(cx); cx.simulate_input("MissingSymbol");
    graph.update(cx, |graph, _| { graph._search_task = None; graph.searching = true; });
    cx.simulate_keystrokes("enter");
    let generation = graph.read_with(cx, |graph, _| graph.state.pending_accept.expect("queued Enter"));
    graph.update(cx, |graph, cx| graph.deliver_search(generation, "MissingSymbol", completed, cx));
    draw(cx);
    cx.update(|window, cx| {
        let graph = graph.read(cx); assert!(graph.find_open() && graph.find_focused(window, cx));
        assert_eq!(graph.state.pending_accept, None); assert_eq!(graph.focused(), None);
    });
    cx.simulate_input("x");
    assert_eq!(graph.read_with(cx, |graph, cx| graph.find.read(cx).value().to_string()), "MissingSymbolx");
    cx.simulate_keystrokes("escape"); settle(cx);
}

/// The focus card is a card beside the map from 640 design px and a sheet
/// under it below, held 16 px through the edge: a window dragged across 640
/// keeps the card where it was until it is 16 px past, and a window resting on
/// the edge, ten px either side, does not swap it back and forth.
#[gpui::test]
fn the_focus_card_holds_its_place_on_a_window_edge(cx: &mut TestAppContext) {
    cx.update(|cx| { gpui_component::init(cx); crate::probe::enable(cx); });
    cx.update(|cx| set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx));
    let initial = Bounds::new(point(px(80.0), px(50.0)), size(px(900.0), px(640.0)));
    let (host, cx) = cx.add_window_view(|window, cx| Region {
        graph: cx.new(|cx| GraphView::with_scene(scene(), Start::Focus(0), window, cx)), bounds: initial });
    let graph = host.read_with(cx, |host, _| host.graph.clone());
    settle(cx);
    // A sheet under the map spans the region less its 12 px gutters; a card beside it is 340 wide.
    let mut place = |cx: &mut VisualTestContext, width: f32| {
        host.update(cx, |host, cx| { host.bounds = Bounds::new(point(px(120.0), px(60.0)), size(px(width), px(620.0))); cx.notify(); });
        settle(cx);
        let card = graph.read_with(cx, |graph, _| graph.card_bounds.expect("the measured card"));
        if (f32::from(card.size.width) - (width - 24.0)).abs() < 1.0 { "sheet" } else if f32::from(card.size.width) == 340.0 { "beside" } else { "neither" }
    };
    assert_eq!(place(cx, 900.0), "beside");
    assert_eq!(place(cx, 650.0), "beside", "inside the band on the way down: held");
    assert_eq!(place(cx, 630.0), "beside", "still inside it: held");
    assert_eq!(place(cx, 620.0), "sheet", "past it: a sheet");
    assert_eq!(place(cx, 650.0), "sheet", "inside the band on the way up: held");
    for pass in 0..8 {
        let width = 640.0 + if pass % 2 == 0 { -10.0 } else { 10.0 };
        assert_eq!(place(cx, width), "sheet", "flipped at {width}");
    }
    assert_eq!(place(cx, 670.0), "beside", "past the band: a card again");
}

/// A window dragged across the card's edge carries the card from where it was
/// to where it goes: the words are drawn at the old place on the first frame,
/// pass through places between, and arrive; no frame covers most of the way.
/// Under reduced motion the card is simply there.
#[gpui::test]
fn the_focus_card_is_carried_between_beside_and_a_sheet(cx: &mut TestAppContext) {
    cx.update(|cx| { gpui_component::init(cx); crate::probe::enable(cx); });
    for reduced in [false, true] {
        cx.update(|cx| set_facet(Facet { reduced_motion: reduced, ..Facet::default() }, cx));
        let initial = Bounds::new(point(px(80.0), px(50.0)), size(px(900.0), px(640.0)));
        let (host, cx) = cx.add_window_view(|window, cx| Region {
            graph: cx.new(|cx| GraphView::with_scene(scene(), Start::Focus(0), window, cx)), bounds: initial });
        settle(cx);
        let title = |cx: &mut VisualTestContext| {
            let ledger = cx.update(|_, cx| crate::probe::take(cx));
            let text = ledger.texts.iter().find(|text| text.key == "graph-focus-title").expect("the card's title");
            (text.bounds.x, text.bounds.y)
        };
        cx.update(|_, cx| { crate::probe::take(cx); });
        draw(cx);
        let beside = title(cx);
        // One step from a card beside the map to a sheet under it.
        host.update(cx, |host, cx| { host.bounds = Bounds::new(point(px(80.0), px(50.0)), size(px(520.0), px(640.0))); cx.notify(); });
        let mut path = Vec::new();
        for _ in 0..60 {
            draw(cx);
            path.push(title(cx));
        }
        let (first, last) = (path[0], path[path.len() - 1]);
        let travel = ((last.0 - beside.0).powi(2) + (last.1 - beside.1).powi(2)).sqrt();
        assert!(travel > 150.0, "reduced {reduced}: the sheet is a long way from the card ({travel:.0} px)");
        let from_first = ((last.0 - first.0).powi(2) + (last.1 - first.1).powi(2)).sqrt();
        if reduced {
            assert!(from_first < 1.0, "reduced motion: the card is at its place on the first frame ({from_first:.0} px short)");
            continue;
        }
        assert!(from_first > 0.7 * travel, "the words start near the old place: {from_first:.0} of {travel:.0} px still to go on the first frame");
        let mut widest = 0.0_f32;
        let mut between = 0;
        for pair in path.windows(2) {
            let step = ((pair[1].0 - pair[0].0).powi(2) + (pair[1].1 - pair[0].1).powi(2)).sqrt();
            widest = widest.max(step);
            let left = ((last.0 - pair[1].0).powi(2) + (last.1 - pair[1].1).powi(2)).sqrt();
            if left > 8.0 && left < from_first - 8.0 {
                between += 1;
            }
        }
        assert!(between >= 4, "carried through {between} frames between the two places: {path:?}");
        assert!(widest < 0.6 * travel, "one frame covered {widest:.0} of {travel:.0} px");
    }
}
