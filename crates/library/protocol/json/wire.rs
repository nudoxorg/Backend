//! Defines json wire behavior for `backend-library`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the json wire invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Declarative wire vocabulary for application JSON.

mod adaptive;
mod application;
mod compiler;
mod envelope;
mod scalar;

pub(crate) use envelope::{AdapterErrorEnvelope, ApplicationReplyWire};
