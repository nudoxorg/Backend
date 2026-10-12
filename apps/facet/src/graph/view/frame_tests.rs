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

#[gpui::test]
fn native_secondary_find_edits_locally_without_declaration_activation(cx: &mut TestAppContext) {
    use super::InteractionPhase;
    use std::rc::Rc;
    cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); });
    for local_allowed in [true, false] {
        for chord in ["secondary-k", "/", "secondary-alt-k"] {
            let (graph, cx) = cx.add_window_view(|window, cx| {
                let mut graph = GraphView::with_scene(scene(), Start::World, window, cx);
                graph.on_interaction_admission(Rc::new(move |phase, _| match phase {
                    InteractionPhase::Render => true,
                    InteractionPhase::LocalFocus => local_allowed,
                    InteractionPhase::Activate => false,
                }));
                graph
            });
            draw(cx);
            cx.update(|window, cx| window.focus(&graph.focus_handle(cx), cx));
            cx.simulate_keystrokes(chord);
            draw(cx);
            let opens = local_allowed && chord != "secondary-alt-k";
            cx.update(|window, cx| {
                let graph = graph.read(cx);
                assert_eq!(graph.find_open(), opens, "chord={chord}, local_allowed={local_allowed}");
                assert_eq!(graph.find_focused(window, cx), opens,
                    "the real query input, not a logical selection, owns local typing");
            });
            if opens {
                cx.simulate_input("Page");
                cx.simulate_keystrokes("enter");
                draw(cx);
                cx.update(|window, cx| {
                    let graph = graph.read(cx);
                    assert_eq!(graph.find.read(cx).value().as_ref(), "Page");
                    assert!(graph.find_open() && graph.find_focused(window, cx));
                    assert_eq!(graph.focused(), None,
                        "local query editing cannot borrow declaration activation authority");
                });
            }
        }
    }
}


// These targets are read from the committed native tree, never semantic
// callbacks. Bounds, role, action and authored identity must agree.
fn tour_button(cx: &mut VisualTestContext, id: gpui::ElementId, label: &str)
    -> (gpui::accesskit::NodeId, gpui::Point<Pixels>) {
    cx.update(|window, _| {
        let tree = window.a11y_tree().expect("forced tour tree");
        let matches = tree.nodes.iter().filter(|(_, node)| node.label() == Some(label)).collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "one authored tour control: {label}");
        let (target, node) = matches[0];
        assert_eq!(node.role(), gpui::Role::Button);
        assert!(!node.is_disabled() && node.supports_action(gpui::AccessibleAction::Click));
        let json: serde_json::Value = serde_json::from_str(&window.debug_a11y_tree_json().expect("native identity")).expect("tree JSON");
        let ids = json["nodes"].as_object().expect("nodes").values()
            .filter(|node| node["element_id"].as_str() == Some(format!("{id:?}").as_str())).collect::<Vec<_>>();
        assert_eq!(ids.len(), 1, "one authored native ID");
        assert_eq!(ids[0]["aria"]["label"].as_str(), Some(label));
        let actual = gpui::accesskit::NodeId(ids[0]["accesskit_id"].as_str().expect("AccessKit id").parse().expect("node id"));
        assert_eq!(actual, *target, "authored geometry and actual action are one target");
        let bounds = window.a11y_node_bounds(*target).expect("native control bounds");
        assert!(bounds.size.width > px(0.0) && bounds.size.height > px(0.0));
        (*target, bounds.center())
    })
}
fn tour_ax_click(cx: &mut VisualTestContext, target: gpui::accesskit::NodeId) {
    cx.update(|window, cx| window.simulate_a11y_action(gpui::accesskit::ActionRequest {
        action: gpui::AccessibleAction::Click, target_tree: gpui::accesskit::TreeId::ROOT,
        target_node: target, data: None,
    }, cx));
}

// A native button activates on the matching release. GPUI's text-oriented
// simulate_keystrokes helper emits KeyDown only, so it cannot click a button.
fn tour_key_click(cx: &mut VisualTestContext, key: &str) {
    let keystroke = gpui::Keystroke::parse(key).expect("native tour button key");
    cx.update(|window, cx| {
        window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
            keystroke: keystroke.clone(), is_held: false, prefer_character_input: false,
        }), cx);
        window.dispatch_event(gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }), cx);
    });
    cx.run_until_parked();
}

