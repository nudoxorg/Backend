//! Hostile full-image seam falsifiers.
//!
//! These remain deliberately small: the image builder admits an ordinary
//! captured-empty declaration plane, then the test exercises the complete
//! writer/reopen boundary rather than a second fixture-only semantic model.

use alloc::vec;

#[cfg(feature = "mmap")]
use crate::ir::DocInput;
use crate::ir::{
    BorrowedTree, BuiltinType, CSharpFacts, CSharpMemberEffects, CSharpNullability,
    CSharpPartialRole, CSharpReferenceKind, CSharpVersion, ConcreteType, CorePayloadHash,
    DeclarationFamilyId, EntityAuthorityFacts, EntityId, EntityVersion, FactAvailability,
    FreePredicate, Ir, IrBuilder, ItemKind, LanguageExtensionInput, LanguageProfile,
    ParentageAuthority, RustEdition, RustFacts, RustOwnership, SemanticCoreReader,
    SemanticImageAuthority, SemanticImageEncodeError, SemanticReader, SignatureCarrierBinding,
    SignatureCarrierBindingRole, SignatureCarrierBindingsObservation, SignatureCarrierOwnerInput,
    SignatureCarrierRole, SignatureCarrierRoleObservation, TreeItemInput, TupleElement,
    TupleElementKind, TypeExpr, TypeParameterBound, TypeScriptSource, VariadicForm,
    VariantFingerprint, Visibility,
};

use super::wire::{
    DIRECTORY_BYTES, ENTITY_ROW_BYTES, FullDirectoryKind, HEADER_BYTES, RANGE_ROW_BYTES,
    SIGNATURE_CARRIER_RANGE_ROW_BYTES, SIGNATURE_CARRIER_TARGET_ROW_BYTES,
    SPARSE_BINDING_ROW_BYTES,
};
use super::{
    FullSemanticImageError, FullSemanticImageFault, FullSemanticImageField,
    SemanticImageProofOwner, SemanticImageView, encode_full_semantic_image,
    full_semantic_image_len,
};

fn version(value: u8) -> EntityVersion {
    EntityVersion {
        family: DeclarationFamilyId::from_raw([value; 16]),
        variant: VariantFingerprint::from_raw([value.wrapping_add(1); 16]),
        core_payload: CorePayloadHash::from_raw([value.wrapping_add(2); 16]),
    }
}

fn authority() -> EntityAuthorityFacts {
    EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        members: FactAvailability::Captured,
        documentation: FactAvailability::Captured,
        attributes: FactAvailability::Captured,
        visibility: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    }
}

fn image(reversed: bool) -> Result<Ir, crate::ir::BuildError> {
    let alpha = TreeItemInput {
        name: b"alpha",
        kind: ItemKind::Module,
        visibility: Visibility::Private,
        authority: authority(),
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: None,
    };
    let beta = TreeItemInput {
        name: b"beta",
        ..alpha
    };
    let mut builder = IrBuilder::new();
    if reversed {
        let versions = [version(2), version(1)];
        let items = [beta, alpha];
        builder.add_borrowed_tree(BorrowedTree {
            versions: &versions,
            items: &items,
            links: &[],
        })?;
    } else {
        let versions = [version(1), version(2)];
        let items = [alpha, beta];
        builder.add_borrowed_tree(BorrowedTree {
            versions: &versions,
            items: &items,
            links: &[],
        })?;
    }
    builder.finish()
}

fn carrier_image(
    roles: &[SignatureCarrierRole],
    reversed: bool,
) -> Result<Ir, crate::ir::BuildError> {
    let parameter = TreeItemInput {
        name: b"a",
        kind: ItemKind::Parameter,
        visibility: Visibility::Private,
        authority: authority(),
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: None,
    };
    let items = [
        parameter,
        TreeItemInput {
            name: b"b",
            ..parameter
        },
        TreeItemInput {
            name: b"c",
            ..parameter
        },
        TreeItemInput {
            name: b"d",
            ..parameter
        },
    ];
    let versions = [version(1), version(2), version(3), version(4)];
    let mut ordered_items = Vec::with_capacity(roles.len());
    let mut ordered_versions = Vec::with_capacity(roles.len());
    let mut ordered_roles = Vec::with_capacity(roles.len());
    for input_index in 0..roles.len() {
        let canonical_index = if reversed {
            roles.len() - input_index - 1
        } else {
            input_index
        };
        ordered_items.push(items[canonical_index]);
        ordered_versions.push(versions[canonical_index]);
        ordered_roles.push(roles[canonical_index]);
    }
    let mut builder = IrBuilder::new();
    builder.add_borrowed_tree(BorrowedTree {
        versions: &ordered_versions,
        items: &ordered_items,
        links: &[],
    })?;
    builder.capture_signature_carrier_roles(&ordered_roles)?;
    builder.finish()
}

fn signature_binding_image(reversed: bool) -> Result<Ir, crate::ir::BuildError> {
    let mut builder = IrBuilder::new();
    let scalar =
        builder.intern_type(TypeExpr::Concrete(ConcreteType::Builtin(BuiltinType::U32)))?;
    let different_scalar =
        builder.intern_type(TypeExpr::Concrete(ConcreteType::Builtin(BuiltinType::F64)))?;
    let parameter_cells = [
        TupleElement {
            label: None,
            ty: scalar,
            kind: TupleElementKind::Required,
        },
        TupleElement {
            label: None,
            ty: scalar,
            kind: TupleElementKind::Required,
        },
    ];
    let result_cells = [TupleElement {
        label: None,
        ty: scalar,
        kind: TupleElementKind::Required,
    }];
    let parameters = builder.intern_tuple_elements(&parameter_cells)?;
    let results = builder.intern_tuple_elements(&result_cells)?;
    let empty = builder.intern_tuple_elements(&[])?;
    let callable = builder.intern_type(TypeExpr::Concrete(ConcreteType::Function {
        parameters,
        results,
        abi: None,
        variadic: VariadicForm::None,
        unsafe_: false,
    }))?;
    let empty_callable = builder.intern_type(TypeExpr::Concrete(ConcreteType::Function {
        parameters: empty,
        results: empty,
        abi: None,
        variadic: VariadicForm::None,
        unsafe_: false,
    }))?;
    let item = |name, kind, semantic_type| TreeItemInput {
        name,
        kind,
        visibility: Visibility::Private,
        authority: authority(),
        parent: None,
        semantic_type,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: None,
    };
    let original_items = [
        item(b"bind", ItemKind::Function, Some(callable)),
        item(b"unknown", ItemKind::Function, None),
        item(b"empty", ItemKind::Function, Some(empty_callable)),
        item(b"again", ItemKind::Function, Some(callable)),
        item(b"carrier", ItemKind::Parameter, Some(scalar)),
        item(b"different", ItemKind::Parameter, Some(different_scalar)),
        item(b"other_carrier", ItemKind::Parameter, Some(scalar)),
    ];
    let carrier_old_index = original_items
        .iter()
        .position(|item| item.name == b"carrier" && item.kind == ItemKind::Parameter)
        .expect("fixture contains the named parameter carrier");
    let other_carrier_old_index = original_items
        .iter()
        .position(|item| item.name == b"other_carrier" && item.kind == ItemKind::Parameter)
        .expect("fixture contains the second named parameter carrier");
    let original_versions = [
        version(10),
        version(11),
        version(12),
        version(13),
        version(14),
        version(15),
        version(16),
    ];
    let mut ordered_items = Vec::with_capacity(original_items.len());
    let mut ordered_versions = Vec::with_capacity(original_versions.len());
    let mut old_to_new = [0_u32; 7];
    for new_index in 0..original_items.len() {
        let old_index = if reversed {
            original_items.len() - new_index - 1
        } else {
            new_index
        };
        old_to_new[old_index] = u32::try_from(new_index).map_err(|_| {
            crate::ir::BuildError::Capacity(crate::ir::CapacityError {
                space: crate::ir::CapacitySpace::Value,
                actual: new_index,
            })
        })?;
        ordered_items.push(original_items[old_index]);
        ordered_versions.push(original_versions[old_index]);
    }
    builder.add_borrowed_tree(BorrowedTree {
        versions: &ordered_versions,
        items: &ordered_items,
        links: &[],
    })?;

    let mut owners = Vec::new();
    let mut targets = Vec::new();
    for new_index in 0..ordered_items.len() {
        let old_index = if reversed {
            ordered_items.len() - new_index - 1
        } else {
            new_index
        };
        let owner = EntityId::new(u32::try_from(new_index).map_err(|_| {
            crate::ir::BuildError::Capacity(crate::ir::CapacityError {
                space: crate::ir::CapacitySpace::Value,
                actual: new_index,
            })
        })?);
        match old_index {
            0 => {
                owners.push(SignatureCarrierOwnerInput::captured(owner, 2, 1));
                let carrier = EntityId::new(old_to_new[carrier_old_index]);
                let selected = ordered_items
                    .get(usize::try_from(carrier.raw).expect("fixture carrier index fits usize"))
                    .expect("fixture carrier index names an item");
                assert_eq!(selected.name, b"carrier");
                assert_eq!(selected.kind, ItemKind::Parameter);
                targets.extend_from_slice(&[carrier, carrier, carrier]);
            }
            1 => owners.push(SignatureCarrierOwnerInput::unavailable(owner)),
            2 => owners.push(SignatureCarrierOwnerInput::captured(owner, 0, 0)),
            3 => {
                owners.push(SignatureCarrierOwnerInput::captured(owner, 2, 1));
                let other_carrier = EntityId::new(old_to_new[other_carrier_old_index]);
                let selected = ordered_items
                    .get(
                        usize::try_from(other_carrier.raw)
                            .expect("fixture second-carrier index fits usize"),
                    )
                    .expect("fixture second-carrier index names an item");
                assert_eq!(selected.name, b"other_carrier");
                assert_eq!(selected.kind, ItemKind::Parameter);
                targets.extend_from_slice(&[other_carrier, other_carrier, other_carrier]);
            }
            _ => {}
        }
    }
    builder.capture_signature_carrier_bindings(&owners, &targets)?;
    builder.finish()
}

