//! The concrete, fixture-free application behavior owner.

use std::{future::Future, pin::Pin as TaskPin, task::Context};

use nudox_adaptive::{
    BundleFact, BundleState, CapabilityDomain, CapabilityKind, ContentId, ExecutionRequest,
    ExecutionTerminal, LatencyMicros, Pin, PlacementAction, PolicyDecision, PolicyInput,
    RemoteHealth, ResourceBudget, next_action,
};
use nudox_compile_registry::FullRegistry;
use nudox_compile_vocab::{Language, Stage};
use nudox_observe::Probe;

use crate::{
    AdaptiveDisposition, ApplicationEvent, ApplicationInput, ApplicationReply, Capability,
    CapabilityHealth, CapabilityTransition, Diagnostic, DiagnosticCode, DiagnosticDetail,
    ExecutionState, InputText, MAX_REPLY_ROWS, MAX_SEMANTIC_TEXT_BYTES, OperationKey, ReplyBody,
    Terminal, execution::LocalCapabilityExecution,
};

/// One concrete service that owns at most one bounded adaptive effect.
///
/// The compiler registry is an identity passthrough today and is reported as such. Immutable
/// index, graph, and vector providers have no accepted production seam, so their commands return
/// typed degraded dependency facts. C6 policy selection and its local bundle execution are real.
pub struct ApplicationService {
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

impl ApplicationService {
    /// Creates an empty service with no fabricated lower-plane facts.
    #[must_use]
    pub const fn new() -> Self {
        Self {
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
        probe.record_with(|| ApplicationEvent {
            correlation: reply.correlation,
            terminal: reply.terminal,
        });
        reply
    }

    fn dispatch(&mut self, input: &ApplicationInput) -> ApplicationReply {
        match *input {
            ApplicationInput::Generate {
                correlation,
                language,
                stage,
                package,
                source,
            } => Self::generate(correlation, language, stage, package, source),
            ApplicationInput::SnapshotStatus {
                correlation,
                snapshot,
            }
            | ApplicationInput::Locality {
                correlation,
                snapshot,
            } => Self::index_unavailable(correlation, snapshot),
            ApplicationInput::Search {
                correlation,
                snapshot,
                query,
                limit,
            } => Self::search_unavailable(correlation, snapshot, query, limit),
            ApplicationInput::Graph {
                correlation,
                snapshot,
                limit,
            } => Self::remote_unavailable(correlation, snapshot, limit, Capability::Graph),
            ApplicationInput::Vector {
                correlation,
                snapshot,
                limit,
            } => Self::remote_unavailable(correlation, snapshot, limit, Capability::Vector),
            ApplicationInput::Health { correlation } => self.health(correlation),
            ApplicationInput::RecoverLocal {
                correlation,
                pin,
                bundle,
                budget,
            } => self.recover_local(correlation, pin, bundle, budget),
            ApplicationInput::ReleaseLocal {
                correlation,
                pin,
                bundle,
                budget,
            } => self.release_local(correlation, pin, bundle, budget),
            ApplicationInput::PollExecution {
                correlation,
                operation,
            } => self.poll_execution(correlation, operation),
            ApplicationInput::Cancel {
                correlation,
                operation,
            } => self.cancel(correlation, operation),
        }
    }

    fn generate(
        correlation: crate::CorrelationId,
        language: InputText,
        stage: InputText,
        package: InputText,
        source: InputText,
    ) -> ApplicationReply {
        if let Some(diagnostic) = Self::text_bound(package) {
            return Self::rejected(correlation, diagnostic);
        }
        if let Some(diagnostic) = Self::text_bound(source) {
            return Self::rejected(correlation, diagnostic);
        }
        let Some(language) = Self::language(language) else {
            return Self::rejected(correlation, Self::unknown_language(language));
        };
        let Some(stage) = Self::stage(stage) else {
            return Self::rejected(correlation, Self::unknown_stage(stage));
        };
        match FullRegistry.dispatch(language, stage, source.as_bytes()) {
            Ok(compiled) => match InputText::from_compiler_bytes(compiled) {
                Some(passthrough) => ApplicationReply {
                    correlation,
                    body: ReplyBody::CompilerPassthrough {
                        package,
                        language,
                        stage,
                        source: passthrough,
                    },
                    terminal: Terminal::Partial {
                        emitted: 1,
                        unavailable: Capability::CompilerOutput,
                    },
                    diagnostic: Some(Diagnostic {
                        code: DiagnosticCode::DependencyUnavailable,
                        detail: DiagnosticDetail::Capability(Capability::CompilerOutput),
                    }),
                },
                None => Self::rejected(
                    correlation,
                    Diagnostic {
                        code: DiagnosticCode::CompilerOutputUnrepresentable,
                        detail: DiagnosticDetail::Text(source),
                    },
                ),
            },
            Err(source) => Self::rejected(
                correlation,
                Diagnostic {
                    code: DiagnosticCode::UnsupportedCompilerStage,
                    detail: DiagnosticDetail::Frontend(source),
                },
            ),
        }
    }

    fn index_unavailable(
        correlation: crate::CorrelationId,
        snapshot: InputText,
    ) -> ApplicationReply {
        if let Some(diagnostic) = Self::text_bound(snapshot) {
            return Self::rejected(correlation, diagnostic);
        }
        Self::dependency_unavailable(correlation, Capability::Index)
    }

    fn search_unavailable(
        correlation: crate::CorrelationId,
        snapshot: InputText,
        query: InputText,
        limit: u8,
    ) -> ApplicationReply {
        if let Some(diagnostic) = Self::limit(limit) {
            return Self::rejected(correlation, diagnostic);
        }
        if let Some(diagnostic) = Self::text_bound(snapshot) {
            return Self::rejected(correlation, diagnostic);
        }
        if let Some(diagnostic) = Self::text_bound(query) {
            return Self::rejected(correlation, diagnostic);
        }
        Self::dependency_unavailable(correlation, Capability::Index)
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

    fn dependency_unavailable(
        correlation: crate::CorrelationId,
        capability: Capability,
    ) -> ApplicationReply {
        ApplicationReply {
            correlation,
            body: ReplyBody::DependencyUnavailable { capability },
            terminal: Terminal::Degraded {
                emitted: 0,
                unavailable: capability,
            },
            diagnostic: Some(Diagnostic {
                code: DiagnosticCode::DependencyUnavailable,
                detail: DiagnosticDetail::Capability(capability),
            }),
        }
    }

    fn health(&self, correlation: crate::CorrelationId) -> ApplicationReply {
        let analyzer = if self.active_bundle.is_some() {
            CapabilityHealth::LocalReady(Capability::LocalAnalyzer)
        } else {
            CapabilityHealth::Unavailable(Capability::LocalAnalyzer)
        };
        ApplicationReply {
            correlation,
            body: ReplyBody::Health([
                CapabilityHealth::LocalReady(Capability::CompilerRegistry),
                CapabilityHealth::Unavailable(Capability::CompilerOutput),
                CapabilityHealth::Unavailable(Capability::Index),
                CapabilityHealth::Unavailable(Capability::Graph),
                CapabilityHealth::Unavailable(Capability::Vector),
                analyzer,
            ]),
            terminal: Terminal::Partial {
                emitted: 2,
                unavailable: Capability::CompilerOutput,
            },
            diagnostic: None,
        }
    }

    fn recover_local(
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
            remote_health: RemoteHealth::Outage,
            budget,
        };
        self.adapt(correlation, pin, next_action(&input))
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

    fn adapt(
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
            }) => Self::adaptive_reply(
                correlation,
                AdaptiveDisposition::RetryRemote {
                    pin,
                    cause,
                    retries_remaining,
                },
                Terminal::Degraded {
                    emitted: 0,
                    unavailable: Capability::Remote,
                },
                None,
            ),
            PolicyDecision::Act(_unexecutable) => {
                Self::dependency_unavailable(correlation, Capability::Remote)
            }
            non_action => Self::adapt_non_action(correlation, non_action),
        }
    }

