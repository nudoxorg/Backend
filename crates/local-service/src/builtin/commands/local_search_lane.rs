//! One exact local declaration projection, built outside the control owner.
//! This lane never answers a partial registry query or carries query authority.
use super::super::ProductState;
use super::super::discovery_search::LocalDeclarationSearchIndex;
use backend_library::{CommandFailure, QueryPreparationState, ViewRoot, ViewStateRoot};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};

struct Active {
    root: ViewStateRoot,
    cancelled: Arc<AtomicBool>,
    completed: mpsc::Receiver<Result<LocalDeclarationSearchIndex, String>>,
    worker: JoinHandle<()>,
}

#[cfg(test)]
struct Gate {
    entered: mpsc::SyncSender<()>,
    release: mpsc::Receiver<()>,
}

#[derive(Default)]
pub(super) struct LocalSearchLane {
    active: Option<Active>,
    failure: Option<(ViewStateRoot, String)>,
    // A stale completed native writer retires on the next worker, not on poll.
    // At most one result is retained: only one build can be active.
    retired: Option<LocalDeclarationSearchIndex>,
    closed: bool,
    #[cfg(test)]
    next_gate: Option<Gate>,
}

impl LocalSearchLane {
    /// Polling cancels changed selections and joins only a finished thread.
    /// Installation rechecks the exact immutable view captured by the worker.
    pub(super) fn poll(&mut self, view: &ViewRoot, state: &mut ProductState) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        if self.closed || active.root != view.root() {
            active.cancelled.store(true, Ordering::Release);
        }
        if !active.worker.is_finished() {
            return;
        }
        let Some(active) = self.active.take() else {
            return;
        };
        let current =
            !self.closed && !active.cancelled.load(Ordering::Acquire) && active.root == view.root();
        let result = active.completed.try_recv();
        let _ = active.worker.join();
        if current {
            match result {
                Ok(Ok(index)) => {
                    if index.revision() == Some(view.root()) {
                        self.retired = Some(state.install_local_search(index));
                        self.failure = None;
                    } else {
                        self.retired = Some(index);
                        self.failure = Some((
                            active.root,
                            "local declaration preparation changed its exact target".to_owned(),
                        ));
                    }
                }
                Ok(Err(error)) => self.failure = Some((active.root, error)),
                _ => {
                    self.failure = Some((
                        active.root,
                        "local declaration preparation worker stopped without an exact result"
                            .to_owned(),
                    ))
                }
            }
        } else if let Ok(Ok(index)) = result {
            self.retired = Some(index);
        }
    }

    pub(super) fn prepare(
        &mut self,
        view: &ViewRoot,
        state: &mut ProductState,
    ) -> Option<CommandFailure> {
        self.poll(view, state);
        if self.closed {
            return Some(CommandFailure::IncoherentView(
                "local declaration preparation is closed".to_owned(),
            ));
        }
        if state.local_search_ready(view) {
            return None;
        }
        if let Some((root, error)) = self.failure.take()
            && root == view.root()
        {
            return Some(CommandFailure::IncoherentView(format!(
                "local_declaration_preparation_refused: {error}"
            )));
        }
        let preparation = if let Some(active) = &self.active {
            if active.cancelled.load(Ordering::Acquire) {
                QueryPreparationState::Retiring
            } else {
                QueryPreparationState::Preparing
            }
        } else {
            if let Err(error) = self.start(view.clone(), state) {
                return Some(CommandFailure::IncoherentView(error));
            }
            QueryPreparationState::Preparing
        };
        Some(CommandFailure::QueryPreparation {
            basis: view.root().into(),
            state: preparation,
        })
    }

    fn start(&mut self, view: ViewRoot, state: &mut ProductState) -> Result<(), String> {
        let root = view.root();
        let cancelled = Arc::new(AtomicBool::new(false));
        let control = Arc::clone(&cancelled);
        let (sender, completed) = mpsc::sync_channel(1);
        let (retire, retirement) = mpsc::channel::<(
            LocalDeclarationSearchIndex,
            Option<LocalDeclarationSearchIndex>,
        )>();
        #[cfg(test)]
        let gate = self.next_gate.take();
        let worker = thread::Builder::new()
            .name("locald-index-search-prepare".to_owned())
            .spawn(move || {
                // Native writer destruction may join merge threads. Receive
                // ownership only after spawn succeeds and retire it here.
                let Ok(previous) = retirement.recv() else {
                    return;
                };
                drop(previous);
                #[cfg(test)]
                if let Some(gate) = gate {
                    let _ = gate.entered.send(());
                    while !control.load(Ordering::Acquire) {
                        match gate
                            .release
                            .recv_timeout(std::time::Duration::from_millis(10))
                        {
                            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                        }
                    }
                }
                let result = catch_unwind(AssertUnwindSafe(|| {
                    LocalDeclarationSearchIndex::build_immutable_controlled(&view, Some(&control))
                }))
                .unwrap_or_else(|_| {
                    Err("local declaration preparation worker panicked".to_owned())
                });
                let _ = sender.send(result);
            })
            .map_err(|error| format!("start local declaration preparation: {error}"))?;
        let previous = (state.take_local_search(), self.retired.take());
        // The receiver's first instruction waits for this one bounded transfer.
        let _ = retire.send(previous);
        self.active = Some(Active {
            root,
            cancelled,
            completed,
            worker,
        });
        Ok(())
    }

    /// A disconnected request owns no cancellation authority over shared work.
    /// Only selection invalidation and owner shutdown retire this projection.
    pub(super) fn request_close(&mut self) {
        self.closed = true;
        if let Some(active) = &self.active {
            active.cancelled.store(true, Ordering::Release);
        }
    }

    pub(super) fn close(&mut self) {
        self.request_close();
        if let Some(active) = self.active.take() {
            drop(active.completed);
            let _ = active.worker.join();
        }
        self.retired = None;
    }

    #[cfg(test)]
    pub(super) fn hold_next(&mut self) -> (mpsc::Receiver<()>, mpsc::SyncSender<()>) {
        let (entered, observed) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(1);
        self.next_gate = Some(Gate {
            entered,
            release: released,
        });
        (observed, release)
    }

    #[cfg(test)]
    pub(super) fn active_cancellation(&self) -> Option<Arc<AtomicBool>> {
        self.active
            .as_ref()
            .map(|active| Arc::clone(&active.cancelled))
    }
}

impl Drop for LocalSearchLane {
    fn drop(&mut self) {
        self.close();
    }
}