#[test]
fn large_mixed_tuple_signature_reopens_with_linear_binding_cell_advances()
-> Result<(), crate::ir::BuildError> {
    const PARAMETER_COUNT: usize = 64;

    let mut builder = IrBuilder::new();
    let scalar = builder.intern_type(TypeExpr::Concrete(ConcreteType::Builtin(
        BuiltinType::U32,
    )))?;
    let argument_name = builder.intern_atom(b"argument")?;
    let result_name = builder.intern_atom(b"result")?;
    let mut parameters = Vec::with_capacity(PARAMETER_COUNT);
    for position in 0..PARAMETER_COUNT {
        parameters.push(TupleElement {
            label: (position % 2 == 0).then_some(argument_name),
            ty: scalar,
            kind: if position + 1 == PARAMETER_COUNT {
                TupleElementKind::Optional
            } else {
                TupleElementKind::Required
            },
        });
    }
    let parameter_list = builder.intern_tuple_elements(&parameters)?;
    let result_list = builder.intern_tuple_elements(&[TupleElement {
        label: Some(result_name),
        ty: scalar,
        kind: TupleElementKind::Required,
    }])?;
    let function_type = builder.intern_type(TypeExpr::Concrete(ConcreteType::Function {
        parameters: parameter_list,
        results: result_list,
        abi: None,
        variadic: VariadicForm::None,
        unsafe_: false,
    }))?;
    let function = TreeItemInput {
        name: b"large",
        kind: ItemKind::Function,
        visibility: Visibility::Private,
        authority: authority(),
        parent: None,
        semantic_type: Some(function_type),
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: None,
    };
    let carrier = TreeItemInput {
        name: b"carrier",
        kind: ItemKind::Parameter,
        semantic_type: Some(scalar),
        ..function
    };
    let items = [function, carrier];
    let versions = [version(30), version(31)];
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &[],
    })?;
    builder.capture_signature_carrier_bindings(
        &[SignatureCarrierOwnerInput::captured(
            EntityId::new(0),
            u32::try_from(PARAMETER_COUNT).expect("fixture count fits the wire width"),
            1,
        )],
        &vec![EntityId::new(1); PARAMETER_COUNT + 1],
    )?;

    let ir = builder.finish()?;
    let bytes = encoded(&ir)?;
    super::typed_decode::reset_tuple_element_visits();
    let view = SemanticImageView::reopen(&bytes).expect("mixed signature image reopens");
    assert_eq!(
        super::typed_decode::tuple_element_visits(),
        u64::try_from((PARAMETER_COUNT + 1) * 2).expect("fixture work fits the metric"),
        "grammar admission and binding validation each advance through every tuple cell once"
    );

    let entity_by_name = |name: &[u8]| {
        view.canonical_entities()
            .find_map(|entity| (view.atom(entity.name) == Some(name)).then_some(entity.id))
    };
    let owner = entity_by_name(b"large").expect("canonical function owner");
    let carrier = entity_by_name(b"carrier").expect("canonical shared carrier");
    let function_type = view
        .entity(owner)
        .and_then(|entity| entity.semantic_type)
        .expect("function semantic type");
    let TypeExpr::Concrete(ConcreteType::Function {
        parameters,
        results,
        ..
    }) = view.ty(function_type).expect("reopened function type")
    else {
        panic!("function declaration has a function type");
    };
    let parameter_cells = view
        .tuple_elements(parameters)
        .expect("parameter tuple cells")
        .collect::<Vec<_>>();
    assert_eq!(parameter_cells.len(), PARAMETER_COUNT);
    for (position, cell) in parameter_cells.iter().enumerate() {
        if position % 2 == 0 {
            let label = cell.label.expect("alternating parameter label");
            assert_eq!(view.atom(label), Some(&b"argument"[..]));
        } else {
            assert_eq!(cell.label, None, "anonymous tuple cell at {position}");
        }
        assert_eq!(
            cell.kind,
            if position + 1 == PARAMETER_COUNT {
                TupleElementKind::Optional
            } else {
                TupleElementKind::Required
            }
        );
    }
    let result_cells = view
        .tuple_elements(results)
        .expect("result tuple cell")
        .collect::<Vec<_>>();
    assert_eq!(result_cells.len(), 1);
    assert_eq!(
        result_cells[0]
            .label
            .and_then(|label| view.atom(label)),
        Some(&b"result"[..])
    );
    assert_eq!(result_cells[0].kind, TupleElementKind::Required);

    let Some(SignatureCarrierBindingsObservation::Captured(bindings)) =
        SemanticReader::signature_carrier_bindings(&view, owner)
    else {
        panic!("mixed tuple signature has captured carrier bindings");
    };
    let bindings = bindings.collect::<Vec<_>>();
    assert_eq!(bindings.len(), PARAMETER_COUNT + 1);
    for (position, binding) in bindings[..PARAMETER_COUNT].iter().enumerate() {
        assert_eq!(
            *binding,
            SignatureCarrierBinding {
                owner,
                role: SignatureCarrierBindingRole::Parameter,
                position: u32::try_from(position).expect("fixture index fits wire width"),
                carrier,
            }
        );
    }
    assert_eq!(
        bindings[PARAMETER_COUNT],
        SignatureCarrierBinding {
            owner,
            role: SignatureCarrierBindingRole::Result,
            position: 0,
            carrier,
        }
    );
    assert_eq!(
        SemanticReader::signature_carrier_role(&view, carrier),
        Some(SignatureCarrierRoleObservation::Captured(SignatureCarrierRole::Both))
    );
    Ok(())
}

fn encoded(ir: &Ir) -> Result<alloc::vec::Vec<u8>, crate::ir::BuildError> {
    let length = full_semantic_image_len(ir).expect("full image plan is admitted");
    let mut bytes = vec![0; length];
    encode_full_semantic_image(ir, &mut bytes).expect("full image writes");
    Ok(bytes)
}

fn lane_payload_offset(bytes: &[u8], kind: FullDirectoryKind) -> usize {
    let entry = HEADER_BYTES + kind.index() * DIRECTORY_BYTES;
    u32::from_le_bytes(
        bytes[entry + 4..entry + 8]
            .try_into()
            .expect("directory offset cell"),
    ) as usize
}

fn set_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn signature_binding_size_preflight_checks_wire_width_without_allocating() {
    for (row_bytes, field) in [
        (
            SIGNATURE_CARRIER_RANGE_ROW_BYTES,
            FullSemanticImageField::SignatureCarrierBindingRanges,
        ),
        (
            SIGNATURE_CARRIER_TARGET_ROW_BYTES,
            FullSemanticImageField::SignatureCarrierBindingTargets,
        ),
    ] {
        let max_wire_bytes = u32::MAX as usize;
        let largest_rows = max_wire_bytes / row_bytes;
        let largest_bytes = largest_rows * row_bytes;
        let observed_bytes = super::plan::signature_binding_lane_bytes(
            largest_rows,
            row_bytes,
            field,
        )
        .expect("largest whole-row count fits the wire width");
        assert_eq!(observed_bytes, largest_bytes);
        assert!(matches!(
            super::plan::signature_binding_lane_bytes(largest_rows + 1, row_bytes, field),
            Err(FullSemanticImageFault::LengthOverflow { field: observed }) if observed == field
        ));
    }

    let half_wire = (u32::MAX as usize) / 2;
    let observed_bytes = super::plan::signature_binding_lanes_bytes(half_wire, half_wire)
        .expect("two half-width lanes fit the directory wire width");
    assert_eq!(observed_bytes, half_wire * 2);
    assert!(matches!(
        super::plan::signature_binding_lanes_bytes(half_wire + 1, half_wire + 1),
        Err(FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::Directory
        })
    ));
}

