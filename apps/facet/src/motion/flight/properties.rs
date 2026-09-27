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
    // Independent pinned maximum of the concave log-width arch.
    let split = 0.4608437861426786;
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
                        Travel::Reading(_) => unreachable!("legacy route fixture"),
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

/// Native interrupt trace escaped focus twice at2400/2520ms. Its old
/// below-max lens blend rebounded82.4965→82.2248→82.4237 near rest. These
/// independently pinned endpoint scales cover the exact small-gap region;
/// every 16ms clock phase uses the unchanged probe continuity allowance.
#[test]
fn interrupted_small_lens_has_no_blend_rebound_and_truthful_frame_velocity() {
    use super::{GraphPath, Travel, plan_travel};
    let at = Instant::now();
    let target = Camera::new(-7.7217, 3.0701, 82.4353);
    for width in [62.7522, 63.2092, 70.0, 82.4] {
        let from = Camera::new(-13.3482, 8.553, width);
        let route = GraphPath::new(from, target, Travel::Survey(target));
        let mut previous = width;
        for index in 0..=1000 {
            let camera = route.at_t(f64::from(index) / 1000.0);
            assert!(
                camera.w >= previous - 1e-11,
                "uncarried lens reversed near landing: {previous}→{}",
                camera.w
            );
            assert!(camera.w <= target.w + 1e-11);
            previous = camera.w;
        }
        for log_velocity in [0.0, 0.8, 0.9, 1.0, -1.0] {
            let trip = plan_travel(
                from,
                (-96.53, 47.0, log_velocity),
                target,
                at,
                Pacing::GRAPH_TRAVEL,
                Travel::Survey(target),
            );
            for phase in 0..16 {
                let mut time = f64::from(phase) / 1000.0;
                while time + 0.016 <= trip.duration.as_secs_f64() {
                    let before = trip.at(time);
                    let after = trip.at(time + 0.016);
                    let v0 = trip.velocity(at + Duration::from_secs_f64(time)).2 * before.w;
                    let v1 = trip.velocity(at + Duration::from_secs_f64(time + 0.016)).2 * after.w;
                    let allowed = 1.5 * v0.abs().max(v1.abs()) * 0.016
                        + 0.02 * (target.w - before.w).abs()
                        + 0.001;
                    assert!(
                        (after.w - before.w).abs() <= allowed + 1e-8,
                        "phase{phase} w{width} carry{log_velocity} at{time}: {}→{} exceeds truthful velocity{v0}/{v1}, allowed{allowed}",
                        before.w,
                        after.w
                    );
                    time += 0.016;
                }
            }
            assert_eq!(trip.sample(at + trip.duration), target);
            assert_eq!(trip.velocity(at + trip.duration), (0.0, 0.0, 0.0));
        }
    }
}

fn reading_room(bottom: bool) -> super::FlightRoom {
    if bottom {
        super::FlightRoom {
            left: -0.5,
            right: 0.5,
            top: -0.3,
            bottom: 0.05,
            anchor: (-0.22, -0.125),
        }
    } else {
        super::FlightRoom {
            left: -0.5,
            right: 0.15,
            top: -0.3,
            bottom: 0.3,
            anchor: (-0.15, 0.0),
        }
    }
}

fn reading(
    from: Camera,
    to: Camera,
    kind: super::FocusKind,
    room: super::FlightRoom,
) -> super::FocusRoute {
    super::FocusRoute {
        departure: Some((
            from.x + room.anchor.0 * from.w,
            from.y + room.anchor.1 * from.w,
        )),
        arrival: (to.x + room.anchor.0 * to.w, to.y + room.anchor.1 * to.w),
        context: Camera::new(
            (from.x + to.x) / 2.0,
            (from.y + to.y) / 2.0 + 60.0,
            10_000.0,
        ),
        room,
        kind,
    }
}

