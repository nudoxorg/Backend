//! The serving loop only admits, hands off, and observes one exclusive refresh worker.

use super::super::embedding_refresh::{self, Event, Failure, Phase, Worker};
use super::{CommandAdapter, ProductDaemon};
use backend_engine::application::{LocalCompilerEmbeddingPhase, LocalCompilerEmbeddingReceipt};
use backend_library::{
    EmbeddingRefreshAction as Action, EmbeddingRefreshCompilerReceipt as CompilerReceipt,
    EmbeddingRefreshFailure as Refusal, EmbeddingRefreshFailureKind as Kind,
    EmbeddingRefreshObservation as Observation, EmbeddingRefreshPhase as WirePhase,
    EmbeddingRefreshQueryStatus as QueryStatus, EmbeddingRefreshTicket as Ticket, ProductText,
};
use std::{
    num::NonZeroU64,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

pub(super) struct Active {
    ticket: Ticket,
    phase: WirePhase,
    worker: Worker,
    compiler: Option<CompilerReceipt>,
    terminal: Option<Result<embedding_refresh::Committed, Failure>>,
    admission_refusal: Option<Refusal>,
}

/// Bound history to this owner lease; old tickets never authorize another owner's work.
#[derive(Default)]
pub(super) struct Owner {
    active: Option<Active>,
    receipts: std::collections::VecDeque<Retained>,
    next: u64,
}
struct Retained {
    observation: Observation,
    // Keep the owned original error chain alongside its bounded wire projection.
    _cause: Option<Failure>,
}
impl Owner {
    pub(super) fn active(&self) -> bool {
        self.active.is_some()
    }
    pub(super) fn close(&mut self) {
        // Shutdown is the only serving path permitted to join an unfinished worker.
        if let Some(active) = self.active.take() {
            active.worker.cancel();
            drop(active);
        }
    }
    fn retain(&mut self, observation: Observation, cause: Option<Failure>) {
        if self.receipts.len() == 8 {
            self.receipts.pop_front();
        }
        self.receipts.push_back(Retained {
            observation,
            _cause: cause,
        });
    }
}

/// The bounded queue accepts the original owner before the irreversible lifetime change.
/// A refusal restores that exact owner and drops a side-effect-free renewal. After acceptance,
/// commit is infallible and the worker owns retirement even if operation control later stops.
pub(super) fn handoff_original_owner(
    worker: &mut Worker,
    provider: &mut super::super::super::query::RemoteSemantic,
    renewal: Option<super::super::search_lane::SelectedLifetimeRenewal<'_>>,
) -> Result<(), Refusal> {
    let Some(original) = provider.take_local_for_retirement() else {
        return Err(refusal(
            Kind::Query,
            "Original local query owner is not available for its exact refresh handoff.",
        ));
    };
    if let Err(error) = worker.handoff(original) {
        let original = match error {
            std::sync::mpsc::TrySendError::Full(original)
            | std::sync::mpsc::TrySendError::Disconnected(original) => original,
        };
        *provider = original;
        return Err(refusal(
            Kind::Worker,
            "Spawned refresh worker refused its original owner handoff; original query owner and selected lifetime retained.",
        ));
    }
    if let Some(renewal) = renewal {
        renewal.commit();
    }
    Ok(())
}

fn refusal(kind: Kind, detail: impl Into<String>) -> Refusal {
    // Preserve a UTF-8 prefix within the product bound, rather than replacing an actual cause.
    let detail = detail.into();
    let end = detail
        .char_indices()
        .map(|(offset, _)| offset)
        .take_while(|offset| *offset <= backend_library::MAX_PRODUCT_TEXT_BYTES)
        .last()
        .unwrap_or(0);
    let detail = if detail.len() > backend_library::MAX_PRODUCT_TEXT_BYTES {
        &detail[..end]
    } else {
        &detail
    };
    Refusal {
        kind,
        detail: ProductText::new(detail).unwrap_or_else(|_| {
            ProductText::from_static("embedding refresh refused without a printable diagnostic")
        }),
    }
}
fn control_kind(error: &backend_compile::EmbeddingExecutableError) -> Option<Kind> {
    match error {
        backend_compile::EmbeddingExecutableError::Process(
            backend_compile::ProcessError::Cancelled,
        ) => Some(Kind::Cancelled),
        backend_compile::EmbeddingExecutableError::Process(
            backend_compile::ProcessError::Deadline,
        ) => Some(Kind::Deadline),
        _ => None,
    }
}
fn runtime_kind(error: &backend_engine::application::EmbeddingRuntimeError) -> Kind {
    use backend_engine::application::EmbeddingRuntimeError as E;
    match error {
        E::Activation(error) => control_kind(error).unwrap_or(Kind::Configuration),
        E::Process(backend_compile::ProcessError::Cancelled) => Kind::Cancelled,
        E::Process(backend_compile::ProcessError::Deadline) => Kind::Deadline,
        _ => Kind::Configuration,
    }
}
fn compiler_kind(
    error: &backend_engine::application::LocalCompilerEmbeddingTransitionError,
) -> Kind {
    use backend_engine::application::LocalCompilerEmbeddingTransitionError as E;
    match error {
        E::WorkspaceOwner(_) => Kind::Owner,
        E::Busy => Kind::Busy,
        E::Activation(error) => control_kind(error).unwrap_or(Kind::Compiler),
        E::Configuration(error) => runtime_kind(error),
        _ => Kind::Compiler,
    }
}
fn failure(error: &Failure) -> Refusal {
    let kind = match error {
        Failure::NotConfigured => Kind::NotConfigured,
        Failure::ConfigurationChanged { .. } => Kind::Configuration,
        Failure::Owner(_) => Kind::Owner,
        Failure::Capture(error) => runtime_kind(error),
        Failure::Query(super::super::super::query::LocalSemanticError::Inference(error)) => {
            control_kind(error).unwrap_or(Kind::Query)
        }
        Failure::Query(super::super::super::query::LocalSemanticError::OwnerRefreshRefusal(
            cause,
        )) => cause.kind,
        Failure::Query(_) => Kind::Query,
        Failure::Activation(error) => match error.reason() {
            backend_engine::application::EmbeddingUnavailableReason::ActivationCancelled => {
                Kind::Cancelled
            }
            backend_engine::application::EmbeddingUnavailableReason::ActivationDeadline => {
                Kind::Deadline
            }
            _ => Kind::Activation,
        },
        Failure::Compiler(error) => compiler_kind(error),
        Failure::Control(error) => control_kind(error).unwrap_or(Kind::Activation),
        Failure::Stopped | Failure::Panicked => Kind::Worker,
    };
    refusal(kind, error.to_string())
}
fn compiler_receipt(receipt: LocalCompilerEmbeddingReceipt) -> Option<CompilerReceipt> {
    match receipt.phase {
        LocalCompilerEmbeddingPhase::Retired { .. } => Some(CompilerReceipt::Retired {
            configuration: receipt.configuration.as_bytes(),
        }),
        LocalCompilerEmbeddingPhase::Activated { execution } => Some(CompilerReceipt::Activated {
            configuration: receipt.configuration.as_bytes(),
            execution: execution.into(),
        }),
        // This worker never admits ResumeUnavailable; do not invent activation from it.
        LocalCompilerEmbeddingPhase::Unavailable { .. } => None,
    }
}
fn exact_owner(daemon: &ProductDaemon, ticket: &Ticket) -> Result<(), Refusal> {
    let owner = daemon.engine().daemon().owner().lease();
    if owner.epoch() != ticket.owner_epoch || owner.fence() != ticket.owner_fence {
        return Err(refusal(
            Kind::Owner,
            "Accepted refresh belongs to a replaced owner lease; no query replacement was installed.",
        ));
    }
    owner
        .assert_current()
        .map_err(|error| refusal(Kind::Owner, error.to_string()))
}
fn wire_phase(phase: Phase) -> WirePhase {
    match phase {
        Phase::Capturing => WirePhase::Capturing,
        Phase::Prepared => WirePhase::Prepared,
        Phase::RetiringQuery => WirePhase::RetiringQuery,
        Phase::RetiringCompiler => WirePhase::RetiringCompiler,
        Phase::Activating => WirePhase::Activating,
        Phase::Committing => WirePhase::Committing,
    }
}

impl CommandAdapter {
    fn record_query_refresh_refusal(&mut self, cause: Refusal) -> Refusal {
        match self.remote_semantic.finish_local_refusal(
            super::super::super::query::LocalSemanticError::OwnerRefreshRefusal(cause.clone()),
        ) {
            Ok(()) => cause,
            Err(error) => refusal(
                cause.kind,
                format!(
                    "{}; query owner refused its unavailable transition: {error}",
                    cause.detail.as_str()
                ),
            ),
        }
    }
    fn refresh_quiescence(&self) -> Result<(), Refusal> {
        if !self.compiler.is_ready() {
            return Err(refusal(
                Kind::Compiler,
                "Actual compiler actor stopped before refresh handoff.",
            ));
        }
        if self.indexing.is_some() || self.search_lane.active() || self.owner_cluster.is_some() {
            return Err(refusal(
                Kind::Busy,
                "Indexing, queued mutations, query preparation or cluster ownership is active; observe that operation before deliberately retrying embedding-refresh.",
            ));
        }
        match self.index_operations.has_prepared() {
            Ok(false) => Ok(()),
            Ok(true) => Err(refusal(
                Kind::Busy,
                "A prepared publication is unresolved; recover its exact operation before embedding-refresh.",
            )),
            Err(error) => Err(refusal(
                Kind::Busy,
                format!("Cannot prove publication quiescence: {error}"),
            )),
        }
    }

    pub(super) fn embedding_refresh(
        &mut self,
        daemon: &ProductDaemon,
        request: Action,
    ) -> Observation {
        if let Action::Cancel { ticket } = &request {
            self.cancel_embedding_refresh(ticket);
        }
        match request {
            Action::Observe { ticket: requested } | Action::Cancel { ticket: requested } => {
                if let Some(active) = &self.embedding_refresh.active
                    && active.ticket == requested
                {
                    return Observation::Pending {
                        ticket: requested,
                        phase: active.phase,
                    };
                }
                self.embedding_refresh
                    .receipts
                    .iter()
                    .find(|retained| retained.observation.ticket() == Some(&requested))
                    .map(|retained| retained.observation.clone())
                    .unwrap_or(Observation::Unknown { ticket: requested })
            }
            Action::Start {
                configuration,
                timeout_seconds,
            } => {
                self.start_embedding_refresh(daemon, configuration, timeout_seconds, Worker::start)
            }
        }
    }

    // The single admission implementation accepts the actual thread factory. Tests inject
    // only its real io::Error boundary; they cannot manufacture an activated Worker.
    pub(super) fn start_embedding_refresh(
        &mut self,
        daemon: &ProductDaemon,
        configuration: [u8; 32],
        timeout_seconds: u16,
        spawn: impl FnOnce(embedding_refresh::Input) -> std::io::Result<Worker>,
    ) -> Observation {
        if let Err(error) = daemon.engine().daemon().owner().lease().assert_current() {
            return Observation::Refused {
                cause: refusal(Kind::Owner, error.to_string()),
            };
        }
        if let Some(active) = &self.embedding_refresh.active {
            if let Err(cause) = exact_owner(daemon, &active.ticket) {
                return Observation::Refused { cause };
            }
            if active.ticket.configuration == configuration {
                return Observation::Pending {
                    ticket: active.ticket.clone(),
                    phase: active.phase,
                };
            }
            return Observation::Refused {
                cause: refusal(
                    Kind::Busy,
                    "Another exact embedding configuration has an accepted refresh; observe or cancel its issued ticket first.",
                ),
            };
        }
        if let Some(observation) = self.embedding_refresh.receipts.back()
            && matches!(&observation.observation, Observation::Committed { ticket, query: QueryStatus::Admitted, .. } if ticket.configuration == configuration)
        {
            return observation.observation.clone();
        }
        if !self.waiting.is_empty() {
            return Observation::Refused {
                cause: refusal(
                    Kind::Busy,
                    "Already admitted mutations are queued; observe them before deliberately retrying embedding-refresh.",
                ),
            };
        }
        if let Err(cause) = self.refresh_quiescence() {
            return Observation::Refused { cause };
        }
        if !self.remote_semantic.can_refresh_local() {
            return Observation::Refused {
                cause: refusal(
                    Kind::ExplicitRemote,
                    "Explicit remote semantic configuration remains authoritative; local embedding-refresh cannot replace it.",
                ),
            };
        }
        let owner = daemon.engine().daemon().owner().lease().clone();
        if let Err(error) = owner.assert_current() {
            return Observation::Refused {
                cause: refusal(Kind::Owner, error.to_string()),
            };
        }
        let timeout = match backend_engine::application::LocalCompilerTimeout::new(
            Duration::from_secs(u64::from(timeout_seconds)),
        ) {
            Ok(timeout) => timeout,
            Err(error) => {
                return Observation::Refused {
                    cause: refusal(Kind::Deadline, error.to_string()),
                };
            }
        };
        let Some(deadline) = Instant::now().checked_add(*timeout) else {
            return Observation::Refused {
                cause: refusal(
                    Kind::Deadline,
                    "Owner operation deadline cannot be represented.",
                ),
            };
        };
        let Some(sequence) = self
            .embedding_refresh
            .next
            .checked_add(1)
            .and_then(NonZeroU64::new)
        else {
            return Observation::Refused {
                cause: refusal(
                    Kind::Worker,
                    "Owner refresh sequence is exhausted; no operation was accepted.",
                ),
            };
        };
        let root = match self.product_state.workspace_path() {
            Ok(root) => root.join("compiler"),
            Err(error) => {
                return Observation::Refused {
                    cause: refusal(Kind::Configuration, error),
                };
            }
        };
        let issued = Ticket {
            sequence,
            owner_epoch: owner.epoch(),
            owner_fence: owner.fence(),
            configuration,
        };
        let admission = match self.compiler.admit_embedding_refresh(owner.clone()) {
            Ok(admission) => admission,
            Err(error) => {
                return Observation::Refused {
                    cause: refusal(compiler_kind(&error), error.to_string()),
                };
            }
        };
        let input = embedding_refresh::Input {
            compiler: self.compiler.clone(),
            admission,
            owner,
            configuration: root.join("embedding.config"),
            descriptor: root.join("embedding-query.config"),
            expected: configuration,
            cache_directory: daemon
                .engine()
                .daemon()
                .owner()
                .open_embedding_cache_directory()
                .ok(),
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline,
        };
        let worker = match spawn(input) {
            Ok(worker) => worker,
            Err(error) => {
                return Observation::Refused {
                    cause: refusal(
                        Kind::Worker,
                        format!(
                            "Refresh worker creation failed before any old owner was moved: {error}"
                        ),
                    ),
                };
            }
        };
        self.embedding_refresh.next = sequence.get();
        self.embedding_refresh.active = Some(Active {
            ticket: issued.clone(),
            phase: WirePhase::Capturing,
            worker,
            compiler: None,
            terminal: None,
            admission_refusal: None,
        });
        Observation::Pending {
            ticket: issued,
            phase: WirePhase::Capturing,
        }
    }

    pub(super) fn cancel_embedding_refresh(&mut self, requested: &Ticket) {
        if let Some(active) = &self.embedding_refresh.active
            && &active.ticket == requested
        {
            active.worker.cancel();
        }
    }

    pub(super) fn poll_embedding_refresh(
        &mut self,
        daemon: &mut ProductDaemon,
    ) -> Vec<(u64, Result<Vec<u8>, super::BuiltinModelError>)> {
        let Some(mut active) = self.embedding_refresh.active.take() else {
            return Vec::new();
        };
        let mut ready = Vec::new();
        while let Ok(event) = active.worker.events.try_recv() {
            match event {
                Event::Prepared(identity) => {
                    active.phase = WirePhase::Prepared;
                    if active.worker.finished() {
                        continue;
                    }
                    let admission = active
                        .worker
                        .check_handoff()
                        .map_err(|error| {
                            refusal(
                                control_kind(&error).unwrap_or(Kind::Activation),
                                error.to_string(),
                            )
                        })
                        .and_then(|()| exact_owner(daemon, &active.ticket))
                        .and_then(|()| self.refresh_quiescence());
                    let admission = admission.and_then(|()| {
                        if identity.as_bytes() != active.ticket.configuration {
                            return Err(refusal(
                                Kind::Configuration,
                                "Prepared configuration differs from accepted ticket.",
                            ));
                        }
                        active.worker.check_handoff().map_err(|error| {
                            refusal(
                                control_kind(&error).unwrap_or(Kind::Activation),
                                error.to_string(),
                            )
                        })?;
                        let workspace = daemon.engine().daemon().owner().snapshot().root();
                        let view = daemon.engine().daemon().library().view();
                        let renewal = if self
                            .search_snapshots
                            .selected_for(workspace, view)
                            .is_some()
                        {
                            Some(
                                self.search_lane
                                    .prepare_selected_lifetime_renewal(
                                        &self.search_snapshots,
                                        workspace,
                                        view,
                                    )
                                    .map_err(|error| refusal(Kind::Query, error.to_string()))?,
                            )
                        } else {
                            None
                        };
                        handoff_original_owner(
                            &mut active.worker,
                            &mut self.remote_semantic,
                            renewal,
                        )?;
                        // Complete already owed requests from their current exact lexical view.
                        // No failed semantic projection or pending receipt is silently discarded.
                        while let Some(pending) = self.semantic_search_replies.pop_front() {
                            ready.push((pending.ticket, self.execute(daemon, &pending.body)));
                        }
                        Ok(())
                    });
                    if let Err(cause) = admission {
                        active.admission_refusal = Some(cause);
                        active.worker.cancel();
                    }
                }
                Event::Phase(phase) => active.phase = wire_phase(phase),
                Event::Retired(receipt) | Event::CompilerCommitted(receipt) => {
                    active.compiler = compiler_receipt(receipt)
                }
                Event::Terminal(result) => active.terminal = Some(result),
            }
        }
        if !active.worker.finished() {
            self.embedding_refresh.active = Some(active);
            return ready;
        }
        // Terminal send precedes thread exit. It may have happened after the first drain
        // returned Empty, so consume the final bounded channel before classifying loss.
        while let Ok(event) = active.worker.events.try_recv() {
            match event {
                Event::Terminal(result) => active.terminal = Some(result),
                Event::Retired(receipt) | Event::CompilerCommitted(receipt) => {
                    active.compiler = compiler_receipt(receipt)
                }
                Event::Phase(phase) => active.phase = wire_phase(phase),
                Event::Prepared(_) => active.phase = WirePhase::Prepared,
            }
        }
        let joined = active.worker.join_finished();
        let result = active
            .terminal
            .take()
            .unwrap_or_else(|| Err(Failure::Stopped));
        let result = match joined {
            Some(Err(error)) => Err(error),
            _ => result,
        };
        // If a panic happened after compiler commit but before its event, recover only
        // the actor's actual exact receipt. Never infer it from the worker phase.
        if active.phase == WirePhase::Committing
            && let Some(receipt) = self.compiler.embedding_generation_receipt()
            && receipt.configuration.as_bytes() == active.ticket.configuration
        {
            active.compiler = compiler_receipt(receipt);
        }
        let mut retained_cause = None;
        let observation = match result {
            Ok(committed) => {
                let query = match committed.query {
                    Ok(admitted) => match exact_owner(daemon, &active.ticket) {
                        Ok(()) => self.remote_semantic.install_local_admitted(admitted),
                        Err(cause) => Err(
                            super::super::super::query::LocalSemanticError::OwnerRefreshRefusal(
                                cause,
                            ),
                        ),
                    }
                    .map_err(Failure::Query),
                    Err(error) => Err(error),
                };
                let query = match query {
                    Ok(()) => QueryStatus::Admitted,
                    Err(error) => {
                        let cause = self.record_query_refresh_refusal(failure(&error));
                        retained_cause = Some(error);
                        QueryStatus::Unavailable { cause }
                    }
                };
                match committed.compiler.phase {
                    LocalCompilerEmbeddingPhase::Activated { execution } => {
                        Observation::Committed {
                            ticket: active.ticket,
                            execution: execution.into(),
                            query,
                        }
                    }
                    _ => Observation::Failed {
                        ticket: active.ticket,
                        cause: refusal(
                            Kind::Compiler,
                            "Worker returned no actual activated compiler receipt.",
                        ),
                        compiler: active.compiler,
                    },
                }
            }
            Err(error) => {
                let cause = active
                    .admission_refusal
                    .take()
                    .unwrap_or_else(|| failure(&error));
                let cause = if matches!(
                    self.remote_semantic,
                    super::super::super::query::RemoteSemantic::LocalRetiring
                ) {
                    self.record_query_refresh_refusal(cause)
                } else {
                    cause
                };
                retained_cause = Some(error);
                Observation::Failed {
                    ticket: active.ticket,
                    cause,
                    compiler: active.compiler,
                }
            }
        };
        self.embedding_refresh.retain(observation, retained_cause);
        ready
    }
}
