//! Value assertions use the pinned world; scale coverage belongs to the live
//! fixture. An absent/changed live symbol must not invalidate pinned truth.
use super::{Current, Src, scene};
use crate::gallery::{Shot, capture, find, run};
use crate::motion::Camera;
use backend_gui_harness::Script;

fn check(id: &str, times: &[u64]) -> (Vec<(u64, Option<u32>, Camera)>, Vec<crate::gallery::Frame>) {
    let scene = find(id).unwrap_or_else(|| panic!("missing graph scenario {id}"));
    let mut shot = Shot::new(&scene);
    shot.scale = 1;
    shot.probe = true;
    shot.times = times.to_vec();
    let mut states = Vec::new();
    run(&scene, &shot, &mut |tick, _, cx| {
        if times.contains(&tick.drawn.at_ms) {
            let current = cx
                .global::<Current>()
                .0
                .upgrade()
                .unwrap_or_else(|| panic!("graph view gone"));
            let view = current.read(cx);
            let cam = view.camera().unwrap_or_else(|| panic!("no camera"));
            assert!(
                cam.x.is_finite() && cam.y.is_finite() && cam.w.is_finite() && cam.w > 0.0,
                "invalid camera {cam:?}"
            );
            states.push((tick.drawn.at_ms, view.focused(), cam));
        }
        Ok(())
    })
    .unwrap_or_else(|error| panic!("{id}: {error}"));
    let frames = capture(&scene, &shot).unwrap_or_else(|error| panic!("{id}: {error}"));
    (states, frames)
}

#[test]
fn pinned_interruption_retargets_then_returns_to_exact_world_with_same_history() {
    let (states, frames) = check(
        "graph-check-interrupt",
        &[1900, 2100, 2300, 2450, 5800, 7600],
    );
    assert_eq!(
        states.iter().map(|s| s.1).collect::<Vec<_>>(),
        vec![None, Some(0), Some(5), None, None, None]
    );
    assert_ne!(
        states[1].2, states[2].2,
        "cross-package retarget must move the camera"
    );
    assert_eq!(states[4].2, states[5].2, "camera drifts after settle");
    assert!(!frames[4].ledger.any_live() && !frames[5].ledger.any_live());
    assert_eq!(
        frames[4].ledger.frames_requested, frames[5].ledger.frames_requested,
        "settled graph still requests frames"
    );
    let fresh_scene = find("graph-pinned-world").unwrap_or_else(|| panic!("missing pinned world"));
    let mut shot = Shot::new(&fresh_scene);
    shot.scale = 1;
    shot.times = vec![7600];
    // The mint visit trail is intentional persistent history. Compare the
    // interrupted endpoint against fully settled visits to the same identities,
    // rather than erasing that history by comparing to an unvisited world.
    shot.script = Some(Script::parse(
        "leave @0; key / @200; type \"glyph::RelationLabel\" @240; key enter @280; key / @1200; type \"serde_json::de::from_str\" @1240; key enter @1280; key escape @2400; key escape @2520; leave @2600; leave @6000"
    ).unwrap_or_else(|error| panic!("{error}")));
    let fresh = capture(&fresh_scene, &shot).unwrap_or_else(|error| panic!("{error}"));
    if frames[5].image != fresh[0].image {
        let path = std::env::temp_dir().join("facet-graph-interruption-regression");
        std::fs::create_dir_all(&path).unwrap();
        frames[5].image.save(path.join("interrupted.png")).unwrap();
        fresh[0].image.save(path.join("fresh.png")).unwrap();
        let mismatch = frames[5].image.enumerate_pixels().zip(fresh[0].image.enumerate_pixels())
            .filter(|(a,b)| a.2 != b.2).map(|(a,_)| (a.0,a.1)).collect::<Vec<_>>();
        panic!("settled interrupted navigation differs from settled visits with identical history: {} changed pixels, first {:?}, diagnostic {}", mismatch.len(), mismatch.first(), path.display());
    }
}

#[test]
fn input_after_idle_really_opens_find_and_returns_to_quiet() {
    let scene = find("graph-check-idle").unwrap_or_else(|| panic!("missing idle scenario"));
    let mut shot = Shot::new(&scene);
    shot.scale = 1;
    shot.probe = true;
    shot.times = vec![1800, 2100, 5800, 7600];
    let frames = capture(&scene, &shot).unwrap_or_else(|error| panic!("{error}"));
    assert_ne!(
        frames[0].image.as_raw(),
        frames[1].image.as_raw(),
        "input after idle was ignored"
    );
    assert_eq!(
        frames[0].image.as_raw(),
        frames[2].image.as_raw(),
        "closing find left a stale panel/constellation"
    );
    assert_eq!(
        frames[2].image.as_raw(),
        frames[3].image.as_raw(),
        "quiet tail changes pixels"
    );
    assert_eq!(
        frames[2].ledger.frames_requested, frames[3].ledger.frames_requested,
        "idle graph keeps requesting frames"
    );
    assert!(!frames[3].ledger.any_live());
}

