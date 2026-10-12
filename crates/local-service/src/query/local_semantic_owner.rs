//! One exclusive semantic worker; the serving owner never waits for model inference.

use super::local_semantic::LocalSemanticWorkerError;
use super::{
    AdmittedLocalQueryRecipe, LocalAnswer, LocalQuery, LocalSemantic, LocalSemanticError,
    QueryCoordinator, QueryResult,
};
use backend_compile::EmbeddingMemoryReservation;
use backend_library::{
    QueryPreparationState, SemanticSearchReason, SemanticSearchStatus, ViewStateRoot,
};
use backend_version::CoverageWitness;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

/// A query answer whose scratch/output charge lives through actual page projection.
pub(super) struct SemanticAnswer {
    pub(super) result: QueryResult,
    pub(super) status: SemanticSearchStatus,
    pub(super) _memory: Option<EmbeddingMemoryReservation>,
}
impl SemanticAnswer {
    pub(super) fn lexical(local: LocalAnswer, reason: SemanticSearchReason) -> Self {
        Self::with_status(local, SemanticSearchStatus::Unavailable { reason })
    }
    fn with_status(local: LocalAnswer, status: SemanticSearchStatus) -> Self {
        Self {
            result: local.finish(),
            status,
            _memory: None,
        }
    }
}

struct QueryKey {
    text: String,
    query: LocalQuery,
    // Covers bounded dispatch text/descriptor clones until the final query-key owner drops.
    _memory: EmbeddingMemoryReservation,
}
struct Selection {
    coordinator: QueryCoordinator,
    coverage: CoverageWitness,
    cancelled: Arc<AtomicBool>,
}
impl Selection {
    fn current(&self, workspace: backend_engine::WorkspaceRoot, view: ViewStateRoot) -> bool {
        self.coordinator.workspace_root() == workspace
            && self.coordinator.corpus.view.root() == view
            && !self.cancelled.load(Ordering::Acquire)
    }
    fn matches(
        &self,
        coordinator: &QueryCoordinator,
        coverage: CoverageWitness,
        cancelled: &Arc<AtomicBool>,
    ) -> bool {
        Arc::ptr_eq(&self.coordinator.corpus, &coordinator.corpus)
            && self.coverage == coverage
            && Arc::ptr_eq(&self.cancelled, cancelled)
            && !self.cancelled.load(Ordering::Acquire)
    }
}
struct Completed {
    key: Arc<QueryKey>,
    answer: Option<SemanticAnswer>,
}
struct Worker {
    selection: Selection,
    completed: mpsc::Receiver<(LocalSemantic, Option<Completed>)>,
    worker: JoinHandle<()>,
}
enum Execution {
    Ready {
        core: LocalSemantic,
        selection: Option<Selection>,
        completed: Option<Completed>,
    },
    Working(Worker),
    Closed,
}

/// Owns one selected local projection and at most one inference worker/completion.
pub struct LocalSemanticOwner {
    admitted: Arc<AdmittedLocalQueryRecipe>,
    execution: Execution,
}
impl LocalSemanticOwner {
    pub(super) fn open(
        provision: &backend_engine::application::EmbeddingRuntimeProvision,
        path: &Path,
    ) -> Result<Self, LocalSemanticError> {
        let core = LocalSemantic::open(provision, path)?;
        Ok(Self {
            admitted: core.admitted(),
            execution: Execution::Ready {
                core,
                selection: None,
                completed: None,
            },
        })
    }
    pub(super) fn recipe(&self) -> backend_extension_qdrant::EmbeddingRecipe {
        self.admitted.recipe()
    }
    pub(super) fn failure(&self) -> Option<&LocalSemanticError> {
        match &self.execution {
            Execution::Ready { core, .. } => core.failure(),
            Execution::Working(_) | Execution::Closed => None,
        }
    }
    pub(super) fn inventory_status(&self) -> backend_engine::CapabilityStatus {
        match &self.execution {
            Execution::Ready { core, .. } => core.inventory_status(),
            // The complete owner has moved to a worker. Its prior query answer is not available.
            Execution::Working(_) | Execution::Closed => {
                LocalSemantic::from_admitted(Arc::clone(&self.admitted)).inventory_status()
            }
        }
    }
    /// Actual preparation state; a caller's disconnect does not set shared cancellation.
    #[must_use]
    pub fn preparation(&self) -> Option<QueryPreparationState> {
        match &self.execution {
            Execution::Working(worker) => {
                Some(if worker.selection.cancelled.load(Ordering::Acquire) {
                    QueryPreparationState::Retiring
                } else {
                    QueryPreparationState::Preparing
                })
            }
            Execution::Closed => Some(QueryPreparationState::Retiring),
            Execution::Ready { .. } => None,
        }
    }
    pub(super) fn poll(&mut self, workspace: backend_engine::WorkspaceRoot, view: ViewStateRoot) {
        match &mut self.execution {
            Execution::Working(worker) => {
                if !worker.selection.current(workspace, view) {
                    worker.selection.cancelled.store(true, Ordering::Release);
                }
                if !worker.worker.is_finished() {
                    return;
                }
            }
            Execution::Ready {
                core,
                selection,
                completed,
            } => {
                if selection
                    .as_ref()
                    .is_some_and(|selected| !selected.current(workspace, view))
                {
                    if let Some(selected) = selection.as_ref() {
                        selected.cancelled.store(true, Ordering::Release);
                    }
                    core.refuse(cancelled_failure());
                    *selection = None;
                    *completed = None;
                }
                return;
            }
            Execution::Closed => return,
        }
        let Execution::Working(worker) = std::mem::replace(&mut self.execution, Execution::Closed)
        else {
            return;
        };
        let received = worker.completed.try_recv();
        let joined = worker.worker.join(); // is_finished was checked; serving poll never waits.
        let (mut core, mut completed) = match received {
            Ok(receipt) if joined.is_ok() => receipt,
            _ => {
                let mut core = LocalSemantic::from_admitted(Arc::clone(&self.admitted));
                core.refuse(LocalSemanticError::Worker(if joined.is_err() {
                    LocalSemanticWorkerError::Panicked
                } else {
                    LocalSemanticWorkerError::Stopped
                }));
                (core, None)
            }
        };
        // Recheck after joining, including cancellation after computation but before drain.
        let current = worker.selection.current(workspace, view);
        if !current {
            core.refuse(cancelled_failure());
            completed = None;
        }
        self.execution = Execution::Ready {
            core,
            selection: current.then_some(worker.selection),
            completed,
        };
    }

