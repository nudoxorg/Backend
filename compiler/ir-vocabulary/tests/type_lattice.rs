//! Proves the frozen type-expression lattice: tag round-trips, per-tag cell
//! ownership, child-count laws, and the named unknown-reason reasons the
//! 87,101-position Python census froze. Assertions retain exact operands.

use compiler_ir_vocabulary::{
    AnonRecordForm, ChildCountLaw, MappedModifier, NominalRef, PrimitiveShape, SemanticTypeChild,
    SemanticTypeFault, SemanticTypeRecord, SemanticTypeTag, TypeCell, TypeChildTarget, TypeReason,
    TypeRef, TypeWidth, Variance,
};

fn record(tag: SemanticTypeTag) -> SemanticTypeRecord<'static> {
    let mut row = SemanticTypeRecord::leaf(tag);
    row.children = compiler_ir_vocabulary::ListSpan::new(0, 0);
    row
}

#[test]
fn every_tag_round_trips_through_its_frozen_discriminant() {
    // Decoding every frozen code and re-encoding the result must be the
    // identity on 0..24, and 24 must stay outside the registry.
    for ordinal in 0_u8..24 {
        assert_eq!(
            SemanticTypeTag::try_from(ordinal).map(u8::from),
            Ok(ordinal)
        );
    }
    assert_eq!(
        SemanticTypeTag::try_from(24),
        Err(compiler_ir_vocabulary::SemanticTypeTagError { actual: 24 })
    );
}

#[test]
fn leaf_tags_reject_any_foreign_cell() {
    // Tags whose closed child law is exactly zero children.
    for tag in [
        SemanticTypeTag::Never,
        SemanticTypeTag::Any,
        SemanticTypeTag::Inferred,
        SemanticTypeTag::Tuple,
        SemanticTypeTag::Union,
        SemanticTypeTag::Intersection,
        SemanticTypeTag::ImplTrait,
        SemanticTypeTag::DynTrait,
        SemanticTypeTag::TemplateLiteral,
    ] {
        let mut row = record(tag);
        row.payload0 = 1;
        assert_eq!(
            row.validate(0),
            Err(SemanticTypeFault::ReservedCell {
                tag,
                cell: TypeCell::Payload0,
                actual: 1,
            })
        );
        let mut row = record(tag);
        row.payload1 = 1;
        assert_eq!(
            row.validate(0),
            Err(SemanticTypeFault::ReservedCell {
                tag,
                cell: TypeCell::Payload1,
                actual: 1,
            })
        );
        let mut row = record(tag);
        row.text = Some(b"stray");
        assert_eq!(
            row.validate(0),
            Err(SemanticTypeFault::ReservedCell {
                tag,
                cell: TypeCell::Text,
                actual: 1,
            })
        );
    }
    // Conditional and Apply own children, so their cell laws are proven at
    // a legal child count.
    for tag in [SemanticTypeTag::Conditional, SemanticTypeTag::Apply] {
        let count = if tag == SemanticTypeTag::Conditional {
            4
        } else {
            1
        };
        let mut row = record(tag);
        row.children = compiler_ir_vocabulary::ListSpan::new(0, count);
        row.payload0 = 1;
        assert_eq!(
            row.validate(count),
            Err(SemanticTypeFault::ReservedCell {
                tag,
                cell: TypeCell::Payload0,
                actual: 1,
            })
        );
    }
}

#[test]
fn self_type_allows_only_its_explicit_language_spelling() {
    let mut row = record(SemanticTypeTag::SelfType);
    row.text = Some(b"this");
    assert_eq!(row.validate(0), Ok(()));
    row.payload0 = 1;
    assert_eq!(
        row.validate(0),
        Err(SemanticTypeFault::ReservedCell {
            tag: SemanticTypeTag::SelfType,
            cell: TypeCell::Payload0,
            actual: 1,
        })
    );
}

#[test]
fn leaf_tags_reject_children() {
    for tag in [
        SemanticTypeTag::SelfType,
        SemanticTypeTag::Never,
        SemanticTypeTag::Any,
        SemanticTypeTag::Unknown,
        SemanticTypeTag::Nominal,
        SemanticTypeTag::TypeVar,
        SemanticTypeTag::Inferred,
    ] {
        let mut row = record(tag);
        row.children = compiler_ir_vocabulary::ListSpan::new(0, 1);
        assert_eq!(
            row.validate(1),
            Err(SemanticTypeFault::ChildCount {
                tag,
                law: ChildCountLaw { min: 0, max: 0 },
                actual: 1,
            })
        );
    }
}