#[test]
fn pinned_scene_has_named_relations_and_stable_identity() {
    let map = scene(Src::Pinned);
    assert_eq!(map.world.len(), 14);
    assert_eq!(map.world.node(0).name.as_ref(), "RelationLabel");
    assert_eq!(map.world.node(5).name.as_ref(), "from_str");
    assert_eq!(
        map.world.node(5).ret.as_deref(),
        Some("Result<Value, Error>")
    );
    assert_eq!(
        map.world.in_edges(0).map(|(id, _)| id).collect::<Vec<_>>(),
        vec![2, 3, 4]
    );
}

#[test]
fn reduced_weather_lands_without_live_tracks_and_equals_fresh_narrow_light_world() {
    use crate::tokens::Appearance;
    let (_, frames) = check("graph-check-weather", &[448, 1100, 2800, 4600]);
    assert!(
        frames[0].ledger.reduced_motion,
        "mid-flight motion-off was not applied"
    );
    assert!(
        !frames[2].ledger.any_live() && !frames[3].ledger.any_live(),
        "reduced graph retained motion"
    );
    assert_eq!(
        frames[2].ledger.frames_requested, frames[3].ledger.frames_requested,
        "reduced graph did not become idle"
    );
    let fresh_scene = find("graph-pinned-world").unwrap_or_else(|| panic!("missing pinned world"));
    let mut shot = Shot::new(&fresh_scene);
    shot.scale = 1;
    shot.size = (640, 824);
    shot.text_scale = 2.0;
    shot.appearance = Appearance::Glacier;
    shot.reduced_motion = true;
    shot.times = vec![4600];
    shot.script = Some(Script::parse("leave @0").unwrap_or_else(|error| panic!("{error}")));
    let fresh = capture(&fresh_scene, &shot).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(frames[3].image.dimensions(), fresh[0].image.dimensions());
    assert_eq!(
        frames[3].image.as_raw(),
        fresh[0].image.as_raw(),
        "weather left stale narrow/light layout"
    );
}

#[test]
fn pinned_chain_has_real_receiver_steps_and_preserves_optional_failure() {
    use crate::graph::discovery::Discovery;
    let map = scene(Src::Pinned);
    let result = Discovery::new(&map.world).query(&map.world, "Invocation -> list of text");
    assert!(
        result.rows.is_empty(),
        "fixture must require two calls, not a direct shortcut"
    );
    assert_eq!(result.chains.len(), 1);
    assert_eq!(result.chains[0].path, vec![10, 12, 13]);
    assert_eq!(
        result.chains[0]
            .stops
            .iter()
            .map(|s| s.node)
            .collect::<Vec<_>>(),
        vec![10, 11]
    );
    assert_eq!(result.chains[0].code, "invocation.grammar()?.aliases()");
}

#[test]
fn narrow_graph_uses_its_measured_window_origin_and_leaves_room_for_card() {
    use crate::graph::camera::View;
    let target_scene =
        find("graph-pinned-offset").unwrap_or_else(|| panic!("missing offset scenario"));
    let mut shot = Shot::new(&target_scene);
    shot.scale = 1;
    shot.probe = true;
    shot.times = vec![1000];
    let map = scene(Src::Pinned);
    let mut checked = false;
    run(&target_scene, &shot, &mut |tick, _, cx| {
        if tick.drawn.at_ms == 1000 {
            let entity = cx
                .global::<Current>()
                .0
                .upgrade()
                .unwrap_or_else(|| panic!("graph view gone"));
            let graph = entity.read(cx);
            assert_eq!(graph.focused(), Some(0));
            let camera = graph.camera().unwrap_or_else(|| panic!("camera missing"));
            let viewport = View {
                x: 96.0,
                y: 60.0,
                w: 480.0,
                h: 740.0,
            };
            let (x, y) = viewport.to_screen(&camera, map.layout.x[0], map.layout.y[0]);
            let card = tick.ledger.bounds("graph-focus-card").unwrap_or_else(|| panic!("focus card was not measured"));
            assert!((x - (96.0 + 0.28 * 480.0)).abs() < 0.01 && y >= 100.0 && y < card.y - 40.0,
                "symbol must use the narrow reading anchor in the translated space above its card: ({x},{y}) card {card:?}");
            assert!(card.x >= 96.0 && card.y >= 60.0 && card.x + card.width <= 576.5 && card.y + card.height <= 800.5,
                "card escapes translated graph canvas: {card:?}");
            let gem = graph
                .focus_bounds()
                .unwrap_or_else(|| panic!("focused gem bounds missing"));
            assert!(
                f32::from(gem.origin.x) >= 96.0 && f32::from(gem.origin.y) >= 60.0,
                "focused gem ignored canvas origin: {gem:?}"
            );
            assert!(
                f32::from(gem.origin.x + gem.size.width) <= 576.5
                    && f32::from(gem.origin.y + gem.size.height) <= 800.5,
                "focused gem escapes narrow canvas: {gem:?}"
            );
            checked = true;
        }
        Ok(())
    })
    .unwrap_or_else(|error| panic!("{error}"));
    assert!(checked, "offset checkpoint not observed");
}

