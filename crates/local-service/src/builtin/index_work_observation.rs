//! Opt-in private worker observations. Never a receipt or readiness capability.
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_ACTIVE_SOURCES: usize = super::source_budget::MAX_SCAN_WORKERS;
const MAX_BYTES: usize = 8192;
const INTERVAL: Duration = Duration::from_millis(250);

pub(super) struct IndexWorkObservation {
    path: PathBuf,
    ticket: backend_library::IndexJobTicket,
    operation_key: Option<backend_library::IndexOperationKey>,
    started: Instant,
    cancelled: Arc<AtomicBool>,
    state: Mutex<State>,
    #[cfg(test)]
    source_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

struct State {
    phase: &'static str,
    phase_started: u128,
    sequence: u64,
    attempt: u64,
    total: usize,
    finished: usize,
    acknowledged: usize,
    active: [Option<serde_json::Value>; MAX_ACTIVE_SOURCES],
    last_written: Option<Instant>,
}

impl IndexWorkObservation {
    pub(super) fn enabled(
        workspace: &Path,
        ticket: backend_library::IndexJobTicket,
        operation_key: Option<backend_library::IndexOperationKey>,
        cancelled: Arc<AtomicBool>,
    ) -> Option<Arc<Self>> {
        (cfg!(unix)
            && std::env::var_os("BACKEND_INDEX_WORK_OBSERVATION").as_deref()
                == Some(std::ffi::OsStr::new("1")))
        .then(|| Arc::new(Self::new(workspace, ticket, operation_key, cancelled)))
    }

    fn new(
        workspace: &Path,
        ticket: backend_library::IndexJobTicket,
        operation_key: Option<backend_library::IndexOperationKey>,
        cancelled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            path: workspace.join("index-work-observation-v1.json"),
            ticket,
            operation_key,
            started: Instant::now(),
            cancelled,
            #[cfg(test)]
            source_hook: Mutex::new(None),
            state: Mutex::new(State {
                phase: "reserved",
                phase_started: unix_millis(),
                sequence: 0,
                attempt: 0,
                total: 0,
                finished: 0,
                acknowledged: 0,
                active: std::array::from_fn(|_| None),
                last_written: None,
            }),
        }
    }

    #[cfg(test)]
    pub(super) fn for_test(
        workspace: &Path,
        ticket: backend_library::IndexJobTicket,
        cancelled: Arc<AtomicBool>,
    ) -> Arc<Self> {
        Arc::new(Self::new(workspace, ticket, None, cancelled))
    }

    #[cfg(test)]
    pub(super) fn hold_source_for_test(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.source_hook.lock().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    // Called only by existing workers. The owner never performs observation IO.
    pub(super) fn phase(&self, phase: &'static str) {
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.phase = phase;
            state.phase_started = unix_millis();
            state.sequence = state.sequence.saturating_add(1);
        }
        self.flush(true);
    }

