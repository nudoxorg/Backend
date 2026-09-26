//! Local capability execution and adaptive placement.
//!
//! Command admission stays in the parent. These methods own recovery, release,
//! and the in-flight execution poll.

use std::{
    pin::Pin as TaskPin,
    task::{Context, Poll, Waker},
};

use backend_execution::adaptive::{
    BundleFact, BundleState, CapabilityDomain, CapabilityKind, ContentId, ExecutionRequest,
    ExecutionTerminal, LatencyMicros, Pin, PlacementAction, PolicyDecision, PolicyInput,
    RemoteHealth, ResourceBudget, next_action,
};

use super::{ActiveBundle, ActiveExecution, ApplicationService};
use crate::interface::{
    AdaptiveDisposition, ApplicationOutcome, ApplicationReply, Capability, CapabilityTransition,
    Diagnostic, DiagnosticCode, DiagnosticDetail, ExecutionState, InconsistentRecovery,
    OperationKey, ReplyBody, execution::LocalCapabilityExecution,
};

impl<
    Compiler: crate::interface::CompilerCapability,
    Retrieval: crate::interface::RetrievalCapability,
> ApplicationService<Compiler, Retrieval>
{
    /// Admits a local recovery placement for one exact bundle.
    pub(super) fn recover_local(
        &mut self,
        correlation: crate::interface::CorrelationId,
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

    /// Recovers a local bundle after an inconsistent remote observation.
    pub(super) fn recover_inconsistent(
        &mut self,
        recovery: InconsistentRecovery,
    ) -> ApplicationReply {
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

    /// Releases one local bundle when no execution is in flight.
    pub(super) fn release_local(
        &mut self,
        correlation: crate::interface::CorrelationId,
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
        correlation: crate::interface::CorrelationId,
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
        correlation: crate::interface::CorrelationId,
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
        correlation: crate::interface::CorrelationId,
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

    /// Polls one admitted local capability execution.
    pub(super) fn poll_execution(
        &mut self,
        correlation: crate::interface::CorrelationId,
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

    /// Cancels one admitted local capability execution.
    pub(super) fn cancel(
        &mut self,
        correlation: crate::interface::CorrelationId,
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
}