#[test]
fn reach_and_tour_keys_show_the_pinned_content_and_walk_real_stops() {
    let target =
        find("graph-check-reach-tour").unwrap_or_else(|| panic!("missing reach/tour scenario"));
    let mut shot = Shot::new(&target);
    shot.scale = 1;
    shot.probe = true;
    shot.times = vec![400, 1100, 1400, 1700, 1850, 2050, 4200, 5800, 7600];
    let mut checked = 0;
    run(&target, &shot, &mut |tick, _, cx| {
        let at = tick.drawn.at_ms;
        if !shot.times.contains(&at) {
            return Ok(());
        }
        let entity = cx
            .global::<Current>()
            .0
            .upgrade()
            .unwrap_or_else(|| panic!("graph gone"));
        let view = entity.read(cx);
        match at {
            400 => {
                let reach = view
                    .reach()
                    .unwrap_or_else(|| panic!("R did not show reach"));
                assert_eq!(reach.source, 5);
                assert_eq!(reach.all, vec![0, 9]);
                assert_eq!(reach.yours, 2);
                assert!(
                    tick.ledger
                        .texts
                        .iter()
                        .any(|t| t.key == "graph-reach-summary" && t.content.contains("2 direct")),
                    "actual reach summary missing: {:?}",
                    tick.ledger.texts
                );
                assert_eq!(view.tour_stop(), None);
            }
            1100 => assert!(view.reach().is_none(), "R again did not hide reach"),
            1400 => assert_eq!(
                view.tour_stop(),
                Some(5),
                "tour must start at the serde_json door"
            ),
            1700 | 2050 => assert_eq!(view.tour_stop(), Some(8), "right must walk to Value"),
            1850 => assert_eq!(view.tour_stop(), Some(5), "left must return to from_str"),
            4200 => {
                assert!(view.reach().is_none() && view.tour_stop().is_none());
                assert_eq!(view.focused(), None);
                assert!(tick.ledger.any_live(), "return flight was not observed");
                for track in tick.ledger.tracks.iter().filter(|track| track.live) {
                    assert!(track.at_ms as f64 <= track.started_ms as f64 + track.budget_ms + 1.0,
                        "return flight exceeded its unchanged budget: {track:?}");
                }
            }
            5800 | 7600 => {
                assert!(view.reach().is_none() && view.tour_stop().is_none());
                assert_eq!(view.focused(), None);
                assert!(!tick.ledger.any_live(), "live tracks at {at}ms: {:?}", tick.ledger.tracks);
            }
            _ => unreachable!(),
        }
        checked += 1;
        Ok(())
    })
    .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(checked, 9, "all checkpoints must actually run");
}

