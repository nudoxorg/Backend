//! One exclusive projection owner crosses the control loop through bounded
//! messages. Concurrent callers share that preparation, with at most 64 owed
//! replies. A different selection is refused until the active worker retires;
//! it never queues another corpus or starts another native writer.

use super::super::query::SearchSnapshotOwner;
use backend_engine::{CoverageCapability, Query, ViewRoot, WorkspaceRoot};
use backend_extension_trustfall::SemanticQueryCorpus;
use backend_version::CoverageWitness;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};

pub(super) const MAX_SEARCH_WAITERS: usize = super::adapter::MAX_WAITING_COMMANDS;

pub(super) enum Failure {
    Preparation(super::super::query::QueryError),
    Cancelled,
    Panicked,
    Stopped,
}

impl Failure {
    pub(super) fn into_command_failure(self) -> backend_engine::CommandFailure {
        backend_engine::CommandFailure::InvalidQuery(match self {
            Self::Preparation(error) => format!("search_preparation_refused: {error}"),
            Self::Cancelled => {
                "search_preparation_cancelled: retry against the current view".to_owned()
            }
            Self::Panicked => "search_preparation_worker_panicked: retry".to_owned(),
            Self::Stopped => "search_preparation_worker_stopped: retry".to_owned(),
        })
    }
}

pub(super) enum Rejection {
    Full,
    Retiring,
}

impl Rejection {
    pub(super) fn into_command_failure(self) -> backend_engine::CommandFailure {
        backend_engine::CommandFailure::InvalidQuery(match self {
            Self::Full => "search_preparation_over_capacity: reply queue is full; retry after current work completes",
            Self::Retiring => "search_preparation_retiring: retry after the previous native owner retires",
        }.to_owned())
    }
}

/// Only immutable selected evidence enters the native preparation thread.
pub(super) struct Selection {
    pub(super) workspace: WorkspaceRoot,
    pub(super) view: ViewRoot,
    pub(super) capability: CoverageCapability,
    pub(super) coverage: CoverageWitness,
    pub(super) corpus: SemanticQueryCorpus,
}

pub(super) struct Waiter {
    pub(super) ticket: u64,
    pub(super) request_id: u64,
    pub(super) query: Query,
    pub(super) certificate: Option<super::super::WireCertificate>,
}

pub(super) struct Completion {
    pub(super) projection: Option<SearchSnapshotOwner>,
    pub(super) result: Result<(), Failure>,
    pub(super) waiters: Vec<Waiter>,
    pub(super) current: bool,
}

struct Active {
    workspace: WorkspaceRoot,
    view: backend_library::ViewStateRoot,
    cancelled: Arc<AtomicBool>,
    waiters: Vec<Waiter>,
    completed: mpsc::Receiver<(SearchSnapshotOwner, Result<(), Failure>)>,
    worker: JoinHandle<()>,
}

#[cfg(test)]
struct Gate {
    entered: mpsc::SyncSender<()>,
    release: mpsc::Receiver<()>,
}

#[derive(Default)]
pub(super) struct SearchLane {
    active: Option<Active>,
    closed: bool,
    #[cfg(test)]
    next_gate: Option<Gate>,
}

impl SearchLane {
    /// Registers another caller for precisely the active preparation. No
    /// corpus is captured for a duplicate, stale, or over-capacity caller.
    pub(super) fn share(
        &mut self,
        workspace: WorkspaceRoot,
        view: backend_library::ViewStateRoot,
        waiter: Waiter,
    ) -> Result<(), (Rejection, Waiter)> {
        let Some(active) = self.active.as_mut() else {
            return Err((Rejection::Retiring, waiter));
        };
        if active.workspace != workspace || active.view != view {
            active.cancelled.store(true, Ordering::Release);
            return Err((Rejection::Retiring, waiter));
        }
        if self.closed || active.cancelled.load(Ordering::Acquire) {
            return Err((Rejection::Retiring, waiter));
        }
        if active.waiters.len() >= MAX_SEARCH_WAITERS {
            return Err((Rejection::Full, waiter));
        }
        active.waiters.push(waiter);
        Ok(())
    }

    pub(super) fn active(&self) -> bool {
        self.active.is_some()
    }

