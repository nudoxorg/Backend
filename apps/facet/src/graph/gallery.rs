//! Graph scenes over the real workspace (`Nudox-Design-System/v4/graph/
//! world.json`): the altitudes, hover, focus + prism, the two flights of
//! the prototype's stills, and a storm. Each scene is the prototype's canvas
//! region at 1440 × 824 (its 1440 × 900 window less the 50 px titlebar and
//! the 26 px status bar).
//!
//! `…-proto` scenes draw the prototype's own positions (`world.js`) so a
//! side-by-side compares the renderer alone; the others draw the Rust
//! layout.

use super::draw::Strategy;
use super::layout::{Box2, Layout, Territory, layout_of};
use super::model::{NodeId, World};
use super::scene::Scene as MapScene;
use super::view::{GraphView, Start};
use crate::gallery::{
    Scene, declare_adapter, declare_annotator, declare_quiet, declare_script, declare_state,
};
use crate::motion::Camera;
use crate::overlay::float;
use crate::theme::ActiveFacet;
use gpui::{
    AnyView, App, AppContext, Context, Entity, Focusable, IntoElement, ParentElement, Render,
    Styled, Window, div,
};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// The prototype's canvas.
const CANVAS: (u32, u32) = (1440, 824);

#[cfg(test)]
#[path = "gallery_checks.rs"]
mod adversarial;
#[path = "gallery_fixture.rs"]
mod pinned;