#[cfg(feature = "mmap")]
fn large_encoded_image() -> Vec<u8> {
    let large_documentation = "selected native image mapping ".repeat(2_000);
    let docs = [DocInput::Text(&large_documentation)];
    let item = TreeItemInput {
        name: b"mapped",
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        authority: authority(),
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &docs,
        attributes: &[],
        source: None,
        extension: None,
    };
    let versions = [version(7)];
    let items = [item];
    let mut builder = IrBuilder::new();
    builder
        .add_borrowed_tree(BorrowedTree {
            versions: &versions,
            items: &items,
            links: &[],
        })
        .expect("large mapped fixture builds");
    encoded(&builder.finish().expect("large mapped IR finalizes"))
        .expect("large mapped fixture encodes")
}

#[test]
fn an_admission_proof_fails_closed_for_a_different_backing_allocation() {
    let image_a = encoded(&image(false).expect("first canonical IR builds"))
        .expect("first canonical image encodes");
    let image_b = encoded(&csharp_image().expect("second canonical IR builds"))
        .expect("second canonical image encodes");
    assert_ne!(image_a, image_b);

    let admitted = SemanticImageView::reopen(&image_a).expect("first image admits");
    let proof = admitted.proof();
    assert!(matches!(
        SemanticImageView::reopen_proven(&image_b, proof),
        Err(FullSemanticImageError::ProofBackingMismatch)
    ));
}

#[test]
fn owned_proof_cache_only_admits_successful_validation() {
    use crate::ir::{reset_semantic_image_validations, semantic_image_validations};

    let valid = encoded(&image(false).expect("canonical IR builds"))
        .expect("canonical image encodes")
        .into_boxed_slice();
    let owner = SemanticImageProofOwner::new(valid);
    reset_semantic_image_validations();
    assert!(owner.reopen().is_ok());
    assert!(owner.reopen().is_ok());
    assert_eq!(semantic_image_validations(), 1);

    let invalid = SemanticImageProofOwner::new(Box::<[u8]>::default());
    reset_semantic_image_validations();
    assert!(invalid.reopen().is_err());
    assert!(invalid.reopen().is_err());
    assert_eq!(semantic_image_validations(), 2);
}

#[test]
fn legacy_full_images_report_role_unavailable_without_inventing_negative_facts()
-> Result<(), crate::ir::BuildError> {
    let legacy = encoded(&image(false)?)?;
    assert_eq!(u16::from_le_bytes([legacy[4], legacy[5]]), 1);
    assert_eq!(u16::from_le_bytes([legacy[6], legacy[7]]), 26);
    let view = SemanticImageView::reopen(&legacy).expect("legacy image reopens");
    assert_eq!(
        SemanticReader::signature_carrier_role(&view, EntityId::new(0)),
        Some(SignatureCarrierRoleObservation::Unavailable)
    );
    assert_eq!(
        SemanticReader::signature_carrier_role(&view, EntityId::new(99)),
        None
    );
    Ok(())
}

#[test]
fn complete_carrier_roles_round_trip_in_canonical_entity_order() -> Result<(), crate::ir::BuildError>
{
    let expected = [
        SignatureCarrierRole::NotCarrier,
        SignatureCarrierRole::Input,
        SignatureCarrierRole::Result,
        SignatureCarrierRole::Both,
    ];
    let ir = carrier_image(&expected, true)?;
    for item in ir.items() {
        let observed =
            SemanticReader::signature_carrier_role(&ir, item.id()).expect("owned entity exists");
        let role = if item.name() == b"a" {
            expected[0]
        } else if item.name() == b"b" {
            expected[1]
        } else if item.name() == b"c" {
            expected[2]
        } else if item.name() == b"d" {
            expected[3]
        } else {
            return Err(crate::ir::BuildError::Dangling {
                space: crate::ir::SemanticSpace::Entity,
                raw: item.id().raw,
            });
        };
        assert_eq!(observed, SignatureCarrierRoleObservation::Captured(role));
    }

    let bytes = encoded(&ir)?;
    let forward_bytes = encoded(&carrier_image(&expected, false)?)?;
    assert_eq!(
        bytes, forward_bytes,
        "canonical entity ordering must make role-image bytes independent of insertion order"
    );
    assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 2);
    assert_eq!(u16::from_le_bytes([bytes[6], bytes[7]]), 27);
    // The result owner retains the exact canonical payload and cached proof,
    // so the role lane must survive the owned-output reopen boundary.
    let image_owner = super::SemanticImageProofOwner::new(bytes.into_boxed_slice());
    let view = image_owner.reopen().expect("schema-2 image reopens");
    for entity in view.canonical_entities() {
        let name = view.atom(entity.name).expect("canonical entity name");
        let expected_role = if name == b"a" {
            expected[0]
        } else if name == b"b" {
            expected[1]
        } else if name == b"c" {
            expected[2]
        } else if name == b"d" {
            expected[3]
        } else {
            return Err(crate::ir::BuildError::Dangling {
                space: crate::ir::SemanticSpace::Entity,
                raw: entity.id.raw,
            });
        };
        assert_eq!(
            SemanticReader::signature_carrier_role(&view, entity.id),
            Some(SignatureCarrierRoleObservation::Captured(expected_role))
        );
    }
    Ok(())
}

