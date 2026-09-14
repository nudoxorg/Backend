//! Hostile local-first synchronization tests for the portable client controller.

use core::num::NonZeroU64;

use backend_semantic::ir::EntityId;
use backend_version::{
    ArtifactId, ContentId, GenerationId, IndexExactSegmentDomain, IndexLexicalSegmentDomain,
    IrFragmentDomain, IrFragmentEncoding, ObjectDomain, derive_index_snapshot,
};
use interface_core::{
    ClientIndex, ClientManifest, ClientSyncError, ClientSyncPhase, DemandSelection,
    EffectiveSearchResult, LocalDelta, LocalQueryTerminal, LocalSelection, ManifestEpoch,
    ManifestError, ManifestSegment, OverlayKey, RemoteGeneration, RemoteManifest, RemoteSearch,
    RemoteSearchCandidate, ResidentRange, SegmentDemand, SegmentId, SegmentRange, SelectionOutput,
    SelectionScratch, SyncCancellation, SyncTerminal,
};
use backend_semantic::index_core::{EntityArtifactIdentity, EntityDocumentId, IndexSnapshot};

#[derive(Debug)]
enum TestError {
    Manifest(ManifestError),
    Range(interface_core::SegmentRangeError),
    Sync(ClientSyncError),
    MissingNonZero,
    Unexpected(&'static str),
}

impl core::fmt::Display for TestError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Manifest(error) => write!(formatter, "manifest error: {error:?}"),
            Self::Range(error) => write!(formatter, "range error: {error:?}"),
            Self::Sync(error) => write!(formatter, "sync error: {error:?}"),
            Self::MissingNonZero => formatter.write_str("expected a non-zero value"),
            Self::Unexpected(message) => write!(formatter, "unexpected result: {message}"),
        }
    }
}

impl std::error::Error for TestError {}

impl From<ManifestError> for TestError {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}

impl From<interface_core::SegmentRangeError> for TestError {
    fn from(error: interface_core::SegmentRangeError) -> Self {
        Self::Range(error)
    }
}

impl From<ClientSyncError> for TestError {
    fn from(error: ClientSyncError) -> Self {
        Self::Sync(error)
    }
}

fn nonzero(value: u64) -> Result<NonZeroU64, TestError> {
    NonZeroU64::new(value).ok_or(TestError::MissingNonZero)
}

fn range(start: u64, length: u64) -> Result<SegmentRange, TestError> {
    Ok(SegmentRange::new(start, nonzero(length)?)?)
}

fn object(label: &[u8]) -> ContentId<ObjectDomain> {
    ContentId::from_canonical_bytes(label)
}

fn exact(label: &[u8]) -> ContentId<IndexExactSegmentDomain> {
    ContentId::from_canonical_bytes(label)
}

fn document(label: u32) -> EntityDocumentId {
    EntityDocumentId {
        artifact: EntityArtifactIdentity::Compact(
            ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(b"sync-test"),
        ),
        entity: EntityId::new(label),
    }
}

fn manifest(
    generation_bytes: &[u8],
    epoch: u64,
    exact_segments: &[ContentId<IndexExactSegmentDomain>],
) -> Result<ClientManifest, TestError> {
    let generation = GenerationId::from_canonical_bytes(generation_bytes);
    let lexical: [ContentId<IndexLexicalSegmentDomain>; 0] = [];
    let snapshot = derive_index_snapshot(generation, exact_segments, &lexical)
        .map_err(ManifestError::Identity)?;
    let remote = RemoteGeneration::new(generation, snapshot);
    let mut exact_descriptors = Vec::with_capacity(exact_segments.len());
    for id in exact_segments {
        exact_descriptors.push(ManifestSegment::exact(*id, nonzero(32)?));
    }
    Ok(ClientManifest::from_lanes(
        remote,
        ManifestEpoch::new(nonzero(epoch)?),
        exact_descriptors.into_boxed_slice(),
        Box::new([]),
    )?)
}

