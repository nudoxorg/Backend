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
        Arc, Mutex,
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
    fn answer(
        mut self,
        local: LocalAnswer,
        admitted: &AdmittedLocalQueryRecipe,
    ) -> (SemanticAnswer, Option<Self>) {
        let Some(answer) = &self.answer else {
            // A failed owned attempt is delivered once, never a negative cache hit forever.
            return (SemanticAnswer::lexical(local, self.failure), None);
        };
        let bytes = answer
            ._memory
            .as_ref()
            .map_or(0, EmbeddingMemoryReservation::bytes);
        match admitted.runtime().memory_budget().reserve(bytes) {
            Ok(memory) => (
                SemanticAnswer {
                    result: answer.result.clone(),
                    status: answer.status,
                    _memory: Some(memory),
                },
                Some(self),
            ),
            // Cache retention/copy is optional. Transfer the exact owed allocation and
            // its original affine lease rather than require two simultaneous answers.
            Err(_) => (self.answer.take().expect("present answer"), None),
        }
    }
}

#[derive(Default)]
struct CompletionCache(Mutex<VecDeque<Completed>>);
impl std::fmt::Debug for CompletionCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompletionCache").finish_non_exhaustive()
    }
}
impl CompletionCache {
    fn take(&self, text: &str, query: &LocalQuery) -> Option<Completed> {
        let mut entries = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        let at = entries
            .iter()
            .position(|done| done.key.text == text && &done.key.query == query)?;
        entries.remove(at)
    }
    fn insert(&self, completed: Completed, policy: QueryCompletionPolicy) {
        let mut entries = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        entries.push_back(completed);
        trim_completed(&mut entries, policy);
    }
    fn clear(&self) {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clear();
    }
    fn trim(&self, policy: QueryCompletionPolicy) {
        trim_completed(
            &mut self.0.lock().unwrap_or_else(|poison| poison.into_inner()),
            policy,
        );
    }
}
impl backend_compile::EmbeddingMemoryReclaimer for CompletionCache {
    fn reclaim(&self, shortage: u64) -> bool {
        let mut dropped = false;
        let mut released = 0u64;
        while released < shortage {
            let oldest = {
                let Ok(mut entries) = self.0.try_lock() else {
                    break;
                };
                entries.pop_front()
            };
            let Some(oldest) = oldest else {
                break;
            };
            released = released.saturating_add(oldest.retained_bytes());
            // Destruction and credit release run outside the cache lock. The serving
            // owner cannot wait for a worker to destroy canonical result allocations.
            drop(oldest);
            dropped = true;
        }
        dropped
    }
}

struct Worker {
    selection: Selection,
    key: Arc<QueryKey>,
    completion_owed: bool,
    completed: mpsc::Receiver<(LocalSemantic, Option<Completed>)>,
    worker: JoinHandle<()>,
}
enum Execution {
    Ready {
        core: LocalSemantic,
        selection: Option<Selection>,
        owed_completion: Option<Completed>,
    },
    Working(Worker),
    Closed,
}