#[test]
fn unknown_records_reject_unnamed_reasons_and_demand_spellings() {
    // A spelling-carrying reason without its text is a missing cell.
    let mut row = record(SemanticTypeTag::Unknown);
    row.payload0 = u32::from(TypeReason::UnresolvedExternal);
    assert_eq!(
        row.validate(0),
        Err(SemanticTypeFault::MissingCell {
            tag: SemanticTypeTag::Unknown,
            cell: TypeCell::Text,
        })
    );
    // The same reason with the exact spelling validates.
    row.text = Some(b"std::collections::HashMap");
    assert_eq!(row.validate(0), Ok(()));
    // A non-spelling reason with text is a reserved cell.
    let mut row = record(SemanticTypeTag::Unknown);
    row.payload0 = u32::from(TypeReason::Unannotated);
    row.text = Some(b"stray");
    assert_eq!(
        row.validate(0),
        Err(SemanticTypeFault::ReservedCell {
            tag: SemanticTypeTag::Unknown,
            cell: TypeCell::Text,
            actual: 1,
        })
    );
    // An unknown reason code keeps its exact operand.
    let mut row = record(SemanticTypeTag::Unknown);
    row.payload0 = 7;
    assert_eq!(
        row.validate(0),
        Err(SemanticTypeFault::Reason { actual: 7 })
    );
}

#[test]
fn every_unknown_reason_is_named_and_distinct() {
    // The census froze exactly seven reasons; a catch-all arm anywhere here
    // would recreate the one-opcode collapse.
    let reasons = [
        TypeReason::Unannotated,
        TypeReason::DynamicallyTyped,
        TypeReason::UnresolvedLocalName,
        TypeReason::UnresolvedExternal,
        TypeReason::TruncatedAtDepthLimit,
        TypeReason::OracleGap,
        TypeReason::NoIrRepresentation,
    ];
    for (ordinal, reason) in reasons.iter().enumerate() {
        assert_eq!(u32::from(*reason), ordinal as u32);
        assert_eq!(TypeReason::try_from(ordinal as u32), Ok(*reason));
    }
    assert_eq!(
        TypeReason::try_from(8),
        Err(compiler_ir_vocabulary::TypeReasonError { actual: 8 })
    );
    // Exactly three reasons retain a spelling; the rest are honestly bare.
    assert!(TypeReason::UnresolvedLocalName.carries_spelling());
    assert!(TypeReason::UnresolvedExternal.carries_spelling());
    assert!(TypeReason::NoIrRepresentation.carries_spelling());
    assert!(!TypeReason::Unannotated.carries_spelling());
    assert!(!TypeReason::DynamicallyTyped.carries_spelling());
    assert!(!TypeReason::TruncatedAtDepthLimit.carries_spelling());
    assert!(!TypeReason::OracleGap.carries_spelling());
    // The stable tags stay distinct so renderers and metrics can name them.
    let mut tags: Vec<&'static str> = reasons.iter().map(|reason| reason.tag()).collect();
    tags.sort_unstable();
    tags.dedup();
    assert_eq!(tags.len(), reasons.len());
}

#[test]
fn nominal_records_demand_a_typed_target() {
    let mut row = record(SemanticTypeTag::Nominal);
    assert_eq!(
        row.validate(0),
        Err(SemanticTypeFault::MissingCell {
            tag: SemanticTypeTag::Nominal,
            cell: TypeCell::Nominal,
        })
    );
    row.nominal = Some(NominalRef::Local(compiler_ir_vocabulary::EntityId::new(7)));
    assert_eq!(row.validate(0), Ok(()));
}