#[test]
fn exact_signature_bindings_preserve_owner_slots_empty_and_unavailable_states()
-> Result<(), crate::ir::BuildError> {
    let ir = signature_binding_image(false)?;
    let find_owned = |name: &[u8]| {
        ir.items()
            .find(|item| item.name() == name)
            .map(|item| item.id())
    };
    let bind = find_owned(b"bind").expect("known callable owner");
    let unknown = find_owned(b"unknown").expect("unknown callable owner");
    let empty = find_owned(b"empty").expect("known empty callable owner");
    let again = find_owned(b"again").expect("second owner of the shared carrier");
    let carrier = find_owned(b"carrier").expect("shared carrier row");
    let other_carrier = find_owned(b"other_carrier").expect("second typed carrier row");
    let item_type = |id| {
        ir.items()
            .find(|item| item.id() == id)
            .and_then(|item| item.semantic_type())
    };
    assert_eq!(item_type(bind), item_type(again));
    assert_ne!(carrier, other_carrier);
    assert_eq!(item_type(carrier), item_type(other_carrier));
    assert!(matches!(
        SemanticReader::signature_carrier_bindings(&ir, carrier),
        None
    ));

    let Some(SignatureCarrierBindingsObservation::Captured(bindings)) =
        SemanticReader::signature_carrier_bindings(&ir, bind)
    else {
        panic!("known function type must expose captured bindings");
    };
    let observed: Vec<_> = bindings.collect();
    assert_eq!(
        observed,
        vec![
            SignatureCarrierBinding {
                owner: bind,
                role: SignatureCarrierBindingRole::Parameter,
                position: 0,
                carrier,
            },
            SignatureCarrierBinding {
                owner: bind,
                role: SignatureCarrierBindingRole::Parameter,
                position: 1,
                carrier,
            },
            SignatureCarrierBinding {
                owner: bind,
                role: SignatureCarrierBindingRole::Result,
                position: 0,
                carrier,
            },
        ]
    );
    assert!(matches!(
        SemanticReader::signature_carrier_bindings(&ir, unknown),
        Some(SignatureCarrierBindingsObservation::Unavailable)
    ));
    let Some(SignatureCarrierBindingsObservation::Captured(empty_bindings)) =
        SemanticReader::signature_carrier_bindings(&ir, empty)
    else {
        panic!("known empty function type must remain captured");
    };
    assert_eq!(empty_bindings.len(), 0);
    let Some(SignatureCarrierBindingsObservation::Captured(again_bindings)) =
        SemanticReader::signature_carrier_bindings(&ir, again)
    else {
        panic!("second owner bindings must be captured");
    };
    assert_eq!(
        again_bindings.collect::<Vec<_>>(),
        vec![
            SignatureCarrierBinding {
                owner: again,
                role: SignatureCarrierBindingRole::Parameter,
                position: 0,
                carrier: other_carrier,
            },
            SignatureCarrierBinding {
                owner: again,
                role: SignatureCarrierBindingRole::Parameter,
                position: 1,
                carrier: other_carrier,
            },
            SignatureCarrierBinding {
                owner: again,
                role: SignatureCarrierBindingRole::Result,
                position: 0,
                carrier: other_carrier,
            },
        ]
    );
    assert_eq!(
        SemanticReader::signature_carrier_role(&ir, carrier),
        Some(SignatureCarrierRoleObservation::Captured(
            SignatureCarrierRole::Both
        ))
    );
    assert_eq!(
        SemanticReader::signature_carrier_role(&ir, other_carrier),
        Some(SignatureCarrierRoleObservation::Captured(
            SignatureCarrierRole::Both
        ))
    );

    let bytes = encoded(&ir)?;
    assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 3);
    assert_eq!(u16::from_le_bytes([bytes[6], bytes[7]]), 29);
    assert_eq!(bytes, encoded(&signature_binding_image(true)?)?);
    let view = SemanticImageView::reopen(&bytes).expect("schema-3 image reopens");
    let find_borrowed = |name: &[u8]| {
        view.canonical_entities()
            .find_map(|entity| (view.atom(entity.name) == Some(name)).then_some(entity.id))
    };
    let bind = find_borrowed(b"bind").expect("canonical bind owner");
    let unknown = find_borrowed(b"unknown").expect("canonical unknown owner");
    let empty = find_borrowed(b"empty").expect("canonical empty owner");
    let again = find_borrowed(b"again").expect("canonical second owner");
    let carrier = find_borrowed(b"carrier").expect("canonical carrier row");
    let other_carrier = find_borrowed(b"other_carrier").expect("canonical second carrier row");
    let Some(SignatureCarrierBindingsObservation::Captured(bindings)) =
        SemanticReader::signature_carrier_bindings(&view, bind)
    else {
        panic!("reopened function bindings must be captured");
    };
    assert_eq!(
        bindings.collect::<Vec<_>>(),
        vec![
            SignatureCarrierBinding {
                owner: bind,
                role: SignatureCarrierBindingRole::Parameter,
                position: 0,
                carrier,
            },
            SignatureCarrierBinding {
                owner: bind,
                role: SignatureCarrierBindingRole::Parameter,
                position: 1,
                carrier,
            },
            SignatureCarrierBinding {
                owner: bind,
                role: SignatureCarrierBindingRole::Result,
                position: 0,
                carrier,
            },
        ]
    );
    let Some(SignatureCarrierBindingsObservation::Captured(bindings)) =
        SemanticReader::signature_carrier_bindings(&view, again)
    else {
        panic!("reopened second owner bindings must be captured");
    };
    assert_eq!(
        bindings.collect::<Vec<_>>(),
        vec![
            SignatureCarrierBinding {
                owner: again,
                role: SignatureCarrierBindingRole::Parameter,
                position: 0,
                carrier: other_carrier,
            },
            SignatureCarrierBinding {
                owner: again,
                role: SignatureCarrierBindingRole::Parameter,
                position: 1,
                carrier: other_carrier,
            },
            SignatureCarrierBinding {
                owner: again,
                role: SignatureCarrierBindingRole::Result,
                position: 0,
                carrier: other_carrier,
            },
        ]
    );
    assert!(matches!(
        SemanticReader::signature_carrier_bindings(&view, unknown),
        Some(SignatureCarrierBindingsObservation::Unavailable)
    ));
    let Some(SignatureCarrierBindingsObservation::Captured(empty_bindings)) =
        SemanticReader::signature_carrier_bindings(&view, empty)
    else {
        panic!("reopened empty signature must stay captured");
    };
    assert_eq!(empty_bindings.len(), 0);
    let Some(SignatureCarrierBindingsObservation::Captured(again_bindings)) =
        SemanticReader::signature_carrier_bindings(&view, again)
    else {
        panic!("reopened second-owner bindings must be captured");
    };
    assert_eq!(
        again_bindings.collect::<Vec<_>>(),
        vec![
            SignatureCarrierBinding {
                owner: again,
                role: SignatureCarrierBindingRole::Parameter,
                position: 0,
                carrier: other_carrier,
            },
            SignatureCarrierBinding {
                owner: again,
                role: SignatureCarrierBindingRole::Parameter,
                position: 1,
                carrier: other_carrier,
            },
            SignatureCarrierBinding {
                owner: again,
                role: SignatureCarrierBindingRole::Result,
                position: 0,
                carrier: other_carrier,
            },
        ]
    );
    assert_eq!(
        SemanticReader::signature_carrier_role(&view, carrier),
        Some(SignatureCarrierRoleObservation::Captured(
            SignatureCarrierRole::Both
        ))
    );
    assert_eq!(
        SemanticReader::signature_carrier_role(&view, other_carrier),
        Some(SignatureCarrierRoleObservation::Captured(
            SignatureCarrierRole::Both
        ))
    );
    Ok(())
}

#[test]
fn schema_two_function_has_role_but_no_binding_claim() -> Result<(), crate::ir::BuildError> {
    let function = TreeItemInput {
        name: b"historical",
        kind: ItemKind::Function,
        visibility: Visibility::Private,
        authority: authority(),
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: None,
    };
    let versions = [version(42)];
    let items = [function];
    let mut builder = IrBuilder::new();
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &[],
    })?;
    builder.capture_signature_carrier_roles(&[SignatureCarrierRole::NotCarrier])?;
    let bytes = encoded(&builder.finish()?)?;
    assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 2);
    let view = SemanticImageView::reopen(&bytes).expect("schema-2 function reopens");
    let owner = view.canonical_entities().next().expect("function row").id;
    assert_eq!(
        SemanticReader::signature_carrier_role(&view, owner),
        Some(SignatureCarrierRoleObservation::Captured(
            SignatureCarrierRole::NotCarrier
        ))
    );
    assert!(matches!(
        SemanticReader::signature_carrier_bindings(&view, owner),
        Some(SignatureCarrierBindingsObservation::Unavailable)
    ));
    Ok(())
}

#[test]
fn schema_one_function_reports_signature_bindings_unavailable() -> Result<(), crate::ir::BuildError>
{
    let function = TreeItemInput {
        name: b"historical",
        kind: ItemKind::Function,
        visibility: Visibility::Private,
        authority: authority(),
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: None,
    };
    let versions = [version(43)];
    let items = [function];
    let mut builder = IrBuilder::new();
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &[],
    })?;
    let bytes = encoded(&builder.finish()?)?;
    assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 1);
    let view = SemanticImageView::reopen(&bytes).expect("schema-1 function reopens");
    let owner = view.canonical_entities().next().expect("function row").id;
    assert_eq!(
        SemanticReader::signature_carrier_role(&view, owner),
        Some(SignatureCarrierRoleObservation::Unavailable)
    );
    assert!(matches!(
        SemanticReader::signature_carrier_bindings(&view, owner),
        Some(SignatureCarrierBindingsObservation::Unavailable)
    ));
    Ok(())
}

#[test]
fn signature_binding_cursor_is_exact_size_and_fused() -> Result<(), crate::ir::BuildError> {
    fn assert_fused<I: core::iter::FusedIterator>(_: &I) {}

    let ir = signature_binding_image(false)?;
    let bind = ir
        .items()
        .find(|item| item.name() == b"bind" && item.kind() == ItemKind::Function)
        .expect("known function owner")
        .id();
    let carrier = ir
        .items()
        .find(|item| item.name() == b"carrier" && item.kind() == ItemKind::Parameter)
        .expect("known parameter carrier")
        .id();
    let Some(SignatureCarrierBindingsObservation::Captured(mut bindings)) =
        SemanticReader::signature_carrier_bindings(&ir, bind)
    else {
        panic!("known function type must expose captured bindings");
    };
    assert_fused(&bindings);

    let expected = [
        SignatureCarrierBinding {
            owner: bind,
            role: SignatureCarrierBindingRole::Parameter,
            position: 0,
            carrier,
        },
        SignatureCarrierBinding {
            owner: bind,
            role: SignatureCarrierBindingRole::Parameter,
            position: 1,
            carrier,
        },
        SignatureCarrierBinding {
            owner: bind,
            role: SignatureCarrierBindingRole::Result,
            position: 0,
            carrier,
        },
    ];
    let expected_len = expected.len();
    for (index, binding) in expected.into_iter().enumerate() {
        let remaining = expected_len - index;
        assert_eq!(bindings.len(), remaining);
        assert_eq!(bindings.size_hint(), (remaining, Some(remaining)));
        assert_eq!(bindings.next(), Some(binding));
    }
    assert_eq!(bindings.len(), 0);
    assert_eq!(bindings.size_hint(), (0, Some(0)));
    assert_eq!(bindings.next(), None);
    assert_eq!(bindings.next(), None);

    let bytes = encoded(&ir)?;
    let view = SemanticImageView::reopen(&bytes).expect("owned image reopens canonically");
    let canonical_entity = |name: &[u8], kind: ItemKind| {
        view.canonical_entities().find_map(|entity| {
            (entity.kind == kind && view.atom(entity.name) == Some(name)).then_some(entity.id)
        })
    };
    let reopened_bind = canonical_entity(b"bind", ItemKind::Function)
        .expect("canonical remapped binding owner");
    let reopened_carrier = canonical_entity(b"carrier", ItemKind::Parameter)
        .expect("canonical remapped carrier");
    let Some(SignatureCarrierBindingsObservation::Captured(mut reopened)) =
        SemanticReader::signature_carrier_bindings(&view, reopened_bind)
    else {
        panic!("canonical borrowed reader exposes captured bindings");
    };
    assert_fused(&reopened);
    let expected = [
        SignatureCarrierBinding {
            owner: reopened_bind,
            role: SignatureCarrierBindingRole::Parameter,
            position: 0,
            carrier: reopened_carrier,
        },
        SignatureCarrierBinding {
            owner: reopened_bind,
            role: SignatureCarrierBindingRole::Parameter,
            position: 1,
            carrier: reopened_carrier,
        },
        SignatureCarrierBinding {
            owner: reopened_bind,
            role: SignatureCarrierBindingRole::Result,
            position: 0,
            carrier: reopened_carrier,
        },
    ];
    for (index, binding) in expected.into_iter().enumerate() {
        let remaining = 3 - index;
        assert_eq!(reopened.len(), remaining);
        assert_eq!(reopened.size_hint(), (remaining, Some(remaining)));
        assert_eq!(reopened.next(), Some(binding));
    }
    assert_eq!(reopened.len(), 0);
    assert_eq!(reopened.size_hint(), (0, Some(0)));
    assert_eq!(reopened.next(), None);
    assert_eq!(reopened.next(), None);
    Ok(())
}

