//! Opt-in tracing of the actual native window's GPUI draws.
//!
//! Sampling never refreshes a window. A bounded channel sends frame timings
//! to an append-only JSONL writer; overload is reported instead of retaining
//! an unbounded history. These are CPU draw and invalidation timings, not GPU
//! presentation timestamps or measurements of input before invalidation.

use super::json::Json;
use gpui::{
    App, Global, Subscription, Task,
    profiler::{FrameTiming, FrameTimingCollector},
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const POLL: Duration = Duration::from_millis(200);
const SYNC: Duration = Duration::from_secs(1);
const MAX_BATCH: usize = 256;
const QUEUED_BATCHES: usize = 4;
const MAX_WINDOWS: usize = 64;

struct Batch {
    frames: Vec<FrameTiming>,
    observed: u64,
    dropped: u64,
    unix_ms: f64,
}

struct Sampler {
    collector: FrameTimingCollector,
    sender: SyncSender<Batch>,
    observed: u64,
    dropped: u64,
}

impl Sampler {
    fn poll(&mut self) -> bool {
        let mut frames = self.collector.collect_unseen();
        self.observed = self.observed.saturating_add(frames.len() as u64);
        if frames.len() > MAX_BATCH {
            self.dropped = self
                .dropped
                .saturating_add((frames.len() - MAX_BATCH) as u64);
            frames.truncate(MAX_BATCH);
        }
        // Empty polls publish accounting too: dropped batches cannot stay
        // invisible just because the native window has become idle.
        let batch = Batch {
            frames,
            observed: self.observed,
            dropped: self.dropped,
            unix_ms: unix_ms(),
        };
        match self.sender.try_send(batch) {
            Ok(()) => true,
            Err(TrySendError::Full(batch)) => {
                self.dropped = self.dropped.saturating_add(batch.frames.len() as u64);
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

struct NativeTrace {
    task: Task<()>,
    sampler: Rc<RefCell<Sampler>>,
    worker: JoinHandle<()>,
    _quit: Subscription,
}
impl Global for NativeTrace {}

/// Starts a native trace before opening its window. The app owns its task
/// until quit; tracing is inactive unless the caller explicitly invokes this.
pub(crate) fn start(path: &Path, cx: &mut App) -> io::Result<()> {
    if cx.has_global::<NativeTrace>() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "a native trace is already running",
        ));
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let session = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let mut writer = BufWriter::with_capacity(16 * 1024, file);
    write_row(
        &mut writer,
        Json::obj([
            ("event", Json::str("start")),
            ("schema", Json::num(1)),
            ("session", Json::str(&session)),
            ("pid", Json::num(std::process::id())),
            ("unix_ms", Json::num(unix_ms())),
            ("poll_ms", Json::num(POLL.as_millis() as f64)),
            ("durable_period_ms", Json::num(SYNC.as_millis() as f64)),
            ("queued_batches", Json::num(QUEUED_BATCHES as f64)),
            ("max_batch_records", Json::num(MAX_BATCH as f64)),
            (
                "source",
                Json::str("GPUI Window::draw on the native window"),
            ),
            (
                "limitations",
                Json::str(
                    "CPU cadence; no GPU presentation timestamp; input before the first invalidation is excluded; the shared profiler has a bounded 16 MiB timing ring",
                ),
            ),
        ]),
    )?;
    durable(&mut writer)?;
    let (sender, receiver) = mpsc::sync_channel(QUEUED_BATCHES);
    let worker = std::thread::Builder::new()
        .name("facet-native-trace".into())
        .spawn(move || {
            if let Err(error) = write_batches(writer, receiver, &session) {
                eprintln!("native trace writer stopped: {error}");
            }
        })?;
    gpui::set_frame_trace_enabled(true);
    let sampler = Rc::new(RefCell::new(Sampler {
        collector: FrameTimingCollector::new(),
        sender,
        observed: 0,
        dropped: 0,
    }));
    let polling = sampler.clone();
    let task = cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(POLL).await;
            if !polling.borrow_mut().poll() {
                break;
            }
        }
    });
    let quit = cx.on_app_quit(|cx| {
        let NativeTrace {
            task,
            sampler,
            worker,
            _quit,
        } = cx.remove_global::<NativeTrace>();
        let _ = sampler.borrow_mut().poll();
        drop(task);
        drop(sampler);
        drop(_quit);
        let joined = cx.background_executor().spawn(async move {
            let _ = worker.join();
        });
        async move {
            joined.await;
        }
    });
    cx.set_global(NativeTrace {
        task,
        sampler,
        worker,
        _quit: quit,
    });
    Ok(())
}

