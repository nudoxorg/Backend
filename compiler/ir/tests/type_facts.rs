//! Type-fact wire falsifiers.  Local references are backward-only, so a
//! cycle detector is forbidden: forward and self references are rejected at
//! admission and again when untrusted bytes are reopened.

use compiler_ir::{
    AtomInput, EntityKind, EntityRecord, FragmentError, FragmentView, PrepareError,
    PreparedFragment, PrimitiveType, RecipeFact, SourceIdentity, TypeFactFault, TypeFactInput,
    TypeFactLane, TypeNode, WriteError,
};
use compiler_ir_vocabulary::{
    AtomId, EntityId, ListSpan, NominalRef, SemanticTypeRecord, SemanticTypeTag, TypeRef,
};
use compiler_vocabulary::{Language, NativeTool, Stage};
use heart_identity::{ContentId, SourceFactDomain, ToolchainDomain};
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
}

fn source() -> SourceIdentity {
    SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"type-facts-source"),
        byte_len: 17,
    }
}

fn recipe() -> RecipeFact {
    RecipeFact {
        identity: compiler_vocabulary::CompileRecipeFact::derive(
            Language::Rust,
            Stage::LowerIr,
            NativeTool::Rustc,
            source().identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"type-facts-toolchain"),
        )
        .identity,
        language: Language::Rust,
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
    let prepared = PreparedFragment::prepare_with_type_facts(
        source(),
        recipe(),
        &entities,
        &types,
        &atoms,
        None,
        None,
        lane,
    )?;
    let mut bytes = vec![0; prepared.required_capacity()];
    prepared.write_into(&mut bytes)?;
    Ok(bytes)
}

#[test]
fn type_fact_records_round_trip_through_borrowing_cursor() -> Result<(), TestFailure> {
    let inputs = records();
    let lane = TypeFactLane {
        inputs: &inputs,
        children: &[],
    };
    let bytes = write(&lane)?;
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
        children: &[],
    };
    assert!(matches!(
        lane.admit(1, &[]),
        Err(TypeFactFault::NominalForward {
            ordinal: 0,
            target: 0
        })
    ));
}

#[test]
fn section_header_and_trailing_bytes_are_rejected() -> Result<(), TestFailure> {
    let inputs = records();
    let lane = TypeFactLane {
        inputs: &inputs,
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