    pub(super) fn start(
        &mut self,
        mut projection: SearchSnapshotOwner,
        selection: Selection,
        waiter: Waiter,
    ) -> Result<(), String> {
        if self.closed || self.active.is_some() {
            return Err("search preparation owner is unavailable; retry".to_owned());
        }
        let workspace = selection.workspace;
        let view = selection.view.root();
        let cancelled = Arc::new(AtomicBool::new(false));
        let control = Arc::clone(&cancelled);
        let (sender, completed) = mpsc::sync_channel(1);
        #[cfg(test)]
        let gate = self.next_gate.take();
        let worker = thread::Builder::new()
            .name("locald-search-prepare".to_owned())
            .spawn(move || {
                #[cfg(test)]
                if let Some(gate) = gate {
                    let _ = gate.entered.send(());
                    let _ = gate.release.recv();
                }
                let result = catch_unwind(AssertUnwindSafe(|| {
                    if control.load(Ordering::Acquire) {
                        return Err(Failure::Cancelled);
                    }
                    let prepared = projection
                        .select_controlled(
                            selection.workspace,
                            selection.view,
                            selection.capability,
                            selection.coverage,
                            selection.corpus,
                            Some(&control),
                        )
                        .map(|_| ());
                    if control.load(Ordering::Acquire) {
                        return Err(Failure::Cancelled);
                    }
                    prepared.map_err(Failure::Preparation)?;
                    Ok(())
                }))
                .unwrap_or_else(|_| Err(Failure::Panicked));
                // At most one completion, containing the same exclusive owner.
                let _ = sender.send((projection, result));
            })
            .map_err(|error| format!("start search preparation: {error}"))?;
        self.active = Some(Active {
            workspace,
            view,
            cancelled,
            waiters: vec![waiter],
            completed,
            worker,
        });
        Ok(())
    }

    /// Reject stale callers immediately, while the uninterruptible portion of
    /// a cancelled native commit may still be retiring on the one worker.
    pub(super) fn invalidate(
        &mut self,
        workspace: WorkspaceRoot,
        view: backend_library::ViewStateRoot,
    ) -> Vec<Waiter> {
        let Some(active) = self.active.as_mut() else {
            return Vec::new();
        };
        if active.workspace == workspace && active.view == view {
            return Vec::new();
        }
        active.cancelled.store(true, Ordering::Release);
        std::mem::take(&mut active.waiters)
    }

    pub(super) fn drain(
        &mut self,
        workspace: WorkspaceRoot,
        view: backend_library::ViewStateRoot,
    ) -> Option<Completion> {
        let active = self.active.as_ref()?;
        // A receipt arrives before the worker's last instruction. Only join
        // after kernel completion; owner polling never waits for native work.
        if !active.worker.is_finished() {
            return None;
        }
        let active = self.active.take()?;
        let current = !self.closed
            && !active.cancelled.load(Ordering::Acquire)
            && active.workspace == workspace
            && active.view == view;
        let received = active.completed.try_recv();
        let _ = active.worker.join();
        let (projection, result) = match received {
            Ok((projection, result)) => (Some(projection), result),
            Err(_) => (None, Err(Failure::Stopped)),
        };
        Some(Completion {
            projection,
            result,
            waiters: active.waiters,
            current,
        })
    }

    pub(super) fn abandon_reply(&mut self, ticket: u64) {
        if let Some(active) = self.active.as_mut() {
            active.waiters.retain(|waiter| waiter.ticket != ticket);
            if active.waiters.is_empty() {
                active.cancelled.store(true, Ordering::Release);
            }
        }
    }

    /// Retirement is joined. Shutdown may wait for a native commit already in
    /// progress; the active control loop and cancellation never perform it.
    pub(super) fn close(&mut self) {
        self.closed = true;
        if let Some(active) = self.active.take() {
            active.cancelled.store(true, Ordering::Release);
            drop(active.completed);
            let _ = active.worker.join();
        }
    }

    #[cfg(test)]
    pub(super) fn hold_next(&mut self) -> (mpsc::Receiver<()>, mpsc::SyncSender<()>) {
        let (entered, receipt) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        self.next_gate = Some(Gate {
            entered,
            release: gate,
        });
        (receipt, release)
    }
}

impl Drop for SearchLane {
    fn drop(&mut self) {
        self.close();
    }
}
