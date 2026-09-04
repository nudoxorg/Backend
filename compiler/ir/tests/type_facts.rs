//! Type-fact wire falsifiers.  Local references are backward-only, so a
//! cycle detector is forbidden: forward and self references are rejected at
//! admission and again when untrusted bytes are reopened.

use compiler_ir::{
    AtomInput, EntityKind, EntityRecord, FragmentError, FragmentSemantics, FragmentView,
    PrepareError, PreparedFragment, PrimitiveType, RecipeFact, SourceIdentity, TypeFactFault,
    TypeFactInput, TypeFactLane, TypeNode, WriteError,
};
use compiler_ir_vocabulary::{
    AtomId, EntityId, ExternalEntityRef, ListSpan, NominalRef, SemanticTypeRecord, SemanticTypeTag,
    TypeChildTarget, TypeRef,
};
use compiler_vocabulary::{LanguageProfile, NativeTool, RustEdition, Stage};
use core::num::ParseIntError;
use heart_identity::{ContentId, IrFragmentDomain, SourceFactDomain, ToolchainDomain};
use thiserror::Error;

#[derive(Debug, Error)]
enum TestFailure {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    View(#[from] FragmentError),
    #[error(transparent)]
    Write(#[from] WriteError),
    #[error("type-fact admission failed: {0:?}")]
    Admission(TypeFactFault),
    #[error(transparent)]
    Fixture(#[from] ParseIntError),
}

fn source() -> SourceIdentity {
    SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"type-facts-source"),
        byte_len: 17,
    }
}

fn recipe() -> RecipeFact {
    let profile = LanguageProfile::Rust(RustEdition::Rust2024);
    RecipeFact {
        identity: compiler_vocabulary::CompileRecipeFact::derive(
            profile,
            Stage::LowerIr,
            NativeTool::Rustc,
            source().identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"type-facts-toolchain"),
        )
        .identity,
        profile,
        stage: Stage::LowerIr,
        tool: NativeTool::Rustc,
        toolchain: ContentId::<ToolchainDomain>::from_canonical_bytes(b"type-facts-toolchain"),
    }
}

fn records() -> [TypeFactInput<'static>; 3] {
    [
        TypeFactInput {
            owner: EntityId::new(0),
            record: SemanticTypeRecord::leaf(SemanticTypeTag::SelfType),
        },
        TypeFactInput {
            owner: EntityId::new(0),
            record: SemanticTypeRecord {
                tag: SemanticTypeTag::Nominal,
                payload0: 0,
                payload1: 0,
                text: None,
                text2: None,
                nominal: Some(NominalRef::Local(EntityId::new(0))),
                children: ListSpan::new(0, 0),
            },
        },
        TypeFactInput {
            owner: EntityId::new(0),
            record: SemanticTypeRecord {
                tag: SemanticTypeTag::TypeVar,
                payload0: 0,
                payload1: 0,
                text: Some(b"T"),
                text2: None,
                nominal: None,
                children: ListSpan::new(0, 0),
            },
        },
    ]
}

