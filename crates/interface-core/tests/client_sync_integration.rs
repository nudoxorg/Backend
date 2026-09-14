//! Hostile integration coverage for the portable client synchronization control plane.
//!
//! These tests intentionally exercise only the public `interface-core` vocabulary.  Segment
//! payloads are not fabricated here: the client receives immutable descriptors and caller-owned
//! residence receipts, then reports exact missing ranges without mutating output on rejection.

use core::num::NonZeroU64;

use backend_semantic::ir::EntityId;
use backend_version::{
    ArtifactId, ContentId, GenerationId, IndexExactSegmentDomain, IndexLexicalSegmentDomain,
    IndexSnapshotDomain, IrFragmentDomain, IrFragmentEncoding, ObjectDomain, derive_index_snapshot,
};
use interface_core::{
    BaseGeneration, ClientIndex, ClientManifest, ClientSyncError, ClientSyncPhase, DemandSelection,
    EffectiveSearchResult, LocalDelta, LocalQueryTerminal, LocalSelection, ManifestEpoch,
    ManifestError, ManifestSegment, OverlayKey, OverlayObservation, RemoteGeneration,
    RemoteManifest, RemoteSearch, RemoteSearchCandidate, RemoteSearchTerminal, ResidentRange,
    SegmentDemand, SegmentId, SegmentRange, SelectionOutput, SelectionScratch, SyncCancellation,
    SyncTerminal,
};
use backend_semantic::index_core::{EntityArtifactIdentity, EntityDocumentId};

fn generation(value: u8) -> GenerationId {
    GenerationId::from_digest([value; 32])
}

fn snapshot(value: u8) -> ContentId<IndexSnapshotDomain> {
    ContentId::from_canonical_bytes(&[value])
}

fn object(value: u8) -> ContentId<ObjectDomain> {
    ContentId::from_canonical_bytes(&[value])
}

fn document(value: u32) -> EntityDocumentId {
    EntityDocumentId {
        artifact: EntityArtifactIdentity::Compact(
            ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(b"client-sync"),
        ),
        entity: EntityId::new(value),
    }
}

fn exact(value: u8) -> SegmentId {
    SegmentId::Exact(exact_id(value))
}

fn lexical(value: u8) -> SegmentId {
    SegmentId::Lexical(lexical_id(value))
}

fn exact_id(value: u8) -> ContentId<IndexExactSegmentDomain> {
    ContentId::from_canonical_bytes(&[value])
}

fn lexical_id(value: u8) -> ContentId<IndexLexicalSegmentDomain> {
    ContentId::from_canonical_bytes(&[value])
}

fn length(value: u64) -> NonZeroU64 {
    match NonZeroU64::new(value) {
        Some(value) => value,
        None => panic!("test segment lengths are nonzero"),
    }
}

fn manifest(generation_value: u8, epoch: u64) -> ClientManifest {
    let generation = generation(generation_value);
    let exact_id = exact_id(1);
    let lexical_id = lexical_id(1);
    let derived = derive_index_snapshot(generation, &[exact_id], &[lexical_id])
        .unwrap_or_else(|_| panic!("fixture snapshot identity is representable"));
    ClientManifest::from_lanes(
        RemoteGeneration::new(generation, derived),
        ManifestEpoch::new(length(epoch)),
        vec![ManifestSegment::exact(exact_id, length(100))].into_boxed_slice(),
        vec![ManifestSegment::lexical(lexical_id, length(200))].into_boxed_slice(),
    )
    .unwrap_or_else(|_| panic!("fixture manifest has valid lanes"))
}

fn accept_initial(client: &mut ClientIndex) -> BaseGeneration {
    match client.accept_manifest(
        RemoteManifest::initial(manifest(1, 1)),
        SyncCancellation::Continue,
    ) {
        SyncTerminal::AcceptedInitial { base } => base,
        terminal => panic!("unexpected initial terminal: {terminal:?}"),
    }
}

#[test]
fn stale_out_of_order_and_partial_manifests_are_non_mutating() {
    let mut client = ClientIndex::new();
    let original = client.phase();
    assert_eq!(
        client.accept_manifest(
            RemoteManifest::partial(RemoteGeneration::new(generation(7), snapshot(7))),
            SyncCancellation::Continue,
        ),
        SyncTerminal::Partial {
            generation: RemoteGeneration::new(generation(7), snapshot(7)),
        }
    );
    assert_eq!(client.phase(), original);
    let base = accept_initial(&mut client);

    assert!(matches!(
        client.accept_manifest(
        RemoteManifest::initial(manifest(1, 1)),
            SyncCancellation::Continue,
        ),
        SyncTerminal::Stale { accepted, .. } if accepted == base
    ));
    assert!(matches!(
        client.accept_manifest(
            RemoteManifest::advance(
                RemoteGeneration::new(generation(99), snapshot(99)),
                manifest(2, 2),
            ),
            SyncCancellation::Continue,
        ),
        SyncTerminal::OutOfOrder { expected, .. } if expected == base
    ));
    assert_eq!(client.phase(), ClientSyncPhase::Synchronizing { base });
}

