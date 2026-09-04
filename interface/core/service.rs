//! Defines service behavior for `interface-core`, whose purpose is to own the transport-independent application service and reply vocabulary.
//! This module owns the service invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The concrete, fixture-free application behavior owner.

use std::{
    future::Future,
    pin::Pin as TaskPin,
    task::{Context, Poll, Waker},
};

use compiler_registry::FullRegistry;
use heart_adaptive::{
    BundleFact, BundleState, CapabilityDomain, CapabilityKind, ContentId, ExecutionRequest,
    ExecutionTerminal, LatencyMicros, Pin, PlacementAction, PolicyDecision, PolicyInput,
    RemoteHealth, ResourceBudget, next_action,
};
use heart_observe::Probe;

use crate::{
    AdaptiveDisposition, ApplicationEvent, ApplicationInput, ApplicationOutcome, ApplicationReply,
    Capability, CapabilityHealth, CapabilityTransition, CompilerCapability, CompilerReadiness,
    CompilerRequest, CompilerTerminal, Diagnostic, DiagnosticCode, DiagnosticDetail,
    ExecutionReply, ExecutionState, GenerateRequest, InconsistentRecovery, InputText,
    MAX_REPLY_ROWS, MAX_SEMANTIC_TEXT_BYTES, OperationKey, ReplyBody, RetrievalCapability,
    RetrievalReadiness, RetrievalRequest, UnavailableCompiler, UnavailableRetrieval,
    execution::LocalCapabilityExecution,
};

