//! Emitting a finished blob: recording its manifest to the transactional outbox
//! and its sections to the object store, then signalling the rest of the
//! pipeline (via [`crate::coordination`]) that a new generation exists.
//!
//! "Emit" is the terminal step of a blob's life — the manifest and all pending
//! `cas/` sections are committed idempotently, and one [`OutboxEntry`] per
//! derived sink is appended so qdrant/terminus/tantivy can poll the fan-out.

use super::{BlobManifest, creation::PendingSection};
use crate::{coordination::Outbox, error::BlobError, store::Store};

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
/// appends one outbox intent per derived sink. `// object-store writes may run
/// on spawn_blocking depending on backend`.
///
/// Consumes the manifest + sections: this is the end of the blob's assembly
/// life, after which it exists only as durable content-addressed state.
pub async fn emit(
	store: &Store,
	outbox: &Outbox,
	manifest: BlobManifest,
	sections: Vec<PendingSection>,
) -> Result<Emitted, BlobError> {
	let _ = (store, outbox, manifest, sections);
	todo!("idempotent-put each section, record the manifest, fan out outbox intents per SinkKind")
}
