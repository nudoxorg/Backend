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
    #[cfg(target_os = "linux")]
    _native_clock: Option<backend_platform::monotonic::EpochAnchor>,
    out: Mutex<Output>,
}

struct Output {
    writer: BufWriter<File>,
    failed: bool,
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
        #[cfg(target_os = "linux")]
        let native_clock = backend_platform::monotonic::EpochAnchor::capture().ok();
        #[cfg(target_os = "linux")]
        let epoch = native_clock.as_ref().map_or_else(Instant::now, backend_platform::monotonic::EpochAnchor::epoch);
        #[cfg(not(target_os = "linux"))]
        let epoch = Instant::now();
        #[cfg(target_os = "linux")]
        let native_detail = native_clock_detail(native_clock.as_ref());
        #[cfg(not(target_os = "linux"))]
        let native_detail = "";
        let unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |since| since.as_secs_f64() * 1e3);
        let mut out = BufWriter::with_capacity(64 * 1024, file);
        let failed = writeln!(
            out,
            "{{\"t_ms\":0,\"name\":\"trace.epoch\",\"detail\":\"pid {}\",\"unix_ms\":{unix_ms:.3}{native_detail}}}",
            std::process::id()
        ).is_err();
        Some(Sink {
            epoch,
            #[cfg(target_os = "linux")]
            _native_clock: native_clock,
            out: Mutex::new(Output { writer: out, failed }),
        })
    });
    STATE.store(if sink.is_some() { ON } else { OFF }, Ordering::Relaxed);
    sink.as_ref()
}

#[cfg(target_os = "linux")]
fn native_clock_detail(clock: Option<&backend_platform::monotonic::EpochAnchor>) -> String {
    clock.map(|clock| {
        let [before, after] = clock.interval_ns();
        let (volume, object) = clock.namespace().parts();
        format!(",\"monotonic_epoch_interval_ns\":[{before},{after}],\"time_namespace\":{{\"volume\":{volume},\"object\":{object}}}")
    }).unwrap_or_default()
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
    let _ = write_completed(sink, line);
}

fn write_completed(sink: &Sink, line: &str) -> bool {
    let mut out = sink
        .out
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    write_line(&mut out, line)
}

fn write_line(out: &mut Output, line: &str) -> bool {
    let completed = out
        .writer
        .write_all(line.as_bytes())
        .and_then(|()| out.writer.write_all(b"\n"))
        .and_then(|()| out.writer.flush())
        .is_ok();
    out.failed |= !completed;
    completed
}

