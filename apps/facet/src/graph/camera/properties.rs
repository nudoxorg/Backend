//! Properties of the actual camera segment kernel, independent of GPUI draw
//! scheduling. Fixtures assert world/screen values, not implementation tags.
use super::{Camera, Rig, Sample, Segment, View};
use std::time::{Duration, Instant};

fn sample(rig: &mut Rig, at: Instant) -> Sample {
    rig.direct_at(at).map_or(
        Sample {
            camera: rig.cam,
            velocity: rig.velocity,
            live: false,
        },
        |s| s.0,
    )
}

fn close(a: Camera, b: Camera, scale: f64) {
    for (x, y) in [(a.x, b.x), (a.y, b.y), (a.w, b.w)] {
        assert!(
            (x - y).abs() <= 2e-9 * scale.max(1.0),
            "{a:?} differs from {b:?}"
        );
    }
}

fn assert_rest(rig: &Rig, frame: Sample, expected: Camera) {
    assert!(!frame.live);
    assert_eq!(frame.camera, expected);
    assert_eq!(frame.velocity, (0.0, 0.0, 0.0));
    assert!(matches!(rig.segment, Segment::Still));
}

#[test]
fn pointer_anchor_property_spans_zoom_clamps_views_and_delayed_frames() {
    let epoch = Instant::now();
    for w in [2.5, 40.0, 400.0, 1e6] {
        for factor in [0.001, 0.125, 0.5, 1.0, 2.0, 32.0] {
            for view_w in [480.0, 640.0, 1440.0, 2560.0] {
                for (left, top) in [(0.0, 0.0), (37.0, -91.0), (1433.0, 621.0)] {
                    let view = View {
                        x: left,
                        y: top,
                        w: view_w,
                        h: 824.0,
                    };
                    for (ux, uy) in [(0.0, 0.0), (0.25, 0.75), (0.5, 0.5), (0.75, 0.25)] {
                        let from = Camera::new(-137.0, 291.0, w);
                        let (px, py) = (view.x + view.w * ux, view.y + view.h * uy);
                        let fixed = view.to_world(&from, px, py);
                        let mut rig = Rig::new(from);
                        rig.zoom_about(&view, px, py, factor, 2e6);
                        let expected = rig.target();
                        let budget = match rig.segment {
                            Segment::Ease(ease) => ease.budget,
                            _ => 0.0,
                        };
                        let first = epoch + Duration::from_millis(2_000);
                        assert_eq!(
                            sample(&mut rig, first).camera,
                            from,
                            "a delayed first frame is exact"
                        );
                        for ms in [1, 7, 16, 70, 143, 260, 641, 999, 2_000] {
                            let frame = sample(&mut rig, first + Duration::from_millis(ms));
                            let point = view.to_world(&frame.camera, px, py);
                            let eps = w.max(frame.camera.w) * 1e-12 + 1e-9;
                            assert!(
                                (point.0 - fixed.0).abs() < eps && (point.1 - fixed.1).abs() < eps,
                                "w={w} factor={factor} view={view:?} t={ms}: {fixed:?} -> {point:?}"
                            );
                            assert!(frame.camera.w >= super::MIN_W && frame.camera.w <= 2e6);
                            if ms as f64 >= budget {
                                assert!(!frame.live, "missed fixed budget {budget} at {ms}ms");
                                assert_eq!(frame.camera, expected);
                            }
                        }
                        assert_eq!(rig.cam.w, (w * factor).clamp(2.5, 2e6));
                        let final_frame = sample(&mut rig, first + Duration::from_millis(2_100));
                        assert_rest(&rig, final_frame, expected);
                    }
                }
            }
        }
    }
}