#[test]
fn native_scenarios_preserve_state_picking_and_geometry_across_weather_and_cadence() {
    use crate::tokens::Appearance;
    use backend_gui_harness::Act;
    use std::collections::BTreeSet;
    let cases = [
        "graph-check-idle",
        "graph-check-interrupt",
        "graph-check-wheel-drag",
        "graph-check-hover",
        "graph-check-keyboard",
        "graph-check-chain",
        "graph-check-reach-tour",
    ];
    let variants = [
        (1440, 16, Appearance::Abyss, 1.0, false),
        (480, 8, Appearance::Glacier, 2.0, false),
        (640, 33, Appearance::Abyss, 1.5, true),
        (760, 16, Appearance::Glacier, 1.25, false),
    ];
    let mut completed = 0;
    for (width, cadence, theme, scale, reduced) in variants {
        for id in cases {
            let target = find(id).unwrap_or_else(|| panic!("missing scenario {id}"));
            let mut shot = Shot::new(&target);
            shot.size = (width, 824);
            shot.scale = 1;
            shot.frame_ms = cadence;
            shot.appearance = theme;
            shot.text_scale = scale;
            shot.reduced_motion = reduced;
            shot.probe = true;
            shot.times.clear();
            shot.until_ms = 0;
            // Resolve the same registered script, then transform physical input
            // coordinates so the gesture stays inside this measured viewport.
            let mut boot = shot.clone();
            boot.script = Some(Script::new());
            boot.frame_ms = 0;
            boot.times = vec![0];
            let mut declared = None;
            run(&target, &boot, &mut |_, _, cx| {
                declared = Some(crate::gallery::declared_script(cx)?);
                Ok(())
            })
            .unwrap_or_else(|e| panic!("{e}"));
            let mut script =
                declared.unwrap_or_else(|| panic!("scenario did not declare a script"));
            for event in &mut script.events {
                match &mut event.act {
                    Act::Move { x, .. }
                    | Act::Down { x, .. }
                    | Act::Up { x, .. }
                    | Act::Click { x, .. }
                    | Act::Scroll { x, .. }
                    | Act::Zoom { x, .. } => *x *= width as f32 / 1440.0,
                    Act::Drag { from_x, to_x, .. } => {
                        *from_x *= width as f32 / 1440.0;
                        *to_x *= width as f32 / 1440.0;
                    }
                    _ => {}
                }
            }
            shot.script = Some(script);
            shot.until_ms = 8400;
            let mut seen = BTreeSet::new();
            let mut inspected = 0;
            let mut final_camera = None;
            let mut early_camera = None;
            let mut selected_before_enter = None;
            run(&target, &shot, &mut |tick, _, cx| {
                let entity = cx
                    .global::<Current>()
                    .0
                    .upgrade()
                    .unwrap_or_else(|| panic!("graph gone"));
                let graph = entity.read(cx);
                let state = graph.inspect(cx);
                let cam = graph
                    .camera()
                    .unwrap_or_else(|| panic!("{id}: camera absent"));
                assert!(
                    cam.x.is_finite() && cam.y.is_finite() && cam.w.is_finite() && cam.w > 0.0,
                    "{id}/{width}/{cadence}: {cam:?}"
                );
                let owners = usize::from(graph.reach().is_some())
                    + usize::from(graph.tour_stop().is_some())
                    + usize::from(state.held_chain.is_some());
                assert!(
                    owners <= 1 && (!state.find_open || owners == 0),
                    "{id}: incompatible modes"
                );
                if state.find_open && !state.searching && !state.rows.is_empty() {
                    seen.insert("find-result");
                }
                if graph.focused() == Some(0) {
                    seen.insert("focus0");
                }
                if graph.focused() == Some(5) {
                    seen.insert("focus5");
                }
                if let Some(hover) = state.hover {
                    assert!((hover as usize) < graph.world().len());
                    if hover == 0 {
                        seen.insert("hover0");
                    } else {
                        seen.insert("hover-other");
                    }
                    if graph.stats().edges > 0 {
                        seen.insert("lit-edges");
                    }
                }
                if let Some(q) = state.hover_slot {
                    let frame = state
                        .frame
                        .as_ref()
                        .unwrap_or_else(|| panic!("hover slot without prism frame"));
                    assert_eq!(
                        frame.slots.get(q).and_then(|s| s.node),
                        state.hover,
                        "hover proxy/node mismatch"
                    );
                }
                if let Some(q) = state.prism_selected {
                    assert!(
                        state
                            .frame
                            .as_ref()
                            .and_then(|f| f.slots.get(q))
                            .is_some_and(|s| s.node.is_some()),
                        "selected proxy is not real"
                    );
                    seen.insert("walked-proxy");
                    if id == "graph-check-keyboard" && (2200..2320).contains(&tick.drawn.at_ms) {
                        selected_before_enter = state.frame.as_ref().and_then(|f|f.slots.get(q)).and_then(|s|s.node);
                    }
                }
                if id == "graph-check-keyboard" && (2320..4300).contains(&tick.drawn.at_ms)
                    && selected_before_enter.is_some_and(|node|node!=0 && graph.focused()==Some(node)) {
                    seen.insert("followed-proxy");
                }
                if let Some((node, gathered)) = state.prism {
                    assert!(
                        (node as usize) < graph.world().len()
                            && gathered.is_finite()
                            && (0.0..=1.0).contains(&gathered)
                    );
                }
                if let Some(path) = &state.held_chain {
                    assert_eq!(path, &vec![10, 12, 13]);
                    seen.insert("held-chain");
                    assert!(
                        tick.ledger
                            .texts
                            .iter()
                            .any(|t| t.key == "graph-chain-title"
                                && t.content.contains("Invocation")),
                        "held chain not visibly named"
                    );
                }
                if tick.ledger.texts.iter().any(|t| {
                    t.key == "graph-chain-code" && t.content == "invocation.grammar()?.aliases()"
                }) {
                    seen.insert("code");
                }
                if graph.reach().is_some() {
                    seen.insert("reach");
                }
                if graph.tour_stop().is_some() {
                    seen.insert("tour");
                }
                for text in &tick.ledger.texts {
                    if text.key.starts_with("graph-focus-") {
                        let card = tick.ledger.bounds("graph-focus-card").unwrap_or_else(|| panic!("focus text without measured card"));
                        let contained = |bounds: &crate::probe::BoundsSample| bounds.x >= card.x - 0.5 && bounds.y >= card.y - 0.5
                            && bounds.x + bounds.width <= card.x + card.width + 0.5
                            && bounds.y + bounds.height <= card.y + card.height + 0.5;
                        let reachable = tick.ledger.scrolls.iter().any(|scroll| scroll.key == "graph-focus-scroll"
                            && contained(&scroll.viewport) && scroll.reaches(&text.bounds));
                        assert!(contained(&text.bounds) || reachable,
                            "{id}/{width}/{cadence}: focus text {} escapes card without actual measured scroll reachability: text {:?}, card {card:?}",text.key,text.bounds);
                    }
                    if text.key.starts_with("graph-") {
                        assert!(
                            !text.clipped_without_ellipsis() && !text.clipped_vertically(),
                            "{id}/{width}/{cadence}: clipped {} `{}`",
                            text.key,
                            text.content
                        );
                    }
                }
                for b in &tick.ledger.bounds {
                    assert!(
                        [b.x, b.y, b.width, b.height].iter().all(|v| v.is_finite()),
                        "nonfinite measured bounds"
                    );
                    if matches!(
                        b.key.as_str(),
                        "graph-focus-card" | "graph-tour-body" | "graph-chain-body"
                    ) {
                        assert!(
                            b.within(width as f32 + 0.5, 824.5),
                            "{id}/{width}: plate outside viewport {b:?}"
                        );
                    }
                }
                if (300..480).contains(&tick.drawn.at_ms) {
                    early_camera.get_or_insert(cam);
                }
                if (800..1050).contains(&tick.drawn.at_ms)
                    && early_camera.is_some_and(|old| old != cam)
                {
                    seen.insert("gesture-moved");
                }
                if tick.drawn.at_ms >= 8000 {
                    assert_eq!(graph.focused(), None, "{id}: focus survived final back");
                    assert!(
                        !state.find_open
                            && !state.searching
                            && state.held_chain.is_none()
                            && graph.reach().is_none()
                            && graph.tour_stop().is_none()
                    );
                    assert!(
                        !tick.ledger.any_live() && !tick.drawn.requested(),
                        "{id}: quiet tail requests frames"
                    );
                    if let Some(previous) = final_camera {
                        assert_eq!(cam, previous, "{id}: settled camera drifts");
                    }
                    final_camera = Some(cam);
                    seen.insert("quiet");
                }
                inspected += 1;
                Ok(())
            })
            .unwrap_or_else(|e| panic!("{id}/{width}/{cadence}: {e}"));
            let required: &[&str] = match id {
                "graph-check-idle" => &["find-result", "quiet"],
                "graph-check-interrupt" => &["focus0", "focus5", "quiet"],
                "graph-check-wheel-drag" => &["focus0", "gesture-moved", "quiet"],
                "graph-check-hover" => &["hover0", "hover-other", "lit-edges", "quiet"],
                "graph-check-keyboard" => &["focus0", "walked-proxy", "followed-proxy", "quiet"],
                "graph-check-chain" => &["held-chain", "code", "quiet"],
                "graph-check-reach-tour" => &["reach", "tour", "quiet"],
                _ => unreachable!(),
            };
            assert!(
                inspected > 200 && final_camera.is_some(),
                "scenario did not inspect a full frame distribution"
            );
            for requirement in required {
                assert!(
                    seen.contains(requirement),
                    "{id}/{width}/{cadence}: missing actual coverage `{requirement}`, saw {seen:?}"
                );
            }
            completed += 1;
        }
    }
    assert_eq!(completed, 28, "all cadence/weather scenarios must execute");
}

