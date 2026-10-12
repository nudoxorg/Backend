//! One exclusive projection owner crosses the control loop through bounded
//! messages. Callers observe readiness without retaining an owed RPC.
//! A different selection is refused until the active worker retires;
//! it never queues another corpus or starts another native writer.

#[cfg(any(test, feature = "cluster-process-journey-hooks"))]
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

/// A validated renewal borrows the exact selected lifetime until ownership handoff.
/// Dropping it preserves the original selection; commit cannot allocate or refuse.
pub(super) struct SelectedLifetimeRenewal<'a> {
    selected: &'a mut Arc<AtomicBool>,
    replacement: Arc<AtomicBool>,
}
impl SelectedLifetimeRenewal<'_> {
    pub(super) fn commit(self) {
        let retired = std::mem::replace(self.selected, self.replacement);
        retired.store(true, Ordering::Release);
    }
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
    /// Prepare retirement without cancelling the semantic lifetime or changing the admitted
    /// lexical projection. This is lawful only for the current successful selection,
    /// never an active/unprepared/superseded view. No corpus preparation is repeated.
    pub(super) fn prepare_selected_lifetime_renewal(
        &mut self,
        projection: &SearchSnapshotOwner,
        workspace: WorkspaceRoot,
        view: &ViewRoot,
    ) -> Result<SelectedLifetimeRenewal<'_>, super::super::query::QueryError> {
        if self.closed
            || self.active.is_some()
            || projection.selected_for(workspace, view).is_none()
        {
            return Err(super::super::query::QueryError::InvalidView);
        }
        let Some((selected_workspace, selected_view, retired)) = self.selected.as_mut() else {
            return Err(super::super::query::QueryError::InvalidView);
        };
        if *selected_workspace != workspace
            || *selected_view != view.root()
            || retired.load(Ordering::Acquire)
        {
            return Err(super::super::query::QueryError::InvalidView);
        }
        Ok(SelectedLifetimeRenewal {
            selected: retired,
            replacement: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Unit fixture for an already genuinely kernel-admitted selection, not owner-startup proof.
    #[cfg(test)]
    pub(super) fn fixture_for_admitted_selection(
        projection: &SearchSnapshotOwner,
        workspace: WorkspaceRoot,
        view: &ViewRoot,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, super::super::query::QueryError> {
        if projection.selected_for(workspace, view).is_none() || cancelled.load(Ordering::Acquire) {
            return Err(super::super::query::QueryError::InvalidView);
        }
        Ok(Self {
            selected: Some((workspace, view.root(), cancelled)),
            ..Self::default()
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
        #[cfg(feature = "cluster-process-journey-hooks")]
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
                    #[cfg(feature = "cluster-process-journey-hooks")]
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
                            let read = || super::super::read_indexed_sources(&snapshot);
                            #[cfg(feature = "cluster-process-journey-hooks")]
                            {
                                diagnostic::measure(source_phase, read)
                            }
                            #[cfg(not(feature = "cluster-process-journey-hooks"))]
                            {
                                read()
                            }
                        },
                        |sources| {
                            let admit = || {
                                super::super::view_build::semantic_query_corpus(
                                    &snapshot,
                                    &capture.compiler,
                                    &sources,
                                    &mut generations,
                                    &mut image_rows,
                                )
                            };
                            #[cfg(feature = "cluster-process-journey-hooks")]
                            {
                                diagnostic::measure(corpus_phase, admit)
                            }
                            #[cfg(not(feature = "cluster-process-journey-hooks"))]
                            {
                                let mut admit = admit;
                                admit()
                            }
                        },
                    );
                    #[cfg(feature = "cluster-process-journey-hooks")]
                    if let Some(observation) = diagnostic.as_mut() {
                        observation.generation_after =
                            Some(diagnostic::Generations::capture(&generations));
                        observation.corpus = corpus.as_ref().ok().map(diagnostic::Corpus::capture);
                    }
                    let corpus = corpus.map_err(Failure::Capture)?;
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
                    let prepare = || {
                        projection
                            .select_controlled(
                                selection.workspace,
                                selection.view,
                                selection.capability,
                                selection.coverage,
                                selection.corpus,
                                Some(&control),
                            )
                            .map(|selected| {
                                #[cfg(feature = "cluster-process-journey-hooks")]
                                {
                                    selected.preparation_diagnostic_binding()
                                }
                                #[cfg(not(feature = "cluster-process-journey-hooks"))]
                                {
                                    let _ = selected;
                                }
                            })
                    };
                    #[cfg(feature = "cluster-process-journey-hooks")]
                    let prepared = diagnostic::measure(
                        diagnostic
                            .as_mut()
                            .map(|observation| &mut observation.phases.lexical_projection),
                        prepare,
                    )
                    .map(|binding| {
                        if let Some(observation) = diagnostic.as_mut() {
                            observation.lexical_binding =
                                Some(diagnostic::LexicalBinding::capture(binding));
                        }
                    });
                    #[cfg(not(feature = "cluster-process-journey-hooks"))]
                    let prepared = prepare();
                    if control.load(Ordering::Acquire) {
                        return Err(Failure::Cancelled);
                    }
                    prepared.map_err(Failure::Preparation)?;
                    Ok(())
                }))
                .unwrap_or_else(|_| Err(Failure::Panicked));
                #[cfg(feature = "cluster-process-journey-hooks")]
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

    /// Retirement is joined. Shutdown may wait for a native commit already in
    /// progress; the active control loop and cancellation never perform it.
    pub(super) fn close(&mut self) {
        self.closed = true;
        if let Some((_, _, cancelled)) = self.selected.take() {
            cancelled.store(true, Ordering::Release);
        }
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

#[cfg(all(test, unix))]
mod refresh_tests {
    use super::*;
    use crate::builtin::query::RemoteSemantic;
    use std::time::{Duration, Instant};
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn selected_recipe_refresh_retires_actual_worker_without_revoking_lexical_selection()
    -> TestResult {
        let fixture =
            crate::builtin::query::local_semantic::tests::fixture_with_mode(4096, "gate-second")?;
        let (projection, library, view) =
            crate::builtin::query::local_semantic::tests::selected_owner_many(300)?;
        let workspace = crate::builtin::genesis()
            .map_err(|error| error.to_string())?
            .root();
        let coordinator = projection
            .selected_for(workspace, &view)
            .ok_or("selected projection")?;
        let old = Arc::new(AtomicBool::new(false));
        // Represents the exact successful SearchLane completion; the projection itself
        // is genuinely selected by the normal kernel, not a root-only fabricated owner.
        let mut lane = SearchLane {
            selected: Some((workspace, view.root(), Arc::clone(&old))),
            ..SearchLane::default()
        };
        let mut provider = RemoteSemantic::select_owner(
            &fixture.provision,
            &fixture.descriptor,
            RemoteSemantic::Unconfigured,
        );
        let runtime = fixture.provision.runtime().ok_or("fixture runtime")?;
        let account = runtime.memory_budget().clone();
        let baseline = account.used();
        let request =
            backend_engine::Query::new("fixture", view.root(), backend_engine::QueryLimit::new(3)?);
        let expected = crate::builtin::query::search_page(
            coordinator,
            &library,
            &mut RemoteSemantic::Unconfigured,
            crate::builtin::admitted_coverage()?,
            &request,
        )
        .map_err(|error| error.to_string())?
        .0;
        let first = crate::builtin::query::search_page_with_cancellation(
            coordinator,
            &library,
            &mut provider,
            crate::builtin::admitted_coverage()?,
            &request,
            Arc::clone(&old),
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(first.0, expected);
        let deadline = Instant::now() + Duration::from_secs(8);
        while !fixture.entered.exists() {
            if Instant::now() >= deadline {
                return Err("actual semantic gate not entered".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let builds = projection.projection_builds();
        lane.prepare_selected_lifetime_renewal(&projection, workspace, &view)?
            .commit();
        assert!(old.load(Ordering::Acquire));
        let current = lane
            .selected_cancellation(workspace, view.root())
            .ok_or("renewed selected lifetime")?;
        assert!(!Arc::ptr_eq(&old, &current));
        let mut retired = provider.take_local_for_retirement().ok_or("local owner")?;
        let worker = std::thread::spawn(move || {
            retired.close_local();
        });
        let during = crate::builtin::query::search_page_with_cancellation(
            coordinator,
            &library,
            &mut provider,
            crate::builtin::admitted_coverage()?,
            &request,
            Arc::clone(&current),
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(during.0, expected);
        assert_eq!(
            during.1,
            backend_library::SemanticSearchStatus::Unavailable {
                reason: backend_library::SemanticSearchReason::NoActiveProjection
            }
        );
        worker.join().map_err(|_| "retirement worker panic")?;
        assert_eq!(
            account.used(),
            baseline,
            "real query/cache/projection credits joined before model retirement"
        );
        assert!(
            !fixture.release.exists(),
            "actual child cancelled, gate not manually released"
        );
        assert!(!current.load(Ordering::Acquire));
        assert_eq!(
            projection.projection_builds(),
            builds,
            "no lexical reprojection"
        );
        let absent = SearchSnapshotOwner::default();
        assert!(
            lane.prepare_selected_lifetime_renewal(&absent, workspace, &view)
                .is_err()
        );
        assert!(Arc::ptr_eq(
            &current,
            &lane
                .selected_cancellation(workspace, view.root())
                .ok_or("still current")?
        ));
        let (_, _, replacement) =
            crate::builtin::query::local_semantic::tests::selected_owner_many(301)?;
        assert_ne!(replacement.root(), view.root());
        lane.invalidate(workspace, replacement.root());
        assert!(current.load(Ordering::Acquire));
        assert!(
            lane.prepare_selected_lifetime_renewal(&projection, workspace, &view)
                .is_err(),
            "cannot renew a superseded selection"
        );
        Ok(())
    }
}