fn write<'bytes>(lane: &TypeFactLane<'bytes>) -> Result<Vec<u8>, TestFailure> {
    let entities = [EntityRecord {
        semantic_type: compiler_ir_vocabulary::TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Function,
    }];
    let types = [TypeNode::Primitive(PrimitiveType::Bool)];
    let atoms = [AtomInput { bytes: b"entity" }];
    let prepared = PreparedFragment::prepare_with_semantics(
        source(),
        recipe(),
        &entities,
        &types,
        &atoms,
        FragmentSemantics {
            type_facts: Some(lane),
            ..FragmentSemantics::default()
        },
    )?;
    let mut bytes = vec![0; prepared.required_capacity()];
    prepared.write_into(&mut bytes)?;
    // Keep the legacy assertions below on the frozen schema-1 grammar while
    // the production writer now emits schema 2.
    let section_count = u16::from_le_bytes([bytes[6], bytes[7]]);
    let mut type_offset = 0;
    for ordinal in 0..usize::from(section_count) {
        let entry = 12 + ordinal * 16;
        if u16::from_le_bytes([bytes[entry], bytes[entry + 1]]) == 9 {
            type_offset = u32::from_le_bytes([
                bytes[entry + 8],
                bytes[entry + 9],
                bytes[entry + 10],
                bytes[entry + 11],
            ]) as usize;
            break;
        }
    }
    bytes.drain(type_offset + 4..type_offset + 8);
    bytes[4..6].copy_from_slice(&1_u16.to_le_bytes());
    let new_length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    bytes[8..12].copy_from_slice(&new_length.to_le_bytes());
    for ordinal in 0..usize::from(section_count) {
        let entry = 12 + ordinal * 16;
        let offset = u32::from_le_bytes([
            bytes[entry + 8],
            bytes[entry + 9],
            bytes[entry + 10],
            bytes[entry + 11],
        ]) as usize;
        if offset > type_offset {
            bytes[entry + 8..entry + 12]
                .copy_from_slice(&u32::try_from(offset - 4).unwrap_or(u32::MAX).to_le_bytes());
        }
        if u16::from_le_bytes([bytes[entry], bytes[entry + 1]]) == 9 {
            let count = u16::from_le_bytes([bytes[entry + 4], bytes[entry + 5]]);
            bytes[entry + 4..entry + 6].copy_from_slice(&(count - 4).to_le_bytes());
            let length = u32::from_le_bytes([
                bytes[entry + 12],
                bytes[entry + 13],
                bytes[entry + 14],
                bytes[entry + 15],
            ]);
            bytes[entry + 12..entry + 16].copy_from_slice(&(length - 4).to_le_bytes());
        }
    }
    Ok(bytes)
}

#[test]
fn schema_three_rejects_invalid_computed_rows_before_publication() {
    let declared = [TypeFactInput {
        owner: EntityId::new(0),
        record: SemanticTypeRecord::leaf(SemanticTypeTag::SelfType),
    }];

    let invalid_owner = [TypeFactInput {
        owner: EntityId::new(1),
        record: SemanticTypeRecord::leaf(SemanticTypeTag::SelfType),
    }];
    let lane = TypeFactLane {
        inputs: &declared,
        computed: &invalid_owner,
        children: &[],
    };
    assert!(matches!(
        write(&lane),
        Err(TestFailure::Prepare(PrepareError::TypeFacts {
            fault: TypeFactFault::Owner { ordinal: 1, .. }
        }))
    ));

    let invalid_child = [TypeFactInput {
        owner: EntityId::new(0),
        record: SemanticTypeRecord {
            tag: SemanticTypeTag::Tuple,
            payload0: 0,
            payload1: 0,
            text: None,
            text2: None,
            nominal: None,
            children: ListSpan::new(0, 1),
        },
    }];
    let child = [compiler_ir_vocabulary::SemanticTypeChild {
        target: TypeChildTarget::Type(TypeRef::Local(compiler_ir_vocabulary::TypeId::new(2))),
        name: None,
        flags: 0,
    }];
    let lane = TypeFactLane {
        inputs: &declared,
        computed: &invalid_child,
        children: &child,
    };
    assert!(matches!(
        write(&lane),
        Err(TestFailure::Prepare(PrepareError::TypeFacts {
            fault: TypeFactFault::ChildTargetOutOfRange {
                ordinal: 1,
                target: 2,
                record_count: 2,
                ..
            }
        }))
    ));

    let invalid_record = [TypeFactInput {
        owner: EntityId::new(0),
        record: SemanticTypeRecord {
            tag: SemanticTypeTag::SelfType,
            payload0: 9,
            payload1: 0,
            text: None,
            text2: None,
            nominal: None,
            children: ListSpan::new(0, 0),
        },
    }];
    let lane = TypeFactLane {
        inputs: &declared,
        computed: &invalid_record,
        children: &[],
    };
    assert!(matches!(
        write(&lane),
        Err(TestFailure::Prepare(PrepareError::TypeFacts {
            fault: TypeFactFault::Record { ordinal: 1, .. }
        }))
    ));
}

