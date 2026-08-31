//! Closed JSON projections for compiler and publication facts.

mod authority;
mod native;
mod publication;
mod terminal;

pub(super) use authority::GeneratedArtifactWire;
pub(super) use terminal::CompilerTerminalWire;