#[test]
fn primitive_rows_own_cells_by_shape() {
    // Integer: signedness bit below a 17-bit width cell.
    let mut row = record(SemanticTypeTag::Primitive);
    row.payload0 = u32::from(PrimitiveShape::Integer);
    row.payload1 = 1 | (TypeWidth::Fixed(64).to_cell() << 1);
    assert_eq!(row.validate(0), Ok(()));
    // Zero width is malformed even with the signedness bit set.
    let mut row = record(SemanticTypeTag::Primitive);
    row.payload0 = u32::from(PrimitiveShape::Integer);
    row.payload1 = 1;
    assert_eq!(row.validate(0), Err(SemanticTypeFault::Width { actual: 1 }));
    // Float carries the width cell directly.
    let mut row = record(SemanticTypeTag::Primitive);
    row.payload0 = u32::from(PrimitiveShape::Float);
    row.payload1 = TypeWidth::Arch.to_cell();
    assert_eq!(row.validate(0), Ok(()));
    // Bool owns no width and no text.
    let mut row = record(SemanticTypeTag::Primitive);
    row.payload0 = u32::from(PrimitiveShape::Bool);
    row.payload1 = 1;
    assert_eq!(
        row.validate(0),
        Err(SemanticTypeFault::ReservedCell {
            tag: SemanticTypeTag::Primitive,
            cell: TypeCell::Payload1,
            actual: 1,
        })
    );
    // Builtin demands its spelling.
    let mut row = record(SemanticTypeTag::Primitive);
    row.payload0 = u32::from(PrimitiveShape::Builtin);
    assert_eq!(
        row.validate(0),
        Err(SemanticTypeFault::MissingCell {
            tag: SemanticTypeTag::Primitive,
            cell: TypeCell::Text,
        })
    );
    row.text = Some(b"Date");
    assert_eq!(row.validate(0), Ok(()));
    // An unknown shape keeps its exact operand.
    let mut row = record(SemanticTypeTag::Primitive);
    row.payload0 = 9;
    assert_eq!(
        row.validate(0),
        Err(SemanticTypeFault::PrimitiveShape { actual: 9 })
    );
}

#[test]
fn pointer_and_reference_primitives_own_exactly_one_child() {
    for shape in [
        PrimitiveShape::MutPointer,
        PrimitiveShape::ConstPointer,
        PrimitiveShape::Reference,
    ] {
        let mut row = record(SemanticTypeTag::Primitive);
        row.payload0 = u32::from(shape);
        assert_eq!(
            row.validate(0),
            Err(SemanticTypeFault::ChildCount {
                tag: SemanticTypeTag::Primitive,
                law: ChildCountLaw { min: 1, max: 1 },
                actual: 0,
            })
        );
    }
    // A reference may carry mutability and an optional lifetime spelling.
    let mut row = record(SemanticTypeTag::Primitive);
    row.payload0 = u32::from(PrimitiveShape::Reference);
    row.payload1 = 1;
    row.text = Some(b"'a");
    row.children = compiler_ir_vocabulary::ListSpan::new(0, 1);
    assert_eq!(row.validate(1), Ok(()));
}

#[test]
fn structural_tags_enforce_their_child_count_laws() {
    let laws = [
        (SemanticTypeTag::Slice, 1_u32, 1_u32),
        (SemanticTypeTag::Array, 1, 1),
        (SemanticTypeTag::Annotated, 1, 1),
        (SemanticTypeTag::Conditional, 4, 4),
        (SemanticTypeTag::Mapped, 2, 3),
    ];
    for (tag, min, max) in laws {
        let mut row = record(tag);
        row.text = Some(b"x");
        let count = if min == 0 { 0 } else { min - 1 };
        assert_eq!(
            row.validate(count),
            Err(SemanticTypeFault::ChildCount {
                tag,
                law: ChildCountLaw { min, max },
                actual: count,
            })
        );
    }
    // A conditional with its four arms validates once its tag-owned cells
    // are legal.
    let mut row = record(SemanticTypeTag::Conditional);
    row.children = compiler_ir_vocabulary::ListSpan::new(0, 4);
    assert_eq!(row.validate(4), Ok(()));
}

#[test]
fn wildcard_mapped_and_anonymous_rows_reject_out_of_range_cells() {
    // Wildcard variance outside the closed set.
    let mut row = record(SemanticTypeTag::Wildcard);
    row.payload0 = u32::from(Variance::Contravariant) + 1;
    assert_eq!(
        row.validate(0),
        Err(SemanticTypeFault::ReservedCell {
            tag: SemanticTypeTag::Wildcard,
            cell: TypeCell::Payload0,
            actual: 3,
        })
    );
    // Mapped modifiers outside the tri-state set.
    let mut row = record(SemanticTypeTag::Mapped);
    row.payload0 = u32::from(MappedModifier::Absent) + 1;
    row.text = Some(b"K");
    assert_eq!(
        row.validate(2),
        Err(SemanticTypeFault::ReservedCell {
            tag: SemanticTypeTag::Mapped,
            cell: TypeCell::Payload0,
            actual: 3,
        })
    );
    // Anonymous-record forms outside the closed set.
    let mut row = record(SemanticTypeTag::AnonymousRecord);
    row.payload0 = u32::from(AnonRecordForm::Interface) + 1;
    assert_eq!(
        row.validate(0),
        Err(SemanticTypeFault::ReservedCell {
            tag: SemanticTypeTag::AnonymousRecord,
            cell: TypeCell::Payload0,
            actual: 2,
        })
    );
}