#[test]
fn schema_three_reopen_reports_the_full_type_lane_for_computed_children()
-> Result<(), TestFailure> {
    let declared = [TypeFactInput {
        owner: EntityId::new(0),
        record: SemanticTypeRecord::leaf(SemanticTypeTag::SelfType),
    }];
    let computed = [TypeFactInput {
        owner: EntityId::new(0),
        record: SemanticTypeRecord {
            tag: SemanticTypeTag::Tuple,
            payload0: 0,
            payload1: 0,
            text: None,
            text2: None,
            nominal: None,
            children: ListSpan::new(0, 1),
        },
    }];
    let children = [compiler_ir_vocabulary::SemanticTypeChild {
        target: TypeChildTarget::Type(TypeRef::Local(compiler_ir_vocabulary::TypeId::new(0))),
        name: None,
        flags: 0,
    }];
    let lane = TypeFactLane {
        inputs: &declared,
        computed: &computed,
        children: &children,
    };
    let bytes = write(&lane)?;
    let payload = FragmentView::validate(&bytes)?
        .type_fact_payload()
        .ok_or(TestFailure::Admission(TypeFactFault::TrailingBytes { declared: 0 }))?;
    let offset = payload.as_ptr() as usize - bytes.as_ptr() as usize;
    // schema-3 header (8), two 24-byte leaf records, pooled-child count (4),
    // and the one-byte local-child tag precede the raw target word.
    let mut mutated = bytes;
    mutated[offset + 61..offset + 65].copy_from_slice(&2_u32.to_le_bytes());
    assert!(matches!(
        FragmentView::validate(&mutated),
        Err(FragmentError::TypeFacts {
            fault: TypeFactFault::ChildTargetOutOfRange {
                ordinal: 1,
                position: 0,
                target: 2,
                record_count: 2,
            }
        })
    ));
    Ok(())
}

#[test]
fn type_fact_records_round_trip_through_borrowing_cursor() -> Result<(), TestFailure> {
    let inputs = records();
    let lane = TypeFactLane {
        inputs: &inputs,
        computed: &[],
        children: &[],
    };
    let bytes = write(&lane)?;
    const SCHEMA_ONE_FIXTURE_HEX: &str = "4e584952010007005f01000001000100010000007c0000000c00000002000100010000008800000008000000030001000100000090000000080000000400010006000000980000000600000005000100010000009e000000240000000600010001000000c2000000440000000900010059000000060100005900000000000000000000000000000000000000000000000000000006000000656e74697479110000000ed17c05fdfd1061b3c018ed85341054eb81923049ece75cecdc6ec1655bea2c0003010011e60d30e5e9ae1517ed90faf4701f9b3a80e9b8bea7e09e6fd577c20e5b23dc0f2f126c609c6b82f2fd9500d043920c1da1436695fd5dec0626561b1fb83c9503000000000000000000000000000000000000000000000000000000000000000a0000000000000000000001000000000000000000000000000000000c00000000000000000101000000540000000000000000000000000000";
    let fixture = (0..SCHEMA_ONE_FIXTURE_HEX.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&SCHEMA_ONE_FIXTURE_HEX[at..at + 2], 16))
        .collect::<Result<Vec<_>, ParseIntError>>()?;
    assert_eq!(bytes, fixture);
    let view = FragmentView::validate(&bytes)?;
    let mut cursor =
        view.type_facts()
            .ok_or(TestFailure::Admission(TypeFactFault::TrailingBytes {
                declared: 0,
            }))?;
    for expected in inputs {
        let actual = cursor
            .next()
            .ok_or(TestFailure::Admission(TypeFactFault::TrailingBytes {
                declared: 3,
            }))?
            .map_err(TestFailure::Admission)?;
        assert_eq!(actual.owner, expected.owner);
        assert_eq!(actual.record, expected.record);
    }
    assert!(cursor.next().is_none());
    Ok(())
}

#[test]
fn unknown_record_tag_is_rejected_with_the_observed_byte() -> Result<(), TestFailure> {
    let inputs = records();
    let lane = TypeFactLane {
        inputs: &inputs,
        computed: &[],
        children: &[],
    };
    let bytes = write(&lane)?;
    let payload =
        FragmentView::validate(&bytes)?
            .type_fact_payload()
            .ok_or(TestFailure::Admission(TypeFactFault::TrailingBytes {
                declared: 0,
            }))?;
    let offset = payload.as_ptr() as usize - bytes.as_ptr() as usize;
    let mut mutated = bytes;
    mutated[offset + 8] = u8::MAX;
    assert!(matches!(
        FragmentView::validate(&mutated),
        Err(FragmentError::TypeFacts {
            fault: TypeFactFault::Tag {
                ordinal: 0,
                field: "record",
                actual: u8::MAX
            }
        })
    ));
    Ok(())
}

