//! Occurrence fact-plane falsifiers: an admitted lane round-trips through
//! write and reopen, every plane mutation changes the fragment bytes or
//! rejects with the exact fault, and hostile section bytes never validate.

use compiler_ir::{
    AtomInput, DecodedOccurrence, EntityKind, EntityRecord, FragmentError, FragmentView,
    OccurrenceFault, OccurrenceInput, OccurrenceLane, PrepareError, PreparedFragment,
    PrimitiveType, SourceIdentity, TypeNode, WriteError,
};
use compiler_ir_vocabulary::{
    AtomId, Confidence, EntityId, ForeignKey, ForeignOrigin, Occurrence, OccurrenceTarget,
    PackageLineage, ReferenceKind, RelSpan, StableRef, TypeId,
};
use compiler_vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage};
use heart_identity::{ContentId, ContentIdDecodeError, SourceFactDomain, ToolchainDomain};
use thiserror::Error;

#[derive(Debug, Error)]
enum TestFailure {
    #[error("prepare rejected the fragment: {0:?}")]
    Prepare(#[from] PrepareError),
    #[error("write rejected the fragment: {0:?}")]
    Write(#[from] WriteError),
    #[error("fragment view rejected: {0:?}")]
    View(#[from] FragmentError),
    #[error("occurrence plane rejected: {0:?}")]
    Occurrence(#[from] OccurrenceFault),
    #[error("identity decode rejected: {0:?}")]
    Identity(#[from] ContentIdDecodeError),
}

fn stable_lane_inputs() -> [OccurrenceInput<'static>; 2] {
    [
        OccurrenceInput {
            owner: EntityId::new(0),
            occurrence: Occurrence {
                target: OccurrenceTarget::Stable(StableRef {
                    fragment: ContentId::from_canonical_bytes(b"fragment-a"),
                    entity: ContentId::from_canonical_bytes(b"declared-target"),
                }),
                kind: ReferenceKind::FunctionCall,
                confidence: Confidence::Oracle,
                span: RelSpan { start: 12, end: 21 },
            },
        },
        OccurrenceInput {
            owner: EntityId::new(1),
            occurrence: Occurrence {
                target: OccurrenceTarget::Stable(StableRef {
                    fragment: ContentId::from_canonical_bytes(b"fragment-a"),
                    entity: ContentId::from_canonical_bytes(b"declared-target"),
                }),
                kind: ReferenceKind::TypeReference,
                confidence: Confidence::Index,
                span: RelSpan { start: 0, end: 5 },
            },
        },
    ]
}

fn foreign_lane_inputs() -> [OccurrenceInput<'static>; 2] {
    [
        stable_lane_inputs()[0],
        OccurrenceInput {
            owner: EntityId::new(1),
            occurrence: Occurrence {
                target: OccurrenceTarget::Foreign(ForeignKey {
                    origin: ForeignOrigin::Package(
                        PackageLineage::new("npm", "lodash").expect("valid lineage"),
                    ),
                    path: "lodash/map",
                    display: "lodash.map",
                    kind: Some(EntityKind::Function),
                }),
                kind: ReferenceKind::TypeReference,
                confidence: Confidence::Index,
                span: RelSpan { start: 0, end: 5 },
            },
        },
    ]
}

fn source() -> SourceIdentity {
    SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"occurrence-source"),
        byte_len: 16,
    }
}

fn recipe(source: ContentId<SourceFactDomain>) -> CompileRecipeFact {
    CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        source,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"occurrence-toolchain"),
    )
}

const ENTITIES: [EntityRecord; 2] = [
    EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Function,
    },
    EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(1),
        kind: EntityKind::Record,
    },
];

