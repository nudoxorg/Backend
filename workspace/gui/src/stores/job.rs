//! `JobStore` — GUI-PLAN §12.6
//!
//! Tracks all background jobs (compile, sync, embed, index) and their
//! pipeline stages. Fed by the long-lived `jobs()` receiver. Emits
//! `ToastRequest` to `ShellStore` on completion.

use std::collections::VecDeque;
use std::time::Instant;

use gpui::{Context, SharedString, Task};

// ---------------------------------------------------------------------------
// Id types
// ---------------------------------------------------------------------------

/// Opaque identifier for a background job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct JobId(pub u64);

/// Opaque identifier for the log-filter scope associated with a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LogFilterId(pub u64);

// ---------------------------------------------------------------------------
// Kind + Phase
// ---------------------------------------------------------------------------

/// What kind of background work this job represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    /// IR compilation of a trusted-local package.
    Compile,
    /// Fetching a remote generation.
    Sync,
    /// Embedding generation for the vector index.
    Embed,
    /// Tantivy / catalog indexing.
    Index,
}

// ---------------------------------------------------------------------------
// Row
// ---------------------------------------------------------------------------

/// A single job row — the fully render-ready state the view consumes.
///
/// Fields are `SharedString` / primitive so render does zero string work
/// (GUI-PLAN §1.1.4).
#[derive(Clone, Debug)]
pub struct JobRow {
    pub id: JobId,
    pub kind: JobKind,
    /// Display label pre-formatted at update time.
    pub label: SharedString,
    /// `None` for indeterminate; `Some(f)` for a `0.0..=1.0` fill fraction.
    pub progress: Option<f32>,
    /// Current phase description (e.g. "Compiling nudox-ir…").
    pub phase: SharedString,
    /// When the job started (used by the view to format elapsed time).
    pub started: Instant,
    /// Log-filter scope for deep-linking from the Jobs panel to the Log panel.
    pub log_filter: LogFilterId,
    /// `true` if the job has completed successfully.
    pub completed: bool,
    /// `true` if the job failed.
    pub failed: bool,
    /// Error message, if any (pre-formatted).
    pub error_message: Option<SharedString>,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// `JobStore` — GUI-PLAN §12.6.
pub struct JobStore {
    /// Jobs that are currently active (in progress).
    pub active: Vec<JobRow>,
    /// Recently completed / failed jobs. Capped at 100 (most recent last).
    pub recent: VecDeque<JobRow>,

    // Stream ownership (LD-18).
    //
    // The jobs channel is app-lifetime; we own a single drain task for it.
    pub(crate) _drain_task: Option<Task<()>>,
}

impl JobStore {
    pub fn new() -> Self {
        Self {
            active: Vec::new(),
            recent: VecDeque::new(),
            _drain_task: None,
        }
    }

    // ── Internal state helpers ─────────────────────────────────────────────

    /// Transition a job from active to recent.
    ///
    /// Called when the drain receives a completion or failure event.
    /// Emits `ToastRequest` so `ShellStore` can enqueue a notification.
    fn complete_job(&mut self, id: JobId, cx: &mut Context<Self>) {
        let pos = self.active.iter().position(|r| r.id == id);
        if let Some(idx) = pos {
            let mut row = self.active.remove(idx);
            row.completed = true;
            let is_error = row.failed;
            let msg: std::sync::Arc<str> = if let Some(ref e) = row.error_message {
                std::sync::Arc::from(e.as_ref())
            } else {
                std::sync::Arc::from(row.label.as_ref())
            };
            // Push to recent (cap 100).
            if self.recent.len() >= 100 {
                self.recent.pop_front();
            }
            self.recent.push_back(row.clone());

            cx.emit(crate::stores::events::ToastRequest {
                message: msg,
                action: None,
                is_error,
            });
            cx.notify();
        }
    }

    /// Upsert a job row into the active list.
    fn upsert_active(&mut self, row: JobRow) {
        if let Some(existing) = self.active.iter_mut().find(|r| r.id == row.id) {
            *existing = row;
        } else {
            self.active.push(row);
        }
    }

    // ── Public intent methods ──────────────────────────────────────────────

    /// Request cancellation of a job.
    ///
    /// In production this sends `ClientCommand::CancelJob`; for now it marks
    /// the job as failed locally so the view can reflect intent immediately.
    pub fn cancel(&mut self, id: JobId, cx: &mut Context<Self>) {
        if let Some(row) = self.active.iter_mut().find(|r| r.id == id) {
            row.failed = true;
            row.error_message = Some(SharedString::from("Cancelled"));
        }
        self.complete_job(id, cx);
    }
}

impl Default for JobStore {
    fn default() -> Self {
        Self::new()
    }
}

impl gpui::EventEmitter<crate::stores::events::ToastRequest> for JobStore {}