#[test]
fn zoom_is_covariant_under_world_scaling_and_viewport_translation() {
    let at = Instant::now();
    let base_view = View {
        x: 0.0,
        y: 0.0,
        w: 1440.0,
        h: 824.0,
    };
    for factor in [0.125, 0.5, 2.0, 8.0] {
        let from = Camera::new(-137.0, 291.0, 400.0);
        let mut baseline = Rig::new(from);
        baseline.zoom_about(&base_view, 1080.0, 206.0, factor, 1e8);
        sample(&mut baseline, at);
        let expected: Vec<_> = [0, 1, 70, 260, 641, 999, 2_000]
            .map(|ms| sample(&mut baseline, at + Duration::from_millis(ms)).camera)
            .to_vec();
        for scale in [0.25, 1.0, 10.0] {
            for (left, top) in [(0.0, 0.0), (37.0, -91.0), (1433.0, 621.0)] {
                let view = View {
                    x: left,
                    y: top,
                    ..base_view
                };
                let (dx, dy) = (100_000.0, -3_300.0);
                let mut rig = Rig::new(Camera::new(
                    from.x * scale + dx,
                    from.y * scale + dy,
                    from.w * scale,
                ));
                rig.zoom_about(&view, 1080.0 + left, 206.0 + top, factor, 1e8);
                sample(&mut rig, at);
                for (ms, expected) in [0, 1, 70, 260, 641, 999, 2_000].into_iter().zip(&expected) {
                    let camera = sample(&mut rig, at + Duration::from_millis(ms)).camera;
                    close(
                        Camera::new(
                            (camera.x - dx) / scale,
                            (camera.y - dy) / scale,
                            camera.w / scale,
                        ),
                        *expected,
                        from.w,
                    );
                }
            }
        }
    }
}

#[test]
fn absolute_segment_samples_ignore_frame_partition_and_first_frame_delay() {
    let epoch = Instant::now();
    let view = View {
        x: 37.0,
        y: 91.0,
        w: 1440.0,
        h: 824.0,
    };
    for mode in 0..7 {
        for first_delay in [0, 16, 2_000, 20_000] {
            for total in [70, 260, 641, 999, 2_000] {
                let make = || {
                    let mut rig = Rig::new(Camera::new(5.0, -3.0, 400.0));
                    rig.view_width = f64::from(view.w);
                    match mode {
                        0 => rig.zoom_about(&view, 1117.0, 297.0, 0.125, 1e8),
                        1 => rig.pan_px(&view, -900.0, 300.0),
                        2 => rig.fling(0.5, -0.25),
                        3 => {
                            rig.zoom_about(&view, 1117.0, 297.0, 2.5 / 400.0, 1e8);
                            rig.pan_px(&view, 300.0, -117.0);
                        }
                        4 => {
                            rig.zoom_about(&view, 1117.0, 297.0, 0.125, 1e8);
                            rig.nudge(-0.13, 0.21);
                        }
                        5 => {
                            rig.zoom_about(&view, 1117.0, 297.0, 8.0, 1e8);
                            rig.scale(0.25, 1e8);
                        }
                        _ => {
                            rig.zoom_about(&view, 1117.0, 297.0, 0.125, 1e8);
                            rig.zoom_about(&view, 397.0, 709.0, 16.0, 1e8);
                        }
                    }
                    rig
                };
                let first = epoch + Duration::from_millis(first_delay);
                let mut single = make();
                let direct_pose = single.cam;
                assert_eq!(sample(&mut single, first).camera, direct_pose);
                let expected = sample(&mut single, first + Duration::from_millis(total));
                for quantum in [1, 7, 16, 33, 143] {
                    let mut partitioned = make();
                    sample(&mut partitioned, first);
                    let mut elapsed = 0;
                    while elapsed < total {
                        elapsed = (elapsed + quantum).min(total);
                        sample(&mut partitioned, first + Duration::from_millis(elapsed));
                    }
                    assert_eq!(
                        partitioned.cam, expected.camera,
                        "mode={mode} delay={first_delay} total={total} quantum={quantum}"
                    );
                    assert_eq!(partitioned.velocity, expected.velocity);
                    assert_eq!(
                        matches!(partitioned.segment, Segment::Still),
                        !expected.live
                    );
                }
            }
        }
    }
}