#[test]
fn child_names_and_flags_are_tag_owned() {
    // Labeled tuple elements carry names; positional ones do not.
    let row = record(SemanticTypeTag::Tuple);
    assert_eq!(
        row.validate_child(
            0,
            &SemanticTypeChild {
                target: TypeChildTarget::Type(TypeRef::Local(compiler_ir_vocabulary::TypeId::new(
                    0
                ))),
                name: None,
                flags: 0,
            }
        ),
        Ok(())
    );
    // Anonymous-record members demand names and own the flag bits.
    let row = record(SemanticTypeTag::AnonymousRecord);
    assert_eq!(
        row.validate_child(
            0,
            &SemanticTypeChild {
                target: TypeChildTarget::Type(TypeRef::Local(compiler_ir_vocabulary::TypeId::new(
                    0
                ))),
                name: None,
                flags: 0,
            }
        ),
        Err(SemanticTypeFault::ChildNameRequired {
            tag: SemanticTypeTag::AnonymousRecord,
            position: 0,
        })
    );
    assert_eq!(
        row.validate_child(
            0,
            &SemanticTypeChild {
                target: TypeChildTarget::Type(TypeRef::Local(compiler_ir_vocabulary::TypeId::new(
                    0
                ))),
                name: Some(b"x"),
                flags: SemanticTypeChild::FLAG_OPTIONAL | SemanticTypeChild::FLAG_READONLY,
            }
        ),
        Ok(())
    );
    // Flag bits outside the closed grammar are rejected everywhere.
    for tag in [SemanticTypeTag::AnonymousRecord, SemanticTypeTag::Tuple] {
        let row = record(tag);
        assert_eq!(
            row.validate_child(
                0,
                &SemanticTypeChild {
                    target: TypeChildTarget::Type(TypeRef::Local(
                        compiler_ir_vocabulary::TypeId::new(0)
                    )),
                    name: Some(b"x"),
                    flags: 0b1000,
                }
            ),
            Err(SemanticTypeFault::ChildFlagsForbidden {
                tag,
                position: 0,
                actual: 0b1000,
            })
        );
    }
    // Text children exist only under template literals.
    let row = record(SemanticTypeTag::Tuple);
    assert_eq!(
        row.validate_child(
            0,
            &SemanticTypeChild {
                target: TypeChildTarget::Text,
                name: None,
                flags: 0,
            }
        ),
        Err(SemanticTypeFault::ChildTextForbidden {
            tag: SemanticTypeTag::Tuple,
            position: 0,
        })
    );
    let row = record(SemanticTypeTag::TemplateLiteral);
    assert_eq!(
        row.validate_child(
            0,
            &SemanticTypeChild {
                target: TypeChildTarget::Text,
                name: None,
                flags: 0,
            }
        ),
        Ok(())
    );
}

#[test]
fn width_cells_reject_reserved_bits_and_zero_widths() {
    assert_eq!(
        TypeWidth::try_from_cell(0),
        Err(compiler_ir_vocabulary::TypeWidthError::Malformed { actual: 0 })
    );
    // The architecture flag plus a nonzero width is malformed.
    let mixed = TypeWidth::ARCH_FLAG | 32;
    assert_eq!(
        TypeWidth::try_from_cell(mixed),
        Err(compiler_ir_vocabulary::TypeWidthError::Malformed { actual: mixed })
    );
    // A reserved bit above the 17-bit budget is malformed.
    let reserved = 1 << 20;
    assert_eq!(
        TypeWidth::try_from_cell(reserved),
        Err(compiler_ir_vocabulary::TypeWidthError::Malformed { actual: reserved })
    );
    assert_eq!(
        TypeWidth::try_from_cell(TypeWidth::ARCH_FLAG),
        Ok(TypeWidth::Arch)
    );
    assert_eq!(
        TypeWidth::try_from_cell(TypeWidth::Fixed(128).to_cell()),
        Ok(TypeWidth::Fixed(128))
    );
}

