//! The leased ambient pulse: one shared clock at no more than 12 frames per
//! second for slow ambient motion (ground twinkle, the running bevel, flowing
//! strands, the working gem).
//!
//! A view that paints ambient motion calls [`lease`] from `render` every time
//! it renders; the lease lasts three ticks. One executor timer ticks while any
//! lease is held and notifies exactly the leasing views; when the last lease
//! lapses the timer stops, so an idle window schedules nothing. The pulse
//! does not tick under reduced motion, while paused, or while frozen (the
//! harness freezes it at a phase unless a scene opts in with [`thaw`]).
//!
//! The pulse time is quantised to whole ticks since the motion epoch, so a
//! frame captured at a virtual time always sees the same phase.

use super::{epoch, now, reduced};
use crate::probe::{self, TrackKind, TrackSample};
use gpui::{App, EntityId, Global, Task, Window};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// One pulse tick: 1/12 s.
pub const TICK: Duration = Duration::from_nanos(1_000_000_000 / 12);
/// How long one `lease` call keeps the pulse running (three ticks).
pub const LEASE: Duration = Duration::from_millis(250);

#[derive(Default)]
struct PulseClock {
    leases: HashMap<EntityId, Instant>,
    running: bool,
    paused: bool,
    frozen: Option<f32>,
    ticks: u64,
    task: Option<Task<()>>,
}

impl Global for PulseClock {}

/// The ambient time a view paints with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pulse {
    /// Seconds since the motion epoch, quantised to whole ticks.
    pub seconds: f32,
}

impl Pulse {
    /// Where this pulse sits in a cycle of `period` seconds, in 0..1.
    #[must_use]
    pub fn phase(self, period: f32) -> f32 {
        if period <= 0.0 {
            return 0.0;
        }
        (self.seconds / period).rem_euclid(1.0)
    }

    /// A smooth 0..=1..=0 wave over `period` seconds, offset by `shift` of a
    /// cycle (for staggering many marks off one pulse).
    #[must_use]
    pub fn wave(self, period: f32, shift: f32) -> f32 {
        let phase = (self.phase(period) + shift).rem_euclid(1.0);
        0.5 - 0.5 * (phase * std::f32::consts::TAU).cos()
    }
}

/// Leases the pulse for the view being rendered and returns its time.
/// Call from `render` (or `prepaint`) every frame the ambient motion is
/// visible; stop calling it and the pulse lets go within three ticks.
pub fn lease(window: &mut Window, cx: &mut App) -> Pulse {
    let now = now(cx);
    let epoch = epoch(cx);
    let clock = cx.default_global::<PulseClock>();
    if let Some(seconds) = clock.frozen {
        return Pulse { seconds };
    }
    let seconds = quantise(now.saturating_duration_since(epoch));
    if reduced(cx) {
        return Pulse { seconds: 0.0 };
    }
    let clock = cx.default_global::<PulseClock>();
    let pulse = Pulse { seconds };
    if clock.paused {
        return pulse;
    }
    clock.leases.insert(window.current_view(), now + LEASE);
    let start = !clock.running;
    if start {
        clock.running = true;
    }
    if start {
        let task = cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if !cx.update(tick) {
                    break;
                }
            }
        });
        cx.default_global::<PulseClock>().task = Some(task);
    }
    probe::record_track(cx, || TrackSample {
        key: "pulse".to_owned(),
        kind: TrackKind::Pulse,
        value: seconds,
        target: seconds,
        velocity: 1.0,
        started_ms: 0.0,
        budget_ms: 0.0,
        at_ms: now.saturating_duration_since(epoch).as_secs_f64() * 1000.0,
        live: true,
        overshoot_ratio: 0.0,
        overshoot_absolute: 0.0,
        group: None,
    });
    pulse
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn quantise(elapsed: Duration) -> f32 {
    let ticks = elapsed.as_nanos() / TICK.as_nanos();
    (ticks as f64 * TICK.as_secs_f64()) as f32
}

fn tick(cx: &mut App) -> bool {
    let now = now(cx);
    let clock = cx.default_global::<PulseClock>();
    clock.leases.retain(|_, expiry| *expiry > now);
    if clock.leases.is_empty() || clock.paused || clock.frozen.is_some() {
        clock.running = false;
        clock.leases.clear();
        return false;
    }
    clock.ticks += 1;
    let mut views = clock.leases.keys().copied().collect::<Vec<_>>();
    views.sort_unstable();
    for view in views {
        cx.notify(view);
    }
    true
}

/// Freezes the pulse at `seconds` (the harness default is 0) and stops it.
pub fn freeze(seconds: f32, cx: &mut App) {
    let clock = cx.default_global::<PulseClock>();
    clock.frozen = Some(seconds);
    clock.leases.clear();
}

/// Lets the pulse follow the executor clock again (a scene that samples the
/// pulse calls this while it is being built).
pub fn thaw(cx: &mut App) {
    cx.default_global::<PulseClock>().frozen = None;
}

/// Pauses or resumes ticking (the shell pauses while its window is inactive).
pub fn pause(paused: bool, cx: &mut App) {
    let clock = cx.default_global::<PulseClock>();
    clock.paused = paused;
    if paused {
        clock.leases.clear();
    }
}

/// Whether the pulse timer is currently scheduled.
#[must_use]
pub fn running(cx: &App) -> bool {
    cx.try_global::<PulseClock>()
        .is_some_and(|clock| clock.running)
}

/// Ticks delivered since the app started (for tests and probes).
#[must_use]
pub fn ticks(cx: &App) -> u64 {
    cx.try_global::<PulseClock>().map_or(0, |clock| clock.ticks)
}

/// The number of views currently holding a lease.
#[must_use]
pub fn leases(cx: &App) -> usize {
    cx.try_global::<PulseClock>()
        .map_or(0, |clock| clock.leases.len())
}