#[gpui::test]
fn retained_tour_buttons_share_native_pointer_keyboard_and_accessibility_without_open_authority(cx: &mut TestAppContext) {
    use std::{cell::Cell, rc::Rc};
    cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); });
    for transport in ["pointer", "keyboard", "accessibility"] {
        let scene = scene();
        let expected = Discovery::from_prepared(Discovery::prepare(&scene.world)).package_tour(1).expect("real tour").clone();
        assert!(expected.shown() && expected.stops.len() > 1);
        let opened = Rc::new(Cell::new(0));
        let (graph, cx) = cx.add_window_view(|window, cx| {
            let mut graph = GraphView::with_scene(scene.clone(), Start::Focus(3), window, cx);
            graph.on_interaction_admission(Rc::new(|phase, _| phase == super::InteractionPhase::LocalFocus));
            let opened = opened.clone();
            graph.on_open(Rc::new(move |_, _, _| opened.set(opened.get() + 1)));
            graph
        });
        cx.update(|window, _| window.set_a11y_forced(true));
        settle(cx);
        let label = format!("Start a guided tour of {}", scene.world.packages[1].name);
        let (start, at) = tour_button(cx, "graph-start-here".into(), &label);
        match transport {
            "pointer" => {
                cx.simulate_mouse_down(at, gpui::MouseButton::Left, gpui::Modifiers::none());
                assert_eq!(graph.read_with(cx, |graph, _| graph.tour_stop()), None, "press alone never starts a tour");
                cx.simulate_mouse_up(at, gpui::MouseButton::Left, gpui::Modifiers::none());
            }
            "keyboard" => {
                cx.update(|window, cx| {
                    let stop = graph.read(cx).tour_focus[0].clone();
                    assert!(window.is_focus_handle_mounted(&stop));
                    stop.focus(window, cx);
                });
                tour_key_click(cx, "enter");
            }
            _ => tour_ax_click(cx, start),
        }
        settle(cx);
        assert_eq!(graph.read_with(cx, |graph, _| graph.tour_stop()), Some(expected.stops[0].node), "{transport}");
        cx.update(|window, cx| {
            let receiver = graph.read(cx).focus_handle.clone();
            window.focus(&receiver, cx);
            let step = graph.update(cx, |graph, cx| graph.step_native(true, window, cx));
            assert_eq!(step, super::NativeFocusStep::Moved);
            let graph = graph.read(cx);
            assert!(graph.tour_focus[1].is_focused(window), "Tab enters the current tour stop without opening Find");
            assert!(!graph.find_open() && graph.focused().is_none());
        });
        let label = format!("Tour stop 2: {}", scene.world.name_of(expected.stops[1].node));
        let (second, at) = tour_button(cx, ("graph-tour-stop", 1usize).into(), &label);
        match transport {
            "pointer" => {
                cx.simulate_mouse_down(at, gpui::MouseButton::Left, gpui::Modifiers::none());
                assert_eq!(graph.read_with(cx, |graph, _| graph.tour_stop()), Some(expected.stops[0].node));
                cx.simulate_mouse_up(at, gpui::MouseButton::Left, gpui::Modifiers::none());
            }
            "keyboard" => {
                cx.update(|window, cx| graph.read(cx).tour_focus[2].clone().focus(window, cx));
                tour_key_click(cx, "space");
            }
            _ => tour_ax_click(cx, second),
        }
        settle(cx);
        assert_eq!(graph.read_with(cx, |graph, _| graph.tour_stop()), Some(expected.stops[1].node), "{transport}");
        cx.update(|window, cx| window.focus(&graph.focus_handle(cx), cx));
        tour_key_click(cx, "enter");
        assert_eq!(opened.get(), 0, "retained tour navigation grants no declaration-open capability");
        assert_eq!(graph.read_with(cx, |graph, _| graph.focused()), None);
    }
}

