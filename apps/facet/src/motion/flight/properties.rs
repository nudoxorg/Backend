//! Screen-space continuity properties of the same flight planner used by
//! GPUI, over repeated reversals and world/viewport transforms.
use super::{Camera, Pacing, Path, plan};
use std::time::{Duration, Instant};

fn screen(camera: Camera, world: (f64, f64), width: f64, origin: (f64, f64)) -> (f64, f64) {
    let k = width / camera.w;
    (
        origin.0 + (world.0 - camera.x) * k,
        origin.1 + (world.1 - camera.y) * k,
    )
}

#[test]
fn repeated_flight_reversals_preserve_screen_position_and_velocity() {
    let epoch = Instant::now();
    for scale in [0.01, 1.0, 100.0] {
        for viewport in [480.0, 1440.0, 2560.0] {
            let origin = (37.0, -91.0);
            let start = Camera::new(17.0 * scale, -11.0 * scale, 200.0 * scale);
            let mut trip = plan(
                start,
                (0.0, 0.0, 0.0),
                Camera::new(500.0 * scale, 300.0 * scale, 40.0 * scale),
                epoch,
                Pacing::GRAPH,
            );
            for act in 0..128 {
                let at = trip.start + trip.duration.mul_f64(0.15 + f64::from(act % 5) * 0.1);
                let current = trip.sample(at);
                let velocity = trip.velocity(at);
                let width_ratio = [0.1, 0.25, 1.0, 4.0, 10.0][act as usize % 5];
                let direction = if act % 2 == 0 { -1.0 } else { 1.0 };
                let target = Camera::new(
                    direction * 1_000.0 * scale,
                    f64::from(act % 7) * 100.0 * scale,
                    200.0 * width_ratio * scale,
                );
                let next = plan(current, velocity, target, at, Pacing::GRAPH);
                assert_eq!(
                    next.sample(at),
                    current,
                    "exact retarget camera at act {act}"
                );
                assert_eq!(
                    next.velocity(at),
                    velocity,
                    "full inherited derivative at act {act}"
                );
                assert!(next.duration <= Duration::from_millis(1_500));
                let delta = Duration::from_micros(1);
                let after = next.sample(at + delta);
                let k = viewport / current.w;
                for world in [
                    (current.x, current.y),
                    (current.x + current.w * 0.3, current.y - current.w * 0.2),
                ] {
                    assert_eq!(
                        screen(next.sample(at), world, viewport, origin),
                        screen(current, world, viewport, origin)
                    );
                    let (a, b) = (
                        screen(current, world, viewport, origin),
                        screen(after, world, viewport, origin),
                    );
                    let observed = (
                        (b.0 - a.0) / delta.as_secs_f64(),
                        (b.1 - a.1) / delta.as_secs_f64(),
                    );
                    let expected = (
                        -k * (velocity.0 + (world.0 - current.x) * velocity.2),
                        -k * (velocity.1 + (world.1 - current.y) * velocity.2),
                    );
                    for (got, want) in [(observed.0, expected.0), (observed.1, expected.1)] {
                        assert!(
                            (got - want).abs() < 0.002 * want.abs().max(1.0) + 0.05,
                            "scale={scale} viewport={viewport} act={act}: screen derivative {observed:?} vs {expected:?}"
                        );
                    }
                }
                for part in [0.0, 0.001, 0.1, 0.5, 0.9, 1.0] {
                    let camera = next.sample(at + next.duration.mul_f64(part));
                    assert!(
                        camera.x.is_finite()
                            && camera.y.is_finite()
                            && camera.w.is_finite()
                            && camera.w > 0.0,
                        "act {act}: {camera:?}"
                    );
                    for (value, from, to, allowance) in [
                        (camera.x, current.x, target.x, next.overshoot[0]),
                        (camera.y, current.y, target.y, next.overshoot[1]),
                        (camera.w, current.w, target.w, next.overshoot[2]),
                    ] {
                        let excess = (from.min(to) - value).max(value - from.max(to)).max(0.0);
                        assert!(
                            excess <= f64::from(allowance) + 1e-6 * current.w.max(target.w),
                            "outside finite trajectory envelope: {camera:?}"
                        );
                    }
                }
                assert_eq!(next.sample(at + next.duration), target);
                assert_eq!(next.velocity(at + next.duration), (0.0, 0.0, 0.0));
                trip = next;
            }
        }
    }
}

