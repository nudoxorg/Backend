//! Public borrowed-query bridge checks for the thin local client wrappers.

use backend_semantic::ir::EntityId;
use backend_version::{ArtifactId, GenerationId, IrFragmentDomain, IrFragmentEncoding};
use interface_core::{query_local_exact, query_local_lexical};
use backend_semantic::index_core::{
    EntityArtifactIdentity, EntityDocumentId, ExactResolution, ExactRow, ExactSegment,
    ExactTerminal, IndexSnapshot, LexicalRow, LexicalScore, LexicalSegment, LexicalSnapshotHit,
    LexicalTerminal, LexicalTopK,
};

type TestResult<T = ()> = Result<T, String>;

fn document(entity: u32) -> EntityDocumentId {
    EntityDocumentId {
        artifact: EntityArtifactIdentity::Compact(
            ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
                b"interface-local-query-bridge",
            ),
        ),
        entity: EntityId::new(entity),
    }
}

fn generation() -> GenerationId {
    GenerationId::from_canonical_bytes(b"interface-local-query-generation")
}

#[test]
fn local_exact_wrapper_borrows_an_admitted_segment_and_returns_its_hit() -> TestResult {
    let rows = [ExactRow::present(b"alpha", b"exact-alpha")];
    let segment = ExactSegment::new(&rows).map_err(|_| "fixed exact test rows are invalid")?;
    let selected = [segment.id];
    let snapshot = IndexSnapshot::new(generation(), &selected, &[])
        .map_err(|_| "fixed exact snapshot is not representable")?;
    let segments = [segment];

    let terminal = query_local_exact(snapshot, &segments, &[], b"alpha")
        .map_err(|_| "wrapper rejected an admitted exact manifest")?;
    assert!(matches!(
        terminal,
        ExactTerminal::Complete {
            resolution: ExactResolution::Present { value, .. },
            ..
        } if value == b"exact-alpha"
    ));
    Ok(())
}

#[test]
fn local_lexical_wrapper_preserves_hit_and_missing_segment_terminal() -> TestResult {
    let rows = [LexicalRow::new(
        b"needle",
        document(7),
        LexicalScore::from(9),
    )];
    let segment = LexicalSegment::new(&rows).map_err(|_| "fixed lexical test rows are invalid")?;
    let missing = backend_semantic::index_core::LexicalSegmentId::from_canonical_bytes(b"missing-local");
    let selected = [segment.id, missing];
    let snapshot = IndexSnapshot::new(generation(), &[], &selected)
        .map_err(|_| "fixed lexical snapshot is not representable")?;
    let segments = [segment];
    let missing_segments = [missing];
    let mut scratch = [None];
    let mut output = [LexicalSnapshotHit::new(
        segment.id,
        b"placeholder",
        document(0),
        LexicalScore::from(0),
    )];
    let top_k = LexicalTopK::new(1).map_err(|_| "one must be an in-bounds top-k")?;

    let terminal = query_local_lexical(
        snapshot,
        &segments,
        &missing_segments,
        b"needle",
        top_k,
        &mut scratch,
        &mut output,
    )
    .map_err(|_| "wrapper rejected an admitted partial lexical manifest")?;
    let LexicalTerminal::Partial { hits, missing, .. } = terminal else {
        return Err("local wrapper did not retain the missing-segment terminal".to_owned());
    };
    assert_eq!(missing, missing_segments.as_slice());
    let Some(hit) = hits.first() else {
        return Err("local wrapper omitted its admitted lexical hit".to_owned());
    };
    assert_eq!(hit.document, document(7));
    assert_eq!(hit.term, b"needle");
    Ok(())
}