#[test]
fn two_same_symbol_moves_without_a_frame_keep_the_real_peek_open() {
    use crate::probe::StackPhase;
    let target = find("graph-check-peek").unwrap_or_else(|| panic!("peek scenario missing"));
    let mut shot = Shot::new(&target);
    shot.scale = 1;
    shot.probe = true;
    shot.times = vec![100, 500, 1000, 1800, 5000];
    let mut checked = 0;
    run(&target, &shot, &mut |tick, _, cx| {
        if !shot.times.contains(&tick.drawn.at_ms) {
            return Ok(());
        }
        let entity = cx
            .global::<Current>()
            .0
            .upgrade()
            .unwrap_or_else(|| panic!("graph gone"));
        let graph = entity.read(cx);
        let state = graph.inspect(cx);
        if tick.drawn.at_ms <= 1000 {
            assert_eq!(
                graph.focused(),
                None,
                "hover fixture must not hide picking behind focus"
            );
            assert_eq!(state.hover, Some(0), "same-symbol jitter changed real pick");
            if tick.drawn.at_ms >= 500 {
                assert!(
                    tick.ledger
                        .stacks
                        .iter()
                        .flat_map(|s| &s.entries)
                        .any(|e| e.kind == "peek" && (e.phase == StackPhase::Open || (tick.drawn.at_ms == 500 && e.phase == StackPhase::Entering))),
                    "intent elapsed but real peek vanished: {:?}",
                    tick.ledger.stacks
                );
            }
        } else {
            assert_eq!(state.hover, None);
            assert!(
                tick.ledger.stacks.iter().all(|s| s.entries.is_empty()),
                "leave leaked peek: {:?}",
                tick.ledger.stacks
            );
        }
        checked += 1;
        Ok(())
    })
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(checked, 5);
}


