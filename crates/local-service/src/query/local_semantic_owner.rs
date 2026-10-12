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
    collections::VecDeque,
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
/// Optional performance retention target in bytes. By default it is inherited from the
/// configured shared runtime credits. One newest owed completion remains deliverable even
/// when it exceeds a smaller retention target; its affine lease still obeys shared credits.
/// Eviction never refuses a corpus or query: an uncached query uses bounded inference/await.
#[derive(Clone, Copy, Debug)]
pub struct QueryCompletionPolicy {
    pub maximum_bytes: u64,
}
struct Completed {
    key: Arc<QueryKey>,
    answer: Option<SemanticAnswer>,
    failure: SemanticSearchReason,
}
impl Completed {
    fn retained_bytes(&self) -> u64 {
        self.key._memory.bytes().saturating_add(
            self.answer
                .as_ref()
                .and_then(|answer| answer._memory.as_ref())
                .map_or(0, EmbeddingMemoryReservation::bytes),
        )
    }
    fn answer(&self, local: LocalAnswer, admitted: &AdmittedLocalQueryRecipe) -> SemanticAnswer {
        let Some(answer) = &self.answer else {
            return SemanticAnswer::lexical(local, self.failure);
        };
        // The cached allocation keeps its affine lease. A separate reservation precedes
        // the deep canonical-row copy returned to the page projector.
        let bytes = answer
            ._memory
            .as_ref()
            .map_or(0, EmbeddingMemoryReservation::bytes);
        match admitted.runtime().memory_budget().reserve(bytes) {
            Ok(memory) => SemanticAnswer {
                result: answer.result.clone(),
                status: answer.status,
                _memory: Some(memory),
            },
            Err(_) => SemanticAnswer::lexical(local, SemanticSearchReason::ProviderUnavailable),
        }
    }
}
struct Worker {
    selection: Selection,
    completed: mpsc::Receiver<(LocalSemantic, Option<Completed>)>,
    worker: JoinHandle<()>,
    cached: VecDeque<Completed>,
}
enum Execution {
    Ready {
        core: LocalSemantic,
        selection: Option<Selection>,
        completed: VecDeque<Completed>,
    },
    Working(Worker),
    Closed,
}

/// Owns one selected local projection and at most one inference worker/completion.
pub struct LocalSemanticOwner {
    admitted: Arc<AdmittedLocalQueryRecipe>,
    execution: Execution,
    completion_policy: QueryCompletionPolicy,
}
impl LocalSemanticOwner {
    pub(super) fn open(
        provision: &backend_engine::application::EmbeddingRuntimeProvision,
        path: &Path,
    ) -> Result<Self, LocalSemanticError> {
        let core = LocalSemantic::open(provision, path)?;
        Ok(Self {
            admitted: core.admitted(),
            completion_policy: QueryCompletionPolicy {
                maximum_bytes: core.admitted().runtime().memory_budget().capacity(),
            },
            execution: Execution::Ready {
                core,
                selection: None,
                completed: VecDeque::new(),
            },
        })
    }
    pub(super) fn recipe(&self) -> backend_extension_qdrant::EmbeddingRecipe {
        self.admitted.recipe()
    }
    /// Exposes the retention setting independently of corpus or inference admission.
    pub fn completion_policy(&self) -> QueryCompletionPolicy {
        self.completion_policy
    }
    pub fn set_completion_policy(&mut self, policy: QueryCompletionPolicy) {
        self.completion_policy = QueryCompletionPolicy {
            maximum_bytes: policy
                .maximum_bytes
                .min(self.admitted.runtime().memory_budget().capacity()),
        };
        let cached = match &mut self.execution {
            Execution::Ready { completed, .. } => completed,
            Execution::Working(worker) => &mut worker.cached,
            Execution::Closed => return,
        };
        trim_completed(cached, self.completion_policy);
    }
    pub(super) fn admit_reply(
        &self,
        bytes: u64,
    ) -> Result<(EmbeddingMemoryReservation, std::time::Instant), LocalSemanticError> {
        let memory = self
            .admitted
            .runtime()
            .memory_budget()
            .reserve(bytes)
            .map_err(LocalSemanticError::Memory)?;
        let deadline = self
            .admitted
            .runtime()
            .inference_deadline()
            .map_err(LocalSemanticError::Inference)?;
        Ok((memory, deadline))
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
                    completed.clear();
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
        let (mut core, completion) = match received {
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
        let mut completed = worker.cached;
        if joined.is_err() {
            completed.clear();
        }
        if let Some(completion) = completion {
            completed.push_back(completion);
            trim_completed(&mut completed, self.completion_policy);
        }
        // Recheck after joining, including cancellation after computation but before drain.
        let current = worker.selection.current(workspace, view);
        if !current {
            core.refuse(cancelled_failure());
            completed.clear();
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
                if worker.selection.matches(coordinator, coverage, &cancelled) {
                    if let Some(at) = worker
                        .cached
                        .iter()
                        .position(|done| done.key.text == text && done.key.query == local.query)
                    {
                        if let Some(done) = worker.cached.remove(at) {
                            let answer = done.answer(local, &self.admitted);
                            worker.cached.push_back(done);
                            return answer;
                        }
                    }
                } else {
                    worker.selection.cancelled.store(true, Ordering::Release);
                    worker.cached.clear();
                }
                return SemanticAnswer::lexical(local, SemanticSearchReason::NoActiveProjection);
            }
            Execution::Closed => {
                return SemanticAnswer::lexical(local, SemanticSearchReason::NoActiveProjection);
            }
            Execution::Ready {
                selection,
                completed,
                ..
            } => {
                if selection
                    .as_ref()
                    .is_some_and(|selected| selected.matches(coordinator, coverage, &cancelled))
                {
                    if let Some(at) = completed
                        .iter()
                        .position(|done| done.key.text == text && done.key.query == local.query)
                    {
                        if let Some(done) = completed.remove(at) {
                            let answer = done.answer(local, &self.admitted);
                            completed.push_back(done);
                            return answer;
                        }
                    }
                } else {
                    completed.clear();
                }
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
            let budget = self.admitted.runtime().memory_budget().clone();
            let bytes = u64::try_from(bytes).map_err(|_| LocalSemanticError::CorpusEnvelope)?;
            let memory = loop {
                match budget.reserve(bytes) {
                    Ok(memory) => break memory,
                    Err(error) => {
                        // Cached results are an optimization. Evict oldest completions before
                        // refusing a new query against the configured shared credit envelope.
                        let evicted = match &mut self.execution {
                            Execution::Ready { completed, .. } => completed.pop_front().is_some(),
                            _ => false,
                        };
                        if !evicted {
                            return Err(LocalSemanticError::Memory(error));
                        }
                    }
                }
            };
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
        let Execution::Ready {
            mut core,
            completed: cached,
            ..
        } = std::mem::replace(&mut self.execution, Execution::Closed)
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
                        failure: SemanticSearchReason::ProviderUnavailable,
                    }),
                    Err(error) => {
                        let failure = error.reason();
                        core.refuse(error);
                        Some(Completed {
                            key: worker_key,
                            answer: None,
                            failure,
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
                    cached,
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
                    completed: cached,
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

// LRU order is oldest at the front, updated on successful lookup. Keep the newest
// completion deliverable for the owed request; all storage remains affine-credit bounded.
fn trim_completed(completed: &mut VecDeque<Completed>, policy: QueryCompletionPolicy) {
    while completed.len() > 1
        && completed.iter().fold(0u64, |bytes, entry| {
            bytes.saturating_add(entry.retained_bytes())
        }) > policy.maximum_bytes
    {
        completed.pop_front();
    }
}
