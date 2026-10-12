//! Canonical row-size fixtures, not a claim of installed HTTPie admission.
#![allow(clippy::expect_used)]

use super::native_history_boundary_policy;
use backend_semantic::ir::{
    BorrowedTree, CanonicalPlaneStreamError, CanonicalSemanticPlaneSegmentRef,
    CanonicalSemanticPlaneSegmentSink, ConcreteType, CorePayloadHash, DeclarationFamilyId,
    EntityAuthorityFacts, EntityVersion, FactAvailability, Ir, IrBuilder, ItemKind,
    JumboRopeLimits, MAX_SEMANTIC_SEGMENT_BYTES, ParentageAuthority, SemanticBuildIdentity,
    SemanticInputWitness, SemanticIrPlane, SemanticPlaneKind, SemanticPlaneRecordError,
    SemanticPlaneSegment, SemanticPlaneSegmentBoundaryPolicy, TreeItemInput, TypeExpr, TypesRows,
    VariantFingerprint, Visibility, decode_semantic_plane_segment, encode_canonical_plane_family,
    stream_canonical_plane_family_with_stable_key_anchors,
    verify_canonical_semantic_plane_segment_boundaries,
};
use backend_semantic::vocabulary::{LanguageProfile, PythonVersion, Stage};
use backend_version::ScopeRoot;

// Exact existing SPIR framing: header, key/tag/length, atom length.
const ATOM_ROW_FRAME_BYTES: usize = (4 + 2 + 1 + 4) + (32 + 1 + 4) + 4;

fn typed_image(atom: &[u8]) -> Ir {
    let mut builder = IrBuilder::new();
    builder
        .set_language_profile(LanguageProfile::Python(PythonVersion::Python312))
        .expect("fixture Python profile");
    let atom = builder.intern_atom(atom).expect("retained type atom");
    let semantic_type = builder
        .intern_type(TypeExpr::Concrete(ConcreteType::Parameter(atom)))
        .expect("referenced type node");
    let version = EntityVersion {
        family: DeclarationFamilyId::from_raw([0x31; 16]),
        variant: VariantFingerprint::from_raw([0x32; 16]),
        core_payload: CorePayloadHash::from_canonical_bytes(b"typed"),
    };
    let item = TreeItemInput {
        name: b"typed",
        anonymous_callable_anchor: None,
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        authority: EntityAuthorityFacts {
            parentage: ParentageAuthority::Root,
            members: FactAvailability::Captured,
            documentation: FactAvailability::Captured,
            attributes: FactAvailability::Captured,
            visibility: FactAvailability::Captured,
            semantic_type: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        },
        parent: None,
        semantic_type: Some(semantic_type),
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: None,
    };
    builder
        .add_borrowed_tree(BorrowedTree {
            versions: &[version],
            items: &[item],
            links: &[],
        })
        .expect("real referenced type tree");
    builder.finish().expect("fixture IR closure")
}

fn input() -> SemanticInputWitness {
    SemanticInputWitness::claimed([0x41; 32], ScopeRoot::from_bytes([0x42; 32]))
}

#[derive(Default)]
struct CapturedSegments(Vec<(SemanticPlaneSegment, Vec<u8>)>);

impl CanonicalSemanticPlaneSegmentSink for CapturedSegments {
    type Error = SemanticPlaneRecordError;

    fn write_segment(
        &mut self,
        segment: CanonicalSemanticPlaneSegmentRef<'_>,
    ) -> Result<(), Self::Error> {
        segment.validate()?;
        self.0.push((segment.metadata()?, segment.bytes().to_vec()));
        Ok(())
    }
}

fn capture(
    image: &Ir,
    policy: SemanticPlaneSegmentBoundaryPolicy,
) -> Result<CapturedSegments, CanonicalPlaneStreamError<SemanticPlaneRecordError>> {
    let mut captured = CapturedSegments::default();
    stream_canonical_plane_family_with_stable_key_anchors(
        image,
        &TypesRows,
        input(),
        policy,
        &mut captured,
    )?;
    let descriptors = captured
        .0
        .iter()
        .map(|(descriptor, _)| *descriptor)
        .collect::<Vec<_>>();
    let payloads = captured
        .0
        .iter()
        .map(|(_, payload)| payload.as_slice())
        .collect::<Vec<_>>();
    let rows = descriptors
        .iter()
        .map(|descriptor| u64::from(descriptor.row_count()))
        .sum();
    verify_canonical_semantic_plane_segment_boundaries(
        SemanticPlaneKind::Ir(SemanticIrPlane::Types),
        &descriptors,
        &payloads,
        policy,
        rows,
    )?;
    Ok(captured)
}

