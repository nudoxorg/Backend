//! Emitting a finished blob: recording its manifest to the transactional outbox
//! and its sections to the object store, then signalling the rest of the
//! pipeline (via [`crate::coordination`]) that a new generation exists.
//!
//! "Emit" is the terminal step of a blob's life — the manifest and all pending
//! `cas/` sections are committed idempotently, and one [`OutboxEntry`] per
//! derived sink is appended so qdrant/terminus/tantivy can poll the fan-out.
//!
//! [`OutboxEntry`]: crate::coordination::OutboxEntry

use strum::IntoEnumIterator;

use super::{BlobManifest, creation::PendingSection};
use crate::{
    coordination::{Outbox, SinkKind},
    error::BlobError,
    store::Store,
};

/// The outcome of emitting a blob: how many sections were newly written vs
/// deduped as already-present.
#[derive(Debug, Clone, Copy)]
pub struct Emitted {
    /// Sections that were freshly written to `cas/`.
    pub written: usize,

    /// Sections that were already present (idempotent no-op).
    pub deduped: usize,
}

/// Emit a finished blob.
///
/// Writes every [`PendingSection`] to the content-addressed store (idempotent —
/// a section whose hash already exists is a no-op), records the manifest, and
/// appends one outbox intent per derived sink.
///
/// Consumes the manifest + sections: this is the end of the blob's assembly
/// life, after which it exists only as durable content-addressed state.
///
/// The compiled-lookup index (SV-6) is written separately by the iroh
/// SyncService apply-hook AFTER a merged change-set is verified and applied —
/// not here. See [`crate::compiled::ObjectCompiledStore::record`].
#[tracing::instrument(skip_all, fields(package = %manifest.package, sections = sections.len()))]
pub async fn emit(
    store: &Store,
    outbox: &Outbox,
    manifest: BlobManifest,
    sections: Vec<PendingSection>,
) -> Result<Emitted, BlobError> {
    // 1. Every section lands in `cas/` idempotently.
    let mut emitted = Emitted { written: 0, deduped: 0 };
    for section in &sections {
        match store.put_section(section).await.map_err(BlobError::from)? {
            true => emitted.written += 1,
            false => emitted.deduped += 1,
        }
    }

    // 2. The manifest becomes its own `cas/` object and the package pointer is
    //    repointed at it.
    let package = manifest.package;
    let generation = store.put_manifest(&manifest).await.map_err(BlobError::from)?;

    // 3. Fan out one idempotent intent per derived sink.
    let kinds: Vec<SinkKind> = SinkKind::iter().collect();
    outbox.append(package, generation, &kinds).await.map_err(BlobError::Outbox)?;

    tracing::info!(
        written = emitted.written,
        deduped = emitted.deduped,
        "blob emitted and fanned out to every derived sink"
    );
    Ok(emitted)
}