#[gpui::test]
fn retained_tour_keys_work_offline_but_retire_with_the_reading_visit(cx: &mut TestAppContext) {
    use std::{cell::Cell, rc::Rc};
    cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); });
    let scene = scene();
    let expected = Discovery::from_prepared(Discovery::prepare(&scene.world)).package_tour(1).expect("tour").clone();
    let allowed = Rc::new(Cell::new(true));
    let (graph, cx) = cx.add_window_view(|window, cx| {
        let mut graph = GraphView::with_scene(scene, Start::Focus(3), window, cx);
        let allowed = allowed.clone();
        graph.on_interaction_admission(Rc::new(move |phase, _| phase == super::InteractionPhase::LocalFocus && allowed.get()));
        graph
    });
    settle(cx);
    cx.update(|window, cx| window.focus(&graph.focus_handle(cx), cx));
    for (key, stop) in [("t", 0), ("right", 1), ("left", 0), ("space", 1)] {
        cx.simulate_keystrokes(key); settle(cx);
        assert_eq!(graph.read_with(cx, |graph, _| graph.tour_stop()), Some(expected.stops[stop].node), "{key}");
    }
    allowed.set(false);
    cx.simulate_keystrokes("left"); cx.simulate_keystrokes("escape");
    assert_eq!(graph.read_with(cx, |graph, _| graph.tour_stop()), Some(expected.stops[1].node), "covered or retired visit cannot navigate the retained tour");
    allowed.set(true);
    cx.simulate_keystrokes("escape");
    assert_eq!(graph.read_with(cx, |graph, _| graph.tour_stop()), None);
}