/// Reversing the pointer-anchored target hundreds of times must never move
/// the drawn camera at the instant of retarget. Screen velocity is checked
/// against an independent forward sample, including off-centre landmarks.
#[test]
fn reversal_storm_has_exact_retarget_position_and_screen_derivatives() {
    let epoch = Instant::now();
    let view = View {
        x: 37.0,
        y: 91.0,
        w: 1440.0,
        h: 824.0,
    };
    let mut rig = Rig::new(Camera::new(-137.0, 291.0, 400.0));
    let mut at = epoch;
    let mut rng = 0x2545_f491_4f6c_dd1d_u64;
    for n in 0..512 {
        rng ^= rng >> 12;
        rng ^= rng << 25;
        rng ^= rng >> 27;
        let offset = if n % 2 == 0 { 0.25 } else { 0.75 };
        let (px, py) = (view.x + view.w * offset, view.y + view.h * (1.0 - offset));
        let mut before = rig.cam;
        let factor = if n % 2 == 0 { 0.5 } else { 2.0 };
        rig.zoom_about(&view, px, py, factor, 1e6);
        match n % 4 {
            0 => rig.pan_px(&view, 300.0, -117.0),
            1 => rig.nudge(-0.13, 0.21),
            2 => rig.scale(1.7, 1e6),
            _ => {}
        }
        if n % 4 == 0 {
            before.x -= 300.0 * before.w / f64::from(view.w);
            before.y += 117.0 * before.w / f64::from(view.w);
        }
        assert_eq!(rig.cam, before);
        let current = sample(&mut rig, at);
        assert_eq!(current.camera, before, "retarget position at act {n}");
        let future = match rig.segment {
            Segment::Ease(ease) => ease.sample(0.001),
            _ => panic!("moving zoom expected"),
        };
        assert_screen_hull(before, rig.target(), future.camera, &view);
        let (vx, vy, zoom) = current.velocity;
        let k = f64::from(view.w) / before.w;
        for (wx, wy) in [
            (before.x, before.y),
            (before.x + before.w * 0.3, before.y - before.w * 0.2),
        ] {
            let screen = |c: Camera| {
                (
                    (wx - c.x) * f64::from(view.w) / c.w,
                    (wy - c.y) * f64::from(view.w) / c.w,
                )
            };
            let (a, b) = (screen(before), screen(future.camera));
            let observed = ((b.0 - a.0) * 1e6, (b.1 - a.1) * 1e6);
            let expected = (
                -k * (vx + (wx - before.x) * zoom),
                -k * (vy + (wy - before.y) * zoom),
            );
            for (got, want) in [(observed.0, expected.0), (observed.1, expected.1)] {
                assert!(
                    (got - want).abs() < 2e-4 * want.abs().max(1.0),
                    "act={n} screen derivative {observed:?} vs {expected:?}"
                );
            }
        }
        at += Duration::from_millis(1 + rng % 33);
        let frame = sample(&mut rig, at);
        assert!(
            frame.camera.x.is_finite()
                && frame.camera.y.is_finite()
                && frame.camera.w.is_finite()
                && frame.camera.w > 0.0
        );
    }
    let expected = rig.target();
    let frame = sample(&mut rig, at + Duration::from_secs(3));
    assert_rest(&rig, frame, expected);
}

#[test]
fn finite_approach_is_monotone_c2_at_join_and_lands_at_exact_rest() {
    for delta in [
        -10_000.0, -10.0, -0.001, -0.00001, 0.00001, 0.001, 10.0, 10_000.0,
    ] {
        for eps in [0.0001, 0.05, 1.0] {
            let axis = super::Decay::new(17.0, 17.0 + delta, eps);
            let at = |t: f64| axis.sample(t, (-t / super::EASE_MS).exp());
            let (lo, hi) = (17.0_f64.min(17.0 + delta), 17.0_f64.max(17.0 + delta));
            let mut previous = 17.0;
            for index in 0..=100 {
                let (value, velocity) = at(axis.duration * f64::from(index) / 100.0);
                assert!(value >= lo && value <= hi);
                assert!((value - previous) * delta.signum() >= 0.0);
                assert!(velocity * delta.signum() >= 0.0);
                previous = value;
            }
            if axis.cutoff > 0.0 {
                let h = 0.0001;
                let before = at(axis.cutoff - h).1;
                let after = at(axis.cutoff + h).1;
                let acceleration = (after - before) / (2.0 * h / 1000.0);
                let expected = -eps * delta.signum() / 0.07_f64.powi(2);
                assert!(
                    (acceleration - expected).abs() < 0.001 * expected.abs().max(1.0),
                    "velocity/acceleration join: delta={delta} eps={eps} {acceleration} vs {expected}"
                );
            }
            let h = (axis.duration - axis.cutoff) * 1e-6;
            let arriving = at(axis.duration - h).1;
            assert!(arriving.abs() < axis.speed.abs() * 1000.0 * 1e-8);
            assert_eq!(at(axis.duration), (17.0 + delta, 0.0));
            assert_eq!(at(axis.duration + 2_000.0), (17.0 + delta, 0.0));
        }
    }
}