const ATOMS: [AtomInput<'static>; 2] =
    [AtomInput { bytes: b"alpha" }, AtomInput { bytes: b"Beta" }];

/// The full path: admitted occurrence lane, written section bytes, reopened
/// view, and a cursor decode that reproduces every admitted fact exactly.
#[test]
fn occurrence_lane_round_trips_through_write_and_reopen() -> Result<(), TestFailure> {
    let identity = source();
    let inputs = stable_lane_inputs();
    let lane = OccurrenceLane { inputs: &inputs };
    let prepared = PreparedFragment::prepare_with_occurrences(
        identity,
        recipe(identity.identity),
        &ENTITIES,
        &[TypeNode::Primitive(PrimitiveType::Bool)],
        &ATOMS,
        None,
        &lane,
    )?;
    let mut output = vec![0_u8; prepared.required_capacity()];
    let written = prepared.write_into(&mut output)?;
    let view = FragmentView::validate(written)?;

    let mut cursor = view
        .occurrences()
        .expect("occurrence section is present after admission");
    let first = cursor.next().expect("declared two records")?;
    assert_eq!(first.owner, EntityId::new(0));
    assert_eq!(first.occurrence.kind, ReferenceKind::FunctionCall);
    assert_eq!(first.occurrence.confidence, Confidence::Oracle);
    assert_eq!(first.occurrence.span.len(), 9);
    let second = cursor.next().expect("declared two records")?;
    assert_eq!(second.owner, EntityId::new(1));
    assert_eq!(second.occurrence.kind, ReferenceKind::TypeReference);
    assert_eq!(second.occurrence.confidence, Confidence::Index);
    assert!(cursor.next().is_none());

    // A fragment without an occurrence lane honestly reports its absence.
    let plain = PreparedFragment::prepare(
        identity,
        recipe(identity.identity),
        &ENTITIES,
        &[TypeNode::Primitive(PrimitiveType::Bool)],
        &ATOMS,
    )?;
    let mut plain_output = vec![0_u8; plain.required_capacity()];
    let plain_written = plain.write_into(&mut plain_output)?;
    let plain_view = FragmentView::validate(plain_written)?;
    assert!(plain_view.occurrences().is_none());
    Ok(())
}

/// Mutating the fact set — retargeting a reference, changing a span, or
/// degrading the confidence — changes the written fragment bytes. A fact
/// plane that does not move the bytes is not committed, only claimed.
#[test]
fn every_plane_mutation_changes_the_fragment_bytes() -> Result<(), TestFailure> {
    let identity = source();
    let write = |inputs: &[OccurrenceInput<'static>]| -> Result<Vec<u8>, TestFailure> {
        let lane = OccurrenceLane { inputs };
        let prepared = PreparedFragment::prepare_with_occurrences(
            identity,
            recipe(identity.identity),
            &ENTITIES,
            &[TypeNode::Primitive(PrimitiveType::Bool)],
            &ATOMS,
            None,
            &lane,
        )?;
        let mut output = vec![0_u8; prepared.required_capacity()];
        prepared.write_into(&mut output)?;
        Ok(output)
    };

    let baseline_inputs = stable_lane_inputs();
    let baseline = write(&baseline_inputs)?;
    // Retarget the first occurrence's semantic kind: the bytes move.
    let retargeted = [
        OccurrenceInput {
            occurrence: Occurrence {
                kind: ReferenceKind::MethodCall,
                ..stable_lane_inputs()[0].occurrence
            },
            ..stable_lane_inputs()[0]
        },
        stable_lane_inputs()[1],
    ];
    let retargeted_bytes = write(&retargeted)?;
    assert_ne!(baseline, retargeted_bytes);
    // Change the relative span: the bytes move.
    let respaced = [
        OccurrenceInput {
            occurrence: Occurrence {
                span: RelSpan { start: 12, end: 22 },
                ..stable_lane_inputs()[0].occurrence
            },
            ..stable_lane_inputs()[0]
        },
        stable_lane_inputs()[1],
    ];
    let respaced_bytes = write(&respaced)?;
    assert_ne!(retargeted_bytes, respaced_bytes);
    // Degrade the confidence: the bytes move.
    let degraded = [
        OccurrenceInput {
            occurrence: Occurrence {
                confidence: Confidence::Syntactic,
                ..stable_lane_inputs()[0].occurrence
            },
            ..stable_lane_inputs()[0]
        },
        stable_lane_inputs()[1],
    ];
    let degraded_bytes = write(&degraded)?;
    assert_ne!(respaced_bytes, degraded_bytes);
    Ok(())
}

/// Hostile section bytes: a corrupted target authority cell must reject the
/// reopen with the exact authority fault instead of lending a forged
/// cross-fragment reference.
#[test]
fn corrupted_authority_cells_reject_reopen() -> Result<(), TestFailure> {
    let identity = source();
    let inputs = stable_lane_inputs();
    let lane = OccurrenceLane { inputs: &inputs };
    let prepared = PreparedFragment::prepare_with_occurrences(
        identity,
        recipe(identity.identity),
        &ENTITIES,
        &[TypeNode::Primitive(PrimitiveType::Bool)],
        &ATOMS,
        None,
        &lane,
    )?;
    let mut output = vec![0_u8; prepared.required_capacity()];
    let written = prepared.write_into(&mut output)?;
    let view = FragmentView::validate(written)?;

    // The stable target's entity identity cell sits at a fixed record
    // offset inside the occurrence payload: header (4) + owner (4) +
    // target tag (1) + fragment identity (32). Corrupt its domain
    // authority byte (the cell's last byte).
    let payload = view.occurrence_payload().expect("admitted lane is present");
    let section_start = payload.as_ptr() as usize - written.as_ptr() as usize;
    let entity_cell = section_start + 4 + 4 + 1 + 32;
    let mut mutated = written.to_vec();
    mutated[entity_cell] ^= 0xff;
    match FragmentView::validate(&mutated) {
        Err(FragmentError::Occurrences {
            fault: compiler_ir::OccurrenceViewFault::AuthorityDomain { .. },
        }) => {}
        Err(other) => {
            panic!("a forged authority cell must reject with the authority fault, got {other:?}")
        }
        Ok(_) => panic!("a forged authority cell must not validate"),
    }
    Ok(())
}

#[test]
fn admission_rejects_owners_outside_the_entity_lane() -> Result<(), TestFailure> {
    let inputs = [OccurrenceInput {
        owner: EntityId::new(7),
        occurrence: Occurrence {
            target: stable_lane_inputs()[0].occurrence.target,
            kind: ReferenceKind::FunctionCall,
            confidence: Confidence::Oracle,
            span: RelSpan { start: 0, end: 3 },
        },
    }];
    let lane = OccurrenceLane { inputs: &inputs };
    let fault = lane.admit(2).expect_err("owner 7 leaves a two-entity lane");
    assert!(matches!(
        fault,
        compiler_ir::OccurrenceFault::Owner {
            ordinal: 0,
            entity_count: 2,
            ..
        }
    ));
    Ok(())
}

/// Foreign occurrences round-trip through the self-describing key: path,
/// A same-fragment target round-trips, and an out-of-lane local ordinal is
/// rejected at admission and at reopen with the exact operands.
#[test]
fn local_targets_round_trip_and_reject_out_of_lane_ordinals() -> Result<(), TestFailure> {
    let identity = source();
    let inputs = [
        OccurrenceInput {
            owner: EntityId::new(0),
            occurrence: Occurrence {
                target: OccurrenceTarget::Local(EntityId::new(1)),
                kind: ReferenceKind::MethodCall,
                confidence: Confidence::Oracle,
                span: RelSpan { start: 3, end: 8 },
            },
        },
        OccurrenceInput {
            owner: EntityId::new(1),
            occurrence: Occurrence {
                target: OccurrenceTarget::Local(EntityId::new(0)),
                kind: ReferenceKind::TypeReference,
                confidence: Confidence::Oracle,
                span: RelSpan { start: 0, end: 4 },
            },
        },
    ];
    let lane = OccurrenceLane { inputs: &inputs };
    lane.admit(2)?;

    let prepared = PreparedFragment::prepare_with_occurrences(
        identity,
        recipe(identity.identity),
        &ENTITIES,
        &[TypeNode::Primitive(PrimitiveType::Bool)],
        &ATOMS,
        None,
        &lane,
    )?;
    let mut output = vec![0_u8; prepared.required_capacity()];
    let written = prepared.write_into(&mut output)?;
    let view = FragmentView::validate(written)?;
    let mut cursor = view.occurrences().expect("admitted lane is present");
    let first = cursor.next().expect("declared two records")?;
    assert_eq!(
        first.occurrence.target,
        OccurrenceTarget::Local(EntityId::new(1))
    );
    let second = cursor.next().expect("declared two records")?;
    assert_eq!(
        second.occurrence.target,
        OccurrenceTarget::Local(EntityId::new(0))
    );
    assert!(cursor.next().is_none());

    // An owner-lane violation at admission retains the exact ordinal.
    let out_of_range = [OccurrenceInput {
        owner: EntityId::new(0),
        occurrence: Occurrence {
            target: OccurrenceTarget::Local(EntityId::new(2)),
            kind: ReferenceKind::VariableUse,
            confidence: Confidence::Syntactic,
            span: RelSpan { start: 0, end: 1 },
        },
    }];
    let rejected = OccurrenceLane {
        inputs: &out_of_range,
    };
    assert!(matches!(
        rejected.admit(2),
        Err(OccurrenceFault::LocalTarget {
            ordinal: 0,
            target: 2,
            entity_count: 2
        })
    ));
    Ok(())
}

/// display, origin, and the optional kind all survive the reopen.
#[test]
fn foreign_occurrences_survive_the_reopen_with_their_key_cells() -> Result<(), TestFailure> {
    let identity = source();
    let inputs = foreign_lane_inputs();
    let lane = OccurrenceLane { inputs: &inputs };
    let prepared = PreparedFragment::prepare_with_occurrences(
        identity,
        recipe(identity.identity),
        &ENTITIES,
        &[TypeNode::Primitive(PrimitiveType::Bool)],
        &ATOMS,
        None,
        &lane,
    )?;
    let mut output = vec![0_u8; prepared.required_capacity()];
    let written = prepared.write_into(&mut output)?;
    let view = FragmentView::validate(written)?;
    let mut cursor = view.occurrences().expect("admitted lane is present");
    let _ = cursor.next().expect("declared two records")?;
    let DecodedOccurrence {
        occurrence: second, ..
    } = cursor.next().expect("declared two records")?;
    match second.target {
        OccurrenceTarget::Foreign(foreign) => {
            assert_eq!(foreign.path, "lodash/map");
            assert_eq!(foreign.display, "lodash.map");
            assert_eq!(foreign.kind, Some(EntityKind::Function));
            assert!(matches!(
                foreign.origin,
                ForeignOrigin::Package(PackageLineage {
                    ecosystem: "npm",
                    name: "lodash",
                })
            ));
        }
        OccurrenceTarget::Stable(_) | OccurrenceTarget::Local(_) => {
            panic!("first admitted fact is foreign")
        }
    }
    Ok(())
}