#[gpui::test]
fn committed_tour_controls_cannot_adopt_a_new_visit_stop_or_scene(cx: &mut TestAppContext) {
    use std::{cell::Cell, rc::Rc};
    cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); });
    let scene = scene();
    let expected = Discovery::from_prepared(Discovery::prepare(&scene.world)).package_tour(1).expect("tour").clone();
    let allowed = Rc::new(Cell::new(true));
    let (graph, cx) = cx.add_window_view(|window, cx| {
        let mut graph = GraphView::with_scene(scene.clone(), Start::Focus(3), window, cx);
        let allowed = allowed.clone();
        graph.on_interaction_admission(Rc::new(move |phase, _| phase == super::InteractionPhase::LocalFocus && allowed.get()));
        graph
    });
    cx.update(|window, _| window.set_a11y_forced(true)); settle(cx);
    let label = format!("Start a guided tour of {}", scene.world.packages[1].name);
    let (start, at) = tour_button(cx, "graph-start-here".into(), &label);
    cx.simulate_mouse_down(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    allowed.set(false); // Cover arrives between real press and release, before redraw.
    let covered_focus = cx.update(|window, cx| (window.focused(cx), window.focus_epoch()));
    cx.simulate_mouse_up(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    tour_ax_click(cx, start);
    tour_key_click(cx, "enter");
    assert_eq!(graph.read_with(cx, |graph, _| graph.tour_stop()), None);
    assert_eq!(cx.update(|window, cx| (window.focused(cx), window.focus_epoch())), covered_focus,
        "denied release, keyboard and AX cannot establish a new focus intent");
    allowed.set(true);
    cx.update(|window, cx| {
        let frame = window.a11y_frame_number();
        // Publish the real navigation transition without dispatching a key:
        // key dispatch draws a dirty window, so its old numeric AX ID would
        // otherwise refer to a newly painted current control.
        assert!(graph.update(cx, |graph, cx| graph.start_tour(1, 0, cx)));
        let current_generation = graph.read(cx).state.generation;
        window.simulate_a11y_action(gpui::accesskit::ActionRequest {
            action: gpui::AccessibleAction::Click, target_tree: gpui::accesskit::TreeId::ROOT,
            target_node: start, data: None,
        }, cx);
        assert_eq!(graph.read(cx).state.generation, current_generation,
            "old start capability cannot restart the new tour even at its first stop");
        assert_eq!(graph.read(cx).tour_stop(), Some(expected.stops[0].node));
        assert_eq!(window.a11y_frame_number(), frame, "the AX action still addresses the original committed control");
    });
    settle(cx);
    let label = format!("Tour stop 1: {}", scene.world.name_of(expected.stops[0].node));
    let (first, _) = tour_button(cx, ("graph-tour-stop", 0usize).into(), &label);
    cx.update(|window, cx| {
        let frame = window.a11y_frame_number();
        graph.update(cx, |graph, cx| graph.go_stop(1, cx));
        window.simulate_a11y_action(gpui::accesskit::ActionRequest {
            action: gpui::AccessibleAction::Click, target_tree: gpui::accesskit::TreeId::ROOT,
            target_node: first, data: None,
        }, cx);
        assert_eq!(graph.read(cx).tour_stop(), Some(expected.stops[1].node), "the old exact-stop callback is retired");
        assert_eq!(window.a11y_frame_number(), frame, "no current stop listener has replaced the old AX action");
    });
    settle(cx);
    let (first, _) = tour_button(cx, ("graph-tour-stop", 0usize).into(), &label);
    cx.update(|window, cx| {
        let frame = window.a11y_frame_number();
        graph.update(cx, |graph, _| graph.scene = Some(Arc::new(Scene::new(scene.world.clone(), scene.layout.clone()))));
        window.simulate_a11y_action(gpui::accesskit::ActionRequest {
            action: gpui::AccessibleAction::Click, target_tree: gpui::accesskit::TreeId::ROOT,
            target_node: first, data: None,
        }, cx);
        assert_eq!(graph.read(cx).tour_stop(), Some(expected.stops[1].node), "the old scene callback is retired");
        assert_eq!(window.a11y_frame_number(), frame, "the replaced scene has not been painted into the old control");
        graph.update(cx, |_, cx| cx.notify());
    });
    settle(cx);
    let (fresh, _) = tour_button(cx, ("graph-tour-stop", 0usize).into(), &label);
    tour_ax_click(cx, fresh);
    assert_eq!(graph.read_with(cx, |graph, _| graph.tour_stop()), Some(expected.stops[0].node),
        "a freshly queried current-scene AX control remains usable");
}
#[gpui::test]
fn stale_tour_focus_cannot_lend_local_enter_to_a_new_find_owner(cx: &mut TestAppContext) {
    use std::rc::Rc;
    cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); });
    let scene = scene();
    let search = Discovery::query_prepared(Discovery::prepare(&scene.world), &scene.world, "Page");
    assert_eq!(search.rows.first().map(|row| row.node), Some(0), "independent prepared Find result");
    let (graph, cx) = cx.add_window_view(|window, cx| {
        let mut graph = GraphView::with_scene(scene, Start::Focus(3), window, cx);
        graph.on_interaction_admission(Rc::new(|phase, _| phase == super::InteractionPhase::LocalFocus));
        graph
    });
    settle(cx);
    cx.update(|window, cx| {
        let stop = graph.read(cx).tour_focus[0].clone();
        assert!(window.is_focus_handle_mounted(&stop));
        stop.focus(window, cx);
    });
    // Hold the original native tree while Find's actual result and current
    // resource producer advance. Its old local tour handle is still focused.
    graph.update(cx, |graph, cx| {
        graph.state.apply(super::Event::OpenFind);
        graph.deliver_search(graph.state.generation, "Page", search, cx);
        graph.on_interaction_admission(Rc::new(|_, _| true));
    });
    let focus = cx.update(|window, cx| (window.focused(cx), window.focus_epoch()));
    cx.simulate_keystrokes("enter");
    graph.read_with(cx, |graph, _| {
        assert!(graph.find_open(), "old tour Enter cannot select Find using a new producer's authority");
        assert_eq!(graph.focused(), Some(3), "the newly ready result cannot replace the current selection");
        assert_eq!(graph.results.first(), Some(&0));
    });
    assert_eq!(cx.update(|window, cx| (window.focused(cx), window.focus_epoch())), focus);
}