#[test]
fn signature_binding_image_rejects_bad_owner_ranges_targets_and_role_union()
-> Result<(), crate::ir::BuildError> {
    let bytes = encoded(&signature_binding_image(false)?)?;
    let view = SemanticImageView::reopen(&bytes).expect("valid binding image");
    let id_by_name = |name: &[u8]| {
        view.canonical_entities()
            .find_map(|entity| (view.atom(entity.name) == Some(name)).then_some(entity.id))
    };
    let bind = id_by_name(b"bind").expect("bind owner");
    let unknown = id_by_name(b"unknown").expect("unknown owner");
    let function_target = bind;
    let carrier = id_by_name(b"carrier").expect("carrier");
    let different = id_by_name(b"different").expect("different carrier");
    let ranges = lane_payload_offset(&bytes, FullDirectoryKind::SignatureCarrierBindingRanges);
    let range_count_entry =
        HEADER_BYTES + FullDirectoryKind::SignatureCarrierBindingRanges.index() * DIRECTORY_BYTES;
    let range_count = u32::from_le_bytes(
        bytes[range_count_entry + 12..range_count_entry + 16]
            .try_into()
            .expect("range count cell"),
    );
    let mut bind_range = None;
    for row in 0..range_count {
        let offset = ranges + usize::try_from(row).unwrap_or(0) * 16;
        let owner = u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("owner cell"));
        if owner == bind.raw {
            bind_range = Some(offset);
            break;
        }
    }
    let bind_range = bind_range.expect("captured bind range");
    let target_start = u32::from_le_bytes(
        bytes[bind_range + 4..bind_range + 8]
            .try_into()
            .expect("target start cell"),
    );
    let targets = lane_payload_offset(&bytes, FullDirectoryKind::SignatureCarrierBindingTargets);

    let mut bad_owner = bytes.clone();
    set_u32(&mut bad_owner, bind_range, unknown.raw);
    assert!(matches!(
        SemanticImageView::reopen(&bad_owner),
        Err(FullSemanticImageError::Full(
            FullSemanticImageFault::SignatureCarrierBindingOwnerSet { .. }
        ))
    ));

    let mut broken_partition = bytes.clone();
    set_u32(&mut broken_partition, bind_range + 4, target_start + 1);
    assert!(matches!(
        SemanticImageView::reopen(&broken_partition),
        Err(FullSemanticImageError::Full(
            FullSemanticImageFault::SignatureCarrierBindingRange { .. }
        ))
    ));

    let mut wrong_type = bytes.clone();
    let target_offset = targets + usize::try_from(target_start).unwrap_or(0) * 4;
    set_u32(&mut wrong_type, target_offset, different.raw);
    assert!(matches!(
        SemanticImageView::reopen(&wrong_type),
        Err(FullSemanticImageError::Full(
            FullSemanticImageFault::SignatureCarrierBindingType { .. }
        ))
    ));

    let mut wrong_kind = bytes.clone();
    set_u32(&mut wrong_kind, target_offset, function_target.raw);
    assert!(matches!(
        SemanticImageView::reopen(&wrong_kind),
        Err(FullSemanticImageError::Full(
            FullSemanticImageFault::SignatureCarrierBindingTargetKind { .. }
        ))
    ));

    let mut wrong_count = bytes.clone();
    set_u32(&mut wrong_count, bind_range + 8, 1);
    assert!(matches!(
        SemanticImageView::reopen(&wrong_count),
        Err(FullSemanticImageError::Full(
            FullSemanticImageFault::SignatureCarrierBindingRange { .. }
        ))
    ));

    let mut count_precedes_bad_target = bytes.clone();
    set_u32(&mut count_precedes_bad_target, bind_range + 8, 1);
    set_u32(&mut count_precedes_bad_target, target_offset, different.raw);
    assert!(matches!(
        SemanticImageView::reopen(&count_precedes_bad_target),
        Err(FullSemanticImageError::Full(
            FullSemanticImageFault::SignatureCarrierBindingRange { .. }
        ))
    ));

    let mut wrong_union = bytes.clone();
    let role_offset = lane_payload_offset(&bytes, FullDirectoryKind::SignatureCarrierRoles)
        + usize::try_from(carrier.raw / 4).unwrap_or(0);
    let shift = (carrier.raw % 4) * 2;
    wrong_union[role_offset] &= !(0b11 << shift);
    assert!(matches!(
        SemanticImageView::reopen(&wrong_union),
        Err(FullSemanticImageError::Full(
            FullSemanticImageFault::SignatureCarrierBindingRoleUnion {
                entity,
                expected: 3,
                observed: 0,
            }
        )) if entity == carrier.raw
    ));

    Ok(())
}

#[test]
fn carrier_role_lane_is_identity_bearing_and_rejects_malformed_capture()
-> Result<(), crate::ir::BuildError> {
    let first_roles = [
        SignatureCarrierRole::NotCarrier,
        SignatureCarrierRole::Input,
        SignatureCarrierRole::Result,
    ];
    let second_roles = [
        SignatureCarrierRole::Input,
        SignatureCarrierRole::NotCarrier,
        SignatureCarrierRole::Result,
    ];
    let first = encoded(&carrier_image(&first_roles, false)?)?;
    let second = encoded(&carrier_image(&second_roles, false)?)?;
    assert_ne!(first, second);
    assert_ne!(
        crate::ir::SemanticImageIdentity::from_encoded_bytes(&first),
        crate::ir::SemanticImageIdentity::from_encoded_bytes(&second),
    );

    let mut bad_padding = first.clone();
    let role_directory =
        HEADER_BYTES + FullDirectoryKind::SignatureCarrierRoles.index() * DIRECTORY_BYTES;
    let role_offset = u32::from_le_bytes(
        bad_padding[role_directory + 4..role_directory + 8]
            .try_into()
            .expect("role lane offset"),
    ) as usize;
    bad_padding[role_offset] |= 0b0100_0000;
    assert!(matches!(
        SemanticImageView::reopen(&bad_padding),
        Err(super::FullSemanticImageError::Full(
            FullSemanticImageFault::SignatureCarrierRolePadding {
                observed: 0b0100_0000,
                ..
            }
        ))
    ));

    let mut wrong_count = first.clone();
    wrong_count[role_directory + 12..role_directory + 16].copy_from_slice(&2_u32.to_le_bytes());
    assert!(matches!(
        SemanticImageView::reopen(&wrong_count),
        Err(super::FullSemanticImageError::Full(
            FullSemanticImageFault::SignatureCarrierRoleCount {
                expected: 3,
                observed: 2,
            }
        ))
    ));

    let mut wrong_length = first.clone();
    wrong_length[role_directory + 8..role_directory + 12].copy_from_slice(&0_u32.to_le_bytes());
    let shortened = wrong_length.len().saturating_sub(1);
    wrong_length.truncate(shortened);
    wrong_length[8..12].copy_from_slice(
        &u32::try_from(shortened)
            .expect("shortened image fits wire length")
            .to_le_bytes(),
    );
    assert!(matches!(
        SemanticImageView::reopen(&wrong_length),
        Err(super::FullSemanticImageError::Full(
            FullSemanticImageFault::SignatureCarrierRoleLength {
                expected: 1,
                observed: 0,
            }
        ))
    ));

    let mut wrong_kind = first.clone();
    let entity_directory = HEADER_BYTES + FullDirectoryKind::Entities.index() * DIRECTORY_BYTES;
    let entity_offset = u32::from_le_bytes(
        wrong_kind[entity_directory + 4..entity_directory + 8]
            .try_into()
            .expect("entity lane offset"),
    ) as usize;
    let role_row = entity_offset + ENTITY_ROW_BYTES + 4;
    wrong_kind[role_row..role_row + 2]
        .copy_from_slice(&u16::from(crate::ir::ItemKind::Function).to_le_bytes());
    assert!(matches!(
        SemanticImageView::reopen(&wrong_kind),
        Err(super::FullSemanticImageError::Full(
            FullSemanticImageFault::SignatureCarrierRoleKind {
                row: 1,
                role: 1,
                ..
            }
        ))
    ));
    Ok(())
}