pub(crate) const SCENES: &[Scene] = &[
    Scene {
        id: "graph-check-hover-phases",
        title: "Live: actual dense hover, leave, drag onset and rapid sparse handoff",
        size: CANVAS,
        build: |w,cx| build(Src::Rust,At::CheckHoverPhases,w,cx),
    },
    Scene {
        id: "graph-live-dense-focus",
        title: "Live: qualified Debug search, high-fanout focus at narrow scale",
        size: CANVAS,
        build: |w,cx| build(Src::Rust,At::DenseFocus,w,cx),
    },
    Scene {
        id: "graph-check-live-hover-naive",
        title: "Control: identical real high-fanout hover, naive drawing",
        size: CANVAS,
        build: |w,cx| with(Strategy::Naive,Src::Rust,At::CheckLiveHover,w,cx),
    },
    Scene {
        id: "graph-check-live-hover-paths",
        title: "Control: identical real high-fanout hover, all paths drawing",
        size: CANVAS,
        build: |w,cx| with(Strategy::AllPaths,Src::Rust,At::CheckLiveHover,w,cx),
    },
    Scene {
        id: "graph-check-parked",
        title: "Pinned: parked pointer tracks actual prism geometry through resize and text scale",
        size: CANVAS,
        build: |w, cx| build(Src::Pinned, At::CheckParked, w, cx),
    },
    Scene {
        id: "graph-check-peek",
        title: "Pinned: same-symbol native hover jitter survives intent and retires on leave",
        size: CANVAS,
        build: |w, cx| build(Src::Pinned, At::CheckPeek, w, cx),
    },
    Scene {
        id: "graph-pinned-offset",
        title: "Pinned: narrow canvas at a nonzero window origin",
        size: (760, 900),
        build: offset,
    },
    Scene {
        id: "graph-check-chain",
        title: "Pinned: two-call shape road, hold, code x-ray, Escape",
        size: CANVAS,
        build: |w, cx| build(Src::Pinned, At::CheckChain, w, cx),
    },
    Scene {
        id: "graph-check-hold",
        title: "Pinned: hold stops a live camera flight without dragging",
        size: CANVAS,
        build: |w, cx| build(Src::Pinned, At::CheckHold, w, cx),
    },
    Scene {
        id: "graph-check-live-hover",
        title: "Live invariant: rapid real-pointer hover on largest fanout",
        size: CANVAS,
        build: |w, cx| build(Src::Rust, At::CheckLiveHover, w, cx),
    },
    Scene {
        id: "graph-check-reach-tour",
        title: "Pinned: reach waves, tour walk, Enter and Escape",
        size: CANVAS,
        build: |w, cx| build(Src::Pinned, At::CheckReachTour, w, cx),
    },
    Scene {
        id: "graph-check-idle",
        title: "Pinned: input after idle, then stop requesting frames",
        size: CANVAS,
        build: |w, cx| build(Src::Pinned, At::CheckIdle, w, cx),
    },
    Scene {
        id: "graph-check-interrupt",
        title: "Pinned: rapid world, symbol, cross-package, back interruptions",
        size: CANVAS,
        build: |w, cx| build(Src::Pinned, At::CheckInterrupt, w, cx),
    },
    Scene {
        id: "graph-check-wheel-drag",
        title: "Pinned: wheel interrupts flight, drag interrupts zoom",
        size: CANVAS,
        build: |w, cx| build(Src::Pinned, At::CheckWheelDrag, w, cx),
    },
    Scene {
        id: "graph-check-hover",
        title: "Pinned: rapid hover sweep, leave, long quiet tail",
        size: CANVAS,
        build: |w, cx| build(Src::Pinned, At::CheckHover, w, cx),
    },
    Scene {
        id: "graph-check-keyboard",
        title: "Pinned: find, prism arrow walk, Enter and Escape",
        size: CANVAS,
        build: |w, cx| build(Src::Pinned, At::CheckKeyboard, w, cx),
    },
    Scene {
        id: "graph-check-weather",
        title: "Pinned: narrow light view and reduced motion during flight",
        size: CANVAS,
        build: |w, cx| build(Src::Pinned, At::CheckWeather, w, cx),
    },
    Scene {
        id: "graph-pinned-world",
        title: "Pinned world for settle equals fresh",
        size: CANVAS,
        build: |w, cx| build(Src::Pinned, At::World, w, cx),
    },
    Scene {
        id: "graph-pinned-focus",
        title: "Pinned RelationLabel for reduced-motion and responsive checks",
        size: CANVAS,
        build: |w, cx| {
            build(
                Src::Pinned,
                At::Focus("present::glyph", "RelationLabel"),
                w,
                cx,
            )
        },
    },
    Scene {
        id: "graph-world",
        title: "World graph: the whole workspace (Rust layout)",
        size: CANVAS,
        build: |w, cx| build(Src::Rust, At::World, w, cx),
    },
    Scene {
        id: "graph-world-proto",
        title: "World graph: the whole workspace (prototype positions)",
        size: CANVAS,
        build: |w, cx| build(Src::Proto, At::World, w, cx),
    },
    Scene {
        id: "graph-present",
        title: "Package altitude: backend-present framed (Rust layout)",
        size: CANVAS,
        build: |w, cx| build(Src::Rust, At::Package("backend-present"), w, cx),
    },
    Scene {
        id: "graph-present-proto",
        title: "Package altitude: backend-present framed (prototype positions)",
        size: CANVAS,
        build: |w, cx| build(Src::Proto, At::Package("backend-present"), w, cx),
    },
    Scene {
        id: "graph-glyph",
        title: "Module altitude: present::glyph framed (Rust layout)",
        size: CANVAS,
        build: |w, cx| build(Src::Rust, At::Module("backend-present", "glyph"), w, cx),
    },
    Scene {
        id: "graph-glyph-proto",
        title: "Module altitude: present::glyph framed (prototype positions)",
        size: CANVAS,
        build: |w, cx| build(Src::Proto, At::Module("backend-present", "glyph"), w, cx),
    },
    Scene {
        id: "graph-members",
        title: "Symbol altitude: RelationLabel's member shells (Rust layout)",
        size: CANVAS,
        build: |w, cx| {
            build(
                Src::Rust,
                At::Close("present::glyph", "RelationLabel", 24.0),
                w,
                cx,
            )
        },
    },
    Scene {
        id: "graph-hover",
        title: "Hover: RelationLabel's neighbourhood lit, bundled, flowing (Rust layout)",
        size: CANVAS,
        build: |w, cx| {
            build(
                Src::Rust,
                At::Hover(
                    "backend-present",
                    "glyph",
                    "present::glyph",
                    "RelationLabel",
                ),
                w,
                cx,
            )
        },
    },
    Scene {
        id: "graph-hover-proto",
        title: "Hover: RelationLabel's neighbourhood (prototype positions)",
        size: CANVAS,
        build: |w, cx| {
            build(
                Src::Proto,
                At::Hover(
                    "backend-present",
                    "glyph",
                    "present::glyph",
                    "RelationLabel",
                ),
                w,
                cx,
            )
        },
    },
    Scene {
        id: "graph-focus",
        title: "Focus + prism: present::page::RelationGroup (Rust layout)",
        size: CANVAS,
        build: |w, cx| {
            build(
                Src::Rust,
                At::Focus("present::page", "RelationGroup"),
                w,
                cx,
            )
        },
    },
    Scene {
        id: "graph-focus-proto",
        title: "Focus + prism: present::page::RelationGroup (prototype positions)",
        size: CANVAS,
        build: |w, cx| {
            build(
                Src::Proto,
                At::Focus("present::page", "RelationGroup"),
                w,
                cx,
            )
        },
    },
    Scene {
        id: "graph-flight-a",
        title: "Flight A: world → RelationLabel through find (/, type, ↵), prism gathers",
        size: CANVAS,
        build: |w, cx| build(Src::Rust, At::FlightA, w, cx),
    },
    Scene {
        id: "graph-flight-b",
        title: "Flight B: RelationLabel → serde_json::de::from_str (out, across, in, gather)",
        size: CANVAS,
        build: |w, cx| build(Src::Rust, At::FlightB, w, cx),
    },
    Scene {
        id: "graph-flight-a-proto",
        title: "Flight A over the prototype's positions (for frame-by-frame comparison)",
        size: CANVAS,
        build: |w, cx| build(Src::Proto, At::FlightA, w, cx),
    },
    Scene {
        id: "graph-flight-b-proto",
        title: "Flight B over the prototype's positions (for frame-by-frame comparison)",
        size: CANVAS,
        build: |w, cx| build(Src::Proto, At::FlightB, w, cx),
    },
    Scene {
        id: "graph-journey",
        title: "Journey: world → (wheel) backend-present → (find, ↵) RelationLabel, prism gathers → Esc → Esc: back to the world",
        size: CANVAS,
        build: |w, cx| build(Src::Rust, At::Journey, w, cx),
    },
    Scene {
        id: "graph-storm",
        title: "Storm ground: the world at package altitude for pan/zoom/focus storms",
        size: CANVAS,
        build: |w, cx| build(Src::Rust, At::Package("backend-engine"), w, cx),
    },
    Scene {
        id: "graph-baseline",
        title: "Perf baseline: a blank canvas (GPU readback cost)",
        size: CANVAS,
        build: blank,
    },
    Scene {
        id: "graph-baseline-cut",
        title: "Perf baseline: a blank canvas and one cut plate (the fixed cost of one MSAA path pass)",
        size: CANVAS,
        build: blank_cut,
    },
    Scene {
        id: "graph-engine",
        title: "Perf: backend-engine framed, ~2 700 symbols as shapes",
        size: CANVAS,
        build: |w, cx| build(Src::Rust, At::Package("backend-engine"), w, cx),
    },
    Scene {
        id: "graph-engine-naive",
        title: "Perf: backend-engine framed, one path per shape",
        size: CANVAS,
        build: |w, cx| {
            with(
                Strategy::Naive,
                Src::Rust,
                At::Package("backend-engine"),
                w,
                cx,
            )
        },
    },
    Scene {
        id: "graph-world-naive",
        title: "Perf: the world, one path per shape and one quad per star, no layer",
        size: CANVAS,
        build: |w, cx| with(Strategy::Naive, Src::Rust, At::World, w, cx),
    },
    Scene {
        id: "graph-world-paths",
        title: "Perf: the world, stars as triangles in the batched paths (no quads)",
        size: CANVAS,
        build: |w, cx| with(Strategy::AllPaths, Src::Rust, At::World, w, cx),
    },
    Scene {
        id: "graph-present-naive",
        title: "Perf: package altitude, one path per shape (no batching)",
        size: CANVAS,
        build: |w, cx| {
            with(
                Strategy::Naive,
                Src::Rust,
                At::Package("backend-present"),
                w,
                cx,
            )
        },
    },
    Scene {
        id: "graph-present-paths",
        title: "Perf: package altitude, stars as triangles too",
        size: CANVAS,
        build: |w, cx| {
            with(
                Strategy::AllPaths,
                Src::Rust,
                At::Package("backend-present"),
                w,
                cx,
            )
        },
    },
];