#[test]
fn manifest_admission_uses_the_shared_server_snapshot_grammar() -> Result<(), TestError> {
    let exact_id = exact(b"exact-one");
    let accepted = manifest(b"sync-generation", 1, &[exact_id])?;
    assert_eq!(accepted.exact().len(), 1);
    assert!(accepted.lexical().is_empty());
    let server_exact = [exact_id];
    let server = IndexSnapshot::new(accepted.generation().generation(), &server_exact, &[])
        .map_err(|_| TestError::Unexpected("server snapshot must admit the shared lane"))?;
    assert_eq!(server.id, accepted.generation().snapshot());

    let wrong = RemoteGeneration::new(
        accepted.generation().generation(),
        backend_version::IndexSnapshotId::from_canonical_bytes(b"wrong-snapshot"),
    );
    let rejected = ClientManifest::from_lanes(
        wrong,
        accepted.epoch(),
        accepted.exact().to_vec().into_boxed_slice(),
        accepted.lexical().to_vec().into_boxed_slice(),
    );
    assert!(matches!(
        rejected,
        Err(ManifestError::SnapshotMismatch { .. })
    ));
    Ok(())
}

#[test]
fn remote_search_reconciles_overlay_tombstones_across_sync_and_advance() -> Result<(), TestError> {
    let first = manifest(b"sync-generation-one", 1, &[exact(b"exact-one")])?;
    let first_generation = first.generation();
    let second = manifest(b"sync-generation-two", 2, &[exact(b"exact-two")])?;
    let alpha = OverlayKey::new(object(b"alpha"));
    let beta = OverlayKey::new(object(b"beta"));
    let gamma = OverlayKey::new(object(b"gamma"));
    let local_beta = object(b"local-beta");
    let mut client = ClientIndex::new();
    client.record_local_delta(LocalDelta::tombstone(alpha))?;
    client.record_local_delta(LocalDelta::upsert(beta, local_beta))?;
    assert!(matches!(
        client.accept_manifest(RemoteManifest::initial(first), SyncCancellation::Continue),
        SyncTerminal::AcceptedInitial { .. }
    ));
    assert!(matches!(
        client.phase(),
        ClientSyncPhase::Synchronizing { .. }
    ));

    let candidates = [
        RemoteSearchCandidate::new(alpha, document(1)),
        RemoteSearchCandidate::new(beta, document(2)),
        RemoteSearchCandidate::new(gamma, document(3)),
    ];
    let search = RemoteSearch::new(first_generation, &candidates);
    let mut output = [None; 3];
    let terminal = client.accept_remote_search(&search, &mut output)?;
    assert!(matches!(
        terminal,
        interface_core::RemoteSearchTerminal::Candidates { returned: 2, .. }
    ));
    assert!(matches!(
        output[0],
        Some(EffectiveSearchResult::LocalOverlay { key, document }) if key == beta && document == local_beta
    ));
    assert!(matches!(
        output[1],
        Some(EffectiveSearchResult::Remote(candidate)) if candidate.key() == gamma
    ));
    assert_eq!(output[2], None);

    assert!(matches!(
        client.accept_manifest(
            RemoteManifest::advance(first_generation, second),
            SyncCancellation::Continue,
        ),
        SyncTerminal::AcceptedAdvance { .. }
    ));
    let next_generation = match client.phase() {
        ClientSyncPhase::Synchronizing { base } => {
            RemoteGeneration::new(base.generation(), base.snapshot())
        }
        _ => {
            return Err(TestError::Unexpected(
                "advance must resume background synchronization",
            ));
        }
    };
    let next_candidates = [
        RemoteSearchCandidate::new(alpha, document(4)),
        RemoteSearchCandidate::new(beta, document(5)),
    ];
    let search = RemoteSearch::new(next_generation, &next_candidates);
    let terminal = client.accept_remote_search(&search, &mut output)?;
    assert!(matches!(
        terminal,
        interface_core::RemoteSearchTerminal::Candidates { returned: 1, .. }
    ));
    assert!(matches!(
        output[0],
        Some(EffectiveSearchResult::LocalOverlay { key, document }) if key == beta && document == local_beta
    ));
    assert_eq!(output[1], None);
    Ok(())
}