    pub(super) fn search(
        &mut self,
        coordinator: &QueryCoordinator,
        coverage: CoverageWitness,
        mut local: LocalAnswer,
        text: &str,
        cancelled: Arc<AtomicBool>,
    ) -> SemanticAnswer {
        self.poll(coordinator.workspace_root(), coordinator.corpus.view.root());
        if cancelled.load(Ordering::Acquire) {
            if let Execution::Ready { core, .. } = &mut self.execution {
                core.refuse(cancelled_failure());
            }
            return SemanticAnswer::with_status(
                local,
                if matches!(self.execution, Execution::Working(_) | Execution::Closed) {
                    SemanticSearchStatus::Unavailable {
                        reason: SemanticSearchReason::NoActiveProjection,
                    }
                } else {
                    SemanticSearchStatus::Unavailable {
                        reason: SemanticSearchReason::ProviderUnavailable,
                    }
                },
            );
        }
        match &mut self.execution {
            Execution::Working(worker) => {
                if !worker.selection.matches(coordinator, coverage, &cancelled) {
                    worker.selection.cancelled.store(true, Ordering::Release);
                }
                return SemanticAnswer::with_status(
                    local,
                    SemanticSearchStatus::Unavailable {
                        reason: SemanticSearchReason::NoActiveProjection,
                    },
                );
            }
            Execution::Closed => {
                return SemanticAnswer::with_status(
                    local,
                    SemanticSearchStatus::Unavailable {
                        reason: SemanticSearchReason::NoActiveProjection,
                    },
                );
            }
            Execution::Ready {
                core,
                selection,
                completed,
                ..
            } => {
                if selection
                    .as_ref()
                    .is_some_and(|selected| selected.matches(coordinator, coverage, &cancelled))
                    && completed
                        .as_ref()
                        .is_some_and(|done| done.key.text == text && done.key.query == local.query)
                {
                    if let Some(completed) = completed.take() {
                        return completed.answer.unwrap_or_else(|| {
                            SemanticAnswer::lexical(
                                local,
                                core.failure().map_or(
                                    SemanticSearchReason::ProviderUnavailable,
                                    LocalSemanticError::reason,
                                ),
                            )
                        });
                    }
                }
                // A completed answer for another exact query is never applied to this request.
                *completed = None;
            }
        }
        let maximum = self.admitted.runtime().maximum_text_bytes();
        let admission = (|| {
            if text.len() > maximum {
                return Err(LocalSemanticError::Inference(
                    backend_compile::EmbeddingExecutableError::TextLimit {
                        observed: text.len(),
                        maximum,
                    },
                ));
            }
            let bytes = text
                .len()
                .checked_mul(32)
                .and_then(|bytes| bytes.checked_add(64 * 1024))
                .ok_or(LocalSemanticError::CorpusEnvelope)?;
            let memory = self
                .admitted
                .runtime()
                .memory_budget()
                .reserve(u64::try_from(bytes).map_err(|_| LocalSemanticError::CorpusEnvelope)?)
                .map_err(LocalSemanticError::Memory)?;
            let deadline = self
                .admitted
                .runtime()
                .inference_deadline()
                .map_err(LocalSemanticError::Inference)?;
            Ok((memory, deadline))
        })();
        let (memory, deadline) = match admission {
            Ok(admission) => admission,
            Err(error) => {
                let reason = error.reason();
                if let Execution::Ready { core, .. } = &mut self.execution {
                    core.refuse(error);
                }
                return SemanticAnswer::lexical(local, reason);
            }
        };
        let seed = match local
            .take_lexical_seed(self.admitted.runtime().memory_budget(), &mut || {
                super::local_semantic::checkpoint(&cancelled, deadline)
            }) {
            Ok(seed) => seed,
            Err(error) => {
                let reason = error.reason();
                if let Execution::Ready { core, .. } = &mut self.execution {
                    core.refuse(error);
                }
                return SemanticAnswer::lexical(local, reason);
            }
        };
        let key = Arc::new(QueryKey {
            text: text.to_owned(),
            query: local.query.clone(),
            _memory: memory,
        });
        let selection = Selection {
            coordinator: coordinator.clone(),
            coverage,
            cancelled: Arc::clone(&cancelled),
        };
        let Execution::Ready { mut core, .. } =
            std::mem::replace(&mut self.execution, Execution::Closed)
        else {
            return SemanticAnswer::lexical(local, SemanticSearchReason::NoActiveProjection);
        };
        let (sender, completed) = mpsc::sync_channel(1);
        let worker_coordinator = coordinator.clone();
        let worker_key = Arc::clone(&key);
        let worker = thread::Builder::new()
            .name("locald-semantic-query".to_owned())
            .spawn(move || {
                let result = core.execute_query(
                    &worker_coordinator,
                    coverage,
                    seed,
                    &worker_key.text,
                    &cancelled,
                    deadline,
                );
                let completed = match result {
                    Ok((result, memory)) => Some(Completed {
                        key: worker_key,
                        answer: Some(SemanticAnswer {
                            result,
                            status: SemanticSearchStatus::Available,
                            _memory: Some(memory),
                        }),
                    }),
                    Err(error) => {
                        core.refuse(error);
                        Some(Completed {
                            key: worker_key,
                            answer: None,
                        })
                    }
                };
                let _ = sender.send((core, completed));
            });
        match worker {
            Ok(worker) => {
                self.execution = Execution::Working(Worker {
                    selection,
                    completed,
                    worker,
                })
            }
            Err(error) => {
                let mut core = LocalSemantic::from_admitted(Arc::clone(&self.admitted));
                core.refuse(LocalSemanticError::Worker(LocalSemanticWorkerError::Spawn(
                    error,
                )));
                self.execution = Execution::Ready {
                    core,
                    selection: None,
                    completed: None,
                };
            }
        }
        match &self.execution {
            Execution::Working(_) => SemanticAnswer::with_status(
                local,
                SemanticSearchStatus::Unavailable {
                    reason: SemanticSearchReason::NoActiveProjection,
                },
            ),
            Execution::Ready { core, .. } => SemanticAnswer::lexical(
                local,
                core.failure().map_or(
                    SemanticSearchReason::ProviderUnavailable,
                    LocalSemanticError::reason,
                ),
            ),
            Execution::Closed => SemanticAnswer::with_status(
                local,
                SemanticSearchStatus::Unavailable {
                    reason: SemanticSearchReason::NoActiveProjection,
                },
            ),
        }
    }

    #[cfg(test)]
    pub(super) fn core(&self) -> Option<&LocalSemantic> {
        match &self.execution {
            Execution::Ready { core, .. } => Some(core),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(super) fn core_mut(&mut self) -> Option<&mut LocalSemantic> {
        match &mut self.execution {
            Execution::Ready { core, .. } => Some(core),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(super) fn admitted_runtime(&self) -> &Arc<backend_compile::EmbeddingExecutable> {
        self.admitted.runtime()
    }

    pub(super) fn close(&mut self) {
        if let Execution::Working(worker) =
            std::mem::replace(&mut self.execution, Execution::Closed)
        {
            worker.selection.cancelled.store(true, Ordering::Release);
            drop(worker.completed);
            let _ = worker.worker.join(); // Owned shutdown retirement; no active serving turn.
        }
    }
}
impl Drop for LocalSemanticOwner {
    fn drop(&mut self) {
        self.close();
    }
}

fn cancelled_failure() -> LocalSemanticError {
    LocalSemanticError::Inference(backend_compile::EmbeddingExecutableError::Process(
        backend_compile::ProcessError::Cancelled,
    ))
}