    fn adapt_non_action(
        correlation: crate::CorrelationId,
        decision: PolicyDecision,
    ) -> ApplicationReply {
        match decision {
            PolicyDecision::NoAction => Self::adaptive_reply(
                correlation,
                AdaptiveDisposition::NoAction,
                Terminal::Complete { emitted: 0 },
                None,
            ),
            PolicyDecision::Overloaded(overload) => Self::adaptive_reply(
                correlation,
                AdaptiveDisposition::Overloaded(overload),
                Terminal::Degraded {
                    emitted: 0,
                    unavailable: Capability::LocalAnalyzer,
                },
                None,
            ),
            PolicyDecision::RecoveryExhausted { pin, cause } => Self::adaptive_reply(
                correlation,
                AdaptiveDisposition::RecoveryExhausted { pin, cause },
                Terminal::Degraded {
                    emitted: 0,
                    unavailable: Capability::Remote,
                },
                None,
            ),
            PolicyDecision::Rejected(source) => Self::adaptive_reply(
                correlation,
                AdaptiveDisposition::Rejected(source),
                Terminal::Failed,
                Some(Diagnostic {
                    code: DiagnosticCode::AdaptivePolicyRejected,
                    detail: DiagnosticDetail::Policy(source),
                }),
            ),
            PolicyDecision::Act(_action) => {
                Self::dependency_unavailable(correlation, Capability::Remote)
            }
        }
    }

