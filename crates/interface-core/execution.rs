//! Defines execution behavior for `interface-core`, whose purpose is to own the transport-independent application service and reply vocabulary.
//! This module owns the execution invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Finite execution future for one locally selected adaptive capability action.

use std::{
    future::Future,
    pin::Pin as TaskPin,
    task::{Context, Poll},
};

use heart_adaptive::{ExecutionRequest, ExecutionTerminal};

use crate::CapabilityTransition;

/// One non-cloneable local action owner with an explicit wake and fused terminal.
pub(crate) struct LocalCapabilityExecution {
    request: Option<ExecutionRequest>,
    pub(crate) transition: CapabilityTransition,
    phase: WakePhase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WakePhase {
    Armed,
    Ready,
    Fused,
}

impl LocalCapabilityExecution {
    pub(crate) const fn new(request: ExecutionRequest, transition: CapabilityTransition) -> Self {
        Self {
            request: Some(request),
            transition,
            phase: WakePhase::Armed,
        }
    }

    pub(crate) fn cancel(mut self) -> Option<ExecutionTerminal> {
        self.phase = WakePhase::Fused;
        self.request.take().map(ExecutionRequest::cancelled)
    }
}

impl Future for LocalCapabilityExecution {
    type Output = Option<ExecutionTerminal>;

    fn poll(mut self: TaskPin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        match self.phase {
            WakePhase::Armed => {
                self.phase = WakePhase::Ready;
                context.waker().wake_by_ref();
                Poll::Pending
            }
            WakePhase::Ready => {
                self.phase = WakePhase::Fused;
                Poll::Ready(self.request.take().map(ExecutionRequest::completed))
            }
            WakePhase::Fused => Poll::Ready(None),
        }
    }
}
