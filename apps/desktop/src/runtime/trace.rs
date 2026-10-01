//! Opt-in latency spans for the latency ledger (`NUDOX_TRACE=<path>`).
//!
//! Off by default and one relaxed atomic load when off. When on, every
//! event is one JSON line: milliseconds since the process's first trace
//! call, a name, and a detail. [`frames`] additionally samples GPUI's own
//! draw timings, so a span such as "a read landed at t" can be joined to
//! "the first frame that drew after t" offline without a second clock.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

const UNKNOWN: u8 = 0;
const OFF: u8 = 1;
const ON: u8 = 2;

static STATE: AtomicU8 = AtomicU8::new(UNKNOWN);
static SINK: OnceLock<Option<Sink>> = OnceLock::new();

struct Sink {
    epoch: Instant,
    out: Mutex<BufWriter<File>>,
}

fn sink() -> Option<&'static Sink> {
    match STATE.load(Ordering::Relaxed) {
        OFF => return None,
        ON => return SINK.get().and_then(Option::as_ref),
        _ => {}
    }
    let sink = SINK.get_or_init(|| {
        let path = std::env::var_os("NUDOX_TRACE")?;
        let file = File::create(path).ok()?;
        let epoch = Instant::now();
        let unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |since| since.as_secs_f64() * 1e3);
        let mut out = BufWriter::with_capacity(64 * 1024, file);
        let _ = writeln!(
            out,
            "{{\"t_ms\":0,\"name\":\"trace.epoch\",\"detail\":\"pid {}\",\"unix_ms\":{unix_ms:.3}}}",
            std::process::id()
        );
        Some(Sink {
            epoch,
            out: Mutex::new(out),
        })
    });
    STATE.store(if sink.is_some() { ON } else { OFF }, Ordering::Relaxed);
    sink.as_ref()
}

/// Whether tracing is on for this process.
#[must_use]
pub fn enabled() -> bool {
    sink().is_some()
}

/// Milliseconds from the trace epoch to `at` (negative before it).
fn ms(sink: &Sink, at: Instant) -> f64 {
    at.checked_duration_since(sink.epoch).map_or_else(
        || -sink.epoch.duration_since(at).as_secs_f64() * 1e3,
        |after| after.as_secs_f64() * 1e3,
    )
}

fn write(sink: &Sink, line: &str) {
    let mut out = sink
        .out
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = out.write_all(line.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            control if control.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", u32::from(control)));
            }
            other => escaped.push(other),
        }
    }
    escaped
}

/// One instant: `name` happened now.
pub fn mark(name: &str, detail: impl std::fmt::Display) {
    if let Some(sink) = sink() {
        let at = ms(sink, Instant::now());
        write(
            sink,
            &format!(
                "{{\"t_ms\":{at:.3},\"name\":\"{}\",\"detail\":\"{}\"}}",
                escape(name),
                escape(&detail.to_string())
            ),
        );
    }
}

/// One span: `name` ran from `started` until now.
pub fn span(name: &str, started: Instant, detail: impl std::fmt::Display) {
    if let Some(sink) = sink() {
        let now = Instant::now();
        write(
            sink,
            &format!(
                "{{\"t_ms\":{:.3},\"start_ms\":{:.3},\"dur_ms\":{:.3},\"name\":\"{}\",\"detail\":\"{}\"}}",
                ms(sink, now),
                ms(sink, started),
                now.duration_since(started).as_secs_f64() * 1e3,
                escape(name),
                escape(&detail.to_string())
            ),
        );
    }
}

fn frame_line(sink: &Sink, frame: &gpui::profiler::FrameTiming) -> String {
    format!(
        "{{\"t_ms\":{:.3},\"name\":\"frame\",\"draw_start_ms\":{:.3},\"draw_ms\":{:.3},\"dirty_ms\":{},\"invalidations\":{}}}",
        ms(sink, frame.draw_end),
        ms(sink, frame.draw_start),
        frame.draw_duration().as_secs_f64() * 1e3,
        frame.dirty_at.map_or_else(
            || "null".to_owned(),
            |dirty| format!("{:.3}", ms(sink, dirty))
        ),
        frame.invalidations,
    )
}

thread_local! {
    static NOW: std::cell::RefCell<Option<gpui::profiler::FrameTimingCollector>> =
        const { std::cell::RefCell::new(None) };
}

/// Writes every GPUI draw recorded since the previous call, now, on the
/// calling thread. For a driver without a live timer loop (the harness's
/// virtual frame clock), called once per played frame; a no-op when
/// tracing is off.
pub fn frames_now() {
    let Some(sink) = sink() else { return };
    NOW.with(|slot| {
        let mut slot = slot.borrow_mut();
        let collector = slot.get_or_insert_with(|| {
            gpui::set_frame_trace_enabled(true);
            gpui::profiler::FrameTimingCollector::new()
        });
        for frame in collector.collect_unseen() {
            write(sink, &frame_line(sink, &frame));
        }
    });
}

/// Samples GPUI's draw timings into the trace until the app quits. Call
/// once, before the first window opens; a no-op when tracing is off.
pub fn frames(cx: &mut gpui::App) {
    let Some(sink) = sink() else { return };
    gpui::set_frame_trace_enabled(true);
    let mut collector = gpui::profiler::FrameTimingCollector::new();
    cx.spawn(async move |cx| {
        loop {
            for frame in collector.collect_unseen() {
                write(sink, &frame_line(sink, &frame));
            }
            cx.background_executor()
                .timer(std::time::Duration::from_millis(100))
                .await;
        }
    })
    .detach();
}