#[test]
fn local_selection_names_missing_ranges_and_commits_only_after_preflight() -> Result<(), TestError>
{
    let exact_id = exact(b"selection-exact");
    let initial = manifest(b"selection-generation", 1, &[exact_id])?;
    let remote = initial.generation();
    let mut client = ClientIndex::new();
    let _ = client.accept_manifest(RemoteManifest::initial(initial), SyncCancellation::Continue);
    let demand = [SegmentDemand::new(SegmentId::Exact(exact_id), range(4, 8)?)];
    let selection = DemandSelection::new(&demand)?;
    {
        let mut scratch = [None; 1];
        let mut output = [None; 1];
        let partial = client.select_local(
            selection,
            SyncCancellation::Continue,
            SelectionScratch::new(&mut scratch),
            SelectionOutput::new(&mut output),
        )?;
        assert!(matches!(
            partial,
            LocalQueryTerminal::Partial { missing: 1, .. }
        ));
        assert!(matches!(output[0], Some(LocalSelection::Missing(_))));

        let invalid = [SegmentDemand::new(
            SegmentId::Exact(exact_id),
            range(30, 8)?,
        )];
        let unchanged = output;
        let rejected = client.select_local(
            DemandSelection::new(&invalid)?,
            SyncCancellation::Continue,
            SelectionScratch::new(&mut scratch),
            SelectionOutput::new(&mut output),
        );
        assert_eq!(rejected, Err(ClientSyncError::DemandRangeOutsideManifest));
        assert_eq!(output, unchanged);
    }

    client.record_resident_range(ResidentRange::new(
        SegmentId::Exact(exact_id),
        range(0, 32)?,
    ))?;
    let proof = {
        let mut scratch = [None; 1];
        let mut output = [None; 1];
        let complete = client.select_local(
            selection,
            SyncCancellation::Continue,
            SelectionScratch::new(&mut scratch),
            SelectionOutput::new(&mut output),
        )?;
        let LocalQueryTerminal::Complete { proof, .. } = complete else {
            return Err(TestError::Unexpected(
                "resident range must complete selection",
            ));
        };
        proof
    };
    assert!(matches!(
        client.complete_sync(proof, SyncCancellation::Continue),
        SyncTerminal::LocalReady { .. }
    ));

    {
        let mut scratch = [None; 1];
        let mut output = [None; 1];
        let cancelled = client.select_local(
            selection,
            SyncCancellation::Cancelled,
            SelectionScratch::new(&mut scratch),
            SelectionOutput::new(&mut output),
        )?;
        assert!(matches!(cancelled, LocalQueryTerminal::Cancelled));
    }
    match client.phase() {
        ClientSyncPhase::LocalReady { base } => assert_eq!(remote.snapshot(), base.snapshot()),
        _ => {
            return Err(TestError::Unexpected(
                "completion must retain the pinned base",
            ));
        }
    }
    Ok(())
}

#[test]
fn empty_or_subset_selection_cannot_mark_a_partial_manifest_locally_ready() -> Result<(), TestError>
{
    let first = exact(b"ready-first");
    let second = exact(b"ready-second");
    let initial = manifest(b"ready-generation", 1, &[first, second])?;
    let mut client = ClientIndex::new();
    let _ = client.accept_manifest(RemoteManifest::initial(initial), SyncCancellation::Continue);

    let empty: [SegmentDemand; 0] = [];
    let empty_proof = {
        let mut scratch = [];
        let mut output = [];
        let terminal = client.select_local(
            DemandSelection::new(&empty)?,
            SyncCancellation::Continue,
            SelectionScratch::new(&mut scratch),
            SelectionOutput::new(&mut output),
        )?;
        let LocalQueryTerminal::Complete { proof, .. } = terminal else {
            return Err(TestError::Unexpected(
                "empty selection must be vacuously complete",
            ));
        };
        proof
    };
    assert!(matches!(
        client.complete_sync(empty_proof, SyncCancellation::Continue),
        SyncTerminal::LocalSelectionIncomplete { .. }
    ));

    client.record_resident_range(ResidentRange::new(SegmentId::Exact(first), range(0, 32)?))?;
    let subset = [SegmentDemand::new(SegmentId::Exact(first), range(0, 32)?)];
    let subset_proof = {
        let mut scratch = [None; 1];
        let mut output = [None; 1];
        let terminal = client.select_local(
            DemandSelection::new(&subset)?,
            SyncCancellation::Continue,
            SelectionScratch::new(&mut scratch),
            SelectionOutput::new(&mut output),
        )?;
        let LocalQueryTerminal::Complete { proof, .. } = terminal else {
            return Err(TestError::Unexpected(
                "resident subset must complete its query",
            ));
        };
        proof
    };
    assert!(matches!(
        client.complete_sync(subset_proof, SyncCancellation::Continue),
        SyncTerminal::LocalSelectionIncomplete { .. }
    ));
    assert!(matches!(
        client.phase(),
        ClientSyncPhase::Synchronizing { .. }
    ));
    Ok(())
}