/// The target landmark began 359.99835px from the centre, but the old
/// independent centre/log-width filter swept it 4,106,375.0995px away at
/// 180ms. The pending pan must preserve its intended goal without a sweep.
#[test]
fn pending_extreme_zoom_then_pan_stays_inside_endpoint_screen_hull() {
    let epoch = Instant::now();
    let view = View {
        x: 0.0,
        y: 0.0,
        w: 1440.0,
        h: 824.0,
    };
    let from = Camera::new(0.0, 0.0, 1e6);
    let mut rig = Rig::new(from);
    rig.zoom_about(&view, 1080.0, 412.0, 2.5e-6, 2e6);
    sample(&mut rig, epoch);
    rig.pan_px(&view, 300.0, 0.0);
    let goal = rig.target();
    close(goal, Camera::new(249998.85416666666, 0.0, 2.5), 1.0);
    let drawn = rig.cam;
    assert_eq!(drawn, Camera::new(-300.0 * 1e6 / 1440.0, 0.0, 1e6));
    let maximum = (goal.x - drawn.x) * 1440.0 / drawn.w;
    assert!((maximum - 659.99835).abs() < 1e-10);
    for ms in 0..=1000 {
        let frame = sample(&mut rig, epoch + Duration::from_millis(ms));
        let offset = (goal.x - frame.camera.x) * 1440.0 / frame.camera.w;
        assert!(
            offset >= -1e-8 && offset <= maximum + 1e-8,
            "t={ms}ms swept to {offset}px"
        );
    }
    let final_frame = sample(&mut rig, epoch + Duration::from_secs(2));
    assert_rest(&rig, final_frame, goal);
}

fn assert_screen_hull(from: Camera, to: Camera, current: Camera, view: &View) {
    for (wx, wy) in [
        (from.x, from.y),
        (to.x, to.y),
        (from.x + from.w * 0.3, from.y - from.w * 0.2),
        (to.x - to.w * 0.49, to.y + to.w * 0.49),
    ] {
        // Independent f64 projection, including viewport translation.
        let project = |c: Camera| {
            (
                (wx - c.x) * f64::from(view.w) / c.w + f64::from(view.x),
                (wy - c.y) * f64::from(view.w) / c.w + f64::from(view.y),
            )
        };
        let (a, b, p) = (project(from), project(to), project(current));
        for (lo, hi, value) in [
            (a.0.min(b.0), a.0.max(b.0), p.0),
            (a.1.min(b.1), a.1.max(b.1), p.1),
        ] {
            let eps = 1e-6 + lo.abs().max(hi.abs()) * 8e-13;
            assert!(
                value >= lo - eps && value <= hi + eps,
                "screen hull [{lo},{hi}] violated by {value}: {from:?} -> {current:?} -> {to:?}"
            );
        }
    }
}