#[cfg(feature = "mmap")]
#[test]
fn mapped_semantic_image_reopens_the_canonical_reader_without_heap_copy() {
    use crate::ir::{GenerationId, SemanticImageIdentity, load_semantic_image_mmap};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FILE: AtomicU64 = AtomicU64::new(1);
    let bytes =
        encoded(&image(false).expect("canonical IR builds")).expect("canonical image encodes");
    let identity = SemanticImageIdentity::from_encoded_bytes(&bytes);
    let generation = GenerationId::from_canonical_bytes(&bytes);
    let path = std::env::temp_dir().join(format!(
        "backend-semantic-image-mmap-{}-{}.nxf",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::write(&path, &bytes).expect("write immutable image fixture");

    let mapped = load_semantic_image_mmap(&path, identity, generation, bytes.len())
        .expect("full image maps and validates");
    assert_eq!(mapped.identity(), identity);
    assert_eq!(mapped.generation(), generation);
    assert_eq!(mapped.view().canonical_entities().len(), 2);
    drop(mapped);
    fs::remove_file(path).expect("remove image fixture");
}

#[cfg(feature = "mmap")]
#[test]
fn mapped_image_range_loader_writes_exact_ranges_into_the_mapping() {
    use crate::ir::{
        GenerationId, SEMANTIC_IMAGE_MMAP_RANGE_BYTES, SemanticImageIdentity,
        load_semantic_image_mmap_from_ranges,
    };

    let bytes = large_encoded_image();
    assert!(bytes.len() > SEMANTIC_IMAGE_MMAP_RANGE_BYTES * 2);

    let identity = SemanticImageIdentity::from_encoded_bytes(&bytes);
    let generation = GenerationId::from_canonical_bytes(&bytes);
    let mut read_calls = 0_u64;
    let (mapped, metrics) = load_semantic_image_mmap_from_ranges(
        u64::try_from(bytes.len()).expect("fixture length fits"),
        identity,
        generation,
        bytes.len(),
        |offset, output| {
            read_calls += 1;
            let start = usize::try_from(offset).expect("range offset fits");
            let end = start.checked_add(output.len()).expect("range end fits");
            output.copy_from_slice(bytes.get(start..end).expect("exact fixture range"));
            Ok::<usize, ()>(output.len())
        },
        || false,
    )
    .expect("exact source ranges fill and fully verify the mapped image");

    let expected_ranges = bytes.len().div_ceil(SEMANTIC_IMAGE_MMAP_RANGE_BYTES);
    assert_eq!(
        metrics.bytes_read,
        u64::try_from(bytes.len()).expect("length fits")
    );
    assert_eq!(
        metrics.identity_hash_bytes,
        u64::try_from(bytes.len()).expect("length fits"),
        "both selected identities are accumulated during the source reads",
    );
    assert_eq!(
        usize::try_from(metrics.range_reads).expect("range count fits"),
        expected_ranges
    );
    assert_eq!(read_calls, metrics.range_reads);
    assert_eq!(mapped.identity(), identity);
    assert_eq!(mapped.generation(), generation);
    assert_eq!(mapped.view().canonical_entities().len(), 1);
}

#[cfg(feature = "mmap")]
#[test]
fn mapped_image_range_loader_rejects_short_and_truncated_sources() {
    use crate::ir::{
        GenerationId, MappedSemanticImageRangeError, SEMANTIC_IMAGE_MMAP_RANGE_BYTES,
        SemanticImageIdentity, load_semantic_image_mmap_from_ranges,
    };

    let bytes = large_encoded_image();
    let identity = SemanticImageIdentity::from_encoded_bytes(&bytes);
    let generation = GenerationId::from_canonical_bytes(&bytes);
    let total = u64::try_from(bytes.len()).expect("fixture length fits");
    let short = load_semantic_image_mmap_from_ranges(
        total,
        identity,
        generation,
        bytes.len(),
        |_offset, output| Ok::<usize, ()>(output.len().saturating_sub(1)),
        || false,
    );
    assert!(matches!(
        short,
        Err(MappedSemanticImageRangeError::ShortRead {
            offset: 0,
            expected,
            observed,
        }) if expected == bytes.len().min(SEMANTIC_IMAGE_MMAP_RANGE_BYTES)
            && observed + 1 == expected
    ));

    let source_length = bytes.len().saturating_sub(1);
    let final_offset =
        (source_length / SEMANTIC_IMAGE_MMAP_RANGE_BYTES) * SEMANTIC_IMAGE_MMAP_RANGE_BYTES;
    let truncated = load_semantic_image_mmap_from_ranges(
        total,
        identity,
        generation,
        bytes.len(),
        |offset, output| {
            let start = usize::try_from(offset).expect("range offset fits");
            let available = source_length.saturating_sub(start).min(output.len());
            output[..available].copy_from_slice(&bytes[start..start + available]);
            Ok::<usize, ()>(available)
        },
        || false,
    );
    assert!(matches!(
        truncated,
        Err(MappedSemanticImageRangeError::ShortRead {
            offset,
            expected,
            observed,
        }) if offset == u64::try_from(final_offset).expect("range offset fits")
            && expected == bytes.len() - final_offset
            && observed + 1 == expected
    ));
}

#[cfg(feature = "mmap")]
#[test]
fn mapped_image_range_loader_rejects_corruption_and_stops_when_cancelled() {
    use crate::ir::{
        GenerationId, MappedSemanticImageError, MappedSemanticImageRangeError,
        SEMANTIC_IMAGE_MMAP_RANGE_BYTES, SemanticImageIdentity,
        load_semantic_image_mmap_from_ranges,
    };

    let bytes = large_encoded_image();
    let identity = SemanticImageIdentity::from_encoded_bytes(&bytes);
    let generation = GenerationId::from_canonical_bytes(&bytes);
    let total = u64::try_from(bytes.len()).expect("fixture length fits");
    let mut corrupt = bytes.clone();
    corrupt[0] ^= 1;
    let rejected = load_semantic_image_mmap_from_ranges(
        total,
        identity,
        generation,
        bytes.len(),
        |offset, output| {
            let start = usize::try_from(offset).expect("range offset fits");
            let end = start.checked_add(output.len()).expect("range end fits");
            output.copy_from_slice(corrupt.get(start..end).expect("exact corrupt range"));
            Ok::<usize, ()>(output.len())
        },
        || false,
    );
    assert!(matches!(
        rejected,
        Err(MappedSemanticImageRangeError::Mapping(
            MappedSemanticImageError::Identity { .. }
        ))
    ));

    let mut read_calls = 0_u64;
    let mut cancel_checks = 0_u64;
    let cancelled = load_semantic_image_mmap_from_ranges(
        total,
        identity,
        generation,
        bytes.len(),
        |offset, output| {
            read_calls += 1;
            let start = usize::try_from(offset).expect("range offset fits");
            let end = start.checked_add(output.len()).expect("range end fits");
            output.copy_from_slice(bytes.get(start..end).expect("exact fixture range"));
            Ok::<usize, ()>(output.len())
        },
        || {
            cancel_checks += 1;
            cancel_checks >= 3
        },
    );
    assert!(matches!(
        cancelled,
        Err(MappedSemanticImageRangeError::Cancelled)
    ));
    assert_eq!(
        read_calls, 1,
        "cancellation is checked between direct mapped reads"
    );
    assert!(bytes.len() > SEMANTIC_IMAGE_MMAP_RANGE_BYTES);
}

fn csharp_image() -> Result<Ir, crate::ir::BuildError> {
    let mut builder = IrBuilder::new();
    builder.set_language_profile(LanguageProfile::CSharp(CSharpVersion::CSharp14))?;
    let constraints = builder.intern_type_parameters(&[])?;
    let attributes = builder.intern_attributes(&[])?;
    let base = CSharpFacts {
        nullability: CSharpNullability::Oblivious,
        reference_kind: CSharpReferenceKind::Value,
        constraints,
        effects: CSharpMemberEffects {
            is_async: false,
            is_iterator: false,
            is_extension: false,
        },
        attributes,
        partial: CSharpPartialRole::None,
        xml_provenance: None,
    };
    let alternate = CSharpFacts {
        nullability: CSharpNullability::Nullable,
        ..base
    };
    let authority = EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        visibility: FactAvailability::Captured,
        language_extension: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    };
    let first = TreeItemInput {
        name: b"first",
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        authority,
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: Some(LanguageExtensionInput::CSharp(&base)),
    };
    let second = TreeItemInput {
        name: b"second",
        extension: Some(LanguageExtensionInput::CSharp(&alternate)),
        ..first
    };
    let versions = [version(1), version(2)];
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &[first, second],
        links: &[],
    })?;
    builder.finish()
}

fn directory(bytes: &[u8], kind: FullDirectoryKind) -> (usize, u32) {
    let offset = HEADER_BYTES + kind.index() * DIRECTORY_BYTES;
    let payload = u32::from_le_bytes(
        bytes[offset + 4..offset + 8]
            .try_into()
            .expect("directory offset"),
    );
    let count = u32::from_le_bytes(
        bytes[offset + 12..offset + 16]
            .try_into()
            .expect("directory count"),
    );
    (usize::try_from(payload).expect("test address space"), count)
}

