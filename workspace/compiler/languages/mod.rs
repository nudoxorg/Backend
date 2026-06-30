//! The per-language compiler backends. Each lowers its toolchain's
//! documentation form into the shared `ir::Index`, and resolves versions over
//! the language's VCS/manifest conventions (`traversal`).

pub mod python;
pub mod rust;
pub mod typescript;
