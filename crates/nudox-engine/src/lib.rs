//! `nudox-engine` — the streaming facade between the IR layer and `lindsey`.
//!
//! # Dependency law (§L0)
//!
//! This crate sits at L3 in the stack:
//!
//! ```text
//! lindsey  →  nudox-engine  →  {nudox-graph, nudox-store}  →  nudox-ir
//! ```
//!
//! `lindsey` must not import `nudox-ir`, `nudox-store`, or `nudox-graph`
//! directly.  `nudox-engine` must not import `gpui`.  Both rules are enforced
//! by `scripts/lint-gui-no-block.sh`.
//!
//! # Modules
//!
//! * [`wire`]   — the complete type vocabulary crossing the engine↔GUI seam
//!               (§L2).  `lindsey` imports only from here.
//! * [`chunk`]  — LR-3/LR-4: the one place IR becomes presentation.
//! * [`runtime`], [`search`], [`query`] — stubs; implemented as separate tasks.

pub mod wire;
pub mod chunk;

/// Stub — owns the Tokio runtime and the `LocalSet` for query execution (LR-9).
/// Implemented as a separate task.
pub mod runtime {}

/// Stub — name/type/semantic fan-out over `PackageIndexes`.
/// Implemented as a separate task.
pub mod search {}

/// Stub — drives `CorpusAdapter` on the `LocalSet`, coalesces rows into
/// `QueryEvent::Rows`.  Implemented as a separate task.
pub mod query {}