#[gpui::test]
fn focused_native_tour_chips_delegate_arrows_and_escape_to_the_current_graph(cx: &mut TestAppContext) {
    use std::{cell::Cell, rc::Rc};
    cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); });
    let scene = scene();
    let expected = Discovery::from_prepared(Discovery::prepare(&scene.world)).package_tour(1).expect("tour").clone();
    let allowed = Rc::new(Cell::new(true));
    let (graph, cx) = cx.add_window_view(|window, cx| {
        let mut graph = GraphView::with_scene(scene, Start::Focus(3), window, cx);
        let allowed = allowed.clone();
        graph.on_interaction_admission(Rc::new(move |phase, _| phase == super::InteractionPhase::LocalFocus && allowed.get()));
        graph
    });
    settle(cx);
    cx.update(|window, cx| window.focus(&graph.focus_handle(cx), cx));
    cx.simulate_keystrokes("t"); settle(cx);
    for (key, stop, slot) in [("right", Some(1), 1), ("left", Some(0), 2), ("escape", None, 1)] {
        cx.update(|window, cx| {
            let stop = graph.read(cx).tour_focus[slot].clone();
            assert!(window.is_focus_handle_mounted(&stop));
            stop.focus(window, cx);
        });
        allowed.set(false);
        cx.simulate_keystrokes(key);
        cx.update(|window, cx| assert!(graph.read(cx).tour_focus[slot].is_focused(window), "a cover cannot hand off native focus"));
        allowed.set(true);
        cx.simulate_keystrokes(key);
        settle(cx);
        cx.update(|window, cx| {
            let graph = graph.read(cx);
            assert_eq!(graph.tour_stop(), stop.map(|at| expected.stops[at].node), "{key}");
            assert!(graph.focus_handle.is_focused(window), "admitted local key keeps a mounted persistent receiver");
            assert!(graph.focused().is_none() && !graph.find_open());
        });
    }
    cx.simulate_keystrokes("t"); settle(cx);
    cx.update(|window, cx| graph.read(cx).tour_focus[1].clone().focus(window, cx));
    cx.simulate_keystrokes("secondary-k"); draw(cx);
    cx.update(|window, cx| {
        let graph = graph.read(cx);
        assert!(graph.find_open() && graph.find_focused(window, cx), "the native chip yields Ctrl/Cmd+K to Graph's real local input");
        assert!(graph.tour_stop().is_none() && graph.focused().is_none());
    });
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
fn restored_selection_keeps_exact_camera_through_first_measured_frame_without_new_history(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    for reduced in [false, true] {
        cx.update(|cx| set_facet(Facet { reduced_motion: reduced, ..Facet::default() }, cx));
        let scene = scene();
        let camera = crate::motion::Camera::new(f64::from(scene.layout.x[3]) + 13.25,
            f64::from(scene.layout.y[3]) - 8.75, 91.5);
        let (graph, cx) = cx.add_window_view(|window, cx|
            GraphView::with_scene(scene.clone(), Start::Restore { camera, focus: Some(3) }, window, cx));
        assert_eq!(graph.read_with(cx, |graph, _| graph.focused()), Some(3), "semantics publish before first paint");
        draw(cx);
        graph.read_with(cx, |graph, _| {
            assert_eq!(graph.camera(), Some(camera), "actual reading chrome cannot overwrite retained placement");
            assert_eq!(graph.prism.as_ref().map(|prism| (prism.node, prism.g)), Some((3, 1.0)));
            assert!(graph.trail.is_empty(), "restoration is not a navigation visit");
            assert!(!graph.find_open() && graph.results.is_empty() && graph.state.pending_accept.is_none());
            assert!(graph.hover.is_none() && graph.peek.is_none());
        });
        settle(cx);
        assert_eq!(graph.read_with(cx, |graph, _| graph.camera()), Some(camera));
        graph.update(cx, |graph, cx| graph.enter(0, cx));
        assert_eq!(graph.read_with(cx, |graph, _| graph.camera()), Some(camera),
            "a restored map begins the next semantic flight from its retained placement, without an artificial package reset");
        settle(cx);
        graph.read_with(cx, |graph, _| {
            assert_eq!(graph.focused(), Some(0));
            assert_ne!(graph.camera(), Some(camera), "later semantic navigation owns framing");
            assert!(!graph.retained_placement);
        });
        graph.update(cx, |graph, cx| graph.show_world(cx));
        settle(cx);
        assert_eq!(graph.read_with(cx, |graph, _| graph.focused()), None);
    }
}

