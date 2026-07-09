//! The per-language compiler backends. Each lowers its toolchain's
//! documentation form into the shared `ir::Index`, and resolves versions over
//! the language's VCS/manifest conventions (`traversal`).
//!
//! [`producer`] is the shared shape (DAEMON-PLAN §2.3): plan → cage/worker → decode.

pub mod go;
/// Sandbox seam for external toolchains and worker-isolated interpreters.
pub mod isolate;
pub mod java;
pub mod nix;
/// Producer trait + shared substrate + one dispatch path.
pub mod producer;
pub mod python;
pub mod rust;
pub mod typescript;
/// Shared `gix` plumbing (clone/fetch/walk/materialize) behind the
/// per-language `traversal` modules.
pub mod vcs;