#[test]
fn manifest_snapshot_proof_mismatch_is_rejected_before_client_mutation() {
    let generation = generation(8);
    let exact_id = exact_id(1);
    let lexical_id = lexical_id(1);
    let expected = derive_index_snapshot(generation, &[exact_id], &[lexical_id])
        .unwrap_or_else(|_| panic!("fixture identity is representable"));
    let result = ClientManifest::from_lanes(
        RemoteGeneration::new(generation, snapshot(8)),
        ManifestEpoch::new(length(1)),
        vec![ManifestSegment::exact(exact_id, length(100))].into_boxed_slice(),
        vec![ManifestSegment::lexical(lexical_id, length(200))].into_boxed_slice(),
    );
    assert_eq!(
        result,
        Err(ManifestError::SnapshotMismatch {
            expected,
            observed: snapshot(8),
        })
    );
}

#[test]
fn cancellation_and_remote_immediate_results_do_not_require_sync_completion() {
    let mut client = ClientIndex::new();
    let mut output: [Option<EffectiveSearchResult>; 1] = [None];
    let key = OverlayKey::new(object(9));
    let candidates = [RemoteSearchCandidate::new(key, document(9))];
    let search = RemoteSearch::new(
        RemoteGeneration::new(generation(1), manifest(1, 1).generation().snapshot()),
        &candidates,
    );
    assert_eq!(
        client.accept_remote_search(&search, &mut output),
        Err(ClientSyncError::RemoteSearchBeforeManifest)
    );
    let base = accept_initial(&mut client);
    assert_eq!(
        client.accept_remote_search(&search, &mut output),
        Ok(RemoteSearchTerminal::Candidates { base, returned: 1 })
    );
    assert!(output[0].is_some());
    assert!(matches!(output[0], Some(EffectiveSearchResult::Remote(_))));

    let tail = EffectiveSearchResult::LocalOverlay {
        key: OverlayKey::new(object(10)),
        document: object(11),
    };
    let mut extended = [None, Some(tail)];
    assert_eq!(
        client.accept_remote_search(&search, &mut extended),
        Ok(RemoteSearchTerminal::Candidates { base, returned: 1 })
    );
    assert_eq!(extended[1], Some(tail));

    let mut cancelled_output = [None; 1];
    let mut scratch = [None; 1];
    let demand = [SegmentDemand::new(
        exact(1),
        SegmentRange::new(0, length(10)).unwrap_or_else(|_| panic!("valid range")),
    )];
    let selection = DemandSelection::new(&demand).unwrap_or_else(|_| panic!("valid demand"));
    let terminal = client
        .select_local(
            selection,
            SyncCancellation::Cancelled,
            SelectionScratch::new(&mut scratch),
            SelectionOutput::new(&mut cancelled_output),
        )
        .unwrap_or_else(|_| panic!("cancellation is a terminal, not an error"));
    assert_eq!(terminal, LocalQueryTerminal::Cancelled);
    assert!(cancelled_output[0].is_none());
    assert!(scratch[0].is_none());
}

#[test]
fn only_a_complete_local_selection_proof_can_finish_sync() {
    let mut client = ClientIndex::new();
    let base = accept_initial(&mut client);
    let demands = [
        SegmentDemand::new(
            exact(1),
            SegmentRange::new(0, length(100)).unwrap_or_else(|_| panic!("valid range")),
        ),
        SegmentDemand::new(
            lexical(1),
            SegmentRange::new(0, length(200)).unwrap_or_else(|_| panic!("valid range")),
        ),
    ];
    for demand in &demands {
        client
            .record_resident_range(ResidentRange::new(demand.segment(), demand.range()))
            .unwrap_or_else(|_| panic!("manifest range is resident"));
    }
    let selection = DemandSelection::new(&demands).unwrap_or_else(|_| panic!("sorted demands"));
    let mut scratch = [None; 2];
    let mut output = [None; 2];
    let terminal = client
        .select_local(
            selection,
            SyncCancellation::Continue,
            SelectionScratch::new(&mut scratch),
            SelectionOutput::new(&mut output),
        )
        .unwrap_or_else(|_| panic!("selection is admitted"));
    let proof = match terminal {
        LocalQueryTerminal::Complete { proof, .. } => proof,
        other => panic!("expected complete proof, got {other:?}"),
    };
    assert_eq!(
        client.complete_sync(proof, SyncCancellation::Continue),
        SyncTerminal::LocalReady { base }
    );
    assert_eq!(client.phase(), ClientSyncPhase::LocalReady { base });
}