#[gpui::test]
fn invalid_passive_restoration_clears_selection_before_prism_construction(cx: &mut TestAppContext) {
    cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); });
    let scene = scene();
    let outside = u32::try_from(scene.world.len()).expect("tiny fixture node limit");
    let camera = crate::motion::Camera::new(8.25, -11.5, 101.75);
    for asynchronous in [false, true] {
        for invalid in [outside, u32::MAX] {
            let start = Start::Restore { camera, focus: Some(invalid) };
            let (graph, cx) = cx.add_window_view(|window, cx| {
                if asynchronous { GraphView::new(scene.world.clone(), start, window, cx) }
                else { GraphView::with_scene(scene.clone(), start, window, cx) }
            });
            graph.read_with(cx, |graph, _| {
                assert_eq!(graph.focused(), None, "invalid restoration never selects a substitute raw id");
                assert!(graph.prism.is_none(), "no Prism::of access occurs for an unadmitted node");
                assert_eq!(graph.start, Start::Restore { camera, focus: None });
            });
            settle(cx);
            graph.read_with(cx, |graph, _| {
                assert_eq!(graph.camera(), Some(camera));
                assert_eq!(graph.focused(), None);
                assert!(graph.prism.is_none() && graph.trail.is_empty());
            });
        }
    }
}