#[derive(Clone, Copy)]
enum Src {
    Pinned,
    Rust,
    Proto,
}

#[derive(Clone, Copy)]
enum At {
    World,
    Package(&'static str),
    Module(&'static str, &'static str),
    Close(&'static str, &'static str, f64),
    Hover(&'static str, &'static str, &'static str, &'static str),
    Focus(&'static str, &'static str),
    FlightA,
    FlightB,
    Journey,
    CheckIdle,
    CheckInterrupt,
    CheckWheelDrag,
    CheckHover,
    CheckKeyboard,
    CheckWeather,
    CheckReachTour,
    CheckLiveHover,
    CheckHoverPhases,
    CheckHold,
    CheckChain,
    CheckPeek,
    CheckParked,
    DenseFocus,
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Nudox-Design-System/v4/graph")
}

/// The fixture world (empty when the file is missing).
pub(crate) fn world() -> Arc<World> {
    static WORLD: OnceLock<Arc<World>> = OnceLock::new();
    WORLD
        .get_or_init(|| {
            let bytes = std::fs::read(fixture().join("world.json")).unwrap_or_default();
            Arc::new(World::from_json(&bytes).unwrap_or_else(|_| empty()))
        })
        .clone()
}

fn empty() -> World {
    match World::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()) {
        Ok(world) => world,
        Err(error) => panic!("an empty world: {error}"),
    }
}

fn scene(src: Src) -> Arc<MapScene> {
    static PINNED: OnceLock<Arc<MapScene>> = OnceLock::new();
    static RUST: OnceLock<Arc<MapScene>> = OnceLock::new();
    static PROTO: OnceLock<Arc<MapScene>> = OnceLock::new();
    match src {
        Src::Pinned => PINNED.get_or_init(|| {
            let world = Arc::new(pinned::world());
            Arc::new(MapScene::new(world.clone(), layout_of(&world)))
        }),
        Src::Rust => RUST.get_or_init(|| {
            let world = world();
            let layout = layout_of(&world);
            Arc::new(MapScene::new(world, layout))
        }),
        Src::Proto => PROTO.get_or_init(|| {
            let world = world();
            let layout = proto_layout(&world).map_or_else(|| layout_of(&world), Arc::new);
            Arc::new(MapScene::new(world, layout))
        }),
    }
    .clone()
}

#[derive(Deserialize)]
struct ProtoLevel {
    x: Vec<f32>,
    y: Vec<f32>,
    r: Vec<f32>,
    hull: Vec<Vec<[f32; 2]>>,
}

#[derive(Deserialize)]
struct Proto {
    x: Vec<f32>,
    y: Vec<f32>,
    r: Vec<f32>,
    #[serde(rename = "mod")]
    modules: ProtoLevel,
    #[serde(rename = "pkg")]
    packages: ProtoLevel,
}

/// The prototype's positions from `world.js` (None when it is missing or
/// does not match the world).
fn proto_layout(world: &World) -> Option<Layout> {
    let text = std::fs::read_to_string(fixture().join("world.js")).ok()?;
    let json = text
        .trim()
        .strip_prefix("window.WORLD=")?
        .trim_end_matches(';');
    let p: Proto = serde_json::from_str(json).ok()?;
    if p.x.len() != world.len() || p.modules.x.len() != world.modules.len() {
        return None;
    }
    let terr = |l: &ProtoLevel| -> Vec<Territory> {
        (0..l.x.len())
            .map(|i| Territory {
                x: l.x[i],
                y: l.y[i],
                r: l.r[i],
                bounds: Box2::around(&l.hull[i]),
                hull: l.hull[i].clone(),
            })
            .collect()
    };
    Some(Layout::from_positions(
        world,
        p.x,
        p.y,
        p.r,
        terr(&p.modules),
        terr(&p.packages),
    ))
}

/// The symbol named `name` in `place` (`present::glyph`).
pub(crate) fn find(world: &World, place: &str, name: &str) -> Option<NodeId> {
    (0..u32::try_from(world.len()).ok()?)
        .find(|&i| world.node(i).name.as_ref() == name && world.qual(i).as_ref() == place)
}

fn package(world: &World, name: &str) -> Option<usize> {
    world.packages.iter().position(|p| p.name.as_ref() == name)
}

fn module(world: &World, pkg: &str, path: &str) -> Option<usize> {
    let p = package(world, pkg)?;
    world
        .modules
        .iter()
        .position(|m| m.pkg as usize == p && m.path.as_ref() == path)
}

fn fanout(world: &World) -> Option<NodeId> {
    world.items.iter().copied().max_by_key(|&i| {
        (
            world.in_edges(i).len() + world.out_edges(i).len(),
            std::cmp::Reverse(i),
        )
    })
}