#[test]
fn flight_geometry_is_covariant_under_world_scale_and_translation() {
    let from = Camera::new(-3.0, 2.0, 4.0);
    for to in [
        Camera::new(40.0, -7.5, 1.5),
        Camera::new(-3.0, 2.0, 0.5),
        Camera::new(-3.0, 2.0, 32.0),
    ] {
        let baseline = Path::new(from, to);
        for scale in [0.001, 0.25, 1.0, 1_000.0] {
            for (dx, dy) in [(0.0, 0.0), (10_000.0, -37.0)] {
                let transform =
                    |c: Camera| Camera::new(c.x * scale + dx, c.y * scale + dy, c.w * scale);
                let path = Path::new(transform(from), transform(to));
                assert!((path.length() - baseline.length()).abs() < 2e-9);
                for index in 0..=100 {
                    let t = f64::from(index) / 100.0;
                    let actual = path.at_t(t);
                    let expected = transform(baseline.at_t(t));
                    for (a, b) in [
                        (actual.x, expected.x),
                        (actual.y, expected.y),
                        (actual.w, expected.w),
                    ] {
                        assert!(
                            (a - b).abs() < 2e-8 * scale.max(1.0),
                            "scale={scale} translation=({dx},{dy}) t={t}: {actual:?} vs {expected:?}"
                        );
                    }
                }
            }
        }
    }
}

/// These limits are fixture requirements, not the trip's declared probe
/// allowance: at most15% viewport translation and1.25x base width scaling.
fn assert_bounded_warp(trip: &super::Trip, tau: f64, viewport: f64) {
    let d = trip.duration.as_secs_f64();
    let base = trip.route.at_t((trip.ease)((tau / d).clamp(0.0, 1.0)));
    let actual = trip.at(tau);
    let ratio = actual.w / base.w;
    assert!(
        ratio >= 0.8 - 1e-12 && ratio <= 1.25 + 1e-12,
        "unbounded zoom {ratio}: {actual:?}"
    );
    let pan = (
        (actual.x - base.x) * viewport / actual.w,
        (actual.y - base.y) * viewport / actual.w,
    );
    assert!(
        pan.0.abs() <= 0.15 * viewport + 1e-6 && pan.1.abs() <= 0.15 * viewport + 1e-6,
        "unbounded inherited screen displacement {pan:?}"
    );
    for world in [
        (trip.route.start().x, trip.route.start().y),
        (trip.route.end().x, trip.route.end().y),
        (base.x + base.w * 0.3, base.y - base.w * 0.2),
    ] {
        let a = screen(base, world, viewport, (37.0, -91.0));
        let b = screen(actual, world, viewport, (37.0, -91.0));
        for (before, after, origin) in [(a.0, b.0, 37.0), (a.1, b.1, -91.0)] {
            let maximum = 0.25 * (before - origin).abs() + 0.15 * viewport;
            assert!(
                (after - before).abs() <= maximum + 1e-5,
                "landmark escaped independently bounded screen warp: {before} -> {after}, limit{maximum}"
            );
        }
    }
}

#[test]
fn extreme_wheel_to_focus_preserves_velocity_without_collapsing_geometry() {
    let at = Instant::now();
    let from = Camera::new(0.0, 0.0, 1e6);
    let velocity = (46_068_526.91, -23_034_263.455, -184.274568);
    let target = Camera::new(0.0, 0.0, 40.0);
    let trip = plan(from, velocity, target, at, Pacing::GRAPH);
    assert_eq!(trip.sample(at), from);
    assert_eq!(trip.velocity(at), velocity);
    // The previous carried pure zoom evaluated ~1.3e-13 width here.
    // The independently expected base path width is the geometric quarter.
    let half_second = trip.at(0.5);
    let expected_base = 1e6_f64 * (40.0_f64 / 1e6).powf(0.25);
    assert!(half_second.w >= 0.8 * expected_base && half_second.w <= 1.25 * expected_base);
    for ms in 0..=1500 {
        assert_bounded_warp(&trip, f64::from(ms) / 1000.0, 1440.0);
        let camera = trip.at(f64::from(ms) / 1000.0);
        let target_offset = screen(camera, (0.0, 0.0), 1440.0, (0.0, 0.0));
        assert!(target_offset.0.abs() <= 216.0 + 1e-6 && target_offset.1.abs() <= 216.0 + 1e-6);
    }
    assert_eq!(trip.sample(at + trip.duration), target);
    assert_eq!(trip.velocity(at + trip.duration), (0.0, 0.0, 0.0));
}