#[gpui::test]
fn restored_camera_survives_resize_and_double_text_scale_with_reachable_card_controls(cx: &mut TestAppContext) {
    cx.update(|cx| { gpui_component::init(cx); crate::probe::enable(cx); });
    for reduced in [false, true] {
        cx.update(|cx| set_facet(Facet { reduced_motion: reduced, ..Facet::default() }, cx));
        let scene = scene();
        let camera = crate::motion::Camera::new(f64::from(scene.layout.x[0]) + 12.0,
            f64::from(scene.layout.y[0]) - 7.0, 83.5);
        let (host, cx) = cx.add_window_view(|window, cx| Region {
            graph: cx.new(|cx| {
                let mut graph = GraphView::with_scene(scene.clone(), Start::Restore { camera, focus: Some(0) }, window, cx);
                graph.set_status("Observed graph".into(), Some("Observed declaration and relation coverage".into()), cx);
                graph
            }),
            bounds: Bounds::new(point(px(80.0), px(50.0)), size(px(900.0), px(640.0))),
        });
        let graph = host.read_with(cx, |host, _| host.graph.clone());
        cx.update(|window, _| window.set_a11y_forced(true));
        settle(cx);
        for (width, text_scale) in [(480.0, 1.0), (900.0, 2.0), (480.0, 2.0), (900.0, 1.0)] {
            cx.update(|_, cx| set_facet(Facet { reduced_motion: reduced, text_scale, ..Facet::default() }, cx));
            let region = Bounds::new(point(px(120.0), px(60.0)), size(px(width), px(620.0)));
            host.update(cx, |host, cx| { host.bounds = region; cx.notify(); });
            draw(cx);
            graph.read_with(cx, |graph, _| {
                assert_eq!(graph.camera(), Some(camera), "the first measured resize frame retains actual placement");
                assert_eq!(graph.focused(), Some(0));
                let card = graph.card_bounds.expect("new measured reading chrome");
                assert!(card.left() >= region.left() && card.top() >= region.top()
                    && card.right() <= region.right() && card.bottom() <= region.bottom());
            });
            settle(cx);
            let card = graph.read_with(cx, |graph, _| graph.card_bounds.expect("settled native card"));
            let wheel = |cx: &mut VisualTestContext, delta| cx.simulate_event(gpui::ScrollWheelEvent {
                position: card.center(), delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(delta))),
                modifiers: gpui::Modifiers::none(), touch_phase: gpui::TouchPhase::Moved,
            });
            wheel(cx, 3000.0); draw(cx);
            cx.update(|_, cx| { crate::probe::take(cx); });
            draw(cx);
            let ledger = cx.update(|_, cx| crate::probe::take(cx));
            assert_content(&ledger, card);
            wheel(cx, -3000.0); draw(cx);
            let (target, bounds) = cx.update(|window, _| {
                let tree = window.a11y_tree().expect("native card controls");
                let target = tree.nodes.iter().find(|(_, node)| node.label() == Some("Graph coverage"))
                    .map(|(id, node)| { assert!(!node.is_disabled() && node.supports_action(gpui::AccessibleAction::Click)); *id })
                    .expect("coverage remains a reachable native control");
                let json: serde_json::Value = serde_json::from_str(&window.debug_a11y_tree_json().expect("actual native bounds")).expect("native tree JSON");
                let bounds = json["nodes"].as_object().expect("nodes").values()
                    .find(|node| node["aria"]["label"].as_str() == Some("Graph coverage"))
                    .expect("coverage geometry")["bounds"].clone();
                (target, bounds)
            });
            let x = bounds["x"].as_f64().expect("x"); let y = bounds["y"].as_f64().expect("y");
            let w = bounds["width"].as_f64().expect("width"); let h = bounds["height"].as_f64().expect("height");
            assert!(w > 0.0 && h > 0.0 && x >= f64::from(f32::from(card.left())) - 0.5
                && y >= f64::from(f32::from(card.top())) - 0.5
                && x + w <= f64::from(f32::from(card.right())) + 0.5
                && y + h <= f64::from(f32::from(card.bottom())) + 0.5,
                "real card scrolling reveals coverage within its painted viewport: {bounds:?}, {card:?}");
            cx.update(|window, cx| window.simulate_a11y_action(gpui::accesskit::ActionRequest {
                action: gpui::AccessibleAction::Click, target_tree: gpui::accesskit::TreeId::ROOT,
                target_node: target, data: None,
            }, cx));
            draw(cx);
            graph.read_with(cx, |graph, _| {
                assert!(graph.status_open, "the reachable native action really opens coverage");
                assert_eq!(graph.camera(), Some(camera), "card overflow and control activation cannot reframe the map");
                assert_eq!(graph.focused(), Some(0));
                assert!(graph.trail.is_empty());
            });
            let close = cx.update(|window, _| window.a11y_tree().expect("open coverage").nodes.iter()
                .find(|(_, node)| node.label() == Some("Hide coverage"))
                .map(|(id, _)| *id).expect("current close action"));
            cx.update(|window, cx| window.simulate_a11y_action(gpui::accesskit::ActionRequest {
                action: gpui::AccessibleAction::Click, target_tree: gpui::accesskit::TreeId::ROOT,
                target_node: close, data: None,
            }, cx));
            draw(cx);
            assert!(!graph.read_with(cx, |graph, _| graph.status_open));
        }
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
        let painted = graph.painted_labels.as_ref().expect("native labels painted");
        let (view, camera) = (&painted.view, &painted.camera);
        let label = painted.territories.iter().find(|label| label.territory.module.is_none()).expect("visible package name");
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
        let painted = graph.painted_labels.as_ref().expect("overview labels");
        painted.territories.iter().filter(|label| label.territory.module.is_none())
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