/// Geometry is resolved at delivery time, then dispatched through the real
/// platform event path. Picking remains the graph's responsibility.
fn adapt(act: &backend_gui_harness::Act, window: &mut Window, cx: &mut App) {
    if let backend_gui_harness::Act::Route { target } = act {
        if let Some(label) = target.strip_prefix("graph-memory ") {
            cx.set_global(MemoryPhase(Some(label.to_owned())));
            eprintln!("FACET_MEMORY_PHASE {label}");
            return;
        }
        if let Some(id)=target.strip_prefix("graph-hover-drag-start ") {
            let node: NodeId=id.parse().unwrap_or_else(|_|panic!("invalid native drag target {id}"));
            let entity=cx.global::<Current>().0.upgrade().unwrap_or_else(||panic!("graph gone"));
            let (x,y)=entity.read(cx).screen_position(node).unwrap_or_else(||panic!("drag target not positioned"));
            cx.set_global(HoverDragAnchor(Some((x,y))));
            let position=gpui::point(gpui::px(x),gpui::px(y));
            window.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                position,pressed_button:None,modifiers:window.modifiers(),
            }),cx);
            window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                position,button:gpui::MouseButton::Left,modifiers:window.modifiers(),click_count:1,first_mouse:false,
            }),cx);
            return;
        }
        for (prefix,up) in [("graph-hover-drag-move ",false),("graph-hover-drag-end ",true)] {
            if let Some(offset)=target.strip_prefix(prefix) {
                let mut values=offset.split_whitespace().map(|value|value.parse::<f32>().unwrap_or_else(|_|panic!("invalid drag offset")));
                let (dx,dy)=(values.next().expect("drag x offset"),values.next().expect("drag y offset"));
                let (x,y)=cx.default_global::<HoverDragAnchor>().0.unwrap_or_else(||panic!("native drag start absent"));
                let position=gpui::point(gpui::px(x+dx),gpui::px(y+dy));
                if up {
                    window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                        position,button:gpui::MouseButton::Left,modifiers:window.modifiers(),click_count:1,
                    }),cx);
                    cx.set_global(HoverDragAnchor(None));
                } else {
                    window.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                        position,pressed_button:Some(gpui::MouseButton::Left),modifiers:window.modifiers(),
                    }),cx);
                }
                return;
            }
        }
        if let Some(id) = target.strip_prefix("graph-prism-hover ") {
            let node: NodeId = id.parse().unwrap_or_else(|_| panic!("invalid prism hover target {id}"));
            let entity = cx.global::<Current>().0.upgrade().unwrap_or_else(|| panic!("graph gone"));
            let frame = entity.read(cx).inspect(cx).frame.unwrap_or_else(|| panic!("prism frame absent"));
            let slot = frame.slots.iter().find(|s| s.node == Some(node)).unwrap_or_else(|| panic!("prism node {node} absent"));
            let b = slot.label.unwrap_or_else(|| panic!("prism label {node} absent"));
            window.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                position: gpui::point(gpui::px((b[0]+b[2])*0.5),gpui::px((b[1]+b[3])*0.5)),
                pressed_button: None, modifiers: window.modifiers(),
            }), cx);
            return;
        }
        if let Some(node) = target.strip_prefix("graph-hover ") {
            let mut args = node.split_whitespace();
            let id = args.next().unwrap_or_else(|| panic!("missing hover ID"));
            let dx: f32 = args.next().map_or(0.0, |v| {
                v.parse().unwrap_or_else(|_| panic!("invalid hover offset"))
            });
            let dy: f32 = args.next().map_or(0.0, |v| {
                v.parse().unwrap_or_else(|_| panic!("invalid hover offset"))
            });
            let node: NodeId = id
                .parse()
                .unwrap_or_else(|_| panic!("invalid graph hover target {node}"));
            let entity = cx
                .global::<Current>()
                .0
                .upgrade()
                .unwrap_or_else(|| panic!("graph gone during hover"));
            let position = entity
                .read(cx)
                .screen_position(node)
                .unwrap_or_else(|| panic!("graph target {node} not positioned"));
            window.dispatch_event(
                gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                    position: gpui::point(gpui::px(position.0 + dx), gpui::px(position.1 + dy)),
                    pressed_button: None,
                    modifiers: window.modifiers(),
                }),
                cx,
            );
            return;
        }
    }
    crate::gallery::adapt(act, window, cx);
}

fn ready(cx: &mut App) -> bool {
    cx.try_global::<Current>()
        .and_then(|c| c.0.upgrade())
        .is_some_and(|view| view.read(cx).ready())
}