#[test]
fn nominal_out_of_range_is_rejected_at_reopen() -> Result<(), TestFailure> {
    let inputs = records();
    let lane = TypeFactLane {
        inputs: &inputs,
        computed: &[],
        children: &[],
    };
    let bytes = write(&lane)?;
    let payload =
        FragmentView::validate(&bytes)?
            .type_fact_payload()
            .ok_or(TestFailure::Admission(TypeFactFault::TrailingBytes {
                declared: 0,
            }))?;
    let offset = payload.as_ptr() as usize - bytes.as_ptr() as usize;
    let mut mutated = bytes;
    mutated[offset + 44..offset + 48].copy_from_slice(&9_u32.to_le_bytes());
    assert!(matches!(
        FragmentView::validate(&mutated),
        Err(FragmentError::TypeFacts {
            fault: TypeFactFault::NominalOutOfRange {
                ordinal: 1,
                target: 9
            }
        })
    ));
    Ok(())
}

#[test]
fn forward_child_is_rejected_at_admission() {
    let inputs = [TypeFactInput {
        owner: EntityId::new(0),
        record: SemanticTypeRecord {
            tag: SemanticTypeTag::Tuple,
            payload0: 0,
            payload1: 0,
            text: None,
            text2: None,
            nominal: None,
            children: ListSpan::new(0, 1),
        },
    }];
    let children = [compiler_ir_vocabulary::SemanticTypeChild {
        target: compiler_ir_vocabulary::TypeChildTarget::Type(TypeRef::Local(
            compiler_ir_vocabulary::TypeId::new(0),
        )),
        name: None,
        flags: 0,
    }];
    let lane = TypeFactLane {
        inputs: &inputs,
        computed: &[],
        children: &children,
    };
    assert!(matches!(
        lane.admit(1, &children),
        Err(TypeFactFault::ForwardReference {
            ordinal: 0,
            position: 0,
            target: 0
        })
    ));
}

#[test]
fn forward_nominal_is_rejected_at_admission() {
    let inputs = [
        TypeFactInput {
            owner: EntityId::new(0),
            record: SemanticTypeRecord {
                tag: SemanticTypeTag::Nominal,
                payload0: 0,
                payload1: 0,
                text: None,
                text2: None,
                nominal: Some(NominalRef::Local(EntityId::new(2))),
                children: ListSpan::new(0, 0),
            },
        },
        TypeFactInput {
            owner: EntityId::new(1),
            record: SemanticTypeRecord::leaf(SemanticTypeTag::SelfType),
        },
        TypeFactInput {
            owner: EntityId::new(2),
            record: SemanticTypeRecord::leaf(SemanticTypeTag::SelfType),
        },
    ];
    let lane = TypeFactLane {
        inputs: &inputs,
        computed: &[],
        children: &[],
    };
    assert!(matches!(
        lane.admit(3, &[]),
        Err(TypeFactFault::NominalForward {
            ordinal: 0,
            target: 2
        })
    ));
}

#[test]
fn self_nominal_admits_and_round_trips_as_the_recursive_terminal() -> Result<(), TestFailure> {
    // A declaration naming its own declared type is the one closed forward
    // case: the diagonal self-nominal every recursive type closes on.
    let inputs = [TypeFactInput {
        owner: EntityId::new(0),
        record: SemanticTypeRecord {
            tag: SemanticTypeTag::Nominal,
            payload0: 0,
            payload1: 0,
            text: None,
            text2: None,
            nominal: Some(NominalRef::Local(EntityId::new(0))),
            children: ListSpan::new(0, 0),
        },
    }];
    let lane = TypeFactLane {
        inputs: &inputs,
        computed: &[],
        children: &[],
    };
    assert!(lane.admit(1, &[]).is_ok());
    let bytes = write(&lane)?;
    let view = FragmentView::validate(&bytes)?;
    let mut cursor =
        view.type_facts()
            .ok_or(TestFailure::Admission(TypeFactFault::TrailingBytes {
                declared: 0,
            }))?;
    let decoded = cursor
        .next()
        .transpose()
        .map_err(TestFailure::Admission)?
        .ok_or(TestFailure::Admission(TypeFactFault::TrailingBytes {
            declared: 0,
        }))?;
    assert_eq!(
        decoded.record.nominal,
        Some(NominalRef::Local(EntityId::new(0)))
    );
    Ok(())
}