#[test]
fn reading_handoff_keeps_scale_and_transfer_lifts_early_into_actual_room() {
    use super::{FocusKind, GraphPath, Travel};
    let room = reading_room(false);
    let from = Camera::new(30.0, 0.0, 200.0);
    let to = Camera::new(110.0, 0.0, 200.0);
    let near = GraphPath::new(
        from,
        to,
        Travel::Reading(reading(from, to, FocusKind::Handoff, room)),
    );
    assert!(
        near.apex <= 224.0,
        "local move must not reveal the 10,000-unit package"
    );
    assert_eq!(near.at_t(0.0), from);
    assert_eq!(near.at_t(1.0), to);
    assert!(
        near.at_t(0.5).y > 0.0,
        "one approach bends toward the common context"
    );
    let far = Camera::new(830.0, 0.0, 200.0);
    let follow = GraphPath::new(
        from,
        far,
        Travel::Reading(reading(from, far, FocusKind::Follow, room)),
    );
    let transfer = GraphPath::new(
        from,
        far,
        Travel::Reading(reading(from, far, FocusKind::Transfer, room)),
    );
    assert!(
        transfer.apex < 2200.0 && transfer.apex > 1600.0,
        "fit the actual corridor, not the whole package"
    );
    assert!(transfer.advance > follow.advance && follow.advance > near.advance);
    // Both endpoint contractions advance continuously; transfer has already
    // crossed its width apex by halfway without a waypoint pause.
    let at_half = transfer.at_t(0.5);
    assert!(at_half.w < transfer.apex);
    assert!(at_half.x > transfer.at_t(0.4).x);
}

