//! One exclusive projection owner crosses the control loop through bounded
//! messages. Callers observe readiness without retaining an owed RPC.
//! A different selection is refused until the active worker retires;
//! it never queues another corpus or starts another native writer.

mod diagnostic;

use super::super::query::SearchSnapshotOwner;
use backend_engine::{CoverageCapability, ViewRoot, WorkspaceRoot};
use backend_extension_trustfall::SemanticQueryCorpus;
use backend_version::CoverageWitness;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};

pub(super) enum Failure {
    Preparation(super::super::query::QueryError),
    Capture(super::super::BuiltinModelError),
    Cancelled,
    Panicked,
    Stopped,
}

impl Failure {
    pub(super) fn into_command_failure(self) -> backend_engine::CommandFailure {
        match self {
            Self::Preparation(error) => backend_engine::CommandFailure::IncoherentView(format!(
                "search_preparation_refused: {error}"
            )),
            Self::Capture(error) => backend_engine::CommandFailure::IncoherentView(format!(
                "search_corpus_capture_refused: {error}"
            )),
            Self::Cancelled => backend_engine::CommandFailure::InvalidQuery(
                "search_preparation_cancelled: retry against the current view".to_owned(),
            ),
            Self::Panicked => backend_engine::CommandFailure::IncoherentView(
                "search_preparation_worker_panicked: retry".to_owned(),
            ),
            Self::Stopped => backend_engine::CommandFailure::IncoherentView(
                "search_preparation_worker_stopped: retry".to_owned(),
            ),
        }
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

/// Constant-time immutable capture; all relation walks and image activation
/// happen on the preparation worker against this checked workspace guard.
pub(super) struct Capture {
    pub(super) snapshot: backend_engine::WorkspaceSnapshot,
    pub(super) view: ViewRoot,
    pub(super) compiler: backend_engine::application::LocalCompilerClient,
    pub(super) generations: super::super::generation_residence::SemanticGenerationResidence,
    // Already-admitted private workspace; observational output only. No
    // diagnostic destination is accepted from an environment variable.
    pub(super) diagnostic_workspace: Option<std::path::PathBuf>,
}

pub(super) struct Completion {
    pub(super) projection: Option<SearchSnapshotOwner>,
    pub(super) result: Result<(), Failure>,
    pub(super) current: bool,
}

struct Active {
    workspace: WorkspaceRoot,
    view: backend_library::ViewStateRoot,
    cancelled: Arc<AtomicBool>,
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
    selected: Option<(
        WorkspaceRoot,
        backend_library::ViewStateRoot,
        Arc<AtomicBool>,
    )>,
    closed: bool,
    #[cfg(test)]
    next_gate: Option<Gate>,
}

impl SearchLane {
    /// Borrows the actual successful preparation lifetime. A caller's
    /// disconnect cannot cancel this shared selected-owner lifetime.
    pub(super) fn selected_cancellation(
        &self,
        workspace: WorkspaceRoot,
        view: backend_library::ViewStateRoot,
    ) -> Option<Arc<AtomicBool>> {
        self.selected
            .as_ref()
            .and_then(|(selected_workspace, selected_view, cancelled)| {
                (*selected_workspace == workspace
                    && *selected_view == view
                    && !cancelled.load(Ordering::Acquire))
                .then(|| Arc::clone(cancelled))
            })
    }
    /// A request observes this one worker; it never waits on its completion.
    pub(super) fn state_for(
        &mut self,
        workspace: WorkspaceRoot,
        view: backend_library::ViewStateRoot,
    ) -> backend_library::QueryPreparationState {
        let Some(active) = self.active.as_ref() else {
            return backend_library::QueryPreparationState::Retiring;
        };
        if active.workspace != workspace || active.view != view {
            active.cancelled.store(true, Ordering::Release);
        }
        if self.closed || active.cancelled.load(Ordering::Acquire) {
            backend_library::QueryPreparationState::Retiring
        } else {
            backend_library::QueryPreparationState::Preparing
        }
    }

    pub(super) fn active(&self) -> bool {
        self.active.is_some()
    }

    pub(super) fn start(
        &mut self,
        mut projection: SearchSnapshotOwner,
        capture: Capture,
    ) -> Result<(), String> {
        if self.closed || self.active.is_some() {
            return Err("search preparation owner is unavailable; retry".to_owned());
        }
        let workspace = capture.snapshot.root();
        let view = capture.view.root();
        let cancelled = Arc::new(AtomicBool::new(false));
        let control = Arc::clone(&cancelled);
        let (sender, completed) = mpsc::sync_channel(1);
        let mut diagnostic = diagnostic::Diagnostic::from_environment(&capture, &projection);
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
                    let snapshot = capture.snapshot;
                    let capability = super::super::builtin_view_capability_for_workspace(&snapshot)
                        .map_err(Failure::Capture)?;
                    let coverage = super::super::admitted_coverage().map_err(Failure::Capture)?;
                    let mut generations = capture.generations;
                    let mut image_rows = super::super::view_build::ImageRowResidence::default();
                    let (source_phase, corpus_phase) = match diagnostic.as_mut() {
                        Some(observation) => (
                            Some(&mut observation.phases.indexed_sources),
                            Some(&mut observation.phases.semantic_corpus),
                        ),
                        None => (None, None),
                    };
                    let corpus = projection.admit_corpus(
                        snapshot.root(),
                        || {
                            let read = || {
                                super::super::read_indexed_sources_controlled(
                                    &snapshot,
                                    Some(&control),
                                )
                            };
                            diagnostic::measure(source_phase, read)
                        },
                        |sources| {
                            let admit = || {
                                super::super::view_build::semantic_query_corpus_controlled(
                                    &snapshot,
                                    &capture.compiler,
                                    &sources,
                                    &mut generations,
                                    &mut image_rows,
                                    Some(&control),
                                )
                            };
                            diagnostic::measure(corpus_phase, admit)
                        },
                    );
                    if let Some(observation) = diagnostic.as_mut() {
                        observation.generation_after =
                            Some(diagnostic::Generations::capture(&generations));
                        observation.corpus = corpus.as_ref().ok().map(diagnostic::Corpus::capture);
                    }
                    let corpus = corpus.map_err(|error| {
                        if control.load(Ordering::Acquire) {
                            Failure::Cancelled
                        } else {
                            Failure::Capture(error)
                        }
                    })?;
                    if control.load(Ordering::Acquire) {
                        return Err(Failure::Cancelled);
                    }
                    let selection = Selection {
                        workspace: snapshot.root(),
                        view: capture.view,
                        capability,
                        coverage,
                        corpus,
                    };
                    let mut selection_diagnostic = diagnostic
                        .as_ref()
                        .map(|_| super::super::query::SelectionPreparationDiagnostic::default());
                    let prepare = || {
                        projection
                            .select_controlled_observed(
                                selection.workspace,
                                selection.view,
                                selection.capability,
                                selection.coverage,
                                selection.corpus,
                                Some(&control),
                                selection_diagnostic.as_mut(),
                            )
                            .map(|selected| selected.preparation_diagnostic_binding())
                    };
                    let selected = diagnostic::measure(
                        diagnostic
                            .as_mut()
                            .map(|observation| &mut observation.phases.lexical_projection),
                        prepare,
                    );
                    if let Some(observation) = diagnostic.as_mut() {
                        observation.selection =
                            selection_diagnostic.map(diagnostic::SelectionTrace::capture);
                    }
                    let prepared = selected.map(|binding| {
                        if let Some(observation) = diagnostic.as_mut() {
                            observation.lexical_binding =
                                Some(diagnostic::LexicalBinding::capture(binding));
                        }
                    });
                    if control.load(Ordering::Acquire) {
                        return Err(Failure::Cancelled);
                    }
                    prepared.map_err(Failure::Preparation)?;
                    Ok(())
                }))
                .unwrap_or_else(|_| Err(Failure::Panicked));
                if let Some(observation) = diagnostic.as_ref() {
                    observation.emit(&projection, &result);
                }
                // At most one completion, containing the same exclusive owner.
                let _ = sender.send((projection, result));
            })
            .map_err(|error| format!("start search preparation: {error}"))?;
        self.active = Some(Active {
            workspace,
            view,
            cancelled,
            completed,
            worker,
        });
        Ok(())
    }

    /// Cancel superseded work without blocking the control loop on retirement.
    pub(super) fn invalidate(
        &mut self,
        workspace: WorkspaceRoot,
        view: backend_library::ViewStateRoot,
    ) {
        if let Some((selected_workspace, selected_view, cancelled)) = &self.selected
            && (*selected_workspace != workspace || *selected_view != view)
        {
            cancelled.store(true, Ordering::Release);
        }
        if let Some(active) = self.active.as_ref() {
            if active.workspace != workspace || active.view != view {
                active.cancelled.store(true, Ordering::Release);
            }
        }
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
        if current && result.is_ok() {
            if let Some((_, _, cancelled)) = &self.selected {
                cancelled.store(true, Ordering::Release);
            }
            self.selected = Some((workspace, view, active.cancelled));
        }
        Some(Completion {
            projection,
            result,
            current,
        })
    }

    /// Signal retirement before joining any other owner lane. Native work can
    /// observe cancellation while those independent workers are being joined.
    pub(super) fn request_close(&mut self) {
        self.closed = true;
        if let Some((_, _, cancelled)) = &self.selected {
            cancelled.store(true, Ordering::Release);
        }
        if let Some(active) = &self.active {
            active.cancelled.store(true, Ordering::Release);
        }
    }

    /// Retirement is joined. Shutdown may wait for a native commit already in
    /// progress; the active control loop and cancellation never perform it.
    pub(super) fn close(&mut self) {
        self.request_close();
        drop(self.selected.take());
        if let Some(active) = self.active.take() {
            drop(active.completed);
            let _ = active.worker.join();
        }
    }

    #[cfg(test)]
    pub(super) fn active_cancellation(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.active.as_ref().expect("active preparation").cancelled)
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