fn snapshot(cx: &mut App, ledger: &crate::probe::Ledger) -> crate::gallery::json::Json {
    use crate::gallery::json::Json;
    let memory_phase = cx.default_global::<MemoryPhase>().0.take();
    let expected_focus = cx.default_global::<ExpectedFocus>().0;
    let expected_hover_targets = cx.default_global::<ExpectedHoverTargets>().0;
    let strategy = cx.default_global::<CurrentStrategy>().0;
    let previous_requests = cx.default_global::<SnapshotRequests>().0;
    let frame_requests = ledger.frames_requested.saturating_sub(previous_requests);
    cx.set_global(SnapshotRequests(ledger.frames_requested));
    let current = cx.global::<Current>();
    let source = match current.1 {
        Src::Pinned => "pinned",
        Src::Rust => "rust-live",
        Src::Proto => "prototype-positions",
    };
    let entity = current
        .0
        .upgrade()
        .unwrap_or_else(|| panic!("graph disappeared while sampling"));
    let graph = entity.read(cx);
    let state = graph.inspect(cx);
    let camera = graph.camera().map_or(Json::Null, |c| {
        Json::obj([
            ("x", Json::num(c.x)),
            ("y", Json::num(c.y)),
            ("w", Json::num(c.w)),
        ])
    });
    let node = |id: Option<NodeId>| {
        id.map_or(Json::Null, |id| {
            Json::obj([
                ("id", Json::num(id)),
                ("name", Json::str(graph.world().node(id).name.to_string())),
            ])
        })
    };
    let selected = state
        .prism_selected
        .and_then(|q| state.frame.as_ref().and_then(|f| f.slots.get(q)))
        .and_then(|s| s.node);
    let mode = if graph.reach().is_some() {
        "reach"
    } else if graph.tour_stop().is_some() {
        "tour"
    } else if state.held_chain.is_some() {
        "chain"
    } else {
        "free"
    };
    let bounds = |b: &crate::probe::BoundsSample| {
        Json::obj([
            ("x", Json::num(b.x)),
            ("y", Json::num(b.y)),
            ("width", Json::num(b.width)),
            ("height", Json::num(b.height)),
        ])
    };
    let stats = graph.stats();
    let retained = graph.retained();
    let snapshot = Json::obj([
        ("source", Json::str(source)),
        ("strategy", Json::str(match strategy { Strategy::Batched=>"batched",Strategy::AllPaths=>"all_paths",Strategy::Naive=>"naive" })),
        ("expected_focus",node(expected_focus)),
        ("find_bounds",ledger.bounds("graph-find-bounds").map_or(Json::Null,bounds)),
        ("scrolls",Json::Arr(ledger.scrolls.iter().map(|scroll|Json::obj([
            ("key",Json::str(scroll.key.clone())),("viewport",bounds(&scroll.viewport)),("content",bounds(&scroll.content)),
        ])).collect())),
        ("prism_room",state.frame.as_ref().map_or(Json::Null,|f|Json::Arr(f.room.into_iter().map(Json::num).collect()))),
        ("world_nodes", Json::num(graph.world().len() as f64)),
        ("world_packages", Json::num(graph.world().packages.len() as f64)),
        ("retained", Json::obj([
            ("motion_tracks",Json::num(retained.motion_tracks as f64)),
            ("trail",Json::num(retained.trail as f64)),
            ("tours",Json::num(retained.tours as f64)),
            ("prism_rows",Json::num(retained.prism_rows as f64)),
            ("search_cache",Json::num(retained.search_cache as f64)),
        ])),
        ("focused", node(graph.focused())),
        ("hovered", node(state.hover)),
        ("hover_strength",Json::num(state.hover_strength)),
        ("fading_hover",state.fading_hover.map_or(Json::Null,|(id,alpha)|Json::obj([
            ("node",node(Some(id))),("strength",Json::num(alpha)),
        ]))),
        ("expected_hover_targets",expected_hover_targets.map_or(Json::Null,|(dense,sparse)|Json::obj([
            ("dense",node(Some(dense))),("sparse",node(Some(sparse))),
        ]))),
        ("selected", node(selected)),
        ("camera", camera),
        ("viewport", state.viewport.map_or(Json::Null, |v| Json::obj([
            ("x", Json::num(v.x)), ("y", Json::num(v.y)),
            ("width", Json::num(v.w)), ("height", Json::num(v.h)),
        ]))),
        ("measured_card_bounds", state.card_bounds.map_or(Json::Null, |b| Json::obj([
            ("x", Json::num(f32::from(b.origin.x))), ("y", Json::num(f32::from(b.origin.y))),
            ("width", Json::num(f32::from(b.size.width))), ("height", Json::num(f32::from(b.size.height))),
        ]))),
        ("focus_bounds", graph.focus_bounds().map_or(Json::Null, |b| Json::obj([
            ("x", Json::num(f32::from(b.origin.x))), ("y", Json::num(f32::from(b.origin.y))),
            ("width", Json::num(f32::from(b.size.width))), ("height", Json::num(f32::from(b.size.height))),
        ]))),
        ("hover_slot", state.hover_slot.map_or(Json::Null, |slot| Json::num(slot as f64))),
        ("pointer_prism_pick", state.pointer.and_then(|(x,y)| state.frame.as_ref().and_then(|f|f.pick(x,y)))
            .map_or(Json::Null, |slot| Json::num(slot as f64))),
        ("pointer", state.pointer.map_or(Json::Null, |(x,y)| Json::obj([
            ("x", Json::num(x)), ("y", Json::num(y)),
        ]))),
        ("moving", Json::Bool(state.moving)),
        ("exploration", Json::str(mode)),
        ("tour_stop", node(graph.tour_stop())),
        ("find_open", Json::Bool(state.find_open)),
        ("query", Json::str(state.query)),
        ("searching", Json::Bool(state.searching)),
        (
            "rows",
            Json::Arr(state.rows.iter().map(|&id| node(Some(id))).collect()),
        ),
        ("chains", Json::num(state.chains as f64)),
        (
            "held_chain",
            state.held_chain.map_or(Json::Null, |path| {
                Json::Arr(path.into_iter().map(Json::num).collect())
            }),
        ),
        (
            "prism",
            state.prism.map_or(Json::Null, |(id, g)| {
                Json::obj([("node", node(Some(id))), ("gathered", Json::num(g))])
            }),
        ),
        (
            "prism_labels",
            Json::Arr(state.frame.as_ref().map_or_else(Vec::new, |f| {
                f.slots
                    .iter()
                    .filter_map(|s| {
                        s.label.map(|b| {
                            Json::obj([
                                ("node", node(s.node)),
                                ("x0", Json::num(b[0])),
                                ("y0", Json::num(b[1])),
                                ("x1", Json::num(b[2])),
                                ("y1", Json::num(b[3])),
                            ])
                        })
                    })
                    .collect()
            })),
        ),
        (
            "card_bounds",
            ledger.bounds("graph-focus-card").map_or(Json::Null, bounds),
        ),
        (
            "pending_motion",
            Json::num(ledger.tracks.iter().filter(|t| t.live).count() as f64),
        ),
        (
            "frames_requested",
            Json::num(frame_requests as f64),
        ),
        ("frame_requests_total", Json::num(ledger.frames_requested as f64)),
        ("floating_entries", Json::num(ledger.stacks.iter().map(|s| s.entries.len()).sum::<usize>() as f64)),
        ("reduced_motion", Json::Bool(ledger.reduced_motion)),
        (
            "drawn",
            Json::obj([
                ("item_candidates", Json::num(stats.item_candidates)),
                ("edge_candidates", Json::num(stats.edge_candidates)),
                ("hover_relations", Json::num(stats.hover_relations)),
                ("hover_routes", Json::num(stats.hover_routes)),
                ("hover_candidates", Json::num(stats.hover_candidates)),
                ("fading_hover_relations", Json::num(stats.fading_hover_relations)),
                ("fading_hover_routes", Json::num(stats.fading_hover_routes)),
                ("fading_hover_candidates", Json::num(stats.fading_hover_candidates)),
                ("items", Json::num(stats.items)),
                ("members", Json::num(stats.members)),
                ("edges", Json::num(stats.edges)),
                ("labels", Json::num(stats.labels)),
                ("paths", Json::num(stats.paths)),
                ("quads", Json::num(stats.quads)),
            ]),
        ),
    ]);
    if let Some(label) = memory_phase {
        let mut phase = snapshot.clone();
        if let Json::Obj(fields) = &mut phase { fields.push(("label".to_owned(),Json::str(label))); }
        eprintln!("FACET_MEMORY_STATE {phase}");
    }
    snapshot
}