#[test]
fn parked_pointer_uses_current_prism_geometry_after_resize_and_text_scale() {
    let target = find("graph-check-parked").unwrap_or_else(|| panic!("parked scene missing"));
    let mut shot = Shot::new(&target);
    shot.scale = 1;
    shot.probe = true;
    shot.times = vec![1050, 1210, 1230, 1250, 1400, 2100, 4200];
    let mut initial = None;
    let mut checked = 0;
    run(&target, &shot, &mut |tick, _, cx| {
        if !shot.times.contains(&tick.drawn.at_ms) { return Ok(()); }
        let entity = cx.global::<Current>().0.upgrade().unwrap_or_else(|| panic!("graph gone"));
        let graph = entity.read(cx);
        let state = graph.inspect(cx);
        if tick.drawn.at_ms < 2200 {
            let pointer = state.pointer.unwrap_or_else(|| panic!("real pointer absent"));
            if let Some(previous) = initial { assert_eq!(pointer,previous,"weather fabricated another pointer move"); }
            else { initial = Some(pointer); assert_eq!(state.hover,Some(4),"initial label never picked actual prism node"); }
            let frame = state.frame.as_ref().unwrap_or_else(|| panic!("prism absent"));
            let picked = frame.pick(pointer.0,pointer.1);
            assert_eq!(state.hover_slot,picked,"hover uses stale geometry after text scale/resize");
            if let Some(slot) = picked { assert_eq!(state.hover,frame.slots[slot].node,"hovered ID differs from visible row under parked pointer"); }
        } else {
            assert_eq!(state.pointer,None); assert_eq!(state.hover,None);
            assert!(!tick.ledger.any_live() && !tick.drawn.requested(),"parked pointer left motion running");
        }
        checked += 1;
        Ok(())
    }).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(checked,7,"all parked-pointer checkpoints must run");
}