#[test]
fn exact_missing_ranges_and_foreign_authority_are_explicit() {
    let mut client = ClientIndex::new();
    let base = accept_initial(&mut client);
    let resident = ResidentRange::new(
        exact(1),
        SegmentRange::new(10, length(20)).unwrap_or_else(|_| panic!("valid range")),
    );
    client
        .record_resident_range(resident)
        .unwrap_or_else(|_| panic!("resident range is in manifest"));
    let demands = [
        SegmentDemand::new(exact(1), resident.range()),
        SegmentDemand::new(
            lexical(1),
            SegmentRange::new(0, length(20)).unwrap_or_else(|_| panic!("valid range")),
        ),
    ];
    let selection = DemandSelection::new(&demands).unwrap_or_else(|_| panic!("sorted demands"));
    let mut scratch = [None; 2];
    let mut output = [None; 2];
    let terminal = client
        .select_local(
            selection,
            SyncCancellation::Continue,
            SelectionScratch::new(&mut scratch),
            SelectionOutput::new(&mut output),
        )
        .unwrap_or_else(|_| panic!("manifest-bounded demands"));
    assert!(matches!(
        terminal,
        LocalQueryTerminal::Partial { missing: 1, .. }
    ));
    assert!(matches!(output[0], Some(LocalSelection::Present(_))));
    assert!(matches!(output[1], Some(LocalSelection::Missing(_))));

    let no_candidates = [];
    let foreign = RemoteSearch::new(
        RemoteGeneration::new(generation(2), snapshot(2)),
        &no_candidates,
    );
    assert!(matches!(
        client.accept_remote_search(&foreign, &mut []),
        Err(ClientSyncError::RemoteSearchGenerationMismatch { expected, observed })
            if expected == base.generation() && observed == generation(2)
    ));
    let foreign_snapshot = RemoteSearch::new(
        RemoteGeneration::new(base.generation(), snapshot(2)),
        &no_candidates,
    );
    assert!(matches!(
        client.accept_remote_search(&foreign_snapshot, &mut []),
        Err(ClientSyncError::RemoteSearchSnapshotMismatch { expected, observed })
            if expected == base.snapshot() && observed == snapshot(2)
    ));
}

#[test]
fn local_overlay_survives_remote_advance_reconnect_and_tombstone() {
    let mut client = ClientIndex::new();
    accept_initial(&mut client);
    let key = OverlayKey::new(object(42));
    client
        .record_local_delta(LocalDelta::upsert(key, object(43)))
        .unwrap_or_else(|_| panic!("first local edit"));
    assert!(matches!(
        client.overlay(key),
        OverlayObservation::Upsert { .. }
    ));
    client.disconnect();
    assert!(matches!(
        client.phase(),
        ClientSyncPhase::Disconnected { .. }
    ));
    client.reconnect();
    assert!(matches!(
        client.phase(),
        ClientSyncPhase::Synchronizing { .. }
    ));
    let previous = RemoteGeneration::new(generation(1), manifest(1, 1).generation().snapshot());
    assert!(matches!(
        client.accept_manifest(
            RemoteManifest::advance(previous, manifest(2, 2)),
            SyncCancellation::Continue,
        ),
        SyncTerminal::AcceptedAdvance { .. }
    ));
    client
        .record_local_delta(LocalDelta::tombstone(key))
        .unwrap_or_else(|_| panic!("tombstone local edit"));
    assert_eq!(client.overlay(key), OverlayObservation::Tombstone);

    let mut effective = [None];
    let candidates = [RemoteSearchCandidate::new(key, document(99))];
    let search = RemoteSearch::new(
        RemoteGeneration::new(generation(2), manifest(2, 2).generation().snapshot()),
        &candidates,
    );
    assert_eq!(
        client.accept_remote_search(&search, &mut effective),
        Ok(RemoteSearchTerminal::Candidates {
            base: BaseGeneration::new(generation(2), manifest(2, 2).generation().snapshot()),
            returned: 0,
        })
    );
    assert!(
        effective[0].is_none(),
        "tombstones must suppress remote candidates"
    );
}
