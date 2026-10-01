//! Frame budgets: the wall time of `Window::draw` (render + layout +
//! prepaint + paint) for every frame of a scene's default script (or, for a
//! scene without one, a pointer sweep across it), at 2x, on the virtual
//! frame loop. Budgets (plan §3.8): p95 under 8 ms at 1440x900 and under
//! 12 ms at 2560x1440, in a release build.
//!
//! The first 100 ms are warm-up (fonts, glyph atlas) and reported apart.
//! Every frame of the run counts, requested or not: each draw is a full
//! re-render, so idle draws measure the same work a requested one does.

use super::{GalleryError, Scene, Shot, run};
use backend_gui_harness::{Act, Script};
use std::time::Duration;

/// One budget.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Budget {
    /// Logical window size.
    pub size: (u32, u32),
    /// Allowed p95 draw time.
    pub p95: Duration,
}

/// The plan's budgets.
pub const BUDGETS: [Budget; 2] = [
    Budget {
        size: (1440, 900),
        p95: Duration::from_millis(8),
    },
    Budget {
        size: (2560, 1440),
        p95: Duration::from_millis(12),
    },
];

/// Draw-time statistics of one run.
#[derive(Clone, Debug, PartialEq)]
pub struct Timing {
    /// The scene.
    pub scene: &'static str,
    /// The logical size.
    pub size: (u32, u32),
    /// Frames measured (after warm-up).
    pub frames: usize,
    /// Of which requested (a real window would have drawn them).
    pub requested: usize,
    /// Median.
    pub p50: Duration,
    /// 95th percentile.
    pub p95: Duration,
    /// Slowest.
    pub max: Duration,
    /// The first frame (cold).
    pub first: Duration,
    /// The budget, when one applies to this size.
    pub budget: Option<Duration>,
    /// Whether the run was a release build.
    pub release: bool,
    /// The slowest measured frame: its time, the acts delivered before it,
    /// and the scene's note on its work.
    pub worst: Option<Worst>,
    /// All script-dispatch batches, including cold inputs; separate from draw.
    pub input: InputTiming,
}

/// Wall time spent dispatching script acts and immediate foreground tasks.
#[derive(Clone,Debug,PartialEq)]
pub struct InputTiming {
    /// Frames carrying input.
    pub batches: usize,
    /// Script acts (a `type` act can dispatch several physical keystrokes).
    pub acts: usize,
    /// Median batch dispatch time.
    pub p50: Duration,
    ///95th percentile batch dispatch time.
    pub p95: Duration,
    /// Maximum batch dispatch time.
    pub max: Duration,
    /// Maximum single-act dispatch time.
    pub max_event: Duration,
    /// The largest batch and its concrete acts.
    pub worst: Option<Worst>,
}

/// The slowest frame of a run, attributed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worst {
    /// Virtual time.
    pub at_ms: u64,
    /// Acts delivered since the previous frame.
    pub acts: Vec<String>,
    /// Invalidations it carried.
    pub invalidations: u64,
    /// The scene's description of its work.
    pub note: Option<String>,
}

impl Timing {
    /// Within budget (and measured in release)?
    #[must_use]
    pub fn passed(&self) -> bool {
        self.release && self.budget.is_none_or(|budget| self.p95 <= budget)
    }
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let index = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

/// A pointer sweep across a `width` x `height` window: a diagonal pass and a
/// horizontal pass, one move per 16 ms.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn sweep(width: u32, height: u32) -> Script {
    let mut script = Script::new();
    let steps = 60_u64;
    for step in 0..=steps {
        let t = step as f32 / steps as f32;
        script.push(
            step * 16,
            Act::Move {
                x: (t * (width as f32 - 1.0)).round(),
                y: (t * (height as f32 - 1.0)).round(),
            },
        );
        script.push(
            (steps + 1 + step) * 16,
            Act::Move {
                x: ((1.0 - t) * (width as f32 - 1.0)).round(),
                y: (height as f32 / 3.0).round(),
            },
        );
    }
    script
}