#[test]
fn opening_find_during_focus_flight_and_closing_it_after_arrival_restores_prism() {
    for reduced in [false,true] {
        let target = find("graph-pinned-world").unwrap_or_else(|| panic!("pinned world absent"));
        let mut shot = Shot::new(&target);
        shot.scale = 1;
        shot.probe = true;
        shot.reduced_motion = reduced;
        shot.times = vec![400,2000,3200,7200];
        shot.script = Some(Script::parse(
            "key / @200; type \"glyph::RelationLabel\" @240; key enter @280; key / @400; key escape @2200; key escape @3800; leave @4000; leave @6200"
        ).unwrap_or_else(|error| panic!("{error}")));
        let mut checked = 0;
        run(&target,&shot,&mut |tick,_,cx| {
            if !shot.times.contains(&tick.drawn.at_ms) { return Ok(()); }
            let entity = cx.global::<Current>().0.upgrade().unwrap_or_else(|| panic!("graph gone"));
            let graph = entity.read(cx); let state = graph.inspect(cx);
            if tick.drawn.at_ms <= 2000 {
                assert_eq!(graph.focused(),Some(0),"find lost in-flight focus, reduced={reduced}");
                assert!(state.find_open,"nativefind did not open, reduced={reduced}");
                assert!(state.frame.is_none(),"find visibly gathers prism roads despite actual search mode, reduced={reduced}, at={}ms",tick.drawn.at_ms);
            } else if tick.drawn.at_ms == 3200 {
                assert!(!state.find_open && !state.searching);
                assert_eq!(graph.focused(),Some(0));
                assert!(state.prism.is_some_and(|(id,g)|id==0 && g>=0.999),"settledfindclosure failed to restore actualgather, reduced={reduced}");
                let frame = state.frame.as_ref().unwrap_or_else(|| panic!("gatheredprism frame absent"));
                assert_eq!(frame.node,0);
                assert!(frame.slots.iter().any(|slot|slot.node==Some(4)),"restored prism omits unrelated render callable");
            } else {
                assert_eq!(graph.focused(),None);
                assert!(!state.find_open && !state.searching && !tick.ledger.any_live() && !tick.drawn.requested());
            }
            checked += 1;
            Ok(())
        }).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(checked,4,"all normal/reduced arrivalfind checkpoints must execute");
    }
}


#[test]
fn real_native_zoom_and_drag_preserve_world_anchors_in_normal_and_offset_viewports() {
    for id in ["graph-pinned-world","graph-pinned-offset"] {
        for drag in [false,true] {
            let target = find(id).unwrap_or_else(|| panic!("scene {id} absent"));
            let mut shot = Shot::new(&target); shot.scale=1; shot.probe=true;
            let (x,y) = if id=="graph-pinned-offset" { (300.0,260.0) } else { (720.0,300.0) };
            shot.times = if drag { vec![900,1200,1400,1800] } else { vec![900,1100,1800,2600] };
            let script = if drag { format!("down {x},{y} @1000; move {},{} @1200; up {},{} @1600; leave @2000",x+60.0,y+30.0,x+60.0,y+30.0) }
                else { format!("wheel-zoom {x},{y} 1.25 @1000; leave @2800") };
            shot.script=Some(Script::parse(&script).unwrap_or_else(|error|panic!("{error}")));
            let mut before = None; let mut checked=0;
            run(&target,&shot,&mut |tick,_,cx| {
                if !shot.times.contains(&tick.drawn.at_ms) { return Ok(()); }
                let entity=cx.global::<Current>().0.upgrade().unwrap_or_else(||panic!("graph gone"));
                let graph=entity.read(cx); let state=graph.inspect(cx);
                let view=state.viewport.unwrap_or_else(||panic!("actualviewport absent"));
                let cam=graph.camera().unwrap_or_else(||panic!("camera absent"));
                if tick.drawn.at_ms==900 { assert!(!state.moving,"gesture must start from actualrest");before=Some(cam); }
                else {
                    let old=before.unwrap_or_else(||panic!("gesture baseline absent"));
                    let anchor=view.to_world(&old,x,y);
                    let point=view.to_world(&cam,if drag{x+60.0}else{x},if drag{y+30.0}else{y});
                    let error=(point.0-anchor.0).hypot(point.1-anchor.1)*view.k(&cam);
                    assert!(error<0.001,"{id}/drag{drag}: actualworldanchor drifted {error}px");
                    if drag {
                        assert_eq!(cam.w,old.w,"drag changedzoom");
                        assert!((cam.x-(old.x-60.0/view.k(&old))).abs()<1e-9 && (cam.y-(old.y-30.0/view.k(&old))).abs()<1e-9,
                            "{id}: native drag handler didnotapplyactualdisplacement");
                    } else if tick.drawn.at_ms==2600 {
                        let expected=(old.w/1.25).max(crate::graph::camera::MIN_W);
                        assert!((cam.w-expected).abs()<1e-9 && cam.w<old.w,"{id}: native zoom handler didnotapplyactualfactor");
                        assert!(!state.moving && !tick.ledger.any_live(),"nativezoom didnotsettle");
                    }
                }
                checked+=1;Ok(())
            }).unwrap_or_else(|error|panic!("{error}"));
            assert_eq!(checked,4,"every normal/offset gesture checkpoint mustexecute");
        }
    }
}


