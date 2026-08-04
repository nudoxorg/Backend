//! `RegistryStore` — GUI-PLAN §12.5
//!
//! Mirrors SyncEngine state: tracks the lifecycle status of corpus generations.
//! This is the single source of truth for generation status; other stores hold
//! `GenerationId`s and re-read this store on receipt of `GenerationChanged`.

use std::collections::HashMap;

use gpui::{Context, SharedString};

// ---------------------------------------------------------------------------
// Public id type (referenced by events.rs and job.rs)
// ---------------------------------------------------------------------------

/// Opaque newtype for a corpus generation.
///
/// Corresponds to a remote generation that the sync engine is fetching or has
/// fetched. In production this is a content-addressed hash; for the stub it
/// is an incrementing `u64`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GenerationId(pub u64);

// ---------------------------------------------------------------------------
// Status types
// ---------------------------------------------------------------------------

/// The lifecycle state of one corpus generation.
#[derive(Clone, Debug, PartialEq)]
pub enum GenStatus {
    /// Queued but not yet started.
    Pending,
    /// The engine is computing what to fetch.
    Planning,
    /// Actively fetching bytes.
    Fetching {
        /// Bytes fetched so far.
        done: u64,
        /// Total bytes to fetch.
        total: u64,
        /// Bytes per second (smoothed).
        bytes_per_sec: f64,
    },
    /// Verifying checksums / applying.
    Verifying,
    /// Fully available locally.
    Ready,
    /// Terminal failure; a `retry` call can restart.
    Failed { message: SharedString },
}

/// A single row in the registry table.
#[derive(Clone, Debug)]
pub struct GenRow {
    pub id: GenerationId,
    pub status: GenStatus,
    /// Package coordinate (label for UI).
    pub label: SharedString,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// `RegistryStore` — fed by the long-lived `sync()` receiver.
pub struct RegistryStore {
    pub generations: HashMap<GenerationId, GenRow>,
}

impl RegistryStore {
    pub fn new() -> Self {
        Self {
            generations: HashMap::new(),
        }
    }

    /// Apply an incremental status update from the sync stream.
    ///
    /// This is the `apply` half of the §7.4 shape, called from the drain
    /// closure. It merges the row in-place and emits `GenerationChanged` so
    /// subscribers can re-read the latest status.
    pub fn apply_generation_update(
        &mut self,
        id: GenerationId,
        row: GenRow,
        cx: &mut Context<Self>,
    ) {
        self.generations.insert(id, row);
        cx.emit(crate::stores::events::GenerationChanged { id });
        cx.notify();
    }

    /// Re-queue a failed generation.
    ///
    /// In production this sends a `ClientCommand::RetryGeneration`; for now
    /// it resets status to `Pending` locally so the view can reflect intent.
    pub fn retry(&mut self, id: GenerationId, cx: &mut Context<Self>) {
        if let Some(row) = self.generations.get_mut(&id) {
            row.status = GenStatus::Pending;
            cx.emit(crate::stores::events::GenerationChanged { id });
            cx.notify();
        }
    }
}

impl Default for RegistryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl gpui::EventEmitter<crate::stores::events::GenerationChanged> for RegistryStore {}