#[test]
fn section_header_and_trailing_bytes_are_rejected() -> Result<(), TestFailure> {
    let inputs = records();
    let lane = TypeFactLane {
        inputs: &inputs,
        computed: &[],
        children: &[],
    };
    let bytes = write(&lane)?;
    let payload =
        FragmentView::validate(&bytes)?
            .type_fact_payload()
            .ok_or(TestFailure::Admission(TypeFactFault::TrailingBytes {
                declared: 0,
            }))?;
    let offset = payload.as_ptr() as usize - bytes.as_ptr() as usize;
    let mut truncated = bytes.clone();
    truncated.truncate(offset + 2);
    assert!(matches!(
        FragmentView::validate(&truncated),
        Err(FragmentError::DeclaredLength { .. })
    ));
    let mut trailing = bytes;
    trailing.push(0);
    assert!(matches!(
        FragmentView::validate(&trailing),
        Err(FragmentError::DeclaredLength { .. })
    ));
    Ok(())
}

#[test]
fn owner_and_child_span_mutations_retain_their_coordinates() -> Result<(), TestFailure> {
    let inputs = records();
    let lane = TypeFactLane {
        inputs: &inputs,
        computed: &[],
        children: &[],
    };
    let bytes = write(&lane)?;
    let payload =
        FragmentView::validate(&bytes)?
            .type_fact_payload()
            .ok_or(TestFailure::Admission(TypeFactFault::TrailingBytes {
                declared: 0,
            }))?;
    let offset = payload.as_ptr() as usize - bytes.as_ptr() as usize;
    let mut owner = bytes.clone();
    owner[offset + 4..offset + 8].copy_from_slice(&9_u32.to_le_bytes());
    assert!(matches!(
        FragmentView::validate(&owner),
        Err(FragmentError::TypeFacts { fault: TypeFactFault::Owner { ordinal: 0, owner, entity_count: 1 } }) if owner.raw == 9
    ));
    let mut span = bytes;
    span[offset + 52..offset + 56].copy_from_slice(&9_u32.to_le_bytes());
    assert!(matches!(
        FragmentView::validate(&span),
        Err(FragmentError::TypeFacts {
            fault: TypeFactFault::ChildSpan {
                ordinal: 1,
                start: 0,
                length: 9,
                child_count: 0
            }
        })
    ));
    Ok(())
}

#[test]
fn out_of_range_child_reopen_retains_the_true_record_ordinal() -> Result<(), TestFailure> {
    let inputs = [
        TypeFactInput {
            owner: EntityId::new(0),
            record: SemanticTypeRecord::leaf(SemanticTypeTag::SelfType),
        },
        TypeFactInput {
            owner: EntityId::new(0),
            record: SemanticTypeRecord {
                tag: SemanticTypeTag::Tuple,
                payload0: 0,
                payload1: 0,
                text: None,
                text2: None,
                nominal: None,
                children: ListSpan::new(0, 1),
            },
        },
    ];
    let children = [compiler_ir_vocabulary::SemanticTypeChild {
        target: compiler_ir_vocabulary::TypeChildTarget::Type(TypeRef::Local(
            compiler_ir_vocabulary::TypeId::new(0),
        )),
        name: None,
        flags: 0,
    }];
    let lane = TypeFactLane {
        inputs: &inputs,
        computed: &[],
        children: &children,
    };
    let bytes = write(&lane)?;
    let payload =
        FragmentView::validate(&bytes)?
            .type_fact_payload()
            .ok_or(TestFailure::Admission(TypeFactFault::TrailingBytes {
                declared: 0,
            }))?;
    let offset = payload.as_ptr() as usize - bytes.as_ptr() as usize;
    let mut mutated = bytes;
    mutated[offset + 57..offset + 61].copy_from_slice(&99_u32.to_le_bytes());
    assert!(matches!(
        FragmentView::validate(&mutated),
        Err(FragmentError::TypeFacts {
            fault: TypeFactFault::ChildTargetOutOfRange {
                ordinal: 1,
                position: 0,
                target: 99,
                record_count: 2
            }
        })
    ));
    Ok(())
}