    fn adaptive_reply(
        correlation: crate::CorrelationId,
        disposition: AdaptiveDisposition,
        terminal: Terminal,
        diagnostic: Option<Diagnostic>,
    ) -> ApplicationReply {
        ApplicationReply {
            correlation,
            body: ReplyBody::Adaptive(disposition),
            terminal,
            diagnostic,
        }
    }

    fn start_execution(
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
            body: ReplyBody::ExecutionStarted {
                operation,
                transition,
            },
            terminal: Terminal::Accepted { operation },
            diagnostic: None,
        }
    }

    fn poll_execution(
        &mut self,
        correlation: crate::CorrelationId,
        operation: OperationKey,
    ) -> ApplicationReply {
        let Some(mut execution) = self.execution.take() else {
            return self.fused_or_rejected(correlation, operation);
        };
        if execution.operation != operation {
            self.execution = Some(execution);
            return Self::rejected(correlation, Self::operation_unavailable(operation));
        }
        let transition = execution.task.transition();
        let waker = std::task::Waker::noop();
        let mut context = Context::from_waker(waker);
        match TaskPin::new(&mut execution.task).poll(&mut context) {
            std::task::Poll::Pending => {
                self.execution = Some(execution);
                Self::execution_reply(
                    correlation,
                    ExecutionState::Pending {
                        operation,
                        transition,
                    },
                )
            }
            std::task::Poll::Ready(Some(terminal)) => {
                let state =
                    self.apply_execution_terminal(operation, execution.pin, transition, terminal);
                self.last_execution = Some(state);
                Self::execution_reply(correlation, state)
            }
            std::task::Poll::Ready(None) => self.fused_or_rejected(correlation, operation),
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
        let transition = execution.task.transition();
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

    fn execution_reply(
        correlation: crate::CorrelationId,
        state: ExecutionState,
    ) -> ApplicationReply {
        let terminal = match state {
            ExecutionState::Pending { operation, .. } => Terminal::Accepted { operation },
            ExecutionState::Completed { .. } => Terminal::Complete { emitted: 1 },
            ExecutionState::Cancelled { .. } => Terminal::Cancelled { emitted: 0 },
            ExecutionState::Failed { .. } => Terminal::Failed,
        };
        ApplicationReply {
            correlation,
            body: ReplyBody::Execution(state),
            terminal,
            diagnostic: None,
        }
    }

    fn language(value: InputText) -> Option<Language> {
        if value.is("rust") {
            Some(Language::RustSubset)
        } else if value.is("typescript") {
            Some(Language::TypeScriptSubset)
        } else {
            None
        }
    }

    fn stage(value: InputText) -> Option<Stage> {
        if value.is("parse") {
            Some(Stage::Parse)
        } else if value.is("lower-ir") {
            Some(Stage::LowerIr)
        } else {
            None
        }
    }

    fn text_bound(value: InputText) -> Option<Diagnostic> {
        if value.len() > MAX_SEMANTIC_TEXT_BYTES {
            Some(Diagnostic {
                code: DiagnosticCode::SemanticTextTooLong,
                detail: DiagnosticDetail::TextLength {
                    actual: value.len(),
                    maximum: MAX_SEMANTIC_TEXT_BYTES,
                    rejected: value,
                },
            })
        } else {
            None
        }
    }

    fn limit(requested: u8) -> Option<Diagnostic> {
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

    fn unknown_language(value: InputText) -> Diagnostic {
        Diagnostic {
            code: DiagnosticCode::UnknownLanguage,
            detail: DiagnosticDetail::Text(value),
        }
    }

    fn unknown_stage(value: InputText) -> Diagnostic {
        Diagnostic {
            code: DiagnosticCode::UnknownStage,
            detail: DiagnosticDetail::Text(value),
        }
    }

    fn operation_unavailable(operation: OperationKey) -> Diagnostic {
        Diagnostic {
            code: DiagnosticCode::OperationUnavailable,
            detail: DiagnosticDetail::Operation(operation),
        }
    }

    fn rejected(correlation: crate::CorrelationId, diagnostic: Diagnostic) -> ApplicationReply {
        ApplicationReply {
            correlation,
            body: ReplyBody::Rejected,
            terminal: Terminal::Failed,
            diagnostic: Some(diagnostic),
        }
    }
}

impl Default for ApplicationService {
    fn default() -> Self {
        Self::new()
    }
}