#[test]
fn mixed_pending_inputs_preserve_intent_and_screen_bounds_across_zoom_ratios() {
    let epoch = Instant::now();
    for width in [2.5, 400.0, 1e6] {
        for factor in [2.5e-6, 0.5, 0.999999999, 1.0, 1.000000001, 2.0, 8e5] {
            for view_w in [480.0, 1440.0, 2560.0] {
                for (left, top) in [(0.0, 0.0), (37.0, -91.0), (1433.0, 621.0)] {
                    let view = View {
                        x: left,
                        y: top,
                        w: view_w,
                        h: 824.0,
                    };
                    for delay in [0, 16, 143, 700] {
                        for input in 0..4 {
                            let mut rig = Rig::new(Camera::new(-137.0, 291.0, width));
                            rig.zoom_about(&view, left + view_w * 0.75, top + 206.0, factor, 2e6);
                            sample(&mut rig, epoch);
                            let at = epoch + Duration::from_millis(delay);
                            let from = sample(&mut rig, at).camera;
                            let pending = rig.target();
                            let old_budget = match rig.segment {
                                Segment::Ease(e) => e.budget,
                                _ => 0.0,
                            };
                            let intended = match input {
                                0 => {
                                    rig.pan_px(&view, 300.0, -117.0);
                                    Camera::new(
                                        pending.x - 300.0 * pending.w / f64::from(view.w),
                                        pending.y + 117.0 * pending.w / f64::from(view.w),
                                        pending.w,
                                    )
                                }
                                1 => {
                                    rig.nudge(-0.13, 0.21);
                                    Camera::new(
                                        pending.x - 0.13 * pending.w,
                                        pending.y + 0.21 * pending.w,
                                        pending.w,
                                    )
                                }
                                2 => {
                                    rig.scale(1.7, 2e6);
                                    Camera::new(
                                        pending.x,
                                        pending.y,
                                        (pending.w * 1.7).clamp(2.5, 2e6),
                                    )
                                }
                                _ => {
                                    let (px, py) = (left + view_w * 0.25, top + 618.0);
                                    let fixed = view.to_world(&from, px, py);
                                    let w = (pending.w * 0.7).clamp(2.5, 2e6);
                                    rig.zoom_about(&view, px, py, 0.7, 2e6);
                                    if w == from.w {
                                        from
                                    } else {
                                        Camera::new(
                                            fixed.0 + 0.25 * w,
                                            fixed.1 - 206.0 / f64::from(view.w) * w,
                                            w,
                                        )
                                    }
                                }
                            };
                            close(rig.target(), intended, pending.w);
                            let goal = rig.target();
                            let from = if input == 0 {
                                let pose = Camera::new(
                                    from.x - 300.0 * from.w / f64::from(view.w),
                                    from.y + 117.0 * from.w / f64::from(view.w),
                                    from.w,
                                );
                                assert_eq!(
                                    rig.cam, pose,
                                    "pan must follow the pointer immediately"
                                );
                                pose
                            } else {
                                assert_eq!(
                                    rig.cam, from,
                                    "zoom/key retarget changed drawn position"
                                );
                                from
                            };
                            let budget = match rig.segment {
                                Segment::Ease(e) => {
                                    let log_span = (goal.w / from.w).ln().abs();
                                    let pixel_span =
                                        (goal.x - from.x).abs().max((goal.y - from.y).abs())
                                            * f64::from(view.w)
                                            / from.w;
                                    let pan_cutoff =
                                        super::EASE_MS * (pixel_span / 0.05).max(1.0).ln();
                                    let zoom_cutoff =
                                        super::EASE_MS * (log_span / super::ZOOM_EPS).max(1.0).ln();
                                    let original = pan_cutoff.max(zoom_cutoff) + 120.0;
                                    let original = if input == 0 { old_budget } else { original };
                                    assert!(
                                        (e.budget - original).abs() < 1e-8,
                                        "budget changed: {} vs {original}",
                                        e.budget
                                    );
                                    e.budget
                                }
                                _ => 0.0,
                            };
                            assert_eq!(sample(&mut rig, at).camera, from);
                            for ms in [1, 7, 16, 33, 70, 140, 180, 260, 400, 641, 999, 2000] {
                                let frame = sample(&mut rig, at + Duration::from_millis(ms));
                                assert_screen_hull(from, goal, frame.camera, &view);
                                if ms as f64 >= budget {
                                    assert_rest(&rig, frame, goal);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn usable_room_framing_projects_to_room_centre_and_fits_both_axes() {
    let full = View {
        x: 37.0,
        y: 91.0,
        w: 1440.0,
        h: 824.0,
    };
    let room = View {
        x: 107.0,
        y: 147.0,
        w: 1000.0,
        h: 712.0,
    };
    let bounds = super::Box2 {
        x0: -20.0,
        x1: 80.0,
        y0: -10.0,
        y1: 50.0,
    };
    let camera = full.frame_in(bounds, &room, 1.2);
    close(camera, Camera::new(48.0, 20.0, 172.8), 1.0);
    let projected = full.to_screen(&camera, 30.0, 20.0);
    assert_eq!(projected, (607.0, 503.0));
    let shorter = View { h: 600.0, ..room };
    let offset = full.frame_in(bounds, &shorter, 1.2);
    close(offset, Camera::new(48.0, 26.72, 172.8), 1.0);
    assert_eq!(full.to_screen(&offset, 30.0, 20.0), (607.0, 447.0));
    for width in [480.0, 640.0, 1440.0, 2560.0] {
        for margin in [0.0, 48.0, 180.0] {
            for offset in [0.0, 37.0, 1433.0] {
                let full = View {
                    x: offset,
                    y: -91.0,
                    w: width,
                    h: 824.0,
                };
                let room = View {
                    x: offset + margin,
                    y: -35.0,
                    w: (width - margin - 210.0).max(48.0),
                    h: 712.0,
                };
                for bounds in [
                    bounds,
                    super::Box2 {
                        x0: -10.0,
                        x1: 10.0,
                        y0: -500.0,
                        y1: 500.0,
                    },
                ] {
                    let camera = full.frame_in(bounds, &room, 1.2);
                    let centre = bounds.center();
                    let projected = full.to_screen(&camera, centre.0, centre.1);
                    assert!((projected.0 - room.x - room.w / 2.0).abs() < 0.001);
                    assert!((projected.1 - room.y - room.h / 2.0).abs() < 0.001);
                    for (x, y) in [(bounds.x0, bounds.y0), (bounds.x1, bounds.y1)] {
                        let (px, py) = full.to_screen(&camera, x, y);
                        assert!(px >= room.x - 0.001 && px <= room.x + room.w + 0.001);
                        assert!(py >= room.y - 0.001 && py <= room.y + room.h + 0.001);
                    }
                    assert_eq!(full.frame_in(bounds, &full, 1.2), full.frame(bounds, 1.2));
                }
            }
        }
    }
}

/// The direct pan contract is stronger than eventual convergence: every
/// landmark moves by the event's pixels immediately, even during a live zoom.
#[test]
fn pointer_pan_is_immediate_and_live_zoom_is_a_screen_translation() {
    let at = Instant::now();
    for width in [2.5, 400.0, 1e6] {
        for view_w in [480.0, 1440.0, 2560.0] {
            for factor in [0.001, 0.5, 2.0, 1000.0] {
                let view = View {
                    x: 37.0,
                    y: -91.0,
                    w: view_w,
                    h: 824.0,
                };
                let from = Camera::new(17.0, -11.0, width);
                let make = || {
                    let mut r = Rig::new(from);
                    r.zoom_about(&view, view.x + view.w * 0.75, view.y + 206.0, factor, 2e6);
                    sample(&mut r, at);
                    r
                };
                for elapsed in [0, 16, 143, 700] {
                    let mut baseline = make();
                    let mut panned = make();
                    let now = at + Duration::from_millis(elapsed);
                    sample(&mut baseline, now);
                    sample(&mut panned, now);
                    let initial = panned.cam;
                    panned.pan_px(&view, 173.0, -89.0);
                    assert_eq!(
                        panned.cam,
                        Camera::new(
                            initial.x - 173.0 * initial.w / f64::from(view_w),
                            initial.y + 89.0 * initial.w / f64::from(view_w),
                            initial.w
                        )
                    );
                    for ms in [0, 1, 7, 16, 70, 260, 999, 2000] {
                        let a = sample(&mut baseline, now + Duration::from_millis(ms)).camera;
                        let b = sample(&mut panned, now + Duration::from_millis(ms)).camera;
                        assert_eq!(a.w, b.w, "pan must not restart or delay zoom");
                        for world in [
                            (from.x, from.y),
                            (from.x + width * 0.3, from.y - width * 0.2),
                        ] {
                            let dx =
                                ((world.0 - b.x) / b.w - (world.0 - a.x) / a.w) * f64::from(view_w);
                            let dy =
                                ((world.1 - b.y) / b.w - (world.1 - a.y) / a.w) * f64::from(view_w);
                            assert!(
                                (dx - 173.0).abs() < 2e-5 && (dy + 89.0).abs() < 2e-5,
                                "exact screen translation lost: {dx},{dy}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn measured_reframe_keeps_semantic_flight_and_once_only_landing_intent() {
    use super::{Landing, Travel};
    let initial = Camera::new(0.0, 0.0, 400.0);
    let mut rig = Rig::new(initial);
    let context = Travel::Focus(Camera::new(300.0, 150.0, 1200.0));
    rig.fly_with(
        Camera::new(500.0, 300.0, 40.0),
        Some(Landing::Gather(7)),
        context,
    );
    for correction in [
        Camera::new(505.0, 300.0, 44.0),
        Camera::new(511.0, 307.0, 55.0),
    ] {
        rig.reframe(correction);
        assert_eq!(rig.cam, initial);
        assert_eq!(rig.destination(), Some(correction));
        assert_eq!(rig.travel(), Some(context));
        assert!(matches!(
            rig.segment,
            Segment::Flight {
                landing: Some(Landing::Gather(7)),
                ..
            }
        ));
    }
    rig.set(Camera::new(511.0, 307.0, 55.0));
    rig.reframe(Camera::new(516.0, 310.0, 56.0));
    assert_eq!(rig.travel(), Some(Travel::Reframe));
    assert!(matches!(rig.segment, Segment::Flight { landing: None, .. }));
}