#[derive(Default)]
struct ExpectedFocus(Option<NodeId>);
impl gpui::Global for ExpectedFocus {}
struct CurrentStrategy(Strategy);
impl Default for CurrentStrategy { fn default()->Self { Self(Strategy::Batched) } }
impl gpui::Global for CurrentStrategy {}

#[derive(Default)]
struct HoverDragAnchor(Option<(f32,f32)>);
impl gpui::Global for HoverDragAnchor {}

#[derive(Default)]
struct ExpectedHoverTargets(Option<(NodeId,NodeId)>);
impl gpui::Global for ExpectedHoverTargets {}

#[derive(Default)]
struct SnapshotRequests(u64);
impl gpui::Global for SnapshotRequests {}

#[derive(Default)]
struct MemoryPhase(Option<String>);
impl gpui::Global for MemoryPhase {}

fn annotate(cx: &mut App) -> String {
    cx.try_global::<Current>()
        .and_then(|c| c.0.upgrade())
        .map(|view| {
            let s = view.read(cx).stats();
            format!(
                "{} items, {} members, {} edges, {} labels, {} paths, {} quads; {} item candidates, {} edge candidates; {} hover relations, {} hover routes, {} hover candidates; {} fading relations, {} fading routes, {} fading candidates",
                s.items, s.members, s.edges, s.labels, s.paths, s.quads, s.item_candidates, s.edge_candidates, s.hover_relations, s.hover_routes, s.hover_candidates, s.fading_hover_relations, s.fading_hover_routes, s.fading_hover_candidates
            )
        })
        .unwrap_or_default()
}

struct Current(gpui::WeakEntity<GraphView>, Src);

impl gpui::Global for Current {}

fn build(src: Src, at: At, window: &mut Window, cx: &mut App) -> AnyView {
    with(Strategy::Batched, src, at, window, cx)
}

