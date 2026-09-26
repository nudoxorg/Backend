//! Long native input runs without a retained per-frame report.
//!
//! `motion-report` retains every ledger and sampled state for inspection.
//! A memory soak must instead discard each frame after scalar accounting,
//! otherwise a linear report accumulator is indistinguishable from a leak.
//! Graph scripts emit `graph-memory` phase markers; the external libproc
//! runner samples the same process and its graph-owned counters at those marks.

use super::{GalleryError, Scene, Shot, json::Json, run_without_timing_history as run};

/// Runs an input script in one native window with constant-size accounting.
/// No screenshots, ledger history, state history, or timing vector are retained.
///
/// # Errors
/// The scene's native session or input delivery fails.
pub fn measure(scene: &Scene, shot: &Shot) -> Result<Json, GalleryError> {
    let mut shot = shot.clone();
    shot.times.clear();
    shot.probe = true;
    let mut frames = 0_u32;
    let mut requested = 0_u32;
    let mut events = 0_u32;
    let mut total_ms = 0.0_f64;
    let mut maximum_ms = 0.0_f64;
    let mut last_requested = None;
    let mut coverage = [0_u32; 7];
    run(scene, &shot, &mut |tick, _, _| {
        frames = frames.saturating_add(1);
        events = events.saturating_add(u32::try_from(tick.events.len()).unwrap_or(u32::MAX));
        if tick.drawn.requested() {
            requested = requested.saturating_add(1);
            last_requested = Some(tick.drawn.at_ms);
        }
        let cpu = tick.drawn.cpu.as_secs_f64() * 1000.0;
        total_ms += cpu;
        maximum_ms = maximum_ms.max(cpu);
        if let Some(Json::Obj(fields)) = &tick.state {
            let field = |name: &str| fields.iter().find(|(key, _)| key == name).map(|(_, value)| value);
            let present = |name| field(name).is_some_and(|value| !matches!(value, Json::Null));
            let gathered = field("prism").is_some_and(|value| {
                if let Json::Obj(fields) = value {
                    fields.iter().any(|(key, value)| key == "gathered" && matches!(value, Json::Num(g) if *g >= 1.0))
                } else { false }
            });
            let results = field("rows").is_some_and(|value| matches!(value, Json::Arr(rows) if !rows.is_empty()));
            let mode = |mode| field("exploration").is_some_and(|value| matches!(value, Json::Str(name) if name == mode));
            let quiet = field("moving") == Some(&Json::Bool(false))
                && !tick.ledger.any_live() && !tick.drawn.requested();
            for (count, observed) in coverage.iter_mut().zip([
                present("focused"), present("hovered"), gathered, results,
                mode("reach"), mode("tour"), quiet,
            ]) {
                if observed { *count = count.saturating_add(1); }
            }
        }
        Ok(())
    })?;
    Ok(Json::obj([
        ("scene", Json::str(scene.id)),
        ("frames", Json::num(frames)),
        ("requested", Json::num(requested)),
        ("events", Json::num(events)),
        ("mean_cpu_ms", Json::num(total_ms / f64::from(frames.max(1)))),
        ("max_cpu_ms", Json::num(maximum_ms)),
        ("last_requested_ms", Json::opt(last_requested.map(|ms| ms as f64))),
        ("until_ms", Json::num(shot.until_ms as f64)),
        ("retained_frames", Json::num(0)),
        ("retained_frame_timings", Json::num(0)),
        ("invalidations_available", Json::Bool(true)),
        ("cpu_is_performance_evidence", Json::Bool(false)),
        ("probe_enabled", Json::Bool(true)),
        ("coverage", Json::obj([
            ("focused_frames", Json::num(coverage[0])),
            ("hovered_frames", Json::num(coverage[1])),
            ("gathered_prism_frames", Json::num(coverage[2])),
            ("search_result_frames", Json::num(coverage[3])),
            ("reach_frames", Json::num(coverage[4])),
            ("tour_frames", Json::num(coverage[5])),
            ("quiet_frames", Json::num(coverage[6])),
        ])),
    ]))
}