#[test]
fn forward_child_reopen_retains_the_true_record_ordinal() -> Result<(), TestFailure> {
    let inputs = [
        TypeFactInput {
            owner: EntityId::new(0),
            record: SemanticTypeRecord::leaf(SemanticTypeTag::SelfType),
        },
        TypeFactInput {
            owner: EntityId::new(0),
            record: SemanticTypeRecord {
                tag: SemanticTypeTag::Tuple,
                payload0: 0,
                payload1: 0,
                text: None,
                text2: None,
                nominal: None,
                children: ListSpan::new(0, 1),
            },
        },
    ];
    let children = [compiler_ir_vocabulary::SemanticTypeChild {
        target: TypeChildTarget::Type(TypeRef::Local(compiler_ir_vocabulary::TypeId::new(0))),
        name: None,
        flags: 0,
    }];
    let lane = TypeFactLane {
        inputs: &inputs,
        computed: &[],
        children: &children,
    };
    let bytes = write(&lane)?;
    let payload =
        FragmentView::validate(&bytes)?
            .type_fact_payload()
            .ok_or(TestFailure::Admission(TypeFactFault::TrailingBytes {
                declared: 0,
            }))?;
    let offset = payload.as_ptr() as usize - bytes.as_ptr() as usize;
    let mut mutated = bytes;
    mutated[offset + 57..offset + 61].copy_from_slice(&1_u32.to_le_bytes());
    assert!(matches!(
        FragmentView::validate(&mutated),
        Err(FragmentError::TypeFacts {
            fault: TypeFactFault::ForwardReference {
                ordinal: 1,
                position: 0,
                target: 1
            }
        })
    ));
    Ok(())
}

#[test]
fn admission_out_of_range_child_returns_the_typed_prepare_fault() {
    let inputs = [TypeFactInput {
        owner: EntityId::new(0),
        record: SemanticTypeRecord {
            tag: SemanticTypeTag::Tuple,
            payload0: 0,
            payload1: 0,
            text: None,
            text2: None,
            nominal: None,
            children: ListSpan::new(0, 1),
        },
    }];
    let children = [compiler_ir_vocabulary::SemanticTypeChild {
        target: TypeChildTarget::Type(TypeRef::Local(compiler_ir_vocabulary::TypeId::new(8))),
        name: None,
        flags: 0,
    }];
    let lane = TypeFactLane {
        inputs: &inputs,
        computed: &[],
        children: &children,
    };
    let entities = [EntityRecord {
        semantic_type: compiler_ir_vocabulary::TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Function,
    }];
    let types = [TypeNode::Primitive(PrimitiveType::Bool)];
    let atoms = [AtomInput { bytes: b"entity" }];
    let result = PreparedFragment::prepare_with_semantics(
        source(),
        recipe(),
        &entities,
        &types,
        &atoms,
        FragmentSemantics {
            type_facts: Some(&lane),
            ..FragmentSemantics::default()
        },
    );
    assert!(matches!(
        result,
        Err(PrepareError::TypeFacts {
            fault: TypeFactFault::ChildTargetOutOfRange {
                ordinal: 0,
                position: 0,
                target: 8,
                record_count: 1
            }
        })
    ));
}

#[test]
fn corrupted_external_nominal_authority_is_rejected_with_an_authority_fault()
-> Result<(), TestFailure> {
    let inputs = [TypeFactInput {
        owner: EntityId::new(0),
        record: SemanticTypeRecord {
            tag: SemanticTypeTag::Nominal,
            payload0: 0,
            payload1: 0,
            text: None,
            text2: None,
            nominal: Some(NominalRef::External(ExternalEntityRef::bind(
                ContentId::<IrFragmentDomain>::from_canonical_bytes(b"foreign"),
                7,
            ))),
            children: ListSpan::new(0, 0),
        },
    }];
    let lane = TypeFactLane {
        inputs: &inputs,
        computed: &[],
        children: &[],
    };
    let bytes = write(&lane)?;
    let payload =
        FragmentView::validate(&bytes)?
            .type_fact_payload()
            .ok_or(TestFailure::Admission(TypeFactFault::TrailingBytes {
                declared: 0,
            }))?;
    let offset = payload.as_ptr() as usize - bytes.as_ptr() as usize;
    let mut mutated = bytes;
    mutated[offset + 4 + 16] ^= 1;
    assert!(matches!(
        FragmentView::validate(&mutated),
        Err(FragmentError::TypeFacts {
            fault: TypeFactFault::Authority { ordinal: 0, .. }
        })
    ));
    Ok(())
}