fn write_batches(
    mut writer: BufWriter<File>,
    receiver: Receiver<Batch>,
    session: &str,
) -> io::Result<()> {
    let mut origin = None::<FrameTiming>;
    let mut previous = BTreeMap::<u64, FrameTiming>::new();
    let mut written = 0_u64;
    let mut last_sync = Instant::now();
    while let Ok(batch) = receiver.recv() {
        for timing in batch.frames {
            let first = *origin.get_or_insert(timing);
            let id = timing.window_id.as_u64();
            let prior = previous.get(&id).copied();
            if previous.len() < MAX_WINDOWS || prior.is_some() {
                previous.insert(id, timing);
            }
            written = written.saturating_add(1);
            write_row(
                &mut writer,
                Json::obj([
                    ("event", Json::str("frame")),
                    ("session", Json::str(session)),
                    // IDs are strings to preserve all 64 native bits in JSON consumers.
                    ("window_id", Json::str(id.to_string())),
                    ("frame", Json::num(written as f64)),
                    ("cold", Json::Bool(prior.is_none())),
                    (
                        "draw_start_ms",
                        Json::num(
                            timing
                                .draw_start
                                .duration_since(first.draw_start)
                                .as_secs_f64()
                                * 1000.0,
                        ),
                    ),
                    (
                        "draw_cpu_ms",
                        Json::num(timing.draw_duration().as_secs_f64() * 1000.0),
                    ),
                    (
                        "dirty_to_draw_start_ms",
                        Json::opt(
                            timing.dirty_at.map(|at| {
                                timing.draw_start.duration_since(at).as_secs_f64() * 1000.0
                            }),
                        ),
                    ),
                    (
                        "dirty_to_draw_end_ms",
                        Json::opt(
                            timing
                                .dirty_to_draw_duration()
                                .map(|duration| duration.as_secs_f64() * 1000.0),
                        ),
                    ),
                    (
                        "inter_draw_ms",
                        Json::opt(prior.map(|prior| {
                            timing
                                .draw_start
                                .duration_since(prior.draw_start)
                                .as_secs_f64()
                                * 1000.0
                        })),
                    ),
                    ("invalidations", Json::num(timing.invalidations as f64)),
                ]),
            )?;
        }
        write_row(
            &mut writer,
            Json::obj([
                ("event", Json::str("poll")),
                ("session", Json::str(session)),
                ("unix_ms", Json::num(batch.unix_ms)),
                ("frames_observed", Json::num(batch.observed as f64)),
                ("frames_written", Json::num(written as f64)),
                ("frames_dropped", Json::num(batch.dropped as f64)),
            ]),
        )?;
        writer.flush()?;
        if last_sync.elapsed() >= SYNC {
            durable(&mut writer)?;
            last_sync = Instant::now();
        }
    }
    durable(&mut writer)
}

fn write_row(writer: &mut impl Write, row: Json) -> io::Result<()> {
    writeln!(writer, "{row}")
}
fn durable(writer: &mut BufWriter<File>) -> io::Result<()> {
    writer.flush()?;
    writer.get_ref().sync_data()
}
fn unix_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        * 1000.0
}
