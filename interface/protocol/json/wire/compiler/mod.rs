//! Defines json wire compiler behavior for `interface-protocol`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the json wire compiler invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Closed JSON projections for compiler and publication facts.

mod authority;
mod native;
mod publication;
mod terminal;

pub(super) use authority::GeneratedArtifactWire;
pub(super) use terminal::CompilerTerminalWire;