/// Measures `scene` at `size` playing `script` (default: its declared
/// script, or a sweep).
///
/// # Errors
/// A capture failure.
pub fn measure(
    scene: &Scene,
    size: (u32, u32),
    script: Option<Script>,
) -> Result<Timing, GalleryError> {
    let mut shot = Shot::new(scene);
    shot.size = size;
    shot.scale = 2;
    shot.times = vec![];
    let script = match script {
        Some(script) => script,
        None => {
            let mut probe = shot.clone();
            probe.times = vec![0];
            probe.frame_ms = 0;
            let declared = run(scene, &probe, &mut |_, _, _| Ok(()))?;
            if declared.is_empty() {
                sweep(size.0, size.1)
            } else {
                declared
            }
        }
    };
    shot.until_ms = script.end_ms() + 400;
    shot.script = Some(script);
    let mut all = Vec::new();
    let mut first = None;
    let mut requested = 0;
    let mut worst: Option<(Duration, Worst)> = None;
    let mut input = Vec::new();
    let mut input_acts=0;
    let mut input_max_event=Duration::ZERO;
    let mut input_worst: Option<(Duration,Worst)>=None;
    run(scene, &shot, &mut |tick, _, _| {
        first.get_or_insert(tick.drawn.cpu);
        if tick.drawn.input_events>0 {
            input.push(tick.drawn.input_cpu);
            input_acts+=tick.drawn.input_events;
            input_max_event=input_max_event.max(tick.drawn.input_max);
            if input_worst.as_ref().is_none_or(|(cpu,_)|tick.drawn.input_cpu>*cpu) {
                input_worst=Some((tick.drawn.input_cpu,Worst {
                    at_ms:tick.drawn.at_ms, acts:tick.events.iter().map(|event|event.act.to_string()).collect(),
                    invalidations:tick.drawn.invalidations,note:tick.note.clone(),
                }));
            }
        }
        if tick.drawn.at_ms >= 100 {
            all.push(tick.drawn.cpu);
            if tick.drawn.requested() {
                requested += 1;
            }
            if worst.as_ref().is_none_or(|(cpu, _)| tick.drawn.cpu > *cpu) {
                worst = Some((
                    tick.drawn.cpu,
                    Worst {
                        at_ms: tick.drawn.at_ms,
                        acts: tick
                            .events
                            .iter()
                            .map(|event| event.act.to_string())
                            .collect(),
                        invalidations: tick.drawn.invalidations,
                        note: tick.note.clone(),
                    },
                ));
            }
        }
        Ok(())
    })?;
    all.sort_unstable();
    input.sort_unstable();
    let budget = BUDGETS
        .iter()
        .find(|budget| budget.size == size)
        .map(|budget| budget.p95);
    Ok(Timing {
        scene: scene.id,
        size,
        frames: all.len(),
        requested,
        p50: percentile(&all, 0.5),
        p95: percentile(&all, 0.95),
        max: all.last().copied().unwrap_or_default(),
        first: first.unwrap_or_default(),
        budget,
        release: !cfg!(debug_assertions),
        worst: worst.map(|(_, worst)| worst),
        input: InputTiming { batches:input.len(),acts:input_acts,p50:percentile(&input,0.5),p95:percentile(&input,0.95),
            max:input.last().copied().unwrap_or_default(),max_event:input_max_event,worst:input_worst.map(|(_,worst)|worst) },
    })
}

/// The worst frame, in words.
#[must_use]
pub fn attribution(timing: &Timing) -> String {
    let draw = timing.worst.as_ref().map_or_else(String::new, |worst| {
        format!(
            "worst frame {:.2} ms at {} ms: {} invalidations{}{}",
            timing.max.as_secs_f64() * 1000.0,
            worst.at_ms,
            worst.invalidations,
            if worst.acts.is_empty() {
                String::new()
            } else {
                format!(", after {}", worst.acts.join("; "))
            },
            worst
                .note
                .as_ref()
                .map_or_else(String::new, |note| format!(", {note}"))
        )
    });
    let input=&timing.input;
    format!("{draw}; input dispatch {} acts in {} batches: p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms (single-act max {:.2} ms){}",
        input.acts,input.batches,input.p50.as_secs_f64()*1000.0,input.p95.as_secs_f64()*1000.0,input.max.as_secs_f64()*1000.0,input.max_event.as_secs_f64()*1000.0,
        input.worst.as_ref().map_or_else(String::new,|worst|format!(" at {} ms after {}",worst.at_ms,worst.acts.join("; "))))
}

/// A timing as one report line.
#[must_use]
pub fn line(timing: &Timing) -> String {
    let ms = |value: Duration| value.as_secs_f64() * 1000.0;
    format!(
        "{:<24} {:>4}x{:<4}  p50 {:>6.2}  p95 {:>6.2}  max {:>6.2}  first {:>7.2} ms  {} frames ({} requested)  {}",
        timing.scene,
        timing.size.0,
        timing.size.1,
        ms(timing.p50),
        ms(timing.p95),
        ms(timing.max),
        ms(timing.first),
        timing.frames,
        timing.requested,
        match (timing.release, timing.budget) {
            (false, _) => "NOT BUDGETED (debug build)".to_owned(),
            (true, Some(budget)) if timing.p95 <= budget =>
                format!("PASS (budget {:.0} ms)", ms(budget)),
            (true, Some(budget)) => format!("FAIL (budget {:.0} ms)", ms(budget)),
            (true, None) => "no budget at this size".to_owned(),
        }
    )
}
