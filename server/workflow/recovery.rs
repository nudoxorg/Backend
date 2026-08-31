//! Defines recovery behavior for `server-workflow`, whose purpose is to reduce durable workflow events into deterministic recovery state.
//! This module owns the recovery invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Recovery command derivation after durable replay.
#![allow(
    missing_docs,
    reason = "state and pending effect are documented by the recovery contract"
)]

use crate::{Effect, WorkflowState, reduce::pending_effect};

/// Replayed state plus exactly the idempotent command still pending, if any.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Recovery {
    pub state: WorkflowState,
    pub pending_effect: Option<Effect>,
}

impl Recovery {
    pub(crate) const fn from_state(state: WorkflowState) -> Self {
        let pending_effect = match state {
            WorkflowState::New => None,
            WorkflowState::Keyed { key, phase } => pending_effect(key, phase),
        };
        Self {
            state,
            pending_effect,
        }
    }
}