/// Owns one selected local projection and at most one inference worker/completion.
pub struct LocalSemanticOwner {
    admitted: Arc<AdmittedLocalQueryRecipe>,
    execution: Execution,
    completion_policy: QueryCompletionPolicy,
    cache: Arc<CompletionCache>,
    #[cfg(test)]
    refuse_next_spawn: bool,
}
impl LocalSemanticOwner {
    pub(super) fn open(
        provision: &backend_engine::application::EmbeddingRuntimeProvision,
        path: &Path,
    ) -> Result<Self, LocalSemanticError> {
        let core = LocalSemantic::open(provision, path)?;
        let cache = Arc::new(CompletionCache::default());
        let reclaimer: Arc<dyn backend_compile::EmbeddingMemoryReclaimer> = cache.clone();
        core.admitted()
            .runtime()
            .memory_budget()
            .register_optional(&reclaimer)?;
        Ok(Self {
            admitted: core.admitted(),
            completion_policy: QueryCompletionPolicy {
                maximum_bytes: core.admitted().runtime().memory_budget().capacity(),
            },
            execution: Execution::Ready {
                core,
                selection: None,
                owed_completion: None,
            },
            cache,
            #[cfg(test)]
            refuse_next_spawn: false,
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
        self.cache.trim(self.completion_policy);
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
    /// The serving adapter supplies interest from its existing admitted live tickets.
    /// Abandonment changes delivery ownership only, never shared worker cancellation.
    pub(super) fn retain_owed_completion(
        &mut self,
        mut interested: impl FnMut(&QueryCoordinator, &str, &LocalQuery) -> bool,
    ) {
        match &mut self.execution {
            Execution::Working(worker) => {
                worker.completion_owed = interested(
                    &worker.selection.coordinator,
                    &worker.key.text,
                    &worker.key.query,
                );
            }
            Execution::Ready {
                selection,
                owed_completion,
                ..
            } => {
                let keep = selection
                    .as_ref()
                    .zip(owed_completion.as_ref())
                    .is_some_and(|(selection, done)| {
                        interested(&selection.coordinator, &done.key.text, &done.key.query)
                    });
                if !keep && let Some(done) = owed_completion.take() {
                    if done.answer.is_some() {
                        self.cache.insert(done, self.completion_policy);
                    }
                    // No waiter owns a failed attempt: release it, never negative-cache it.
                }
            }
            Execution::Closed => {}
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
                owed_completion,
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
                    *owed_completion = None;
                    self.cache.clear();
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
        if joined.is_err() || !core.has_projection() {
            self.cache.clear();
        }
        // The first completion belongs to the owed attempt, not optional retention.
        // Required compiler/query work cannot reclaim it before the reply projects it.
        let owed_completion = if worker.completion_owed {
            completion
        } else {
            if let Some(done) = completion
                && done.answer.is_some()
                && core.has_projection()
            {
                self.cache.insert(done, self.completion_policy);
            }
            None
        };
        // Recheck after joining, including cancellation after computation but before drain.
        let current = worker.selection.current(workspace, view);
        if !current {
            core.refuse(cancelled_failure());
            self.cache.clear();
        }
        self.execution = Execution::Ready {
            core,
            selection: current.then_some(worker.selection),
            owed_completion: if current { owed_completion } else { None },
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
            self.cache.clear();
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
                    if let Some(done) = self.cache.take(text, &local.query) {
                        let (answer, retained) = done.answer(local, &self.admitted);
                        if let Some(retained) = retained {
                            self.cache.insert(retained, self.completion_policy);
                        }
                        return answer;
                    }
                } else {
                    worker.selection.cancelled.store(true, Ordering::Release);
                    self.cache.clear();
                }
                return SemanticAnswer::lexical(local, SemanticSearchReason::NoActiveProjection);
            }
            Execution::Closed => {
                return SemanticAnswer::lexical(local, SemanticSearchReason::NoActiveProjection);
            }
            Execution::Ready {
                selection,
                owed_completion,
                ..
            } => {
                if selection
                    .as_ref()
                    .is_some_and(|selected| selected.matches(coordinator, coverage, &cancelled))
                {
                    if owed_completion
                        .as_ref()
                        .is_some_and(|done| done.key.text == text && done.key.query == local.query)
                    {
                        let done = owed_completion.take().expect("matched owed completion");
                        let (answer, retained) = done.answer(local, &self.admitted);
                        if let Some(retained) = retained {
                            self.cache.insert(retained, self.completion_policy);
                        }
                        return answer;
                    }
                    // Changing query abandons the prior direct caller's receipt.
                    // The adapter serializes waiters and consumes it before advancing.
                    *owed_completion = None;
                    if let Some(done) = self.cache.take(text, &local.query) {
                        let (answer, retained) = done.answer(local, &self.admitted);
                        if let Some(retained) = retained {
                            self.cache.insert(retained, self.completion_policy);
                        }
                        return answer;
                    }
                } else {
                    self.cache.clear();
                }
                // A direct caller changing its query abandons the prior receipt;
                // adapter waiters are serialized and deliver that receipt before advancing.
                *owed_completion = None;
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
            let memory = budget.reserve(bytes).map_err(LocalSemanticError::Memory)?;
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
                    core.record_query_failure(error);
                    if !core.has_projection() {
                        self.cache.clear();
                    }
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
                    core.record_query_failure(error);
                    if !core.has_projection() {
                        self.cache.clear();
                    }
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
            core,
            selection: prior_selection,
            ..
        } = std::mem::replace(&mut self.execution, Execution::Closed)
        else {
            return SemanticAnswer::lexical(local, SemanticSearchReason::NoActiveProjection);
        };
        let (sender, completed) = mpsc::sync_channel(1);
        let worker_coordinator = coordinator.clone();
        let worker_key = Arc::clone(&key);
        // The closure owns only a handoff reference until an actual thread starts.
        // Builder::spawn drops its closure on OS refusal; the caller still owns the
        // original complete core and can reclaim it without rebuilding its projection.
        // No inference, allocation admission or callback runs while this lock is held.
        let handoff = Arc::new(Mutex::new(Some(core)));
        let worker_handoff = Arc::clone(&handoff);
        let worker = self.spawn_worker(move || {
            let mut core = worker_handoff
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .take()
                .expect("one admitted semantic worker handoff");
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
                    core.record_query_failure(error);
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
                    key,
                    completion_owed: true,
                    completed,
                    worker,
                })
            }
            Err(error) => {
                let mut core = handoff
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .take()
                    .expect("spawn refusal retains the unstarted semantic core");
                core.record_query_failure(LocalSemanticError::Worker(
                    LocalSemanticWorkerError::Spawn(error),
                ));
                if !core.has_projection() {
                    self.cache.clear();
                }
                self.execution = Execution::Ready {
                    core,
                    selection: prior_selection,
                    owed_completion: None,
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

    fn spawn_worker(
        &mut self,
        task: impl FnOnce() + Send + 'static,
    ) -> std::io::Result<JoinHandle<()>> {
        #[cfg(test)]
        if std::mem::take(&mut self.refuse_next_spawn) {
            return Err(std::io::Error::other(
                "injected owner thread admission refusal",
            ));
        }
        thread::Builder::new()
            .name("locald-semantic-query".to_owned())
            .spawn(task)
    }

    #[cfg(test)]
    pub(super) fn refuse_next_worker_spawn(&mut self) {
        self.refuse_next_spawn = true;
    }

    #[cfg(test)]
    pub(crate) fn owes_completion(&self) -> bool {
        matches!(
            &self.execution,
            Execution::Ready {
                owed_completion: Some(_),
                ..
            }
        )
    }

    #[cfg(test)]
    pub(crate) fn cached_completions(&self) -> usize {
        self.cache
            .0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .len()
    }

    #[cfg(test)]
    pub(crate) fn has_retained_projection(&self) -> bool {
        matches!(&self.execution, Execution::Ready { core, .. } if core.has_projection())
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
        self.cache.clear();
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
