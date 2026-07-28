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
//! * [`wire`]    — the complete type vocabulary crossing the engine↔GUI seam
//!                (§L2).  `lindsey` imports only from here.
//! * [`chunk`]   — LR-3/LR-4: the one place IR becomes presentation.
//! * [`runtime`] — LR-9: Tokio multi-thread runtime + `LocalSet` for query
//!                 execution; `Engine::start` → `EngineHandle`.
//! * [`doc`]     — `open_symbol`: drives the chunker and emits `DocEvent`
//!                 streams following the §9.3 protocol.
//! * [`search`]  — name/type/semantic fan-out over `PackageIndexes` (LR-10).
//! * [`query`]   — drives `CorpusAdapter` on the `LocalSet`; emits
//!                 `QueryEvent` streams.
//!
//! # `EngineHandle` public surface (§7.1 + §L5)
//!
//! ```text
//! EngineHandle::search(q, gen)          -> (StreamHandle, Receiver<SearchEvent>)
//! EngineHandle::open_symbol(key, gen)   -> (StreamHandle, Receiver<DocEvent>)
//! EngineHandle::open_package(id, gen)   -> (StreamHandle, Receiver<PackageEvent>)  // stub
//! EngineHandle::resolve_project(root)   -> (StreamHandle, Receiver<ProjectEvent>)  // stub
//! EngineHandle::sync()                  -> Receiver<SyncEvent>                     // stub
//! EngineHandle::jobs()                  -> Receiver<JobEvent>                      // stub
//! EngineHandle::command(cmd)            -> ()                                      // stub
//! EngineHandle::query(q, gen)           -> (StreamHandle, Receiver<QueryEvent>)
//! EngineHandle::schema()                -> &'static Schema
//! ```

pub mod wire;
pub mod chunk;
pub mod runtime;
pub mod doc;
pub mod highlight;
pub mod search;
pub mod query;

// Re-export the primary public surface so callers can `use nudox_engine::*`
// if they prefer.
pub use runtime::{Engine, EngineConfig, EngineHandle, StreamHandle};
pub use search::SearchQuery;
pub use query::GraphQuery;
pub use wire::{
    DocEvent, EngineError, Gen, GenerationId, HitRow, KindTag, Provenance, QueryEvent,
    QueryRow, RenderSection, SearchEvent, SearchSectionId, SectionId, SharedStr, SymbolHead,
    SymbolKey,
};

// ---------------------------------------------------------------------------
// Stub methods on EngineHandle for §7.1 completeness
//
// These are declared here rather than in `runtime.rs` so that the runtime
// module stays focused on the actual runtime machinery.  Each stub method is
// tagged with the milestone that will implement it.
// ---------------------------------------------------------------------------

use std::path::PathBuf;

/// Placeholder package event — emitted by the `open_package` stub.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum PackageEvent {
    /// Stub — no packages are streamed yet (M4 work).
    Done { generation: Gen },
}

/// Placeholder project event — emitted by the `resolve_project` stub.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ProjectEvent {
    /// Stub — project resolution not implemented yet (M3 work).
    Done { generation: Gen },
}

/// Placeholder sync event — emitted by the `sync` stub.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum SyncEvent {
    /// Stub.
    Idle,
}

/// Placeholder job event — emitted by the `jobs` stub.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum JobEvent {
    /// Stub.
    Idle,
}

/// A fire-and-forget command to the engine (cancel job, pin, retry, etc.).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ClientCommand {
    /// No-op stub.
    Noop,
}

impl EngineHandle {
    /// Open a package's API surface as a streaming [`PackageEvent`] sequence.
    ///
    /// **Stub (M4).** Returns an immediately-`Done` stream.
    pub fn open_package(
        &self,
        _id: nudox_ir::change::PackageLineageId,
        generation: Gen,
    ) -> (StreamHandle, flume::Receiver<PackageEvent>) {
        let (tx, rx) = flume::bounded::<PackageEvent>(32);
        let (_, cancel_fn) = Self::make_cancel();
        let handle = StreamHandle::new(generation, cancel_fn);
        let _ = tx.try_send(PackageEvent::Done { generation });
        (handle, rx)
    }

    /// Resolve a project workspace and stream package discovery events.
    ///
    /// **Stub (M3).** Returns an immediately-`Done` stream.
    pub fn resolve_project(
        &self,
        _root: PathBuf,
    ) -> (StreamHandle, flume::Receiver<ProjectEvent>) {
        let generation = Gen(0);
        let (tx, rx) = flume::bounded::<ProjectEvent>(32);
        let (_, cancel_fn) = Self::make_cancel();
        let handle = StreamHandle::new(generation, cancel_fn);
        let _ = tx.try_send(ProjectEvent::Done { generation });
        (handle, rx)
    }

    /// Subscribe to the long-lived sync event stream.
    ///
    /// **Stub (M4).** Returns a receiver that immediately closes.
    pub fn sync(&self) -> flume::Receiver<SyncEvent> {
        let (_, rx) = flume::bounded::<SyncEvent>(256);
        rx
    }

    /// Subscribe to the long-lived job event stream.
    ///
    /// **Stub (M4).** Returns a receiver that immediately closes.
    pub fn jobs(&self) -> flume::Receiver<JobEvent> {
        let (_, rx) = flume::bounded::<JobEvent>(256);
        rx
    }

    /// Fire-and-forget a command to the engine (cancel job, pin, retry, …).
    ///
    /// **Stub (M4).** Currently a no-op.
    pub fn command(&self, _cmd: ClientCommand) {
        // No-op until M4 wires up the command channel.
    }
}