fn rows(segments: &CapturedSegments) -> Vec<([u8; 32], u8, Vec<u8>)> {
    segments
        .0
        .iter()
        .flat_map(|(descriptor, payload)| {
            decode_semantic_plane_segment(
                SemanticPlaneKind::Ir(SemanticIrPlane::Types),
                descriptor,
                payload,
            )
            .expect("strict-decoded complete segment")
            .records()
            .map(|row| (row.key(), row.tag(), row.payload().to_vec()))
            .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn native_history_target_preserves_an_8643_byte_typed_row() {
    let atom = vec![b'a'; 8643 - ATOM_ROW_FRAME_BYTES];
    let image = typed_image(&atom);
    let old = SemanticPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(512, 1024, 4096)
        .expect("old production policy");
    assert!(matches!(
        capture(&image, old),
        Err(CanonicalPlaneStreamError::Encoding(
            SemanticPlaneRecordError::OversizedRow {
                observed: 8643,
                maximum: 4096
            }
        ))
    ));
    let policy = native_history_boundary_policy().expect("production boundary policy");
    assert_eq!(policy.target_bytes(), 4096);
    assert_eq!(policy.maximum_bytes(), MAX_SEMANTIC_SEGMENT_BYTES);
    let captured = capture(&image, policy).expect("target-oversize complete typed row");
    assert!(
        captured
            .0
            .iter()
            .any(|(_, payload)| payload.len() > policy.target_bytes())
    );
    assert!(
        captured
            .0
            .iter()
            .all(|(_, payload)| payload.len() <= policy.maximum_bytes())
    );
    let actual_rows = rows(&captured);
    assert_eq!(
        actual_rows
            .iter()
            .filter(|(_, _, payload)| payload.get(4..) == Some(atom.as_slice()))
            .count(),
        1
    );
    let legacy =
        encode_canonical_plane_family(&image, &TypesRows, input(), MAX_SEMANTIC_SEGMENT_BYTES)
            .expect("same complete rows under legacy segmentation");
    let legacy = CapturedSegments(
        legacy
            .iter()
            .map(|segment| {
                (
                    segment.metadata().expect("legacy descriptor"),
                    segment.bytes().to_vec(),
                )
            })
            .collect(),
    );
    assert_eq!(
        actual_rows,
        rows(&legacy),
        "segments never split or rewrite canonical rows"
    );
    let repeated = capture(&image, policy).expect("deterministic repeated segmentation");
    assert_eq!(captured.0, repeated.0);
}

#[test]
fn native_history_target_retains_exact_hard_row_ceiling() {
    let policy = native_history_boundary_policy().expect("production boundary policy");
    for size in [
        policy.target_bytes() - 1,
        policy.target_bytes(),
        policy.target_bytes() + 1,
        policy.maximum_bytes() - 1,
        policy.maximum_bytes(),
    ] {
        let atom = vec![b'a'; size - ATOM_ROW_FRAME_BYTES];
        let captured = capture(&typed_image(&atom), policy).expect("intact row within hard limit");
        // At the hard limit the row must occupy its own segment. Smaller
        // intact rows may share a segment with preceding small rows.
        if size >= policy.maximum_bytes() - 1 {
            assert!(captured.0.iter().any(|(_, payload)| payload.len() == size));
        }
        assert_eq!(
            rows(&captured)
                .iter()
                .filter(|(_, _, payload)| payload.get(4..) == Some(atom.as_slice()))
                .count(),
            1
        );
        assert!(
            captured
                .0
                .iter()
                .all(|(_, payload)| payload.len() <= policy.maximum_bytes())
        );
    }
    let atom = vec![b'a'; policy.maximum_bytes() + 1 - ATOM_ROW_FRAME_BYTES];
    assert!(
        matches!(capture(&typed_image(&atom), policy), Err(CanonicalPlaneStreamError::Encoding(
        SemanticPlaneRecordError::OversizedRow { observed, maximum }
    )) if observed == MAX_SEMANTIC_SEGMENT_BYTES + 1 && maximum == MAX_SEMANTIC_SEGMENT_BYTES)
    );
}

#[test]
fn native_history_target_verifies_all_families_without_promoting_input() {
    let directory = tempfile::tempdir().expect("isolated CAS fixture");
    let store = backend_store::FileStore::open(directory.path(), 8 * 1024 * 1024)
        .expect("bounded real FileStore");
    let policy = native_history_boundary_policy().expect("production boundary policy");
    let policies = backend_replication::SemanticTypedPlaneBoundaryPoliciesV3::new(
        policy, policy, policy, policy, policy, policy, policy,
    );
    let build = SemanticBuildIdentity::new(
        [0x11; 32],
        [0x12; 32],
        LanguageProfile::Python(PythonVersion::Python312),
        Stage::LowerIr,
        [0x13; 32],
        [0x14; 32],
        [0x15; 32],
        [0x16; 32],
    );
    let atom = vec![b'a'; 8643 - ATOM_ROW_FRAME_BYTES];
    let produce = |image: &Ir| {
        backend_replication::produce_semantic_typed_plane_content_v3(
            &store,
            image,
            build,
            input(),
            policies,
            backend_semantic::ir::SemanticTypedPlaneVerificationTierV2::Standard,
            JumboRopeLimits::default(),
        )
        .expect("durable seven-family producer and independent payload verifier")
    };
    let first = produce(&typed_image(&atom));
    assert_eq!(first.manifest().families().len(), 7);
    assert!(
        first
            .manifest()
            .families()
            .iter()
            .all(|family| family.boundary_policy() == policy)
    );
    assert_eq!(first.input_witness(), input());
    assert_eq!(
        first.manifest().input_claim().coverage_state(),
        input().coverage().state()
    );
    assert!(!first.input_witness().coverage().is_authorized_complete());
    assert!(
        first
            .segment_admissions()
            .iter()
            .any(|receipt| receipt.payload_bytes() > 4096)
    );
    assert!(
        first
            .segment_admissions()
            .iter()
            .all(|receipt| receipt.payload_bytes() <= MAX_SEMANTIC_SEGMENT_BYTES as u64)
    );
    assert!(first.verifier_io_metrics().segment_object_reads() > 0);
    let repeated = produce(&typed_image(&atom));
    assert_eq!(
        first.verified_content(),
        repeated.verified_content(),
        "no-op content and generation identity remains exact"
    );
    let mut edited_atom = atom;
    edited_atom[0] = b'b';
    let edited = produce(&typed_image(&edited_atom));
    assert_ne!(
        first.verified_content().content_root(),
        edited.verified_content().content_root(),
        "retained row edit changes content identity"
    );
    assert_ne!(
        first.verified_content().generation_root(),
        edited.verified_content().generation_root()
    );
}