fn binding_offset(bytes: &[u8], kind: FullDirectoryKind, row: u32) -> usize {
    let (offset, count) = directory(bytes, kind);
    assert!(row < count, "binding row is present");
    offset + usize::try_from(row).expect("test row") * SPARSE_BINDING_ROW_BYTES
}

fn binding_fact(bytes: &[u8], kind: FullDirectoryKind, row: u32) -> u32 {
    let offset = binding_offset(bytes, kind, row);
    u32::from_le_bytes(
        bytes[offset + 4..offset + 8]
            .try_into()
            .expect("binding fact"),
    )
}

fn set_binding_fact(bytes: &mut [u8], kind: FullDirectoryKind, row: u32, fact: u32) {
    let offset = binding_offset(bytes, kind, row);
    bytes[offset + 4..offset + 8].copy_from_slice(&fact.to_le_bytes());
}

fn fact_value_range(bytes: &[u8], kind: FullDirectoryKind, row: u32) -> core::ops::Range<usize> {
    let (offset, count) = directory(bytes, kind);
    assert!(row < count, "fact row is present");
    let range = offset + usize::try_from(row).expect("test row") * RANGE_ROW_BYTES;
    let start = u32::from_le_bytes(bytes[range..range + 4].try_into().expect("fact start"));
    let len = u32::from_le_bytes(bytes[range + 4..range + 8].try_into().expect("fact length"));
    let values = offset + usize::try_from(count).expect("test count") * RANGE_ROW_BYTES;
    let start = usize::try_from(start).expect("test start");
    let len = usize::try_from(len).expect("test length");
    values + start..values + start + len
}

#[test]
fn full_image_is_canonical_reopens_complete_reader_and_keeps_borrowed_atoms()
-> Result<(), crate::ir::BuildError> {
    let first = image(false)?;
    let reversed = image(true)?;
    let bytes = encoded(&first)?;
    assert_eq!(bytes, encoded(&reversed)?);

    let view = SemanticImageView::reopen(&bytes).expect("full image reopens");
    let owned = first
        .canonical_entities()
        .map(|entity| {
            (
                entity.version.identity(),
                first.atom(entity.name).map(<[u8]>::to_vec),
            )
        })
        .collect::<alloc::vec::Vec<_>>();
    let reopened = view
        .canonical_entities()
        .map(|entity| {
            (
                entity.version.identity(),
                view.atom(entity.name).map(<[u8]>::to_vec),
            )
        })
        .collect::<alloc::vec::Vec<_>>();
    assert_eq!(owned, reopened);
    assert_eq!(
        view.canonical_types().count(),
        first.canonical_types().count()
    );
    assert_eq!(
        view.canonical_externals().count(),
        first.canonical_externals().count()
    );
    let entity = view.canonical_entities().next().expect("first entity");
    assert_eq!(entity.authority.members, FactAvailability::Captured);
    assert_eq!(entity.authority.documentation, FactAvailability::Captured);
    let atom = view.atom(entity.name).expect("reopened atom");
    let repeated = view.atom(entity.name).expect("repeated atom");
    assert!(core::ptr::eq(atom.as_ptr(), repeated.as_ptr()));
    let start = bytes.as_ptr().addr();
    assert!(
        atom.as_ptr().addr() >= start && atom.as_ptr().addr() + atom.len() <= start + bytes.len()
    );
    Ok(())
}

#[test]
fn full_image_short_output_and_directory_substitution_are_exact_and_non_mutating()
-> Result<(), crate::ir::BuildError> {
    let ir = image(false)?;
    let length = full_semantic_image_len(&ir).expect("full plan");
    let mut short = vec![0xa5; length.saturating_sub(1)];
    let before = short.clone();
    assert!(matches!(
        encode_full_semantic_image(&ir, &mut short),
        Err(SemanticImageEncodeError::Wire(FullSemanticImageFault::OutputTooShort { required, actual }))
            if required == length && actual == before.len()
    ));
    assert_eq!(short, before);
    let mut bytes = encoded(&ir)?;
    bytes[HEADER_BYTES] = 0xff;
    assert!(matches!(
        SemanticImageView::reopen(&bytes),
        Err(super::FullSemanticImageError::Full(
            FullSemanticImageFault::DirectoryKind {
                expected: FullDirectoryKind::Atoms,
                observed: 0xff,
                ..
            }
        ))
    ));
    Ok(())
}

#[test]
fn full_image_rejects_noncanonical_extension_bindings_and_fact_pools()
-> Result<(), crate::ir::BuildError> {
    let bytes = encoded(&csharp_image()?)?;
    let facts = FullDirectoryKind::CSharpFacts;
    let bindings = FullDirectoryKind::CSharpBindings;
    assert_eq!(directory(&bytes, facts).1, 2);
    assert_eq!(directory(&bytes, bindings).1, 2);

    let mut duplicate_entity = bytes.clone();
    let first_binding = binding_offset(&duplicate_entity, bindings, 0);
    let second_binding = binding_offset(&duplicate_entity, bindings, 1);
    let entity: [u8; 4] = duplicate_entity[first_binding..first_binding + 4]
        .try_into()
        .expect("first entity");
    duplicate_entity[second_binding..second_binding + 4].copy_from_slice(&entity);
    assert!(matches!(
        SemanticImageView::reopen(&duplicate_entity),
        Err(super::FullSemanticImageError::Full(
            FullSemanticImageFault::DuplicateExtensionBinding {
                plane: FullDirectoryKind::CSharpFacts,
                entity: 0,
                existing: 0,
                row: 1,
            }
        ))
    ));

    let mut orphan = bytes.clone();
    let first_fact = binding_fact(&orphan, bindings, 0);
    let second_fact = binding_fact(&orphan, bindings, 1);
    assert_ne!(first_fact, second_fact);
    set_binding_fact(&mut orphan, bindings, 1, first_fact);
    assert!(matches!(
        SemanticImageView::reopen(&orphan),
        Err(super::FullSemanticImageError::Full(
            FullSemanticImageFault::UnboundExtensionFact {
                plane: FullDirectoryKind::CSharpFacts,
                fact,
            }
        )) if fact == second_fact
    ));

    let mut duplicate_fact = bytes.clone();
    let first = fact_value_range(&duplicate_fact, facts, 0);
    let second = fact_value_range(&duplicate_fact, facts, 1);
    assert_eq!(first.len(), second.len());
    let first_value = duplicate_fact[first].to_vec();
    duplicate_fact[second].copy_from_slice(&first_value);
    assert!(matches!(
        SemanticImageView::reopen(&duplicate_fact),
        Err(super::FullSemanticImageError::Full(
            FullSemanticImageFault::DuplicateCanonicalRow {
                field: super::FullSemanticImageField::ExtensionFacts,
                existing: 0,
                row: 1,
            }
        ))
    ));

    let mut non_selected_plane = bytes.clone();
    let profile = <[u8; 2]>::from(LanguageProfile::TypeScript(TypeScriptSource::TypeScript));
    non_selected_plane[17..19].copy_from_slice(&profile);
    assert!(matches!(
        SemanticImageView::reopen(&non_selected_plane),
        Err(super::FullSemanticImageError::Full(
            FullSemanticImageFault::ExtensionPlaneAuthority {
                plane: FullDirectoryKind::CSharpFacts,
                authority: SemanticImageAuthority::Language(LanguageProfile::TypeScript(
                    TypeScriptSource::TypeScript
                )),
                facts: 2,
                bindings: 2,
                ..
            }
        ))
    ));

    let mut permuted_facts = bytes;
    let first = fact_value_range(&permuted_facts, facts, 0);
    let second = fact_value_range(&permuted_facts, facts, 1);
    assert_eq!(first.len(), second.len());
    let first_value = permuted_facts[first.clone()].to_vec();
    let second_value = permuted_facts[second.clone()].to_vec();
    permuted_facts[first].copy_from_slice(&second_value);
    permuted_facts[second].copy_from_slice(&first_value);
    for row in 0..directory(&permuted_facts, bindings).1 {
        let fact = binding_fact(&permuted_facts, bindings, row);
        let remapped = match fact {
            0 => 1,
            1 => 0,
            _ => unreachable!("two-fact fixture"),
        };
        set_binding_fact(&mut permuted_facts, bindings, row, remapped);
    }
    assert!(matches!(
        SemanticImageView::reopen(&permuted_facts),
        Err(super::FullSemanticImageError::Full(
            FullSemanticImageFault::CanonicalOrder {
                field: super::FullSemanticImageField::ExtensionFacts,
                previous: 0,
                row: 1,
            }
        ))
    ));
    Ok(())
}