#[gpui::test]
fn painted_item_caption_owns_native_selection_and_rejects_unpainted_changes(cx: &mut TestAppContext) {
    use std::{cell::Cell, rc::Rc};
    cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); });
    let scene = scene();
    let allowed = Rc::new(Cell::new(true));
    let (graph, cx) = cx.add_window_view(|window, cx| {
        let mut graph = GraphView::with_scene(scene.clone(), Start::Focus(0), window, cx);
        assert!(!graph.paint_receipt().expect("constructed scene").was_painted(), "construction is not native paint");
        let allowed = allowed.clone();
        graph.on_interaction_admission(Rc::new(move |_, _| allowed.get())); graph
    });
    let receipt = graph.read_with(cx, |graph, _| graph.paint_receipt().expect("scene"));
    settle(cx);
    assert!(receipt.was_painted());
    for delta in [0.0, 24.0] {
        if delta != 0.0 {
            let old_at = graph.read_with(cx, |graph, _| graph.painted_labels.as_ref().expect("prior paint").nodes.iter().find(|label| label.node == 0 && label.selected).expect("prior caption").bounds.center());
            graph.update(cx, |graph, cx| { let mut camera = graph.camera().expect("camera"); camera.x += delta; camera.w *= 1.1; graph.rig.as_mut().expect("rig").set(camera); cx.notify(); });
            let focused = cx.update(|window, cx| window.focused(cx));
            cx.simulate_mouse_down(old_at, gpui::MouseButton::Left, gpui::Modifiers::none());
            cx.simulate_mouse_up(old_at, gpui::MouseButton::Left, gpui::Modifiers::none());
            assert_eq!(cx.update(|window, cx| window.focused(cx)), focused, "old camera bounds cannot acquire input before actual replacement paint");
            assert!(graph.read_with(cx, |graph, _| graph.drag.is_none()));
            draw(cx);
        }
        let at = graph.read_with(cx, |graph, cx| {
            let painted = graph.painted_labels.as_ref().expect("native metadata");
            let label = painted.nodes.iter().find(|label| label.node == 0 && label.selected).expect("accepted item caption");
            let at = label.bounds.center();
            assert_eq!(graph.painted_node(&painted.view, &painted.camera, f32::from(at.x), f32::from(at.y), cx), Some(0));
            at
        });
        // Change semantic focus without drawing a replacement caption. The
        // old visible text must not focus Graph or select its old declaration.
        graph.update(cx, |graph, _| { graph.state.focus = Some(3); });
        let focused = cx.update(|window, cx| window.focused(cx));
        cx.simulate_mouse_down(at, gpui::MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(at, gpui::MouseButton::Left, gpui::Modifiers::none());
        assert_eq!(cx.update(|window, cx| window.focused(cx)), focused);
        assert_eq!(graph.read_with(cx, |graph, _| graph.focused()), Some(3));
        graph.update(cx, |graph, cx| { graph.state.focus = Some(0); cx.notify(); }); draw(cx);
        let at = graph.read_with(cx, |graph, _| graph.painted_labels.as_ref().expect("repaint").nodes.iter().find(|label| label.node == 0 && label.selected).expect("caption").bounds.center());
        cx.simulate_mouse_down(at, gpui::MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(at, gpui::MouseButton::Left, gpui::Modifiers::none());
        assert_eq!(graph.read_with(cx, |graph, _| graph.focused()), Some(0));
        allowed.set(false);
        let focused = cx.update(|window, cx| window.focused(cx));
        cx.simulate_mouse_down(at, gpui::MouseButton::Left, gpui::Modifiers::none());
        assert_eq!(cx.update(|window, cx| window.focused(cx)), focused);
        cx.simulate_mouse_up(at, gpui::MouseButton::Left, gpui::Modifiers::none());
        allowed.set(true);
    }
}


#[gpui::test]
fn ordinary_item_words_and_marker_select_the_same_exact_declaration(cx: &mut TestAppContext) {
    cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); });
    let scene = scene();
    let (graph, cx) = cx.add_window_view(|window, cx| GraphView::with_scene(scene, Start::World, window, cx));
    settle(cx);
    let (node, at, camera) = graph.read_with(cx, |graph, _| {
        let painted = graph.painted_labels.as_ref().expect("real frame");
        let label = painted.nodes.iter().find(|label| !label.selected && !graph.over_chrome(f32::from(label.bounds.center().x), f32::from(label.bounds.center().y))).expect("visible resting item word");
        (label.node, label.bounds.center(), painted.camera)
    });
    cx.simulate_mouse_down(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(graph.read_with(cx, |graph, _| graph.focused()), Some(node), "the text selects its own declaration rather than the territory or a nearby glyph");
    settle(cx);
    graph.update(cx, |graph, cx| { graph.set_focus(None, false, cx); graph.rig.as_mut().expect("rig").set(camera); });
    draw(cx);
    let marker = graph.read_with(cx, |graph, _| graph.node_bounds(node)).expect("actual marker").center();
    cx.simulate_mouse_down(marker, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(marker, gpui::MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(graph.read_with(cx, |graph, _| graph.focused()), Some(node), "the independent marker path keeps the same exact identity");
}