    // A source-frontier race can trigger a full retry inside the same ticket.
    // The previous scoped scan has joined before this method is called again.
    pub(super) fn begin_source_attempt(&self) {
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.attempt = state.attempt.saturating_add(1);
            state.total = 0;
            state.finished = 0;
            state.acknowledged = 0;
            state.active = std::array::from_fn(|_| None);
        }
        self.phase("source-directory-discovery");
    }

    pub(super) fn sources(&self, total: usize) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).total = total;
    }

    pub(super) fn source_started(&self, worker: usize, ordinal: usize, relative: &Path) {
        let text = relative.to_string_lossy();
        // A short escaped prefix plus exact path digest is bounded even for
        // control characters. It is not a source-fact identity or source credit.
        let mut end = text.len().min(64);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = state.active.get_mut(worker) {
            *slot = Some(serde_json::json!({
                "source_ordinal": ordinal + 1,
                "started_unix_millis": unix_millis(),
                "path_prefix": &text[..end],
                "path_truncated": end < text.len(),
                "path_blake3": blake3::hash(text.as_bytes()).to_hex().to_string(),
                "work": "confined-read-and-structural-analysis",
            }));
        }
        state.sequence = state.sequence.saturating_add(1);
        drop(state);
        #[cfg(test)]
        if let Some(hook) = self
            .source_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            hook();
        }
    }

    pub(super) fn source_finished(&self, worker: usize) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.finished = state.finished.saturating_add(1);
        if let Some(Some(slot)) = state.active.get_mut(worker) {
            slot["work"] = serde_json::json!("waiting-for-result-send-or-ordered-ack");
        }
        state.sequence = state.sequence.saturating_add(1);
    }

    pub(super) fn source_released(&self, worker: usize) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = state.active.get_mut(worker) {
            *slot = None;
        }
        state.sequence = state.sequence.saturating_add(1);
    }

    // Counts ordered Ok(Some(file)) and Ok(None) results acknowledged to workers.
    // It does not count rows, retained source facts, or compiler completion.
    pub(super) fn source_acknowledged(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.acknowledged = state.acknowledged.saturating_add(1);
        state.sequence = state.sequence.saturating_add(1);
    }

    pub(super) fn flush(&self, force: bool) {
        self.flush_at(force, Instant::now());
    }

    fn flush_at(&self, force: bool, now: Instant) {
        let value = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !force
                && state
                    .last_written
                    .is_some_and(|at| now.saturating_duration_since(at) < INTERVAL)
            {
                return;
            }
            state.last_written = Some(now);
            serde_json::json!({
                "schema": "nudox.index-work-observation.v1",
                "process_id": std::process::id(),
                "ticket": self.ticket,
                "operation_key": self.operation_key,
                "sequence": state.sequence,
                "observed_unix_millis": unix_millis(),
                "phase_started_unix_millis": state.phase_started,
                "elapsed_millis": self.started.elapsed().as_millis(),
                "phase": state.phase,
                "source_scan_attempt": state.attempt,
                "sources_total": state.total,
                "source_results_finished": state.finished,
                "source_results_acknowledged": state.acknowledged,
                "workers": state.active,
                "cancelled": self.cancelled.load(Ordering::Acquire),
                "terminal_receipt": false,
                "readiness_authority": false,
                "source_or_result_authority": false,
            })
        };
        if let Ok(bytes) = serde_json::to_vec(&value)
            && bytes.len() <= MAX_BYTES
        {
            // Observations are best effort and unflushed; a failure cannot
            // select, reject, cancel or roll back any index work.
            let _ = backend_platform::durable::write_private_atomic_unflushed(&self.path, &bytes);
        }
    }
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |v| v.as_millis())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn ticket() -> backend_library::IndexJobTicket {
        backend_library::IndexJobTicket::new(
            std::num::NonZeroU64::new(3).expect("nonzero"),
            [4; 16],
            backend_library::PackageReference::parse("/observed-project".to_owned())
                .expect("package"),
        )
    }

    #[test]
    fn bounded_observation_keeps_exact_ticket_and_no_result_authority()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = crate::test_support::private_temp_dir("index-observation-bound")?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let operation_key =
            backend_library::IndexOperationKey::from_bytes([9; 32]).expect("nonzero key");
        let observed = IndexWorkObservation::new(
            directory.path(),
            ticket(),
            Some(operation_key),
            Arc::clone(&cancelled),
        );
        observed.begin_source_attempt();
        observed.sources(800);
        for worker in 0..MAX_ACTIVE_SOURCES {
            observed.source_started(worker, worker, Path::new(&"\n\r\t控制".repeat(300)));
        }
        observed.flush(true);
        let raw = std::fs::read(&observed.path)?;
        assert!(raw.len() <= MAX_BYTES);
        let value: serde_json::Value = serde_json::from_slice(&raw)?;
        assert_eq!(value["ticket"], serde_json::to_value(ticket())?);
        assert_eq!(value["operation_key"], serde_json::to_value(operation_key)?);
        assert_eq!(value["source_scan_attempt"], 1);
        assert_eq!(value["sources_total"], 800);
        assert_eq!(value["terminal_receipt"], false);
        assert_eq!(value["readiness_authority"], false);
        assert_eq!(value["source_or_result_authority"], false);
        assert_eq!(
            value["workers"].as_array().expect("workers").len(),
            MAX_ACTIVE_SOURCES
        );
        assert!(
            value["workers"]
                .as_array()
                .expect("workers")
                .iter()
                .all(|v| v["path_truncated"] == true)
        );
        cancelled.store(true, Ordering::Release);
        observed.flush(true);
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&observed.path)?)?;
        assert_eq!(value["cancelled"], true);
        observed.source_finished(0);
        observed.source_acknowledged();
        observed.begin_source_attempt();
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&observed.path)?)?;
        assert_eq!(value["source_scan_attempt"], 2);
        assert_eq!(value["sources_total"], 0);
        assert_eq!(value["source_results_finished"], 0);
        assert_eq!(value["source_results_acknowledged"], 0);
        assert!(value["workers"].as_array().expect("workers").iter().all(serde_json::Value::is_null));
        assert_eq!(value["ticket"], serde_json::to_value(ticket())?);
        assert_eq!(value["cancelled"], true, "diagnostic reset cannot clear cancellation");
        Ok(())
    }

    #[test]
    fn checkpoint_throttling_and_sink_failure_do_not_change_work_control()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = crate::test_support::private_temp_dir("index-observation-refusal")?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let observed =
            IndexWorkObservation::new(directory.path(), ticket(), None, Arc::clone(&cancelled));
        observed.phase("structural-source-analysis");
        let before = std::fs::read(&observed.path)?;
        let same_instant = observed
            .state
            .lock()
            .expect("state")
            .last_written
            .expect("phase wrote");
        observed.source_acknowledged();
        observed.flush_at(false, same_instant);
        assert_eq!(
            std::fs::read(&observed.path)?,
            before,
            "rate bound preserves last snapshot"
        );
        observed.flush(true);
        assert_ne!(std::fs::read(&observed.path)?, before);
        std::fs::remove_file(&observed.path)?;
        std::fs::create_dir(&observed.path)?;
        observed.phase("await-owner-grant");
        observed.source_acknowledged();
        observed.flush(true);
        assert!(
            !cancelled.load(Ordering::Acquire),
            "diagnostic sink cannot cancel the operation"
        );
        assert_eq!(observed.state.lock().expect("state").acknowledged, 2);
        Ok(())
    }
}