#[test]
fn function_pointer_results_are_committed_by_an_exact_count() {
    // A nonzero result count with no children violates the committed law.
    let mut row = record(SemanticTypeTag::FunctionPointer);
    row.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
    assert_eq!(
        row.validate(0),
        Err(SemanticTypeFault::ChildCount {
            tag: SemanticTypeTag::FunctionPointer,
            law: ChildCountLaw {
                min: 1,
                max: u32::MAX
            },
            actual: 0,
        })
    );
    // Params plus the committed result child validate; the ABI text is
    // optional and defaults to the language convention when absent.
    let mut row = record(SemanticTypeTag::FunctionPointer);
    row.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
    row.text = Some(b"Cdecl");
    row.children = compiler_ir_vocabulary::ListSpan::new(0, 2);
    assert_eq!(row.validate(2), Ok(()));
    // Reserved payload bits below the flag stay rejected.
    let mut row = record(SemanticTypeTag::FunctionPointer);
    row.payload1 = SemanticTypeRecord::LEGACY_RESULT_FLAG | 1;
    assert_eq!(
        row.validate(0),
        Err(SemanticTypeFault::ReservedCell {
            tag: SemanticTypeTag::FunctionPointer,
            cell: TypeCell::Payload1,
            actual: SemanticTypeRecord::LEGACY_RESULT_FLAG | 1,
        })
    );
}

#[test]
fn variadic_function_rows_have_one_final_rest_parameter_and_plain_results() {
    let mut row = record(SemanticTypeTag::FunctionPointer);
    row.payload0 = SemanticTypeRecord::FUNCTION_TYPED_VARIADIC_FLAG;
    row.payload1 = 2;
    assert_eq!(row.validate(3), Ok(()));

    let child = |flags| SemanticTypeChild {
        target: TypeChildTarget::Type(TypeRef::Local(compiler_ir_vocabulary::TypeId::new(0))),
        name: None,
        flags,
    };
    assert_eq!(
        row.validate_child_in_row(0, 3, &child(SemanticTypeChild::FLAG_REST)),
        Err(SemanticTypeFault::ChildFlagsForbidden {
            tag: SemanticTypeTag::FunctionPointer,
            position: 0,
            actual: SemanticTypeChild::FLAG_REST,
        })
    );
    assert_eq!(
        row.validate_child_in_row(0, 3, &child(0)),
        Ok(())
    );
    assert_eq!(
        row.validate_child_in_row(1, 3, &child(SemanticTypeChild::FLAG_REST)),
        Ok(())
    );
    assert_eq!(
        row.validate_child_in_row(2, 3, &child(SemanticTypeChild::FLAG_OPTIONAL)),
        Err(SemanticTypeFault::ChildFlagsForbidden {
            tag: SemanticTypeTag::FunctionPointer,
            position: 2,
            actual: SemanticTypeChild::FLAG_OPTIONAL,
        })
    );

    row.payload0 = 0;
    assert_eq!(
        row.validate_child_in_row(1, 3, &child(SemanticTypeChild::FLAG_REST)),
        Err(SemanticTypeFault::ChildFlagsForbidden {
            tag: SemanticTypeTag::FunctionPointer,
            position: 1,
            actual: SemanticTypeChild::FLAG_REST,
        })
    );
}

#[test]
fn c_variadic_tail_is_distinct_from_typed_rest_and_mixed_forms_fail() {
    let mut c_tail = record(SemanticTypeTag::FunctionPointer);
    c_tail.payload0 = SemanticTypeRecord::FUNCTION_C_VARIADIC_FLAG;
    assert_eq!(c_tail.validate(0), Ok(()));

    let rest = SemanticTypeChild {
        target: TypeChildTarget::Type(TypeRef::Local(compiler_ir_vocabulary::TypeId::new(0))),
        name: None,
        flags: SemanticTypeChild::FLAG_REST,
    };
    assert_eq!(
        c_tail.validate_child_in_row(0, 1, &rest),
        Err(SemanticTypeFault::ChildFlagsForbidden {
            tag: SemanticTypeTag::FunctionPointer,
            position: 0,
            actual: SemanticTypeChild::FLAG_REST,
        })
    );

    c_tail.payload0 = SemanticTypeRecord::FUNCTION_TYPED_VARIADIC_FLAG
        | SemanticTypeRecord::FUNCTION_C_VARIADIC_FLAG;
    assert_eq!(
        c_tail.validate(0),
        Err(SemanticTypeFault::ReservedCell {
            tag: SemanticTypeTag::FunctionPointer,
            cell: TypeCell::Payload0,
            actual: SemanticTypeRecord::FUNCTION_TYPED_VARIADIC_FLAG
                | SemanticTypeRecord::FUNCTION_C_VARIADIC_FLAG,
        })
    );
}