fn write_watermark(sink: &Sink, line: &str) -> bool {
    let mut out = sink
        .out
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // A failed earlier record could have advanced a collector cursor. Do not
    // certify a later batch after silently losing that record, even if IO
    // recovers. Check and write under the same lock as all other trace writes.
    !out.failed && write_line(&mut out, line)
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

fn lifecycle_line(sink: &Sink, event: &gpui::profiler::FrameLifecycleEvent) -> String {
    use gpui::profiler::FrameLifecycleKind;
    let (name, detail) = match event.kind {
        FrameLifecycleKind::Drawn { a11y_frame } => (
            "drawn",
            format!(
                "\"a11y_frame\":{}",
                a11y_frame.map_or_else(|| "null".to_owned(), |frame| frame.to_string())
            ),
        ),
        FrameLifecycleKind::Presentation { result } => {
            ("submission", format!("\"result\":\"{result:?}\""))
        }
        FrameLifecycleKind::LinkStarted { display_id } => {
            ("link_started", format!("\"display_id\":{display_id}"))
        }
        FrameLifecycleKind::LinkStopped { display_id } => {
            ("link_stopped", format!("\"display_id\":{display_id}"))
        }
        FrameLifecycleKind::LinkStartFailed { display_id } => {
            ("link_start_failed", format!("\"display_id\":{display_id}"))
        }
        FrameLifecycleKind::FrameTick { count } => ("frame_tick", format!("\"count\":{count}")),
    };
    let native_window = event
        .native_window_number
        .map_or_else(|| "null".to_owned(), |id| id.to_string());
    format!(
        "{{\"t_ms\":{:.3},\"name\":\"frame_lifecycle\",\"pid\":{},\"window_id\":{},\"native_window\":{},\"frame_id\":{},\"kind\":\"{}\",{}}}",
        ms(sink, event.at),
        std::process::id(),
        event.window_id.as_u64(),
        native_window,
        event.frame_id,
        name,
        detail,
    )
}

fn write_lifecycle_batch(sink: &Sink, batch: gpui::profiler::FrameLifecycleBatch) -> bool {
    let mut completed = true;
    if batch.dropped > 0 {
        completed &= write_completed(
            sink,
            &format!(
                "{{\"t_ms\":{:.3},\"name\":\"frame_lifecycle_dropped\",\"count\":{}}}",
                ms(sink, Instant::now()),
                batch.dropped
            ),
        );
    }
    for event in batch.events {
        completed &= write_completed(sink, &lifecycle_line(sink, &event));
    }
    completed
}

fn write_frame_collection(
    sink: &Sink,
    scope: &str,
    collection: u64,
    collected_through: Instant,
    timings: Vec<gpui::profiler::FrameTiming>,
    lifecycle: gpui::profiler::FrameLifecycleBatch,
) -> bool {
    let timing_count = timings.len();
    let lifecycle_count = lifecycle.events.len();
    let dropped = lifecycle.dropped;
    let mut completed = true;
    for frame in timings {
        completed &= write_completed(sink, &frame_line(sink, &frame));
    }
    completed &= write_lifecycle_batch(sink, lifecycle);
    if !completed {
        return false;
    }
    // This line follows successful writes AND flushes of the entire batch.
    // Its cut precedes collection: writes must not claim later events were
    // observed. Only the app-ui scope brackets UI-thread Drawn/Presentation;
    // native frame-source callbacks on other threads may arrive later.
    write_watermark(
        sink,
        &format!(
            "{{\"t_ms\":{:.3},\"name\":\"frame_collection\",\"pid\":{},\"scope\":\"{}\",\"collection\":{},\"collected_through_ms\":{:.3},\"timings\":{},\"lifecycle\":{},\"dropped\":{}}}",
            ms(sink, Instant::now()),
            std::process::id(),
            scope,
            collection,
            ms(sink, collected_through),
            timing_count,
            lifecycle_count,
            dropped,
        ),
    )
}

thread_local! {
    static NOW: std::cell::RefCell<Option<(gpui::profiler::FrameTimingCollector, gpui::profiler::FrameLifecycleCollector, u64)>> =
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
            (
                gpui::profiler::FrameTimingCollector::new(),
                gpui::profiler::FrameLifecycleCollector::new(),
                0,
            )
        });
        let collected_through = Instant::now();
        let timings = collector.0.collect_unseen();
        let lifecycle = collector.1.collect_unseen();
        collector.2 = collector.2.saturating_add(1);
        let _ = write_frame_collection(
            sink,
            "calling-thread",
            collector.2,
            collected_through,
            timings,
            lifecycle,
        );
    });
}