#[test]
fn input_timing_captures_real_adapter_work_and_resets_after_each_draw() {
    fn slow_adapter(_: &backend_gui_harness::Act,_:&mut gpui::Window,_:&mut gpui::App) {
        let started=std::time::Instant::now();
        while started.elapsed()<std::time::Duration::from_millis(6) { std::hint::spin_loop(); }
    }
    let target=crate::gallery::Scene {
        id:"input-timer-canary",title:"Actual synchronous adapter work",size:(480,824),
        build:|window,cx| {
            let view=super::build(Src::Pinned,super::At::World,window,cx);
            crate::gallery::declare_adapter(slow_adapter,cx);view
        },
    };
    let mut shot=Shot::new(&target);shot.times=vec![100,116];shot.scale=1;
    shot.script=Some(Script::parse("route timer-canary @100").unwrap_or_else(|error|panic!("{error}")));
    let mut checked=0;
    run(&target,&shot,&mut |tick,_,_| {
        if tick.drawn.at_ms==100 {
            assert_eq!(tick.drawn.input_events,1);
            assert!(tick.drawn.input_cpu>=std::time::Duration::from_millis(6),"input timer omitted actual synchronous adapter work");
            assert_eq!(tick.drawn.input_max,tick.drawn.input_cpu);
            checked+=1;
        } else if tick.drawn.at_ms==116 {
            assert_eq!(tick.drawn.input_events,0);assert_eq!(tick.drawn.input_cpu,std::time::Duration::ZERO);
            assert_eq!(tick.drawn.input_max,std::time::Duration::ZERO);checked+=1;
        }
        Ok(())
    }).unwrap_or_else(|error|panic!("{error}"));
    assert_eq!(checked,2,"actual input and quietdraw were not both measured");
}

#[test]
fn native_focus_card_padding_click_keeps_focus_and_does_not_drag_graph() {
    use crate::tokens::Appearance;
    for (size, theme, text_scale) in [((1440,824),Appearance::Abyss,1.0),((480,400),Appearance::Glacier,2.0)] {
        let target=find("graph-pinned-focus").unwrap_or_else(||panic!("focus fixture absent"));
        let mut shot=Shot::new(&target);
        shot.size=size; shot.appearance=theme; shot.text_scale=text_scale;
        shot.scale=1; shot.probe=true; shot.times=vec![1200];
        let mut measured=None;
        run(&target,&shot,&mut |tick,_,cx| {
            if tick.drawn.at_ms==1200 {
                let entity=cx.global::<Current>().0.upgrade().unwrap_or_else(||panic!("graph gone"));
                let graph=entity.read(cx);
                let card=tick.ledger.bounds("graph-focus-card").unwrap_or_else(||panic!("actual card missing"));
                assert_eq!(graph.focused(),Some(0));
                assert!(!graph.inspect(cx).moving,"padding baseline must start at rest");
                measured=Some((card.x+4.0,card.y+4.0,graph.camera().unwrap()));
            }
            Ok(())
        }).unwrap_or_else(|error|panic!("{error}"));
        let (x,y,cam)=measured.unwrap_or_else(||panic!("padding baseline was not observed"));
        shot.times=vec![1400,1450,1520,2600];
        shot.script=Some(Script::parse(&format!("down {x},{y} @1400; move {},{} @1450; up {},{} @1500; leave @1600",x+2.0,y+2.0,x+2.0,y+2.0)).unwrap());
        let mut checked=0;
        run(&target,&shot,&mut |tick,_,cx| {
            if shot.times.contains(&tick.drawn.at_ms) {
                let entity=cx.global::<Current>().0.upgrade().unwrap_or_else(||panic!("graph gone"));
                let graph=entity.read(cx);
                assert_eq!(graph.focused(),Some(0),"card padding cleared focus");
                assert_eq!(graph.camera(),Some(cam),"card padding propagated a graph drag");
                assert!(!graph.inspect(cx).moving,"card padding started camera motion");
                checked+=1;
            }
            Ok(())
        }).unwrap_or_else(|error|panic!("{error}"));
        assert_eq!(checked,4,"all native padding checkpoints must execute");
    }
}
