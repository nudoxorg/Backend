//! Measurement aid: records how long each owner-thread step takes.
//!
//! Enabled only when `NUDOX_OWNER_TRACE` names a file; otherwise every call is a
//! single relaxed load. Lines are `<epoch_ms> <label> <elapsed_ms>`.
use std::io::Write;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static SINK: OnceLock<Option<std::sync::Mutex<std::fs::File>>> = OnceLock::new();
static THRESHOLD_MS: AtomicU64 = AtomicU64::new(5);

fn sink() -> Option<&'static std::sync::Mutex<std::fs::File>> {
    SINK.get_or_init(|| {
        let path = std::env::var_os("NUDOX_OWNER_TRACE")?;
        if let Some(ms) = std::env::var("NUDOX_OWNER_TRACE_MS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            THRESHOLD_MS.store(ms, Ordering::Relaxed);
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(std::sync::Mutex::new)
    })
    .as_ref()
}

fn emit(label: &str, started: Instant) {
    let Some(sink) = sink() else { return };
    let elapsed = started.elapsed();
    if elapsed.as_millis() < u128::from(THRESHOLD_MS.load(Ordering::Relaxed)) {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    if let Ok(mut file) = sink.lock() {
        let _ = writeln!(file, "{now} {label} {}", elapsed.as_millis());
    }
}

/// Times one scope; logs on drop.
pub struct Span {
    label: &'static str,
    started: Instant,
}

/// Starts a span.
pub fn span(label: &'static str) -> Span {
    Span {
        label,
        started: Instant::now(),
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        emit(self.label, self.started);
    }
}

/// Times consecutive laps of one function.
pub struct Laps {
    last: Instant,
}

/// Starts lap timing.
pub fn laps() -> Laps {
    Laps {
        last: Instant::now(),
    }
}

impl Laps {
    /// Logs the time since the previous lap under `label`.
    pub fn lap(&mut self, label: &'static str) {
        emit(label, self.last);
        self.last = Instant::now();
    }
}