/// One monomorphized service that owns the compiler and retrieval capability seams.
///
/// The default [`UnavailableCompiler`] keeps process and UI clients portable. A configured
/// specialization carries a concrete compiler capability without dynamic dispatch, global lookup,
/// or a heavy compiler dependency in this crate.
pub struct ApplicationService<Compiler = UnavailableCompiler, Retrieval = UnavailableRetrieval> {
    /// Concrete compiler capability owned by this monomorphized service specialization.
    pub compiler: Compiler,
    /// Concrete retrieval capability owned by this specialization.
    pub retrieval: Retrieval,
    active_bundle: Option<ActiveBundle>,
    execution: Option<ActiveExecution>,
    last_execution: Option<ExecutionState>,
    next_operation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ActiveBundle {
    pin: Pin,
    bundle: ContentId<CapabilityDomain>,
}

struct ActiveExecution {
    operation: OperationKey,
    pin: Pin,
    task: LocalCapabilityExecution,
}

impl<Compiler> ApplicationService<Compiler> {
    /// Creates a service specialized to one explicitly supplied compiler capability.
    #[must_use]
    pub const fn with_compiler(compiler: Compiler) -> Self {
        Self {
            compiler,
            retrieval: UnavailableRetrieval,
            active_bundle: None,
            execution: None,
            last_execution: None,
            next_operation: 1,
        }
    }
}

impl ApplicationService<UnavailableCompiler> {
    /// Creates a portable service with no fabricated compiler or publication capability.
    #[must_use]
    pub const fn new() -> Self {
        Self::with_compiler(UnavailableCompiler)
    }
}

impl<Compiler: CompilerCapability, Retrieval: RetrievalCapability>
    ApplicationService<Compiler, Retrieval>
{
    /// Creates a service specialized to one compiler and one retrieval capability.
    #[must_use]
    pub const fn with_capabilities(compiler: Compiler, retrieval: Retrieval) -> Self {
        Self {
            compiler,
            retrieval,
            active_bundle: None,
            execution: None,
            last_execution: None,
            next_operation: 1,
        }
    }

    /// Executes one request with disabled typed tracing.
    #[must_use]
    pub fn execute(&mut self, input: &ApplicationInput) -> ApplicationReply {
        let mut disabled = ();
        self.execute_observed(input, &mut disabled)
    }

    /// Executes one request and lazily emits one typed event only when the probe retains it.
    #[must_use]
    pub fn execute_observed<Observer>(
        &mut self,
        input: &ApplicationInput,
        probe: &mut Observer,
    ) -> ApplicationReply
    where
        Observer: Probe<ApplicationEvent>,
    {
        let reply = self.dispatch(input);
        probe.record_with(|| ApplicationEvent::from(&reply));
        reply
    }

    /// Drives one admitted execution from the caller's scheduler wake rather than a request
    /// polling loop.
    ///
    /// The execution future registers `context.waker()` before returning [`Poll::Pending`]. A UI
    /// or other in-process scheduler can therefore await the operation and publish its terminal
    /// reply without manufacturing `PollExecution` commands or using a timer. Process adapters
    /// retain the explicit command because their framed transports are request/response based.
    pub fn poll_admitted_execution(
        &mut self,
        correlation: crate::CorrelationId,
        operation: OperationKey,
        context: &mut Context<'_>,
    ) -> Poll<ApplicationReply> {
        let reply = self.poll_execution(correlation, operation, context.waker());
        if matches!(
            reply.outcome,
            ApplicationOutcome::Resolved(ReplyBody::Execution(ExecutionReply::Pending { .. }))
        ) {
            Poll::Pending
        } else {
            Poll::Ready(reply)
        }
    }

    fn dispatch(&mut self, input: &ApplicationInput) -> ApplicationReply {
        match input {
            ApplicationInput::Generate(request) => self.generate(request),
            ApplicationInput::SnapshotStatus {
                correlation,
                snapshot,
            }
            | ApplicationInput::Locality {
                correlation,
                snapshot,
            } => self.snapshot_facts(*correlation, snapshot),
            ApplicationInput::Search {
                correlation,
                snapshot,
                query,
                limit,
            } => self.search(*correlation, snapshot, query, *limit),
            ApplicationInput::RemoveIndex {
                correlation,
                snapshot,
            } => self.remove_index(*correlation, snapshot),
            ApplicationInput::Graph {
                correlation,
                snapshot,
                limit,
            } => Self::remote_unavailable(*correlation, *snapshot, *limit, Capability::Graph),
            ApplicationInput::Vector {
                correlation,
                snapshot,
                limit,
            } => Self::remote_unavailable(*correlation, *snapshot, *limit, Capability::Vector),
            ApplicationInput::Health { correlation } => self.health(*correlation),
            ApplicationInput::RecoverLocal {
                correlation,
                pin,
                bundle,
                budget,
            } => self.recover_local(*correlation, *pin, *bundle, *budget, RemoteHealth::Outage),
            ApplicationInput::RecoverInconsistent(recovery) => self.recover_inconsistent(*recovery),
            ApplicationInput::ReleaseLocal {
                correlation,
                pin,
                bundle,
                budget,
            } => self.release_local(*correlation, *pin, *bundle, *budget),
            ApplicationInput::PollExecution {
                correlation,
                operation,
            } => self.poll_execution(*correlation, *operation, Waker::noop()),
            ApplicationInput::Cancel {
                correlation,
                operation,
            } => self.cancel(*correlation, *operation),
        }
    }

    fn generate(&mut self, request: &GenerateRequest) -> ApplicationReply {
        let target = request.target;
        let language = target.profile.language();
        match FullRegistry.route(language, target.stage) {
            Ok(_route) => match self.compiler.generate(CompilerRequest {
                profile: target.profile,
                stage: target.stage,
                source: &request.source,
            }) {
                Ok(generated) => ApplicationReply {
                    correlation: target.correlation,
                    outcome: ApplicationOutcome::Resolved(ReplyBody::Generated(generated)),
                },
                Err(CompilerTerminal::Unavailable { .. }) => {
                    Self::dependency_unavailable(target.correlation, Capability::CompilerOutput)
                }
                Err(terminal) => Self::compiler_terminal(target.correlation, terminal),
            },
            Err(source) => Self::rejected(
                target.correlation,
                Diagnostic {
                    code: DiagnosticCode::UnsupportedCompilerStage,
                    detail: DiagnosticDetail::Frontend(source),
                },
            ),
        }
    }

    fn snapshot_facts(
        &self,
        correlation: crate::CorrelationId,
        snapshot: &InputText,
    ) -> ApplicationReply {
        if let Some(diagnostic) = Self::text_bound(*snapshot) {
            return Self::rejected(correlation, diagnostic);
        }
        if self.retrieval.readiness() == RetrievalReadiness::Unavailable {
            return Self::dependency_unavailable(correlation, Capability::Index);
        }
        match self.retrieval.snapshot_status(snapshot) {
            Ok(facts) => ApplicationReply {
                correlation,
                outcome: ApplicationOutcome::Resolved(ReplyBody::Snapshot(facts)),
            },
            Err(cause) => Self::retrieval_terminal(correlation, cause),
        }
    }

    fn search(
        &self,
        correlation: crate::CorrelationId,
        snapshot: &InputText,
        query: &InputText,
        limit: u8,
    ) -> ApplicationReply {
        if let Some(diagnostic) = Self::limit(limit) {
            return Self::rejected(correlation, diagnostic);
        }
        if let Some(diagnostic) = Self::text_bound(*snapshot) {
            return Self::rejected(correlation, diagnostic);
        }
        if let Some(diagnostic) = Self::text_bound(*query) {
            return Self::rejected(correlation, diagnostic);
        }
        if self.retrieval.readiness() == RetrievalReadiness::Unavailable {
            return Self::dependency_unavailable(correlation, Capability::Index);
        }
        match self.retrieval.search(RetrievalRequest {
            snapshot,
            query,
            limit,
        }) {
            Ok(rows) => ApplicationReply {
                correlation,
                outcome: ApplicationOutcome::Resolved(ReplyBody::Retrieval(rows)),
            },
            Err(cause) => Self::retrieval_terminal(correlation, cause),
        }
    }

    fn remove_index(
        &mut self,
        correlation: crate::CorrelationId,
        snapshot: &InputText,
    ) -> ApplicationReply {
        if let Some(diagnostic) = Self::text_bound(*snapshot) {
            return Self::rejected(correlation, diagnostic);
        }
        if self.retrieval.readiness() == RetrievalReadiness::Unavailable {
            return Self::dependency_unavailable(correlation, Capability::Index);
        }
        match self.retrieval.unload(snapshot) {
            Ok(receipt) => ApplicationReply {
                correlation,
                outcome: ApplicationOutcome::Resolved(ReplyBody::IndexRemoved(receipt)),
            },
            Err(cause) => Self::retrieval_terminal(correlation, cause),
        }
    }

    const fn retrieval_terminal(
        correlation: crate::CorrelationId,
        cause: crate::RetrievalCause,
    ) -> ApplicationReply {
        Self::rejected(
            correlation,
            Diagnostic {
                code: DiagnosticCode::RetrievalFailed,
                detail: DiagnosticDetail::Retrieval(cause),
            },
        )
    }

    fn remote_unavailable(
        correlation: crate::CorrelationId,
        snapshot: InputText,
        limit: u8,
        capability: Capability,
    ) -> ApplicationReply {
        if let Some(diagnostic) = Self::limit(limit) {
            return Self::rejected(correlation, diagnostic);
        }
        if let Some(diagnostic) = Self::text_bound(snapshot) {
            return Self::rejected(correlation, diagnostic);
        }
        Self::dependency_unavailable(correlation, capability)
    }

    const fn dependency_unavailable(
        correlation: crate::CorrelationId,
        capability: Capability,
    ) -> ApplicationReply {
        ApplicationReply {
            correlation,
            outcome: ApplicationOutcome::Resolved(ReplyBody::DependencyUnavailable { capability }),
        }
    }

    fn health(&self, correlation: crate::CorrelationId) -> ApplicationReply {
        let analyzer = if self.active_bundle.is_some() {
            CapabilityHealth::LocalReady(Capability::LocalAnalyzer)
        } else {
            CapabilityHealth::Unavailable(Capability::LocalAnalyzer)
        };
        let compiler = match self.compiler.readiness() {
            CompilerReadiness::Unavailable => {
                CapabilityHealth::Unavailable(Capability::CompilerOutput)
            }
            CompilerReadiness::Ready => CapabilityHealth::LocalReady(Capability::CompilerOutput),
        };
        ApplicationReply {
            correlation,
            outcome: ApplicationOutcome::Resolved(ReplyBody::Health([
                CapabilityHealth::LocalReady(Capability::CompilerRegistry),
                compiler,
                match self.retrieval.readiness() {
                    RetrievalReadiness::Ready => CapabilityHealth::LocalReady(Capability::Index),
                    RetrievalReadiness::Unavailable => {
                        CapabilityHealth::Unavailable(Capability::Index)
                    }
                },
                CapabilityHealth::Unavailable(Capability::Graph),
                CapabilityHealth::Unavailable(Capability::Vector),
                analyzer,
            ])),
        }
    }

    fn recover_local(
        &mut self,
        correlation: crate::CorrelationId,
        pin: Pin,
        bundle: ContentId<CapabilityDomain>,
        budget: ResourceBudget,
        remote_health: RemoteHealth,
    ) -> ApplicationReply {
        if let Some(execution) = &self.execution {
            return Self::rejected(
                correlation,
                Self::operation_unavailable(execution.operation),
            );
        }
        let state = if self.active_bundle == Some(ActiveBundle { pin, bundle }) {
            BundleState::ActiveIdle
        } else {
            BundleState::AvailableRequired
        };
        let bundles = [BundleFact {
            capability: CapabilityKind::Analyzer,
            bundle,
            state,
        }];
        let input = PolicyInput {
            pin,
            local: &[],
            remote: &[],
            demand: &[],
            bundles: &bundles,
            remote_health,
            budget,
        };
        self.adapt(correlation, pin, next_action(&input))
    }

    fn recover_inconsistent(&mut self, recovery: InconsistentRecovery) -> ApplicationReply {
        self.recover_local(
            recovery.correlation,
            recovery.expected,
            recovery.bundle,
            recovery.budget,
            RemoteHealth::Inconsistent {
                observed: recovery.observed,
            },
        )
    }

    fn release_local(
        &mut self,
        correlation: crate::CorrelationId,
        pin: Pin,
        bundle: ContentId<CapabilityDomain>,
        budget: ResourceBudget,
    ) -> ApplicationReply {
        if let Some(execution) = &self.execution {
            return Self::rejected(
                correlation,
                Self::operation_unavailable(execution.operation),
            );
        }
        let state = if self.active_bundle == Some(ActiveBundle { pin, bundle }) {
            BundleState::ActiveIdle
        } else {
            BundleState::AvailableIdle
        };
        let bundles = [BundleFact {
            capability: CapabilityKind::Analyzer,
            bundle,
            state,
        }];
        let input = PolicyInput {
            pin,
            local: &[],
            remote: &[],
            demand: &[],
            bundles: &bundles,
            remote_health: RemoteHealth::Healthy {
                observed: pin,
                latency: LatencyMicros::from(0),
            },
            budget,
        };
        self.adapt(correlation, pin, next_action(&input))
    }

    const fn adapt(
        &mut self,
        correlation: crate::CorrelationId,
        pin: Pin,
        decision: PolicyDecision,
    ) -> ApplicationReply {
        match decision {
            PolicyDecision::Act(PlacementAction::AcquireBundle { capability, bundle }) => self
                .start_execution(
                    correlation,
                    pin,
                    PlacementAction::AcquireBundle { capability, bundle },
                    CapabilityTransition::Acquire { capability, bundle },
                ),
            PolicyDecision::Act(PlacementAction::ReleaseBundle { capability, bundle }) => self
                .start_execution(
                    correlation,
                    pin,
                    PlacementAction::ReleaseBundle { capability, bundle },
                    CapabilityTransition::Release { capability, bundle },
                ),
            PolicyDecision::Act(PlacementAction::RetryRemote {
                pin,
                cause,
                retries_remaining,
            }) => ApplicationReply {
                correlation,
                outcome: ApplicationOutcome::Resolved(ReplyBody::Adaptive(
                    AdaptiveDisposition::RetryRemote {
                        pin,
                        cause,
                        retries_remaining,
                    },
                )),
            },
            PolicyDecision::Act(_unexecutable) => {
                Self::dependency_unavailable(correlation, Capability::Remote)
            }
            non_action => Self::adapt_non_action(correlation, non_action),
        }
    }

    const fn adapt_non_action(
        correlation: crate::CorrelationId,
        decision: PolicyDecision,
    ) -> ApplicationReply {
        match decision {
            PolicyDecision::NoAction => ApplicationReply {
                correlation,
                outcome: ApplicationOutcome::Resolved(ReplyBody::Adaptive(
                    AdaptiveDisposition::NoAction,
                )),
            },
            PolicyDecision::Overloaded(overload) => ApplicationReply {
                correlation,
                outcome: ApplicationOutcome::Resolved(ReplyBody::Adaptive(
                    AdaptiveDisposition::Overloaded(overload),
                )),
            },
            PolicyDecision::RecoveryExhausted { pin, cause } => ApplicationReply {
                correlation,
                outcome: ApplicationOutcome::Resolved(ReplyBody::Adaptive(
                    AdaptiveDisposition::RecoveryExhausted { pin, cause },
                )),
            },
            PolicyDecision::Rejected(source) => Self::rejected(
                correlation,
                Diagnostic {
                    code: DiagnosticCode::AdaptivePolicyRejected,
                    detail: DiagnosticDetail::Policy(source),
                },
            ),
            PolicyDecision::Act(_action) => {
                Self::dependency_unavailable(correlation, Capability::Remote)
            }
        }
    }

    const fn start_execution(
        &mut self,
        correlation: crate::CorrelationId,
        pin: Pin,
        action: PlacementAction,
        transition: CapabilityTransition,
    ) -> ApplicationReply {
        let operation = OperationKey(self.next_operation);
        self.next_operation = self.next_operation.saturating_add(1);
        self.last_execution = None;
        self.execution = Some(ActiveExecution {
            operation,
            pin,
            task: LocalCapabilityExecution::new(ExecutionRequest::new(action), transition),
        });
        ApplicationReply {
            correlation,
            outcome: ApplicationOutcome::Resolved(ReplyBody::ExecutionStarted {
                operation,
                transition,
            }),
        }
    }

    fn poll_execution(
        &mut self,
        correlation: crate::CorrelationId,
        operation: OperationKey,
        waker: &Waker,
    ) -> ApplicationReply {
        let Some(mut execution) = self.execution.take() else {
            return self.fused_or_rejected(correlation, operation);
        };
        if execution.operation != operation {
            self.execution = Some(execution);
            return Self::rejected(correlation, Self::operation_unavailable(operation));
        }
        let transition = execution.task.transition;
        let mut context = Context::from_waker(waker);
        match TaskPin::new(&mut execution.task).poll(&mut context) {
            Poll::Pending => {
                self.execution = Some(execution);
                Self::execution_reply(
                    correlation,
                    ExecutionState::Pending {
                        operation,
                        transition,
                    },
                )
            }
            Poll::Ready(Some(terminal)) => {
                let state =
                    self.apply_execution_terminal(operation, execution.pin, transition, terminal);
                self.last_execution = Some(state);
                Self::execution_reply(correlation, state)
            }
            Poll::Ready(None) => self.fused_or_rejected(correlation, operation),
        }
    }

    fn apply_execution_terminal(
        &mut self,
        operation: OperationKey,
        pin: Pin,
        transition: CapabilityTransition,
        terminal: ExecutionTerminal,
    ) -> ExecutionState {
        match terminal {
            ExecutionTerminal::Completed { request: _request } => {
                match transition {
                    CapabilityTransition::Acquire { bundle, .. } => {
                        self.active_bundle = Some(ActiveBundle { pin, bundle });
                    }
                    CapabilityTransition::Release { bundle, .. } => {
                        if self.active_bundle == Some(ActiveBundle { pin, bundle }) {
                            self.active_bundle = None;
                        }
                    }
                }
                ExecutionState::Completed {
                    operation,
                    transition,
                }
            }
            ExecutionTerminal::Cancelled { request: _request } => ExecutionState::Cancelled {
                operation,
                transition,
            },
            ExecutionTerminal::Failed {
                request: _request,
                phase,
            } => ExecutionState::Failed {
                operation,
                transition,
                phase,
            },
        }
    }

    fn cancel(
        &mut self,
        correlation: crate::CorrelationId,
        operation: OperationKey,
    ) -> ApplicationReply {
        let Some(execution) = self.execution.take() else {
            return Self::rejected(correlation, Self::operation_unavailable(operation));
        };
        if execution.operation != operation {
            self.execution = Some(execution);
            return Self::rejected(correlation, Self::operation_unavailable(operation));
        }
        let transition = execution.task.transition;
        let Some(terminal) = execution.task.cancel() else {
            return self.fused_or_rejected(correlation, operation);
        };
        let state = self.apply_execution_terminal(operation, execution.pin, transition, terminal);
        self.last_execution = Some(state);
        Self::execution_reply(correlation, state)
    }

    fn fused_or_rejected(
        &self,
        correlation: crate::CorrelationId,
        operation: OperationKey,
    ) -> ApplicationReply {
        match self.last_execution {
            Some(
                state @ (ExecutionState::Completed {
                    operation: observed,
                    ..
                }
                | ExecutionState::Cancelled {
                    operation: observed,
                    ..
                }
                | ExecutionState::Failed {
                    operation: observed,
                    ..
                }),
            ) if observed == operation => Self::execution_reply(correlation, state),
            _ => Self::rejected(correlation, Self::operation_unavailable(operation)),
        }
    }

    const fn execution_reply(
        correlation: crate::CorrelationId,
        state: ExecutionState,
    ) -> ApplicationReply {
        match state {
            ExecutionState::Pending {
                operation,
                transition,
            } => ApplicationReply {
                correlation,
                outcome: ApplicationOutcome::Resolved(ReplyBody::Execution(
                    ExecutionReply::Pending {
                        operation,
                        transition,
                    },
                )),
            },
            ExecutionState::Completed {
                operation,
                transition,
            } => ApplicationReply {
                correlation,
                outcome: ApplicationOutcome::Resolved(ReplyBody::Execution(
                    ExecutionReply::Completed {
                        operation,
                        transition,
                    },
                )),
            },
            ExecutionState::Cancelled {
                operation,
                transition,
            } => ApplicationReply {
                correlation,
                outcome: ApplicationOutcome::Resolved(ReplyBody::Execution(
                    ExecutionReply::Cancelled {
                        operation,
                        transition,
                    },
                )),
            },
            state @ ExecutionState::Failed { .. } => Self::rejected(
                correlation,
                Diagnostic {
                    code: DiagnosticCode::ExecutionFailed,
                    detail: DiagnosticDetail::Execution(state),
                },
            ),
        }
    }

    fn text_bound(value: InputText) -> Option<Diagnostic> {
        (value.len() > MAX_SEMANTIC_TEXT_BYTES).then(|| Diagnostic {
            code: DiagnosticCode::SemanticTextTooLong,
            detail: DiagnosticDetail::TextLength {
                actual: value.len(),
                maximum: MAX_SEMANTIC_TEXT_BYTES,
                rejected: value,
            },
        })
    }

    #[allow(
        clippy::if_then_some_else_none,
        reason = "the stable compiler cannot evaluate bool::then_some in this required const terminal constructor"
    )]
    const fn limit(requested: u8) -> Option<Diagnostic> {
        if requested > MAX_REPLY_ROWS {
            Some(Diagnostic {
                code: DiagnosticCode::ResultLimitExceeded,
                detail: DiagnosticDetail::Limit {
                    requested,
                    maximum: MAX_REPLY_ROWS,
                },
            })
        } else {
            None
        }
    }

    const fn operation_unavailable(operation: OperationKey) -> Diagnostic {
        Diagnostic {
            code: DiagnosticCode::OperationUnavailable,
            detail: DiagnosticDetail::Operation(operation),
        }
    }

    const fn rejected(
        correlation: crate::CorrelationId,
        diagnostic: Diagnostic,
    ) -> ApplicationReply {
        ApplicationReply {
            correlation,
            outcome: ApplicationOutcome::Failed { diagnostic },
        }
    }

    const fn compiler_terminal(
        correlation: crate::CorrelationId,
        terminal: CompilerTerminal,
    ) -> ApplicationReply {
        Self::rejected(
            correlation,
            Diagnostic {
                code: DiagnosticCode::CompilerTerminal,
                detail: DiagnosticDetail::Compiler(terminal),
            },
        )
    }
}

impl Default for ApplicationService<UnavailableCompiler> {
    fn default() -> Self {
        Self::new()
    }
}
