//! Thin portable entrypoints for the shared borrowed index query engine.

use backend_semantic::index_core::{
    EntityDocumentId, ExactManifest, ExactManifestError, ExactOperation, ExactSegment,
    ExactSegmentId, ExactTerminal, IndexSnapshot, LexicalManifest, LexicalManifestError,
    LexicalOperation, LexicalQueryError, LexicalSegment, LexicalSegmentId, LexicalSnapshotHit,
    LexicalTerminal, LexicalTopK,
};

/// Executes one exact local query through the same borrowed engine used by remote workers.
///
/// Segment bytes and query bytes remain borrowed from their respective callers. The client owns no
/// server runtime, storage adapter, writer, or derived-index backend by depending on this portable
/// logical-core package.
///
/// # Errors
///
/// Returns the engine's typed manifest admission error before attempting lookup.
pub fn query_local_exact<'manifest, 'segment>(
    snapshot: IndexSnapshot<'_>,
    segments: &'manifest [ExactSegment<'segment>],
    missing: &'manifest [ExactSegmentId],
    key: &[u8],
) -> Result<ExactTerminal<'manifest, 'segment>, ExactManifestError> {
    let manifest = ExactManifest::new(snapshot, segments, missing)?;
    Ok(manifest.execute(ExactOperation::new(key)))
}

/// Executes one lexical local query through the shared borrowed ranking engine.
///
/// The request term, segment rows, deduplication scratch, and output all remain independently
/// borrowed. The caller keeps the output owner, so the client control plane never allocates a
/// document forest or an implicit result cache.
///
/// # Errors
///
/// Returns a typed manifest admission failure before execution, or the engine's typed caller
/// scratch/output capacity or scoring failure without replacing an admitted terminal.
pub fn query_local_lexical<'manifest, 'segment, 'output>(
    snapshot: IndexSnapshot<'_>,
    segments: &'manifest [LexicalSegment<'segment>],
    missing: &'manifest [LexicalSegmentId],
    term: &[u8],
    top_k: LexicalTopK,
    seen_documents: &mut [Option<EntityDocumentId>],
    output: &'output mut [LexicalSnapshotHit<'segment>],
) -> Result<LexicalTerminal<'manifest, 'output, 'segment>, LocalLexicalQueryError> {
    let manifest = LexicalManifest::new(snapshot, segments, missing)
        .map_err(LocalLexicalQueryError::Manifest)?;
    manifest
        .execute(LexicalOperation::new(term), top_k, seen_documents, output)
        .map_err(LocalLexicalQueryError::Query)
}

/// Typed local lexical wrapper failure retaining the underlying shared-engine cause.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalLexicalQueryError {
    /// The selected reachable/missing lanes did not match the pinned snapshot.
    Manifest(LexicalManifestError),
    /// Caller scratch/output or bounded scoring execution was rejected.
    Query(LexicalQueryError),
}