#[test]
fn inherited_wheel_and_pan_extremes_obey_fixed_screen_limits_and_cadence() {
    let at = Instant::now();
    for width in [2.5, 400.0, 1e6] {
        for target_width in [2.5, 40.0, 400.0, 1e6] {
            for zoom_velocity in [-184.274568, -1.0, 1.0, 184.274568] {
                for pan_velocity in [-10_000.0, -100.0, 0.0, 100.0, 10_000.0] {
                    for viewport in [480.0, 1440.0, 2560.0] {
                        for (x, y) in [(0.0, 0.0), (37.0, -91.0), (100_000.0, -3300.0)] {
                            let from = Camera::new(x, y, width);
                            let velocity = (
                                pan_velocity * width,
                                -pan_velocity * width * 0.5,
                                zoom_velocity,
                            );
                            let target =
                                Camera::new(x + width * 0.25, y - width * 0.13, target_width);
                            let trip = plan(from, velocity, target, at, Pacing::GRAPH);
                            assert_eq!(trip.sample(at), from);
                            assert_eq!(trip.velocity(at), velocity);
                            for part in 0..=100 {
                                assert_bounded_warp(
                                    &trip,
                                    trip.duration.as_secs_f64() * f64::from(part) / 100.0,
                                    viewport,
                                );
                            }
                            // At every selected absolute timestamp, earlier
                            // samples have no effect on camera or derivative.
                            for total in [16, 70, 260, 641, 1500, 2000] {
                                let now = at + Duration::from_millis(total);
                                let expected = (trip.sample(now), trip.velocity(now));
                                for quantum in [1, 7, 33, 143] {
                                    let mut state = super::State::Flying(trip);
                                    let mut elapsed = 0;
                                    let mut camera = from;
                                    while elapsed < total {
                                        elapsed = (elapsed + quantum).min(total);
                                        camera = super::step(
                                            &mut state,
                                            target,
                                            at + Duration::from_millis(elapsed),
                                            false,
                                            Pacing::GRAPH,
                                        )
                                        .0
                                        .camera;
                                    }
                                    let derivative = match state {
                                        super::State::Flying(active) => active.velocity(now),
                                        _ => (0.0, 0.0, 0.0),
                                    };
                                    assert_eq!((camera, derivative), expected);
                                }
                            }
                            assert_eq!(trip.sample(at + trip.duration), target);
                            assert_eq!(trip.velocity(at + trip.duration), (0.0, 0.0, 0.0));
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn short_horizon_reversals_keep_the_actual_screen_derivative() {
    let epoch = Instant::now();
    for viewport in [480.0, 1440.0, 2560.0] {
        let mut current = Camera::new(17.0, -11.0, 400.0);
        let mut at = epoch;
        for act in 0..128 {
            let sign = if act % 2 == 0 { -1.0 } else { 1.0 };
            let velocity = (
                sign * current.w * 10_000.0,
                -sign * current.w * 3000.0,
                sign * 184.274568,
            );
            let target = Camera::new(
                -sign * 500.0,
                sign * 300.0,
                if act % 2 == 0 { 2.5 } else { 1e6 },
            );
            let trip = plan(current, velocity, target, at, Pacing::GRAPH);
            assert_eq!(trip.sample(at), current);
            assert_eq!(trip.velocity(at), velocity);
            // The finite step resolves even the shortest bounded horizon.
            let delta = Duration::from_nanos(1);
            for elapsed in [
                Duration::ZERO,
                Duration::from_nanos(100),
                Duration::from_micros(1),
                Duration::from_micros(7),
            ] {
                let now = at + elapsed;
                let before = trip.sample(now);
                let after = trip.sample(now + delta);
                let reported = trip.velocity(now);
                for world in [
                    (before.x, before.y),
                    (before.x + before.w * 0.3, before.y - before.w * 0.2),
                ] {
                    let a = screen(before, world, viewport, (37.0, -91.0));
                    let b = screen(after, world, viewport, (37.0, -91.0));
                    let observed = (
                        (b.0 - a.0) / delta.as_secs_f64(),
                        (b.1 - a.1) / delta.as_secs_f64(),
                    );
                    let k = viewport / before.w;
                    let expected = (
                        -k * (reported.0 + (world.0 - before.x) * reported.2),
                        -k * (reported.1 + (world.1 - before.y) * reported.2),
                    );
                    for (got, want) in [(observed.0, expected.0), (observed.1, expected.1)] {
                        assert!(
                            (got - want).abs() < 0.0002 * want.abs().max(1.0) + 1.0,
                            "actual screen derivative act{act} at{elapsed:?}: {observed:?} vs{expected:?}"
                        );
                    }
                }
            }
            for part in [0.0, 0.001, 0.1, 0.5, 0.9, 1.0] {
                assert_bounded_warp(&trip, trip.duration.as_secs_f64() * part, viewport);
            }
            at += trip.duration.mul_f64(0.0001);
            current = trip.sample(at);
            assert!(
                current.x.is_finite()
                    && current.y.is_finite()
                    && current.w.is_finite()
                    && current.w > 0.0
            );
        }
    }
}

#[test]
fn contextual_route_has_pinned_lift_single_signed_bend_and_exact_landing() {
    use super::{GraphPath, Travel};
    let from = Camera::new(0.0, 0.0, 100.0);
    let to = Camera::new(800.0, 0.0, 40.0);
    let context = Camera::new(400.0, 300.0, 1200.0);
    let focus = GraphPath::new(from, to, Travel::Focus(context));
    assert_eq!(focus.apex, 1200.0);
    assert_eq!(focus.normal, (0.0, 1.0));
    assert!((focus.bend - 0.075).abs() < 1e-15);
    let split = 12.0_f64.ln() / (12.0_f64.ln() + 30.0_f64.ln());
    assert!((focus.split - split).abs() < 1e-15);
    let apex = focus.at_t(split);
    assert!((apex.w - 1200.0).abs() < 1e-9);
    assert!((apex.y - 90.0 * 4.0 * split * (1.0 - split)).abs() < 1e-9);
    let middle = focus.at_t(0.5);
    assert!((middle.x - 4000.0 / 7.0).abs() < 1e-9);
    assert!((middle.y / middle.w * 1440.0 - 108.0).abs() < 1e-10);
    let survey = GraphPath::new(from, to, Travel::Survey(context));
    assert!((survey.at_t(0.5).y / survey.at_t(0.5).w * 1440.0 - 79.2).abs() < 1e-10);
    let quiet = GraphPath::new(from, to, Travel::Reframe);
    assert_eq!(quiet.at_t(0.5).y, 0.0);
    assert!((quiet.at_t(0.5).w - 400.0 / 7.0).abs() < 1e-12);
    let left = GraphPath::new(from, to, Travel::Focus(Camera::new(400.0, -300.0, 1200.0)));
    assert!((left.at_t(0.5).y + middle.y).abs() < 1e-9);
    for path in [focus, survey, quiet, left] {
        assert_eq!(path.at_t(0.0), from);
        assert_eq!(path.at_t(1.0), to);
    }
}

/// All landmarks must remain in the endpoint screen hull extended only by
/// the viewport centre and a fixed 8.5% semantic bend. No probe allowance is
/// consulted. The lens cannot collapse below projective interpolation.
#[test]
fn graph_route_landmarks_have_fixed_screen_bounds_and_covariant_reversal() {
    use super::{GraphPath, Travel};
    let from = Camera::new(-17.0, 11.0, 400.0);
    for width in [2.5, 40.0, 400.0, 1e6] {
        for distance in [0.0, 30.0, 800.0, 1e6] {
            let to = Camera::new(from.x + distance, from.y - distance * 0.3, width);
            let context = Camera::new(
                from.x + distance * 0.5,
                from.y + distance * 0.3,
                1200.0 + distance,
            );
            for travel in [
                Travel::Focus(context),
                Travel::Survey(context),
                Travel::Reframe,
            ] {
                let route = GraphPath::new(from, to, travel);
                let reverse = GraphPath::new(to, from, travel);
                for i in 0..=200 {
                    let p = f64::from(i) / 200.0;
                    let current = route.at_t(p);
                    let harmonic = 1.0 / ((1.0 - p) / from.w + p / to.w);
                    assert!(current.w + 1e-7 >= harmonic);
                    assert!(current.w <= route.apex + 1e-6);
                    let reversed = reverse.at_t(1.0 - p);
                    for (a, b) in [
                        (current.x, reversed.x),
                        (current.y, reversed.y),
                        (current.w, reversed.w),
                    ] {
                        assert!(
                            (a - b).abs() < 1e-6 + current.w.max(route.apex) * 1e-10,
                            "reversal altered semantic route: {current:?} vs {reversed:?}"
                        );
                    }
                    for viewport in [480.0, 1440.0, 2560.0] {
                        let origin = (37.0, -91.0);
                        for world in [
                            (from.x, from.y),
                            (to.x, to.y),
                            (from.x + from.w * 0.3, from.y - from.w * 0.2),
                            (to.x - to.w * 0.49, to.y + to.w * 0.49),
                        ] {
                            let a = screen(from, world, viewport, origin);
                            let b = screen(to, world, viewport, origin);
                            let c = screen(current, world, viewport, origin);
                            let bend = if matches!(travel, Travel::Reframe) {
                                0.0
                            } else {
                                0.085 * viewport
                            };
                            for (x, y, value, centre) in
                                [(a.0, b.0, c.0, origin.0), (a.1, b.1, c.1, origin.1)]
                            {
                                let lo = x.min(y).min(centre) - bend;
                                let hi = x.max(y).max(centre) + bend;
                                let eps = 1e-6 + lo.abs().max(hi.abs()) * 1e-11;
                                assert!(
                                    value >= lo - eps && value <= hi + eps,
                                    "context route swept landmark {value} outside fixed screen corridor [{lo},{hi}]"
                                );
                            }
                        }
                    }
                }
                for scale in [0.25, 10.0] {
                    let transform = |c: Camera| {
                        Camera::new(c.x * scale + 10000.0, c.y * scale - 3300.0, c.w * scale)
                    };
                    let travel = match travel {
                        Travel::Focus(c) => Travel::Focus(transform(c)),
                        Travel::Survey(c) => Travel::Survey(transform(c)),
                        Travel::Reframe => Travel::Reframe,
                    };
                    let changed = GraphPath::new(transform(from), transform(to), travel);
                    for p in [0.0, 0.01, 0.22, 0.5, 0.78, 0.99, 1.0] {
                        let a = changed.at_t(p);
                        let b = transform(route.at_t(p));
                        for (x, y) in [(a.x, b.x), (a.y, b.y), (a.w, b.w)] {
                            assert!((x - y).abs() < 1e-6 + route.apex * scale * 1e-10);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn contextual_retarget_is_c1_through_lift_apex_and_landing_with_bounded_carry() {
    use super::{Travel, plan_travel};
    let epoch = Instant::now();
    for viewport in [480.0, 1440.0, 2560.0] {
        for goal_width in [2.5, 40.0, 400.0, 1e6] {
            for phase in [0.03, 0.22, 0.5, 0.78, 0.97] {
                let mut trip = plan_travel(
                    Camera::new(0.0, 0.0, 400.0),
                    (0.0, 0.0, 0.0),
                    Camera::new(800.0, 200.0, goal_width),
                    epoch,
                    Pacing::GRAPH_TRAVEL,
                    Travel::Focus(Camera::new(300.0, 350.0, 1200.0)),
                );
                for act in 0..16 {
                    let now = trip.start + trip.duration.mul_f64(phase);
                    let current = trip.sample(now);
                    let velocity = trip.velocity(now);
                    let sign = if act % 2 == 0 { -1.0 } else { 1.0 };
                    let target = Camera::new(
                        sign * 800.0,
                        sign * 200.0,
                        if act % 2 == 0 { goal_width } else { 400.0 },
                    );
                    let context = Travel::Focus(Camera::new(sign * 400.0, 350.0, 1200.0));
                    let next = plan_travel(
                        current,
                        velocity,
                        target,
                        now,
                        Pacing::GRAPH_TRAVEL,
                        context,
                    );
                    assert_eq!(next.sample(now), current);
                    assert_eq!(next.velocity(now), velocity);
                    let delta = Duration::from_nanos(100);
                    for world in [
                        (current.x, current.y),
                        (current.x + current.w * 0.3, current.y - current.w * 0.2),
                    ] {
                        let a = screen(current, world, viewport, (37.0, -91.0));
                        let b = screen(next.sample(now + delta), world, viewport, (37.0, -91.0));
                        let k = viewport / current.w;
                        let expected = (
                            -k * (velocity.0 + (world.0 - current.x) * velocity.2),
                            -k * (velocity.1 + (world.1 - current.y) * velocity.2),
                        );
                        for (got, want) in [
                            ((b.0 - a.0) / delta.as_secs_f64(), expected.0),
                            ((b.1 - a.1) / delta.as_secs_f64(), expected.1),
                        ] {
                            assert!(
                                (got - want).abs() < 0.002 * want.abs().max(1.0) + 0.05,
                                "phase{phase} act{act}: screen velocity changed {got} vs{want}"
                            );
                        }
                    }
                    for i in 0..=100 {
                        assert_bounded_warp(
                            &next,
                            next.duration.as_secs_f64() * f64::from(i) / 100.0,
                            viewport,
                        );
                    }
                    assert!(next.duration <= Duration::from_millis(1250));
                    assert_eq!(next.sample(now + next.duration), target);
                    assert_eq!(next.velocity(now + next.duration), (0.0, 0.0, 0.0));
                    trip = next;
                }
            }
        }
    }
}

#[test]
fn contextual_state_is_cadence_invariant_and_never_restarts_after_landing() {
    use super::{State, Travel, plan_travel, step_with};
    let at = Instant::now();
    for from_width in [2.5, 400.0, 1e6] {
        for to_width in [2.5, 40.0, 1e6] {
            for velocity in [
                (0.0, 0.0, 0.0),
                (from_width * 10000.0, -from_width * 3000.0, -184.274568),
                (from_width * 0.3, from_width * 0.1, 2.0),
            ] {
                let target = Camera::new(800.0, -200.0, to_width);
                let travel = Travel::Survey(Camera::new(300.0, 350.0, 1200.0));
                let trip = plan_travel(
                    Camera::new(17.0, -11.0, from_width),
                    velocity,
                    target,
                    at,
                    Pacing::GRAPH_TRAVEL,
                    travel,
                );
                for total in [16, 70, 260, 641, 1250, 2000] {
                    let now = at + Duration::from_millis(total);
                    let expected = (trip.sample(now), trip.velocity(now));
                    for quantum in [1, 7, 33, 143] {
                        let mut state = State::Flying(trip);
                        let mut elapsed = 0;
                        let mut shot = None;
                        while elapsed < total {
                            elapsed = (elapsed + quantum).min(total);
                            shot = Some(
                                step_with(
                                    &mut state,
                                    target,
                                    at + Duration::from_millis(elapsed),
                                    false,
                                    Pacing::GRAPH_TRAVEL,
                                    Some(travel),
                                )
                                .0,
                            );
                        }
                        let velocity = match state {
                            State::Flying(active) => active.velocity(now),
                            _ => (0.0, 0.0, 0.0),
                        };
                        assert_eq!((shot.expect("draw").camera, velocity), expected);
                    }
                }
                let mut state = State::Flying(trip);
                for ms in [1250, 1266, 2000, 20000] {
                    let shot = step_with(
                        &mut state,
                        target,
                        at + Duration::from_millis(ms),
                        false,
                        Pacing::GRAPH_TRAVEL,
                        Some(travel),
                    )
                    .0;
                    assert_eq!(shot.camera, target);
                    assert!(!shot.live);
                    assert!(matches!(state, State::Still(_)));
                }
            }
        }
    }
}