fn rust_free_predicate_image() -> Result<Ir, crate::ir::BuildError> {
    let mut builder = IrBuilder::new();
    builder.set_language_profile(LanguageProfile::Rust(RustEdition::Rust2024))?;
    let subject =
        builder.intern_type(TypeExpr::Concrete(ConcreteType::Builtin(BuiltinType::I32)))?;
    let bound_type =
        builder.intern_type(TypeExpr::Concrete(ConcreteType::Builtin(BuiltinType::Bool)))?;
    let bound_lifetime = builder.intern_atom(b"'scope")?;
    let bounds = builder.intern_type_parameter_bounds(&[
        TypeParameterBound::Type(bound_type),
        TypeParameterBound::Lifetime(bound_lifetime),
    ])?;
    let free_predicates = builder.intern_free_predicates(&[FreePredicate { subject, bounds }])?;
    let facts = RustFacts {
        ownership: RustOwnership::SharedBorrow,
        lifetimes: builder.intern_attributes(&[])?,
        where_clauses: builder.intern_type_parameters(&[])?,
        macros: builder.intern_attributes(&[])?,
        const_defaults: builder.intern_attributes(&[])?,
        free_predicates,
    };
    let item = TreeItemInput {
        name: b"owner",
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        authority: EntityAuthorityFacts {
            language_extension: FactAvailability::Captured,
            ..authority()
        },
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: Some(LanguageExtensionInput::Rust(&facts)),
    };
    let versions = [version(1)];
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &[item],
        links: &[],
    })?;
    builder.finish()
}

#[test]
fn full_image_round_trips_a_rust_free_predicate() -> Result<(), crate::ir::BuildError> {
    let ir = rust_free_predicate_image()?;
    let bytes = encoded(&ir)?;
    let view = SemanticImageView::reopen(&bytes).expect("full image reopens");
    let facts = view
        .rust_extension(EntityId::new(0))
        .expect("reopened rust facts");
    assert_eq!(facts.ownership, RustOwnership::SharedBorrow);
    let predicates = view
        .free_predicates(facts.free_predicates)
        .expect("reopened free predicates");
    let rows = predicates.collect::<alloc::vec::Vec<_>>();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        view.ty(rows[0].subject),
        Some(TypeExpr::Concrete(ConcreteType::Builtin(BuiltinType::I32)))
    );
    let bounds = view
        .type_parameter_bounds(rows[0].bounds)
        .expect("reopened predicate bounds")
        .collect::<alloc::vec::Vec<_>>();
    assert_eq!(bounds.len(), 2);
    let TypeParameterBound::Type(first_bound) = bounds[0] else {
        panic!("first predicate bound is not a type");
    };
    assert_eq!(
        view.ty(first_bound),
        Some(TypeExpr::Concrete(ConcreteType::Builtin(BuiltinType::Bool)))
    );
    assert!(matches!(
        bounds[1],
        TypeParameterBound::Lifetime(atom) if view.atom(atom) == Some(&b"'scope"[..])
    ));
    Ok(())
}

/// Focused decode benchmark over a synthetic image of roughly ten thousand
/// typed rows: `ENTITIES` distinct `Parameter`/`CPointer` type pairs plus one
/// shared 512-row type-parameter list that every extension facts row points
/// at (exactly the shape the audit's canonical extension digests sweep).  The
/// grouped decode must complete the whole reopen + types + parameter sweep in
/// bounded time; the former per-row linear edge scans made the same sweep
/// quadratic in the list length.
#[test]
fn grouped_decode_sweeps_ten_thousand_typed_rows_in_bounded_time()
-> Result<(), crate::ir::BuildError> {
    use std::time::{Duration, Instant};

    use crate::ir::{
        CSharpFacts, CSharpMemberEffects, CSharpNullability, CSharpPartialRole,
        CSharpReferenceKind, CSharpVersion, LanguageExtensionInput, Mutability,
        TypeParameterInference, TypeParameterKind,
    };

    const ENTITIES: usize = 4600;
    const SHARED_PARAMETERS: usize = 512;

    fn distinct_version(index: usize) -> crate::ir::EntityVersion {
        let low = u8::try_from(index & 0xff).unwrap_or(1);
        let high = u8::try_from((index >> 8) & 0xff).unwrap_or(1);
        let mut family = [7_u8; 16];
        family[0] = low;
        family[1] = high;
        let mut variant = [9_u8; 16];
        variant[0] = low.wrapping_add(1);
        variant[1] = high;
        crate::ir::EntityVersion {
            family: crate::ir::DeclarationFamilyId::from_raw(family),
            variant: crate::ir::VariantFingerprint::from_raw(variant),
            core_payload: crate::ir::CorePayloadHash::from_raw([11; 16]),
        }
    }

    let mut builder = IrBuilder::new();
    builder.set_language_profile(LanguageProfile::CSharp(CSharpVersion::CSharp14))?;
    let lifetime = builder.intern_atom(b"'a")?;
    let empty_bounds = builder.intern_type_parameter_bounds(&[])?;
    let mixed_bounds =
        builder.intern_type_parameter_bounds(&[TypeParameterBound::Lifetime(lifetime)])?;
    let mut shared = alloc::vec::Vec::with_capacity(SHARED_PARAMETERS);
    for index in 0..SHARED_PARAMETERS {
        let spelling = alloc::format!("T{index}");
        let name = builder.intern_atom(spelling.as_bytes())?;
        shared.push(crate::ir::TypeParameter {
            name,
            bounds: if index % 2 == 0 {
                mixed_bounds
            } else {
                empty_bounds
            },
            default: None,
            variance: crate::ir::Variance::Covariant,
            kind: TypeParameterKind::Type {
                inference: TypeParameterInference::Ordinary,
            },
            requirements: crate::ir::TypeParameterRequirements::none(),
        });
    }
    let constraints = builder.intern_type_parameters(&shared)?;
    let facts = CSharpFacts {
        nullability: CSharpNullability::Oblivious,
        reference_kind: CSharpReferenceKind::Value,
        constraints,
        effects: CSharpMemberEffects {
            is_async: false,
            is_iterator: false,
            is_extension: false,
        },
        attributes: builder.intern_attributes(&[])?,
        partial: CSharpPartialRole::None,
        xml_provenance: None,
    };
    let mut versions = alloc::vec::Vec::with_capacity(ENTITIES);
    let mut items = alloc::vec::Vec::with_capacity(ENTITIES);
    let mut spellings = alloc::vec::Vec::with_capacity(ENTITIES);
    for index in 0..ENTITIES {
        spellings.push(alloc::format!("synthetic-{index}"));
    }
    for index in 0..ENTITIES {
        let spelling = spellings[index].as_bytes();
        let parameter_spelling = alloc::format!("P{index}");
        let parameter = builder.intern_atom(parameter_spelling.as_bytes())?;
        let parameter_type =
            builder.intern_type(TypeExpr::Concrete(ConcreteType::Parameter(parameter)))?;
        let semantic_type = builder.intern_type(TypeExpr::Concrete(ConcreteType::CPointer {
            target: parameter_type,
        }))?;
        versions.push(distinct_version(index));
        items.push(TreeItemInput {
            name: spelling,
            kind: ItemKind::Function,
            visibility: Visibility::Private,
            authority: crate::ir::EntityAuthorityFacts {
                semantic_type: FactAvailability::Captured,
                language_extension: FactAvailability::Captured,
                ..authority()
            },
            parent: None,
            semantic_type: Some(semantic_type),
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: Some(LanguageExtensionInput::CSharp(&facts)),
        });
    }
    let borrowed = crate::ir::BorrowedTree {
        versions: &versions,
        items: &items,
        links: &[],
    };
    builder.add_borrowed_tree(borrowed)?;
    let ir = builder.finish()?;

    let started = Instant::now();
    let bytes = encoded(&ir)?;
    let view = SemanticImageView::reopen(&bytes).expect("synthetic full image reopens");
    let type_rows = view.canonical_types().count();
    let mut facts_rows = 0_usize;
    let mut parameter_rows = 0_usize;
    for (_, row_facts) in view.csharp_extensions() {
        facts_rows += 1;
        for row in view
            .type_parameters(row_facts.constraints)
            .expect("shared parameter list decodes")
        {
            std::hint::black_box(&row);
            parameter_rows += 1;
        }
    }
    let elapsed = started.elapsed();
    eprintln!(
        "grouped-decode benchmark: type_rows={type_rows} facts_rows={facts_rows} parameter_rows={parameter_rows} encoded_bytes={} elapsed_ms={}",
        bytes.len(),
        elapsed.as_millis(),
    );
    assert!(
        type_rows >= 9_000,
        "expected ~10k type rows, saw {type_rows}"
    );
    assert_eq!(facts_rows, ENTITIES);
    assert_eq!(parameter_rows, ENTITIES * SHARED_PARAMETERS);
    // Debug-profile CI headroom; the decode itself is single-pass and the
    // former linear-scan decoder could not finish this sweep at all.
    assert!(
        elapsed < Duration::from_secs(60),
        "grouped decode exceeded the bound: {elapsed:?}"
    );
    Ok(())
}