#[test]
fn reading_landmarks_obey_fixed_projection_hulls_and_one_lens_turn() {
    use super::{FocusKind, GraphPath, Travel};
    for bottom in [false, true] {
        let room = reading_room(bottom);
        for ratio in [0.01, 0.1, 1.0, 10.0, 100.0] {
            for dx in [-1800.0, -80.0, 0.0, 80.0, 1800.0] {
                for kind in [FocusKind::Handoff, FocusKind::Follow, FocusKind::Transfer] {
                    let from = Camera::new(30.0, -40.0, 200.0);
                    let to = Camera::new(from.x + dx, 150.0, 200.0 * ratio);
                    let intent = reading(from, to, kind, room);
                    let path = GraphPath::new(from, to, Travel::Reading(intent));
                    let mut last_width = from.w;
                    let mut descended = false;
                    for index in 0..=1000 {
                        let camera = path.at_t(f64::from(index) / 1000.0);
                        assert!(
                            camera.x.is_finite()
                                && camera.y.is_finite()
                                && camera.w > 0.0
                                && camera.w.is_finite()
                        );
                        assert!(camera.w <= path.apex * (1.0 + 1e-12));
                        if camera.w < last_width - path.apex * 1e-12 {
                            descended = true;
                        }
                        if descended {
                            assert!(
                                camera.w <= last_width + path.apex * 1e-12,
                                "single lens turn: {last_width} -> {}",
                                camera.w
                            );
                        }
                        last_width = camera.w;
                        for point in [intent.departure.expect("reading fixture anchors a departure landmark for the projection hull"), intent.arrival, (400.0, -200.0)] {
                            let endpoints = [
                                ((point.0 - from.x) / from.w, (point.1 - from.y) / from.w),
                                ((point.0 - to.x) / to.w, (point.1 - to.y) / to.w),
                                room.anchor,
                            ];
                            let value = (
                                (point.0 - camera.x) / camera.w,
                                (point.1 - camera.y) / camera.w,
                            );
                            for (axis, got, bow) in [
                                (0, value.0, (path.normal.0 * path.bend).abs()),
                                (1, value.1, (path.normal.1 * path.bend).abs()),
                            ] {
                                let scalar = |p: &(f64, f64)| if axis == 0 { p.0 } else { p.1 };
                                let low =
                                    endpoints.iter().map(scalar).fold(f64::INFINITY, f64::min);
                                let high = endpoints
                                    .iter()
                                    .map(scalar)
                                    .fold(f64::NEG_INFINITY, f64::max);
                                assert!(
                                    got >= low - bow - 1e-9 && got <= high + bow + 1e-9,
                                    "fixed screen hull violated {got} [{low},{high}]"
                                );
                            }
                        }
                    }
                    assert_eq!(path.at_t(1.0), to);
                    // Invert the bounded progress warp to independently check
                    // corridor capture exactly at spatial halfway.
                    let mut low = 0.0;
                    let mut high = 1.0;
                    for _ in 0..40 {
                        let u = (low + high) / 2.0;
                        if u + path.advance * u * (1.0 - u) < 0.5 {
                            low = u;
                        } else {
                            high = u;
                        }
                    }
                    let camera = path.at_t((low + high) / 2.0);
                    for point in [
                        intent
                            .departure
                            .expect("reading fixture provides both landmarks for midpoint capture"),
                        intent.arrival,
                    ] {
                        assert!(
                            room.contains((
                                (point.0 - camera.x) / camera.w,
                                (point.1 - camera.y) / camera.w
                            )),
                            "midpoint must capture both landmarks in actual room"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn invisible_focus_hint_cannot_change_route_after_pan_or_reframe() {
    use super::{FocusKind, GraphPath, Travel};
    let from = Camera::new(4000.0, -300.0, 90.0);
    let to = Camera::new(4700.0, 600.0, 60.0);
    for bottom in [false, true] {
        let room = reading_room(bottom);
        let mut intent = reading(from, to, FocusKind::Transfer, room);
        intent.departure = Some((-10_000.0, 50_000.0));
        let stale = GraphPath::new(from, to, Travel::Reading(intent));
        intent.departure = None;
        intent.context = Camera::new(-9e8, 7e8, 2e9);
        let authoritative = GraphPath::new(from, to, Travel::Reading(intent));
        for index in 0..=1000 {
            assert_eq!(
                stale.at_t(f64::from(index) / 1000.0),
                authoritative.at_t(f64::from(index) / 1000.0)
            );
        }
    }
}

#[test]
fn reading_routes_are_covariant_under_world_translation_and_scale() {
    use super::{FocusKind, GraphPath, Travel};
    let from = Camera::new(30.0, -40.0, 200.0);
    let to = Camera::new(830.0, 150.0, 30.0);
    for bottom in [false, true] {
        let intent = reading(from, to, FocusKind::Transfer, reading_room(bottom));
        let route = GraphPath::new(from, to, Travel::Reading(intent));
        for scale in [0.01, 1.0, 100.0] {
            let transform =
                |p: Camera| Camera::new(p.x * scale + 3700.0, p.y * scale - 9100.0, p.w * scale);
            let point = |p: (f64, f64)| (p.0 * scale + 3700.0, p.1 * scale - 9100.0);
            let mut changed = intent;
            changed.departure = changed.departure.map(point);
            changed.arrival = point(changed.arrival);
            changed.context = transform(changed.context);
            let moved = GraphPath::new(transform(from), transform(to), Travel::Reading(changed));
            for i in 0..=1000 {
                let expected = transform(route.at_t(f64::from(i) / 1000.0));
                let actual = moved.at_t(f64::from(i) / 1000.0);
                for (a, b) in [
                    (actual.x, expected.x),
                    (actual.y, expected.y),
                    (actual.w, expected.w),
                ] {
                    assert!((a - b).abs() <= 1e-7 + route.apex * scale * 1e-10);
                }
            }
        }
    }
}

#[test]
fn reading_reversals_and_room_interruptions_keep_full_derivative_and_exact_rest() {
    use super::{FocusKind, Travel, plan_travel};
    let epoch = Instant::now();
    let mut from = Camera::new(30.0, -40.0, 200.0);
    let mut goal = Camera::new(830.0, 150.0, 30.0);
    let mut trip = plan_travel(
        from,
        (0.0, 0.0, 0.0),
        goal,
        epoch,
        Pacing::GRAPH_TRAVEL,
        Travel::Reading(reading(
            from,
            goal,
            FocusKind::Transfer,
            reading_room(false),
        )),
    );
    for act in 0..128 {
        let at = trip.start
            + trip
                .duration
                .mul_f64([0.01, 0.18, 0.42, 0.72, 0.99][act % 5]);
        from = trip.sample(at);
        let velocity = trip.velocity(at);
        goal = Camera::new(
            if act % 2 == 0 { -800.0 } else { 1400.0 },
            (act % 7) as f64 * 60.0,
            [2.0, 30.0, 200.0, 2000.0][act % 4],
        );
        let intent = reading(from, goal, FocusKind::Transfer, reading_room(act % 2 == 0));
        let next = plan_travel(
            from,
            velocity,
            goal,
            at,
            Pacing::GRAPH_TRAVEL,
            Travel::Reading(intent),
        );
        assert_eq!(next.sample(at), from);
        assert_eq!(next.velocity(at), velocity);
        assert!(next.duration <= Duration::from_millis(1250));
        let delta = Duration::from_micros(1);
        let after = next.sample(at + delta);
        for landmark in [
            (from.x, from.y),
            (from.x + from.w * 0.3, from.y - from.w * 0.2),
        ] {
            let a = screen(from, landmark, 1440.0, (37.0, -91.0));
            let b = screen(after, landmark, 1440.0, (37.0, -91.0));
            let k = 1440.0 / from.w;
            let expected = (
                -k * (velocity.0 + (landmark.0 - from.x) * velocity.2),
                -k * (velocity.1 + (landmark.1 - from.y) * velocity.2),
            );
            for (got, want) in [
                ((b.0 - a.0) / delta.as_secs_f64(), expected.0),
                ((b.1 - a.1) / delta.as_secs_f64(), expected.1),
            ] {
                assert!(
                    (got - want).abs() < 2.0 + want.abs() * 0.03,
                    "screen derivative {got} != {want}"
                );
            }
        }
        for sample in 0..=40 {
            assert_bounded_warp(
                &next,
                next.duration.as_secs_f64() * f64::from(sample) / 40.0,
                1440.0,
            );
        }
        assert_eq!(next.sample(at + next.duration), goal);
        assert_eq!(next.velocity(at + next.duration), (0.0, 0.0, 0.0));
        trip = next;
    }
}

#[test]
fn reading_event_storm_is_invariant_to_frame_cadence_and_delayed_draws() {
    use super::{FocusKind, State, Travel, step_with};
    let epoch = Instant::now();
    let run = |cadence: u64| {
        let mut state = State::Still(Camera::new(30.0, -40.0, 200.0));
        let mut target = Camera::new(830.0, 150.0, 30.0);
        let mut travel = Travel::Reading(reading(
            Camera::new(30.0, -40.0, 200.0),
            target,
            FocusKind::Transfer,
            reading_room(false),
        ));
        step_with(
            &mut state,
            target,
            epoch,
            false,
            Pacing::GRAPH_TRAVEL,
            Some(travel),
        );
        let mut next_draw = cadence;
        let mut events = Vec::new();
        for (index, millis) in [7, 34, 121, 129, 281, 406, 907, 1601]
            .into_iter()
            .enumerate()
        {
            while next_draw < millis {
                step_with(
                    &mut state,
                    target,
                    epoch + Duration::from_millis(next_draw),
                    false,
                    Pacing::GRAPH_TRAVEL,
                    Some(travel),
                );
                next_draw += cadence;
            }
            let at = epoch + Duration::from_millis(millis);
            let before = step_with(
                &mut state,
                target,
                at,
                false,
                Pacing::GRAPH_TRAVEL,
                Some(travel),
            )
            .0;
            let velocity = match state {
                State::Flying(trip) => trip.velocity(at),
                _ => (0.0, 0.0, 0.0),
            };
            target = Camera::new(
                if index % 2 == 0 { -800.0 } else { 1400.0 },
                index as f64 * 30.0,
                [2.5, 40.0, 400.0, 4000.0][index % 4],
            );
            travel = Travel::Reading(reading(
                before.camera,
                target,
                FocusKind::Transfer,
                reading_room(index % 2 == 0),
            ));
            let after = step_with(
                &mut state,
                target,
                at,
                false,
                Pacing::GRAPH_TRAVEL,
                Some(travel),
            )
            .0;
            assert_eq!(after.camera, before.camera);
            events.push((after.camera, velocity));
        }
        let end = epoch + Duration::from_secs(4);
        let rest = step_with(
            &mut state,
            target,
            end,
            false,
            Pacing::GRAPH_TRAVEL,
            Some(travel),
        )
        .0;
        assert_eq!(rest.camera, target);
        assert!(!rest.live);
        events
    };
    let baseline = run(1);
    for cadence in [8, 16, 33, 120, 500] {
        assert_eq!(
            run(cadence),
            baseline,
            "cadence{cadence} changed navigation intent or interruption state"
        );
    }
}

#[test]
fn reading_source_and_goal_never_backtrack_along_their_reading_axis() {
    use super::{FocusKind, GraphPath, Travel};
    for bottom in [false, true] {
        let room = reading_room(bottom);
        for ratio in [0.0001, 0.01, 1.0, 100.0, 10_000.0] {
            for direction in [(80.0, 60.0), (-800.0, 150.0), (1800.0, -900.0)] {
                let from = Camera::new(30.0, -40.0, 200.0);
                let source = (
                    from.x + room.anchor.0 * from.w,
                    from.y + room.anchor.1 * from.w,
                );
                let goal = (source.0 + direction.0, source.1 + direction.1);
                let to = Camera::new(
                    goal.0 - room.anchor.0 * 200.0 * ratio,
                    goal.1 - room.anchor.1 * 200.0 * ratio,
                    200.0 * ratio,
                );
                let intent = super::FocusRoute {
                    departure: Some(source),
                    arrival: goal,
                    context: Camera::new(400.0, 600.0, 100_000.0),
                    room,
                    kind: FocusKind::Transfer,
                };
                let path = GraphPath::new(from, to, Travel::Reading(intent));
                let length = direction.0.hypot(direction.1);
                let axis = (direction.0 / length, direction.1 / length);
                let scalar = |camera: Camera, point: (f64, f64)| {
                    ((point.0 - camera.x) / camera.w - room.anchor.0) * axis.0
                        + ((point.1 - camera.y) / camera.w - room.anchor.1) * axis.1
                };
                let mut source_last = scalar(from, source);
                let mut goal_last = scalar(from, goal);
                for index in 1..=1000 {
                    let camera = path.at_t(f64::from(index) / 1000.0);
                    let outgoing = scalar(camera, source);
                    let incoming = scalar(camera, goal);
                    assert!(
                        outgoing <= source_last + 1e-8,
                        "source reversed at{index}: {source_last}->{outgoing}"
                    );
                    assert!(
                        incoming <= goal_last + 1e-8,
                        "goal reversed at{index}: {goal_last}->{incoming}"
                    );
                    source_last = outgoing;
                    goal_last = incoming;
                }
                assert!(goal_last.abs() < 1e-8);
            }
        }
    }
}

#[test]
fn reading_extreme_unhinted_zoom_has_no_synthetic_source_detour_or_geometry_collapse() {
    use super::{FocusKind, GraphPath, Travel, plan_travel};
    let at = Instant::now();
    for bottom in [false, true] {
        let room = reading_room(bottom);
        for (width, target_width) in [(1e6, 40.0), (40.0, 1e6), (1e-6, 1e6), (1e6, 1e-6)] {
            let from = Camera::new(0.0, 0.0, width);
            let arrival = (600.0, 100.0);
            let target = Camera::new(
                arrival.0 - room.anchor.0 * target_width,
                arrival.1 - room.anchor.1 * target_width,
                target_width,
            );
            let intent = super::FocusRoute {
                departure: None,
                arrival,
                context: Camera::new(-9e8, 7e8, 2e9),
                room,
                kind: FocusKind::Transfer,
            };
            let path = GraphPath::new(from, target, Travel::Reading(intent));
            let limit = if width == 1e6 && target_width <= 40.0 {
                1.02
            } else if width >= 2.5 && target_width >= 2.5 {
                1.25
            } else {
                2.0
            };
            assert!(
                path.apex <= width.max(target_width) * limit,
                "unhinted mixed zoom cannot stage a huge synthetic departure lift: {} exceeds independent{limit}x fixture cap",
                path.apex
            );
            for phase in [1e-12, 1e-9, 0.001, 0.1, 0.5, 0.9, 0.999999999999] {
                let camera = path.at_t(phase);
                assert!(
                    camera.x.is_finite()
                        && camera.y.is_finite()
                        && camera.w.is_finite()
                        && camera.w > 0.0
                );
            }
            let velocity = (46.0 * width, -23.0 * width, -184.274568);
            let trip = plan_travel(
                from,
                velocity,
                target,
                at,
                Pacing::GRAPH_TRAVEL,
                Travel::Reading(intent),
            );
            assert_eq!(trip.sample(at), from);
            assert_eq!(trip.velocity(at), velocity);
            for sample in 0..=1000 {
                assert_bounded_warp(
                    &trip,
                    trip.duration.as_secs_f64() * f64::from(sample) / 1000.0,
                    1440.0,
                );
            }
            assert_eq!(trip.sample(at + trip.duration), target);
            assert_eq!(trip.velocity(at + trip.duration), (0.0, 0.0, 0.0));
        }
    }
}

#[test]
fn visible_old_focus_after_overview_cannot_force_a_departure_or_context_lift() {
    use super::{FocusKind, GraphPath, Travel};
    let room = reading_room(false);
    let from = Camera::new(0.0, 0.0, 1e6);
    let arrival = (600.0, 100.0);
    let to = Camera::new(606.0, 100.0, 40.0);
    let mut intent = super::FocusRoute {
        departure: Some((0.0, 0.0)),
        arrival,
        context: Camera::new(1e9, -1e9, 2e9),
        room,
        kind: FocusKind::Transfer,
    };
    assert!(
        room.contains((0.0, 0.0)),
        "old symbol is still onscreen, but no longer the reading departure"
    );
    let stale = GraphPath::new(from, to, Travel::Reading(intent));
    intent.departure = None;
    intent.context = Camera::new(-7e9, 9e9, 2e10);
    let actual = GraphPath::new(from, to, Travel::Reading(intent));
    for i in 0..=1000 {
        assert_eq!(
            stale.at_t(f64::from(i) / 1000.0),
            actual.at_t(f64::from(i) / 1000.0)
        );
    }
}