#[allow(clippy::too_many_lines)]
fn with(strategy: Strategy, src: Src, at: At, window: &mut Window, cx: &mut App) -> AnyView {
    cx.set_global(CurrentStrategy(strategy));
    let map = scene(src);
    let world = map.world.clone();
    let view = super::camera::View {
        x: 0.0,
        y: 0.0,
        w: f32::from(u16::try_from(CANVAS.0).unwrap_or(1440)),
        h: f32::from(u16::try_from(CANVAS.1).unwrap_or(824)),
    };
    let lay = &map.layout;
    let start =
        match at {
            At::World
            | At::DenseFocus
            | At::FlightA
            | At::Journey
            | At::CheckIdle
            | At::CheckInterrupt
            | At::CheckWheelDrag
            | At::CheckKeyboard
            | At::CheckWeather
            | At::CheckHold
            | At::CheckChain => Start::World,
            At::CheckHover => Start::Frame(lay.packages[0].bounds, 1.2),
            At::CheckPeek => Start::Cam(map.focus_cam(&view, 0, 0.0)),
            At::CheckReachTour => Start::Focus(5),
            At::CheckParked => Start::Focus(0),
        At::CheckLiveHover | At::CheckHoverPhases => {
                fanout(&world).map_or(Start::World, |i| Start::Cam(map.focus_cam(&view, i, 0.0)))
            }
            At::Package(p) => package(&world, p)
                .map_or(Start::World, |p| Start::Frame(lay.packages[p].bounds, 1.2)),
            At::Module(p, m) | At::Hover(p, m, _, _) => module(&world, p, m)
                .map_or(Start::World, |m| Start::Frame(lay.modules[m].bounds, 1.2)),
            At::Close(place, name, w) => find(&world, place, name).map_or(Start::World, |i| {
                Start::Cam(Camera::new(
                    f64::from(lay.x[i as usize]),
                    f64::from(lay.y[i as usize]),
                    w,
                ))
            }),
            At::Focus(place, name) => find(&world, place, name).map_or(Start::World, Start::Focus),
            At::FlightB => {
                find(&world, "present::glyph", "RelationLabel").map_or(Start::World, Start::Focus)
            }
        };
    let entity: Entity<GraphView> = cx.new(|cx| {
        let mut view = GraphView::with_scene(map.clone(), start, window, cx);
        view.set_strategy(strategy);
        view
    });
    cx.set_global(Current(entity.downgrade(), src));
    declare_annotator(annotate, cx);
    declare_adapter(adapt, cx);
    declare_quiet(ready, cx);
    declare_state(snapshot, cx);
    window.focus(&entity.focus_handle(cx), cx);
    match at {
        At::Hover(_, _, place, name) => {
            // Rest the real pointer on the symbol.
            if let (Some(i), Start::Frame(b, pad)) = (find(&world, place, name), start) {
                let cam = view.frame(b, pad);
                let (x, y) = view.to_screen(&cam, lay.x[i as usize], lay.y[i as usize]);
                let script: &'static str =
                    Box::leak(format!("move {},{} @40", x.round(), y.round()).into_boxed_str());
                declare_script(script, cx);
            }
        }
        At::DenseFocus => {
            let target=find(&world,"std::fmt","Debug");
            cx.set_global(ExpectedFocus(target));
            if let Some(node)=target {
                let query=format!("{}::{}",world.qual(node),world.node(node).name);
                let script=format!("key / @200; type \"{query}\" @240; key enter @280; leave @4000");
                declare_script(Box::leak(script.into_boxed_str()),cx);
            }
        }
        At::CheckHoverPhases => {
            if let Some(dense)=fanout(&world) {
                let cam=map.focus_cam(&view,dense,0.0);
                let degree=|id| world.in_edges(id).len()+world.out_edges(id).len();
                let sparse=world.items.iter().copied().filter(|&id| id!=dense && world.node(id).module==world.node(dense).module)
                    .filter(|&id| {
                        let (x,y)=view.to_screen(&cam,lay.x[id as usize],lay.y[id as usize]);
                        x>160.0 && x<view.w-160.0 && y>120.0 && y<view.h-120.0 && map.pick(&view,&cam,x,y)==Some(id)
                    }).min_by_key(|&id|(degree(id),id));
                if let Some(sparse)=sparse.filter(|&id|degree(id)*2<degree(dense)) {
                    cx.set_global(ExpectedHoverTargets(Some((dense,sparse))));
                    let script=format!("leave @0; route graph-hover {dense} @200; leave @900; route graph-hover {dense} @1400; route graph-hover-drag-start {dense} @2000; route graph-hover-drag-move 80 40 @2032; route graph-hover-drag-end 80 40 @2300; leave @2400; route graph-hover {dense} @3000; route graph-hover {sparse} @3080; route graph-hover {dense} @3120; route graph-hover {sparse} @3180; route graph-hover {dense} @3260; leave @3600; leave @6200");
                    declare_script(Box::leak(script.into_boxed_str()),cx);
                }
            }
        }
        At::CheckLiveHover => {
            if let Some(i) = fanout(&world) {
                let mut script = String::new();
                for n in 0..32 {
                    if n % 2 == 0 {
                        script.push_str(&format!("route graph-hover {i} @{}; ", 100 + n * 16));
                    } else {
                        script.push_str(&format!("leave @{}; ", 100 + n * 16));
                    }
                }
                script.push_str(&format!("route graph-hover {i} @900; leave @1600; key escape @1800; key escape @1960; leave @2200; leave @4200"));
                declare_script(Box::leak(script.into_boxed_str()), cx);
            }
        }
        At::CheckReachTour => declare_script(
            "key r @200; key r @1000; key t @1200; key right @1600; key left @1760; key right @1920; key enter @2080; key escape @3800; key escape @4000; leave @4200; leave @6200",
            cx,
        ),
        At::CheckHold => declare_script(
            "key / @200; type \"glyph::RelationLabel\" @240; key enter @280; down 50,700 @400; up 50,700 @1000; leave @1200; leave @4000",
            cx,
        ),
        At::CheckChain => declare_script(
            "key / @200; type \"Invocation -> list of text\" @240; key enter @600; hold alt @1800; release alt @2200; key escape @2800; key escape @3000; leave @3200; leave @5200",
            cx,
        ),
        At::CheckParked => declare_script(
            "route graph-prism-hover 4 @1000; text-scale 200% @1200; resize 480x824 @1220; theme glacier @1240; motion off @1280; leave @2200; leave @4000",
            cx,
        ),
        At::CheckPeek => declare_script(
            "route graph-hover 0 @100; route graph-hover 0 1 0 @100; route graph-hover 0 0 1 @100; leave @1200; leave @4000",
            cx,
        ),
        At::CheckIdle => declare_script(
            "leave @0; key / @2000; type \"glyph::RelationLabel\" @2040; key escape @2200; leave @2400; leave @6000",
            cx,
        ),
        At::CheckInterrupt => declare_script(
            "leave @0; key / @2000; type \"glyph::RelationLabel\" @2040; key enter @2080; key / @2200; type \"serde_json::de::from_str\" @2240; key enter @2280; key escape @2400; key escape @2520; leave @2600; leave @6000",
            cx,
        ),
        At::CheckWheelDrag => declare_script(
            "key / @200; type \"glyph::RelationLabel\" @240; key enter @280; wheel-zoom 720,412 1.8 @400; drag 720,412 -> 1040,280 over 240 @480; wheel-zoom 300,300 0.7 @800; key escape @1100; key escape @1220; leave @1400; leave @4000",
            cx,
        ),
        At::CheckKeyboard => declare_script(
            "key / @200; type \"glyph::RelationLabel\" @240; key enter @280; key right @2200; key down @2240; key up @2280; key enter @2320; key escape @4300; key escape @4460; leave @4600; leave @6600",
            cx,
        ),
        At::CheckWeather => declare_script(
            "key / @200; type \"glyph::RelationLabel\" @240; key enter @280; resize 480x824 @400; theme glacier @416; text-scale 150% @432; motion off @448; resize 640x824 @480; text-scale 200% @496; key escape @900; key escape @940; leave @1100; leave @3000",
            cx,
        ),
        At::CheckHover => {
            let mut script = String::new();
            for n in 0..36 {
                script.push_str(&format!(
                    "route graph-hover {} @{}; ",
                    n % world.len(),
                    100 + n * 16
                ));
            }
            script.push_str("route graph-hover 0 @720; leave @800; key escape @900; key escape @1060; leave @1200; leave @4000");
            declare_script(Box::leak(script.into_boxed_str()), cx);
        }
        At::FlightA => declare_script(
            "key / @100; type \"glyph::RelationLabel\" @140; key enter @200",
            cx,
        ),
        At::FlightB => declare_script(
            "key / @100; type \"serde_json::de::from_str\" @140; key enter @200",
            cx,
        ),
        At::Journey => {
            if let Some(p) = package(&world, "backend-present") {
                let cam = map.world_cam(&view);
                let t = &lay.packages[p];
                let (x, y) = view.to_screen(&cam, t.x, t.y);
                let (x, y) = (x.round(), y.round());
                let mut script = String::new();
                for n in 0..10 {
                    script.push_str(&format!("wheel-zoom {x},{y} 1.25 @{}; ", 200 + 50 * n));
                }
                script.push_str("key / @1400; type \"glyph::RelationLabel\" @1450; key enter @1500; key escape @3300; leave @4400; key escape @4500");
                declare_script(Box::leak(script.into_boxed_str()), cx);
            }
        }
        _ => {}
    }
    cx.new(|_| Root { graph: entity }).into()
}