/// Samples GPUI's draw timings into the trace until the app quits. Call
/// once, before the first window opens; a no-op when tracing is off.
pub fn frames(cx: &mut gpui::App) {
    let Some(sink) = sink() else { return };
    gpui::set_frame_trace_enabled(true);
    let mut collector = gpui::profiler::FrameTimingCollector::new();
    let mut lifecycle = gpui::profiler::FrameLifecycleCollector::new();
    let mut collection = 0_u64;
    cx.spawn(async move |cx| {
        loop {
            let collected_through = Instant::now();
            let timings = collector.collect_unseen();
            let batch = lifecycle.collect_unseen();
            collection = collection.saturating_add(1);
            let _ = write_frame_collection(
                sink,
                "app-ui",
                collection,
                collected_through,
                timings,
                batch,
            );
            cx.background_executor()
                .timer(std::time::Duration::from_millis(100))
                .await;
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::profiler::{
        FrameLifecycleBatch, FrameLifecycleEvent, FrameLifecycleKind, FrameTiming,
    };
    use std::fs::OpenOptions;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;

    struct TraceFile(PathBuf);

    impl TraceFile {
        fn create() -> (Self, File) {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "nudox-frame-collection-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .unwrap();
            (Self(path), file)
        }

        fn lines(&self) -> Vec<serde_json::Value> {
            std::fs::read_to_string(&self.0)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }
    }

    impl Drop for TraceFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_epoch_fields_bind_the_retained_monotonic_anchor_and_namespace() {
        let clock = backend_platform::monotonic::EpochAnchor::capture().expect("native Linux anchor");
        let detail = native_clock_detail(Some(&clock));
        let row: serde_json::Value = serde_json::from_str(&format!("{{\"t_ms\":0{detail}}}")).expect("trace epoch fields");
        assert_eq!(row["monotonic_epoch_interval_ns"], serde_json::json!(clock.interval_ns()));
        let (volume, object) = clock.namespace().parts();
        assert_eq!(row["time_namespace"]["volume"].as_u64(), Some(volume));
        assert_eq!(row["time_namespace"]["object"].as_u64().map(u128::from), Some(object));
        assert!(native_clock_detail(None).is_empty(), "unavailable native clock must not claim an exact anchor");
    }

    #[test]
    fn a_collection_watermark_follows_the_flushed_batch_and_keeps_loss_visible() {
        let (file, output) = TraceFile::create();
        let cut = Instant::now();
        let sink = Sink {
            #[cfg(target_os = "linux")]
            _native_clock: None,
            epoch: cut,
            out: Mutex::new(Output {
                writer: BufWriter::new(output),
                failed: false,
            }),
        };
        let timing = FrameTiming {
            window_id: gpui::WindowId::from(1),
            dirty_at: None,
            invalidations: 0,
            draw_start: cut,
            draw_end: cut,
        };
        assert!(write_frame_collection(
            &sink,
            "app-ui",
            7,
            cut,
            vec![timing],
            FrameLifecycleBatch {
                events: vec![
                    FrameLifecycleEvent {
                        window_id: gpui::WindowId::from(1),
                        native_window_number: None,
                        frame_id: 11,
                        at: cut,
                        kind: FrameLifecycleKind::Drawn {
                            a11y_frame: Some(11)
                        }
                    },
                    FrameLifecycleEvent {
                        window_id: gpui::WindowId::from(1),
                        native_window_number: None,
                        frame_id: 11,
                        at: cut,
                        kind: FrameLifecycleKind::Presentation {
                            result: gpui::DrawResult::Presented
                        }
                    },
                ],
                dropped: 2
            }
        ));
        let lines = file.lines();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0]["name"], "frame");
        assert_eq!(lines[1]["name"], "frame_lifecycle_dropped");
        assert_eq!(lines[1]["count"], 2);
        assert_eq!(lines[2]["name"], "frame_lifecycle");
        assert_eq!(lines[2]["kind"], "drawn");
        assert_eq!(lines[2]["frame_id"], 11);
        assert_eq!(lines[2]["a11y_frame"], 11);
        assert_eq!(lines[3]["kind"], "submission");
        assert_eq!(lines[3]["result"], "Presented");
        let watermark = &lines[4];
        assert_eq!(watermark["name"], "frame_collection");
        assert_eq!(watermark["scope"], "app-ui");
        assert_eq!(watermark["pid"], std::process::id());
        assert_eq!(watermark["collection"], 7);
        assert_eq!(watermark["collected_through_ms"], 0.0);
        assert!(watermark["t_ms"].as_f64().unwrap() >= 0.0);
        assert_eq!(watermark["timings"], 1);
        assert_eq!(watermark["lifecycle"], 2);
        assert_eq!(watermark["dropped"], 2);
    }

    #[test]
    fn an_empty_collection_still_reports_completion_without_a_frame() {
        let (file, output) = TraceFile::create();
        let cut = Instant::now();
        let sink = Sink {
            #[cfg(target_os = "linux")]
            _native_clock: None,
            epoch: cut,
            out: Mutex::new(Output {
                writer: BufWriter::new(output),
                failed: false,
            }),
        };
        for collection in [1, 2] {
            assert!(write_frame_collection(
                &sink,
                "calling-thread",
                collection,
                cut,
                Vec::new(),
                FrameLifecycleBatch {
                    events: Vec::new(),
                    dropped: 0
                }
            ));
        }
        let lines = file.lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines
                .iter()
                .map(|line| line["collection"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [1, 2]
        );
        for line in lines {
            assert_eq!(line["name"], "frame_collection");
            assert_eq!(line["scope"], "calling-thread");
            assert_eq!(line["timings"], 0);
            assert_eq!(line["lifecycle"], 0);
            assert_eq!(line["dropped"], 0);
        }
    }

    #[test]
    fn a_failed_batch_write_cannot_publish_a_collection_watermark() {
        let (file, output) = TraceFile::create();
        drop(output);
        let sink = Sink {
            #[cfg(target_os = "linux")]
            _native_clock: None,
            epoch: Instant::now(),
            out: Mutex::new(Output {
                writer: BufWriter::new(File::open(&file.0).unwrap()),
                failed: false,
            }),
        };
        assert!(!write_frame_collection(
            &sink,
            "app-ui",
            1,
            sink.epoch,
            Vec::new(),
            FrameLifecycleBatch {
                events: Vec::new(),
                dropped: 1
            }
        ));
        assert!(
            file.lines().is_empty(),
            "failed records cannot be followed by a success watermark"
        );
        // Repair only the test's file handle. The sticky loss state must still
        // reject later completion; a collector has already consumed that batch.
        sink.out.lock().unwrap().writer =
            BufWriter::new(OpenOptions::new().write(true).open(&file.0).unwrap());
        assert!(!write_frame_collection(
            &sink,
            "app-ui",
            2,
            sink.epoch,
            Vec::new(),
            FrameLifecycleBatch {
                events: Vec::new(),
                dropped: 0
            }
        ));
        assert!(
            file.lines().is_empty(),
            "IO recovery cannot conceal an earlier unrecorded batch"
        );
    }
}