/// The scene root: the graph region and the float layer as the last child
/// (the shell's root plays this part in the product).
fn offset(window: &mut Window, cx: &mut App) -> AnyView {
    let graph = build(
        Src::Pinned,
        At::Focus("present::glyph", "RelationLabel"),
        window,
        cx,
    );
    cx.new(|_| OffsetRoot { graph }).into()
}

struct OffsetRoot {
    graph: AnyView,
}

impl Render for OffsetRoot {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().relative().size_full().child(
            div()
                .absolute()
                .left(gpui::px(96.0))
                .top(gpui::px(60.0))
                .w(gpui::px(480.0))
                .h(gpui::px(740.0))
                .child(self.graph.clone()),
        )
    }
}

struct Root {
    graph: Entity<GraphView>,
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .size_full()
            .child(self.graph.clone())
            .child(float::layer(window, cx))
    }
}

/// A blank canvas with one small cut plate: every facet view pays for one
/// path pass (the plate's chamfer is a path), so this is the floor the
/// graph's GPU time is measured against.
pub(crate) fn blank_cut(_window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|_| BlankCut).into()
}

struct BlankCut;

impl Render for BlankCut {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.palette();
        div().size_full().bg(palette.g0.hsla()).child(
            crate::paint::cut()
                .chamfer(crate::paint::Chamfer::Sm)
                .w(gpui::px(300.0))
                .h(gpui::px(34.0))
                .m(gpui::px(16.0)),
        )
    }
}

/// A blank canvas the size of the graph's, for the GPU-readback baseline.
pub(crate) fn blank(_window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|_| Blank).into()
}

struct Blank;

impl Render for Blank {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(cx.palette().g0.hsla())
    }
}

#[cfg(test)]
mod tests {
    use crate::gallery::{Shot, find, perf, run};
    use backend_gui_harness::Script;
    use std::time::{Duration, Instant};

    fn pct(sorted: &[Duration], p: f64) -> f64 {
        if sorted.is_empty() {
            return 0.0;
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let i = ((sorted.len() - 1) as f64 * p).round() as usize;
        sorted[i].as_secs_f64() * 1000.0
    }

    /// Frame times at 1440 × 900 @2x: the CPU draw (render + layout +
    /// prepaint + paint) of every frame, and the GPU render + readback of
    /// the same frame (`render_to_image`, minus a blank canvas's readback).
    /// Release only: `cargo test --release -p backend-facet --features
    /// gallery --lib graph::gallery::tests::frame_times -- --ignored
    /// --nocapture`.
    #[test]
    #[ignore = "timing: run in release with --ignored --nocapture"]
    fn frame_times() {
        let runs: [(&str, Option<&str>); 12] = [
            ("graph-baseline", None),
            ("graph-baseline-cut", None),
            ("graph-world", None),
            ("graph-world-paths", None),
            ("graph-world-naive", None),
            ("graph-present", None),
            ("graph-present-naive", None),
            ("graph-glyph", None),
            ("graph-engine", None),
            ("graph-engine-naive", None),
            (
                "graph-present",
                Some("drag 900,500 -> 500,380 over 500 @100; move 5,890 @1400"),
            ),
            (
                "graph-flight-b",
                Some(
                    "key / @100; type \"serde_json::de::from_str\" @140; key enter @200; move 5,890 @2200",
                ),
            ),
        ];
        let mut baseline = 0.0;
        for (id, input) in runs {
            let Some(scene) = find(id) else {
                panic!("no scene {id}")
            };
            let mut shot = Shot::new(&scene);
            shot.size = (1440, 900);
            shot.times = vec![];
            let script = match input {
                Some(text) => match Script::parse(text) {
                    Ok(script) => script,
                    Err(error) => panic!("{error}"),
                },
                None => perf::sweep(1440, 900),
            };
            shot.until_ms = script.end_ms() + 400;
            shot.script = Some(script);
            let (mut cpu, mut gpu) = (Vec::new(), Vec::new());
            let result = run(&scene, &shot, &mut |tick, window, _| {
                if tick.drawn.at_ms >= 100 {
                    cpu.push(tick.drawn.cpu);
                    let t = Instant::now();
                    let _ = window.render_to_image();
                    gpu.push(t.elapsed());
                }
                Ok(())
            });
            if let Err(error) = result {
                panic!("{id}: {error}");
            }
            cpu.sort_unstable();
            gpu.sort_unstable();
            let (g50, g99) = (pct(&gpu, 0.5), pct(&gpu, 0.99));
            if id == "graph-baseline-cut" {
                baseline = g50;
            }
            eprintln!(
                "{id:<20} {:<9} frames {:>4}  cpu p50 {:>6.2} p95 {:>6.2} p99 {:>6.2} max {:>6.2} ms | gpu+readback p50 {:>6.2} p99 {:>6.2} ms (less blank+plate: p50 {:>5.2} p99 {:>5.2})",
                if input.is_some() { "scripted" } else { "sweep" },
                cpu.len(),
                pct(&cpu, 0.5),
                pct(&cpu, 0.95),
                pct(&cpu, 0.99),
                pct(&cpu, 1.0),
                g50,
                g99,
                (g50 - baseline).max(0.0),
                (g99 - baseline).max(0.0),
            );
        }
    }
}
