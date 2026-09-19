//! Exercises the semantic-data section grammar through byte-exact mutations.
//! Every structural class of the embedded canonical graph rejects with its
//! exact typed fault and operands before a borrowed view exists.
use backend_semantic::ir::{
    AtomId, ListSpan, ProductChildRole, ProductChildren, ProductConstructorFault,
    ProductConstructorTag, ProductId, ProductListId, ProductRef, SemanticAtom, SemanticProduct,
    SemanticProductChild, SemanticProductConstructor,
};
use backend_semantic::ir::{
    AtomInput, CanonicalDataError, DataFacts, DataOutput, DataResourceBudget, DataScratch,
    EntityKind, EntityRecord, ExtensionFreePredicate, ExtensionPoolsLane, ExtensionTypeParameter,
    ExtensionTypeParameterBound, ExtensionTypeParameterBoundRange, ExtensionTypeParameterRange,
    FragmentError, FragmentView, FreePredicateListId,
    PrepareError, PreparedFragment, PrimitiveType, ReopenedTypeParameterList, SemanticDataFault,
    SourceIdentity, TypeNode, TypeParameterListId, WriteError, canonicalize_data_with_budget,
};
use backend_semantic::vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage};
use core::mem::size_of;
use backend_version::{ContentId, Domain, IrFragmentDomain, SourceFactDomain, ToolchainDomain};
use thiserror::Error;

const HEADER_BYTES: usize = 12;
const DIRECTORY_BYTES: usize = 16;
const SEMANTIC_DIRECTORY: usize = HEADER_BYTES + DIRECTORY_BYTES * 6;
const SEMANTIC_COUNT: usize = SEMANTIC_DIRECTORY + 4;
const SEMANTIC_BYTE_LENGTH: usize = SEMANTIC_DIRECTORY + 12;
// The semantic lane is written last: after the entity, type-node, atom,
// source, and recipe lanes of a seven-section fragment.
const SEMANTIC_RECORD: usize = HEADER_BYTES
    + DIRECTORY_BYTES * 7
    + size_of::<u32>() * 2
    + size_of::<u16>() * 2
    + 8
    + 8
    + 4
    + (size_of::<u32>() + backend_version::HASH_BYTES)
    + (size_of::<u8>() * 4 + backend_version::HASH_BYTES * 2);

/// Payload-relative lane offsets: header(24) + atom stream(4+7).
const ATOM_LENGTH: usize = 24;
const PRODUCT_HEAD: usize = 35;
const CONSTRUCTOR_COUNT: usize = 8;
const CONSTRUCTOR_P1: usize = 51;
const LIST_START: usize = 55;
const LIST_LENGTH: usize = 59;
const CHILD_ROLE: usize = 63;
const CHILD_TAG: usize = 64;
const CHILD_TARGET: usize = 65;
const CHILD_RESERVED: usize = 69;
const CHILD_COUNT: usize = 16;
const SEMANTIC_LENGTH: usize = 24 + (4 + 7) + 8 + 12 + 8 + 38 + 4;
const SEMANTIC_END: usize = SEMANTIC_RECORD + SEMANTIC_LENGTH;

#[derive(Debug, Error)]
enum TestFailure {
    #[error(transparent)]
    Canonical(#[from] CanonicalDataError),
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Write(#[from] WriteError),
    #[error(transparent)]
    Validate(#[from] FragmentError),
}

fn source_identity() -> SourceIdentity {
    SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"semantic-source"),
        byte_len: 15,
    }
}

fn recipe_fact() -> CompileRecipeFact {
    CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        source_identity().identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"semantic-toolchain"),
    )
}

/// One canonical product graph borrowed from caller output and embedded in a
/// complete fragment envelope.
struct DataFragment {
    bytes: Vec<u8>,
    /// Retains the graph borrows that the embedded section commits.
    #[allow(dead_code, reason = "the committed section borrows these lanes")]
    lanes: DataLanes,
}

/// Caller-owned source facts, output lanes, and scratch, mirroring the
/// canonicalizer's disjoint input/output ownership.
struct DataLanes {
    source: SourceLanes,
    out: OutLanes,
    scratch: ScratchLanes,
}

struct SourceLanes {
    atoms: [SemanticAtom<'static>; 1],
    products: [SemanticProduct; 1],
    constructors: [SemanticProductConstructor; 1],
    lists: [ListSpan<ProductChildren>; 1],
    children: [SemanticProductChild; 1],
}

#[allow(
    dead_code,
    reason = "the lanes retain the graph borrows that the embedded section commits"
)]
struct OutLanes {
    atoms: [SemanticAtom<'static>; 1],
    products: [SemanticProduct; 1],
    constructors: [SemanticProductConstructor; 1],
    lists: [ListSpan<ProductChildren>; 1],
    children: [SemanticProductChild; 1],
}

struct ScratchLanes {
    atom_order: [AtomId; 1],
    atom_map: [u32; 1],
    product_order: [ProductId; 1],
    product_map: [u32; 1],
    colors: [u32; 1],
    next_colors: [u32; 1],
    hashes: [u64; 1],
    next_hashes: [u64; 1],
    representatives: [ProductId; 1],
    intern_slots: [u64; 1],
}

fn data_fragment() -> Result<DataFragment, TestFailure> {
    let entities = [EntityRecord {
        semantic_type: backend_semantic::ir::TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Function,
    }];
    let nodes = [TypeNode::Primitive(PrimitiveType::I32)];
    let atoms = [AtomInput { bytes: b"decl" }];
    let mut lanes = DataLanes {
        source: SourceLanes {
            atoms: [SemanticAtom { bytes: b"nominal" }],
            products: [SemanticProduct {
                head: AtomId::new(0),
                children: ProductListId::new(0),
            }],
            constructors: [SemanticProductConstructor::PRODUCT],
            lists: [ListSpan::<ProductChildren>::new(0, 1)],
            children: [SemanticProductChild {
                target: ProductRef::Local(ProductId::new(0)),
                role: ProductChildRole::ProductMember,
            }],
        },
        out: OutLanes {
            atoms: [SemanticAtom { bytes: b"" }],
            products: [SemanticProduct {
                head: AtomId::new(0),
                children: ProductListId::new(0),
            }],
            constructors: [SemanticProductConstructor::PRODUCT],
            lists: [ListSpan::<ProductChildren>::new(0, 0)],
            children: [SemanticProductChild {
                target: ProductRef::Local(ProductId::new(0)),
                role: ProductChildRole::ProductMember,
            }],
        },
        scratch: ScratchLanes {
            atom_order: [AtomId::new(0)],
            atom_map: [0],
            product_order: [ProductId::new(0)],
            product_map: [0],
            colors: [0],
            next_colors: [0],
            hashes: [0],
            next_hashes: [0],
            representatives: [ProductId::new(0)],
            intern_slots: [0],
        },
    };
    let mut scratch = DataScratch {
        atom_order: &mut lanes.scratch.atom_order,
        atom_to_canonical: &mut lanes.scratch.atom_map,
        product_order: &mut lanes.scratch.product_order,
        product_to_canonical: &mut lanes.scratch.product_map,
        colors: &mut lanes.scratch.colors,
        next_colors: &mut lanes.scratch.next_colors,
        hashes: &mut lanes.scratch.hashes,
        next_hashes: &mut lanes.scratch.next_hashes,
        product_representatives: &mut lanes.scratch.representatives,
        intern_slots: &mut lanes.scratch.intern_slots,
    };
    let mut data_output = DataOutput {
        atoms: &mut lanes.out.atoms,
        products: &mut lanes.out.products,
        constructors: &mut lanes.out.constructors,
        lists: &mut lanes.out.lists,
        children: &mut lanes.out.children,
    };
    let graph = canonicalize_data_with_budget(
        DataFacts {
            atoms: &lanes.source.atoms,
            products: &lanes.source.products,
            constructors: &lanes.source.constructors,
            lists: &lanes.source.lists,
            children: &lanes.source.children,
        },
        &mut scratch,
        &mut data_output,
        DataResourceBudget {
            max_refinement_rounds: 4,
            max_sort_comparisons: 128,
            max_hash_evaluations: 32,
            max_intern_probes: 32,
            max_work: 512,
        },
    )?;
    let prepared = PreparedFragment::prepare_with_data(
        source_identity(),
        recipe_fact(),
        &entities,
        &nodes,
        &atoms,
        &graph,
    )?;
    let mut bytes = vec![0; prepared.required_capacity()];
    prepared.write_into(&mut bytes)?;
    Ok(DataFragment { bytes, lanes })
}

#[test]
fn semantic_section_has_exact_payload_layout_and_lends_the_envelope() -> Result<(), TestFailure> {
    let fragment = data_fragment()?;
    assert_eq!(fragment.bytes.len(), SEMANTIC_END);
    assert_eq!(
        &fragment.bytes[SEMANTIC_DIRECTORY..SEMANTIC_DIRECTORY + 8],
        &[7, 0, 1, 0, 105, 0, 0, 0]
    );
    // Header: one atom, product, constructor, list, child, and entity root.
    assert_eq!(
        &fragment.bytes[SEMANTIC_RECORD..SEMANTIC_RECORD + 24],
        &[
            1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0
        ]
    );
    // Atom stream: length 7 followed by "nominal".
    assert_eq!(
        &fragment.bytes[SEMANTIC_RECORD + ATOM_LENGTH..SEMANTIC_RECORD + PRODUCT_HEAD],
        &[7, 0, 0, 0, b'n', b'o', b'm', b'i', b'n', b'a', b'l']
    );
    // Product: head 0, list 0. Constructor: tag Product, reserved cells zero.
    assert_eq!(
        &fragment.bytes[SEMANTIC_RECORD + PRODUCT_HEAD..SEMANTIC_RECORD + PRODUCT_HEAD + 20],
        &[0, 0, 0, 0, 0, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    );
    // List span (0, 1) and the child record: role 7, local tag, target 0.
    assert_eq!(
        &fragment.bytes[SEMANTIC_RECORD + LIST_START..SEMANTIC_RECORD + CHILD_ROLE],
        &[0, 0, 0, 0, 1, 0, 0, 0]
    );
    assert_eq!(
        &fragment.bytes[SEMANTIC_RECORD + CHILD_ROLE..SEMANTIC_RECORD + CHILD_RESERVED],
        &[7, 0, 0, 0, 0, 0]
    );
    assert_eq!(
        &fragment.bytes[SEMANTIC_RECORD + CHILD_RESERVED..SEMANTIC_RECORD + CHILD_RESERVED + 32],
        &[0; 32]
    );
    assert_eq!(&fragment.bytes[SEMANTIC_END - 4..SEMANTIC_END], &[0; 4]);
    let view = FragmentView::validate(&fragment.bytes)?;
    assert_eq!(view.as_ref().as_ptr(), fragment.bytes.as_ptr());
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum Mutation {
    HeaderTruncated,
    AtomLengthOverflow,
    ProductHeadOutside,
    ProductListOutside,
    ConstructorCountMismatch,
    ConstructorReservedPayload,
    ListExtentOutside,
    ChildRoleCodeUnknown,
    ChildRoleMismatch,
    ChildTagUnknown,
    LocalChildTargetOutside,
    LocalReservedNonzero,
    ExternalAuthorityWrongDomain,
    ChildCountTrailing,
}

/// Applies one exact mutation to the committed envelope.
fn mutate(mutation: Mutation, bytes: &mut [u8]) {
    let payload = &mut bytes[SEMANTIC_RECORD..SEMANTIC_END];
    match mutation {
        Mutation::HeaderTruncated => {
            bytes[SEMANTIC_COUNT] = 23;
            bytes[SEMANTIC_BYTE_LENGTH] = 23;
        }
        Mutation::AtomLengthOverflow => payload[ATOM_LENGTH] = u8::MAX,
        Mutation::ProductHeadOutside => payload[PRODUCT_HEAD] = 5,
        Mutation::ProductListOutside => payload[PRODUCT_HEAD + 4] = 5,
        Mutation::ConstructorCountMismatch => payload[CONSTRUCTOR_COUNT] = 2,
        Mutation::ConstructorReservedPayload => payload[CONSTRUCTOR_P1] = 7,
        Mutation::ListExtentOutside => payload[LIST_LENGTH] = 2,
        Mutation::ChildRoleCodeUnknown => payload[CHILD_ROLE] = 9,
        Mutation::ChildRoleMismatch => {
            payload[CHILD_ROLE] = u8::from(ProductChildRole::TupleElement)
        }
        Mutation::ChildTagUnknown => payload[CHILD_TAG] = 7,
        Mutation::LocalChildTargetOutside => payload[CHILD_TARGET] = 5,
        Mutation::LocalReservedNonzero => payload[CHILD_RESERVED] = 1,
        Mutation::ExternalAuthorityWrongDomain => payload[CHILD_TAG] = 1,
        Mutation::ChildCountTrailing => payload[CHILD_COUNT] = 0,
    }
}

fn expected_fault(mutation: Mutation, bytes: &[u8]) -> SemanticDataFault {
    match mutation {
        Mutation::HeaderTruncated => SemanticDataFault::Header {
            required: 24,
            actual: 23,
        },
        Mutation::AtomLengthOverflow => SemanticDataFault::AtomLength {
            ordinal: 0,
            length: u32::from(u8::MAX),
            available: SEMANTIC_END - SEMANTIC_RECORD - ATOM_LENGTH - 4,
        },
        Mutation::ProductHeadOutside => SemanticDataFault::ProductHead {
            product: 0,
            target: 5,
            atom_count: 1,
        },
        Mutation::ProductListOutside => SemanticDataFault::ProductList {
            product: 0,
            target: 5,
            list_count: 1,
        },
        Mutation::ConstructorCountMismatch => SemanticDataFault::ConstructorCount {
            product_count: 1,
            constructor_count: 2,
        },
        Mutation::ConstructorReservedPayload => SemanticDataFault::Constructor {
            product: 0,
            fault: ProductConstructorFault::ReservedPayload {
                tag: ProductConstructorTag::Product,
                payload0: 0,
                payload1: 7,
            },
        },
        Mutation::ListExtentOutside => SemanticDataFault::ListExtent {
            list: 0,
            start: 0,
            length: 2,
            child_count: 1,
        },
        Mutation::ChildRoleCodeUnknown => SemanticDataFault::ChildRoleCode {
            child: 0,
            actual: 9,
        },
        Mutation::ChildRoleMismatch => SemanticDataFault::ChildRole {
            child: 0,
            expected: ProductChildRole::ProductMember,
            actual: ProductChildRole::TupleElement,
        },
        Mutation::ChildTagUnknown => SemanticDataFault::ChildTag {
            child: 0,
            actual: 7,
        },
        Mutation::LocalChildTargetOutside => SemanticDataFault::LocalChild {
            child: 0,
            target: 5,
            product_count: 1,
        },
        Mutation::LocalReservedNonzero => {
            let mut actual = [0; backend_version::HASH_BYTES];
            actual[0] = 1;
            SemanticDataFault::LocalReserved { child: 0, actual }
        }
        Mutation::ExternalAuthorityWrongDomain => {
            let mut raw = [0; backend_version::HASH_BYTES];
            raw[0] = bytes[SEMANTIC_RECORD + CHILD_RESERVED];
            SemanticDataFault::ExternalAuthority {
                child: 0,
                expected: u8::from(<IrFragmentDomain as Domain>::CODE),
                observed: raw[0],
                raw,
            }
        }
        Mutation::ChildCountTrailing => SemanticDataFault::Trailing { actual: 38 },
    }
}

#[test]
fn every_semantic_structural_class_rejects_with_exact_operands() -> Result<(), TestFailure> {
    let fragment = data_fragment()?;
    let cases = [
        Mutation::HeaderTruncated,
        Mutation::AtomLengthOverflow,
        Mutation::ProductHeadOutside,
        Mutation::ProductListOutside,
        Mutation::ConstructorCountMismatch,
        Mutation::ConstructorReservedPayload,
        Mutation::ListExtentOutside,
        Mutation::ChildRoleCodeUnknown,
        Mutation::ChildRoleMismatch,
        Mutation::ChildTagUnknown,
        Mutation::LocalChildTargetOutside,
        Mutation::LocalReservedNonzero,
        Mutation::ExternalAuthorityWrongDomain,
        Mutation::ChildCountTrailing,
    ];
    for mutation in cases {
        let mut bytes = fragment.bytes.clone();
        mutate(mutation, &mut bytes);
        let expected = expected_fault(mutation, &fragment.bytes);
        assert_eq!(
            FragmentView::validate(&bytes).err(),
            Some(FragmentError::SemanticData { fault: expected }),
            "mutation {mutation:?} must reject with its exact structural fault"
        );
    }
    Ok(())
}

#[test]
fn reopened_extension_parameters_parse_mixed_optional_operands_sequentially() {
    // count=2; T has no constraint and default=0; U has constraint=0 and
    // no default; all three reference-list lanes are empty.
    let mut payload = Vec::new();
    payload.extend_from_slice(&2_u32.to_le_bytes());
    payload.push(1);
    payload.extend_from_slice(&1_u32.to_le_bytes());
    payload.extend_from_slice(b"T");
    payload.push(0);
    payload.push(1);
    payload.extend_from_slice(&0_u32.to_le_bytes());
    payload.push(1);
    payload.extend_from_slice(&1_u32.to_le_bytes());
    payload.extend_from_slice(b"U");
    payload.push(1);
    payload.extend_from_slice(&0_u32.to_le_bytes());
    payload.push(0);
    // Schema 4 owns exact list membership even though each parameter still
    // has the legacy optional constraint/default cell grammar.
    payload.extend_from_slice(&1_u32.to_le_bytes());
    payload.extend_from_slice(&0_u32.to_le_bytes());
    payload.extend_from_slice(&2_u32.to_le_bytes());
    for _ in 0..3 {
        payload.extend_from_slice(&0_u32.to_le_bytes());
    }
    let pools = backend_semantic::ir::reopen_extension_pools(4, &payload, 0, 1, 0)
        .expect("mixed optional operands reopen");
    assert_eq!(
        pools.type_parameter(0).expect("first parameter"),
        backend_semantic::ir::DecodedTypeParameter {
            name: b"T",
            default: Some(0),
            semantics: backend_semantic::ir::DecodedTypeParameterSemantics::Legacy { constraint: None },
        }
    );
    assert_eq!(
        pools.type_parameter(1).expect("second parameter"),
        backend_semantic::ir::DecodedTypeParameter {
            name: b"U",
            default: None,
            semantics: backend_semantic::ir::DecodedTypeParameterSemantics::Legacy {
                constraint: Some(0),
            },
        }
    );
    let backend_semantic::ir::ReopenedTypeParameterList::Exact(parameters) = pools
        .type_parameter_list(TypeParameterListId::new(0))
        .expect("schema-four parameter membership")
    else {
        panic!("schema four must not fabricate start-only membership");
    };
    assert_eq!(parameters.length, 2);
    let parameter = parameters.get(1).expect("second schema-four parameter");
    assert_eq!(parameter.name, b"U");
    assert_eq!(
        pools
            .type_parameter_bounds(parameter)
            .expect("legacy bound discovery"),
        None
    );
}

#[test]
fn schema_five_type_parameter_ranges_keep_empty_lists_distinct_from_element_starts()
-> Result<(), backend_semantic::ir::ExtensionPoolFault> {
    let parameters = [ExtensionTypeParameter {
        name: b"T",
        bounds: backend_semantic::ir::ExtensionTypeParameterBoundRange {
            start: 0,
            length: 0,
        },
        default: None,
        variance: backend_semantic::ir::Variance::Invariant,
        kind: backend_semantic::ir::ExtensionTypeParameterKind::Type {
            inference: backend_semantic::ir::TypeParameterInference::Ordinary,
        },
        requirements: backend_semantic::ir::TypeParameterRequirements::none(),
    }];
    // The first list is explicitly empty; the second begins at the same
    // element coordinate but carries T. A raw start-only ID would alias them.
    let ranges = [
        ExtensionTypeParameterRange {
            start: 0,
            length: 0,
        },
        ExtensionTypeParameterRange {
            start: 0,
            length: 1,
        },
    ];
    let lane = ExtensionPoolsLane {
        type_parameters: &parameters,
        type_parameter_bounds: &[],
        type_parameter_lists: &ranges,
        free_predicates: &[],
        free_predicate_lists: &[],
        atom_lists: &[],
        type_lists: &[],
        entity_lists: &[],
    };
    lane.admit(0, 0, 0)?;
    let mut payload = vec![0; lane.payload_len()];
    lane.write_payload(&mut payload);
    let pools = backend_semantic::ir::reopen_extension_pools(7, &payload, 0, 0, 0)?;

    match pools.type_parameter_list(TypeParameterListId::new(0))? {
        ReopenedTypeParameterList::Exact(empty) => {
            assert_eq!(empty.start, 0);
            assert_eq!(empty.length, 0);
            assert!(empty.cursor()?.next().is_none());
        }
        ReopenedTypeParameterList::LegacyStartOnly { start } => {
            return Err(backend_semantic::ir::ExtensionPoolFault::LegacyTypeParameterStart {
                start,
                element_count: 0,
            });
        }
    }

    match pools.type_parameter_list(TypeParameterListId::new(1))? {
        ReopenedTypeParameterList::Exact(nonempty) => {
            assert_eq!(nonempty.get(0)?.name, b"T");
            let mut cursor = nonempty.cursor()?;
            let parameter = cursor.next().transpose()?.ok_or(
                backend_semantic::ir::ExtensionPoolFault::TypeParameterPosition {
                    position: 0,
                    length: nonempty.length,
                },
            )?;
            assert_eq!(parameter.name, b"T");
        }
        ReopenedTypeParameterList::LegacyStartOnly { start } => {
            return Err(backend_semantic::ir::ExtensionPoolFault::LegacyTypeParameterStart {
                start,
                element_count: 1,
            });
        }
    }
    Ok(())
}

#[test]
fn schema_three_type_parameter_starts_remain_explicitly_legacy()
-> Result<(), backend_semantic::ir::ExtensionPoolFault> {
    // An empty element lane and the three required reference-list lanes.
    let payload = [0_u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let pools = backend_semantic::ir::reopen_extension_pools(3, &payload, 0, 0, 0)?;
    assert!(matches!(
        pools.type_parameter_list(TypeParameterListId::new(0))?,
        ReopenedTypeParameterList::LegacyStartOnly { start: 0 }
    ));
    Ok(())
}

#[test]
fn schema_five_rejects_a_type_parameter_range_past_the_element_prefix() {
    let parameters = [ExtensionTypeParameter {
        name: b"T",
        bounds: backend_semantic::ir::ExtensionTypeParameterBoundRange {
            start: 0,
            length: 0,
        },
        default: None,
        variance: backend_semantic::ir::Variance::Invariant,
        kind: backend_semantic::ir::ExtensionTypeParameterKind::Type {
            inference: backend_semantic::ir::TypeParameterInference::Ordinary,
        },
        requirements: backend_semantic::ir::TypeParameterRequirements::none(),
    }];
    let lane = ExtensionPoolsLane {
        type_parameters: &parameters,
        type_parameter_bounds: &[],
        type_parameter_lists: &[ExtensionTypeParameterRange {
            start: 1,
            length: 1,
        }],
        free_predicates: &[],
        free_predicate_lists: &[],
        atom_lists: &[],
        type_lists: &[],
        entity_lists: &[],
    };
    assert_eq!(
        lane.admit(0, 0, 0),
        Err(backend_semantic::ir::ExtensionPoolFault::TypeParameterRange {
            list: 0,
            start: 1,
            length: 1,
            element_count: 1,
        })
    );
}

#[test]
fn schema_five_reopens_plural_bounds_and_closed_parameter_requirements()
-> Result<(), backend_semantic::ir::ExtensionPoolFault> {
    let parameters = [ExtensionTypeParameter {
        name: b"T",
        bounds: backend_semantic::ir::ExtensionTypeParameterBoundRange {
            start: 0,
            length: 2,
        },
        default: None,
        variance: backend_semantic::ir::Variance::Covariant,
        kind: backend_semantic::ir::ExtensionTypeParameterKind::Type {
            inference: backend_semantic::ir::TypeParameterInference::Const,
        },
        requirements: backend_semantic::ir::TypeParameterRequirements {
            primary: backend_semantic::ir::TypeParameterPrimaryRequirement::Reference { nullable: false },
            constructor: true,
            allows_ref_like: false,
        },
    }];
    let bounds = [
        ExtensionTypeParameterBound::Lifetime(b"'scope"),
        ExtensionTypeParameterBound::Type(0),
    ];
    let lists = [ExtensionTypeParameterRange {
        start: 0,
        length: 1,
    }];
    let lane = ExtensionPoolsLane {
        type_parameters: &parameters,
        type_parameter_bounds: &bounds,
        type_parameter_lists: &lists,
        free_predicates: &[],
        free_predicate_lists: &[],
        atom_lists: &[],
        type_lists: &[],
        entity_lists: &[],
    };
    lane.admit(0, 1, 0)?;
    let mut payload = vec![0; lane.payload_len()];
    lane.write_payload(&mut payload);
    let pools = backend_semantic::ir::reopen_extension_pools(7, &payload, 0, 1, 0)?;
    let backend_semantic::ir::ReopenedTypeParameterList::Exact(parameters) =
        pools.type_parameter_list(TypeParameterListId::new(0))?
    else {
        return Err(backend_semantic::ir::ExtensionPoolFault::LegacyTypeParameterStart {
            start: 0,
            element_count: 1,
        });
    };
    let parameter = parameters.get(0)?;
    assert_eq!(
        parameter.semantics,
        backend_semantic::ir::DecodedTypeParameterSemantics::Exact {
            bounds: backend_semantic::ir::ExtensionTypeParameterBoundRange {
                start: 0,
                length: 2,
            },
            variance: backend_semantic::ir::Variance::Covariant,
            kind: backend_semantic::ir::DecodedTypeParameterKind::Type {
                inference: backend_semantic::ir::TypeParameterInference::Const,
            },
            requirements: backend_semantic::ir::TypeParameterRequirements {
                primary: backend_semantic::ir::TypeParameterPrimaryRequirement::Reference {
                    nullable: false
                },
                constructor: true,
                allows_ref_like: false,
            },
        }
    );
    let bounds = pools.type_parameter_bounds(parameter)?.ok_or(
        backend_semantic::ir::ExtensionPoolFault::TypeParameterBounds {
            ordinal: 0,
            start: 0,
            length: 2,
            bound_count: 0,
        },
    )?;
    let mut cursor = bounds.cursor()?;
    assert_eq!(
        cursor.next().transpose()?,
        Some(backend_semantic::ir::DecodedTypeParameterBound::Lifetime(b"'scope"))
    );
    assert_eq!(
        cursor.next().transpose()?,
        Some(backend_semantic::ir::DecodedTypeParameterBound::Type(0))
    );
    assert!(cursor.next().is_none());
    Ok(())
}

#[test]
fn schema_five_rejects_constructor_with_an_implied_value_requirement() {
    let parameters = [ExtensionTypeParameter {
        name: b"T",
        bounds: backend_semantic::ir::ExtensionTypeParameterBoundRange {
            start: 0,
            length: 0,
        },
        default: None,
        variance: backend_semantic::ir::Variance::Invariant,
        kind: backend_semantic::ir::ExtensionTypeParameterKind::Type {
            inference: backend_semantic::ir::TypeParameterInference::Ordinary,
        },
        requirements: backend_semantic::ir::TypeParameterRequirements {
            primary: backend_semantic::ir::TypeParameterPrimaryRequirement::Unmanaged,
            constructor: true,
            allows_ref_like: false,
        },
    }];
    let lane = ExtensionPoolsLane {
        type_parameters: &parameters,
        type_parameter_bounds: &[],
        type_parameter_lists: &[],
        free_predicates: &[],
        free_predicate_lists: &[],
        atom_lists: &[],
        type_lists: &[],
        entity_lists: &[],
    };
    assert_eq!(
        lane.admit(0, 0, 0),
        Err(backend_semantic::ir::ExtensionPoolFault::TypeParameterRequirements {
            ordinal: 0,
            primary: backend_semantic::ir::TypeParameterPrimaryRequirement::Unmanaged,
            constructor: true,
            allows_ref_like: false,
        })
    );
}

#[test]
fn schema_five_rejects_ref_like_with_a_known_reference_requirement() {
    let parameters = [ExtensionTypeParameter {
        name: b"T",
        bounds: backend_semantic::ir::ExtensionTypeParameterBoundRange {
            start: 0,
            length: 0,
        },
        default: None,
        variance: backend_semantic::ir::Variance::Invariant,
        kind: backend_semantic::ir::ExtensionTypeParameterKind::Type {
            inference: backend_semantic::ir::TypeParameterInference::Ordinary,
        },
        requirements: backend_semantic::ir::TypeParameterRequirements {
            primary: backend_semantic::ir::TypeParameterPrimaryRequirement::Reference { nullable: true },
            constructor: false,
            allows_ref_like: true,
        },
    }];
    let lane = ExtensionPoolsLane {
        type_parameters: &parameters,
        type_parameter_bounds: &[],
        type_parameter_lists: &[],
        free_predicates: &[],
        free_predicate_lists: &[],
        atom_lists: &[],
        type_lists: &[],
        entity_lists: &[],
    };
    assert_eq!(
        lane.admit(0, 0, 0),
        Err(backend_semantic::ir::ExtensionPoolFault::TypeParameterRequirements {
            ordinal: 0,
            primary: backend_semantic::ir::TypeParameterPrimaryRequirement::Reference { nullable: true },
            constructor: false,
            allows_ref_like: true,
        })
    );
}

#[test]
fn decoded_reference_list_rejects_an_overflowing_position_without_panicking() {
    let list = backend_semantic::ir::DecodedRefList { words: &[] };
    assert_eq!(list.get(usize::MAX), None);
}

#[test]
fn schema_seven_free_predicates_round_trip_subject_and_multi_bound()
-> Result<(), backend_semantic::ir::ExtensionPoolFault> {
    // Two non-trivial free predicates. The first names a plain type subject
    // with two ordered bounds; the second names an associated-type subject.
    // The subject coordinate is a type fact and the bounds reuse the shared
    // `type_parameter_bounds` lane.
    let bounds = [
        ExtensionTypeParameterBound::Type(1),
        ExtensionTypeParameterBound::Lifetime(b"'scope"),
        ExtensionTypeParameterBound::Type(2),
    ];
    let predicates = [
        ExtensionFreePredicate {
            subject: 0,
            bounds: ExtensionTypeParameterBoundRange {
                start: 0,
                length: 2,
            },
        },
        ExtensionFreePredicate {
            subject: 3,
            bounds: ExtensionTypeParameterBoundRange {
                start: 2,
                length: 1,
            },
        },
    ];
    let lists = [ExtensionTypeParameterRange {
        start: 0,
        length: 2,
    }];
    let lane = ExtensionPoolsLane {
        type_parameters: &[],
        type_parameter_bounds: &bounds,
        type_parameter_lists: &[],
        free_predicates: &predicates,
        free_predicate_lists: &lists,
        atom_lists: &[],
        type_lists: &[],
        entity_lists: &[],
    };
    // Four type facts are named by the subjects and bounds.
    lane.admit(0, 4, 0)?;
    let mut payload = vec![0; lane.payload_len()];
    lane.write_payload(&mut payload);
    let pools = backend_semantic::ir::reopen_extension_pools(7, &payload, 0, 4, 0)?;
    assert_eq!(pools.free_predicate_list_count(), 1);
    let run = pools.free_predicate_list(FreePredicateListId::new(0))?;
    assert_eq!(run.start, 0);
    assert_eq!(run.length, 2);

    let first = pools.free_predicate(0)?;
    assert_eq!(first.subject, 0);
    let first_bounds = pools.free_predicate_bounds(first)?;
    let mut cursor = first_bounds.cursor()?;
    assert_eq!(
        cursor.next().transpose()?,
        Some(backend_semantic::ir::DecodedTypeParameterBound::Type(1))
    );
    assert_eq!(
        cursor.next().transpose()?,
        Some(backend_semantic::ir::DecodedTypeParameterBound::Lifetime(
            b"'scope"
        ))
    );
    assert!(cursor.next().is_none());

    let second = pools.free_predicate(1)?;
    assert_eq!(second.subject, 3);
    let second_bounds = pools.free_predicate_bounds(second)?;
    assert_eq!(second_bounds.length, 1);
    assert_eq!(
        second_bounds.get(0)?,
        backend_semantic::ir::DecodedTypeParameterBound::Type(2)
    );
    Ok(())
}

#[test]
fn schema_seven_rejects_a_free_predicate_subject_outside_the_type_lane() {
    let predicates = [ExtensionFreePredicate {
        subject: 4,
        bounds: ExtensionTypeParameterBoundRange {
            start: 0,
            length: 0,
        },
    }];
    let lane = ExtensionPoolsLane {
        type_parameters: &[],
        type_parameter_bounds: &[],
        type_parameter_lists: &[],
        free_predicates: &predicates,
        free_predicate_lists: &[],
        atom_lists: &[],
        type_lists: &[],
        entity_lists: &[],
    };
    assert_eq!(
        lane.admit(0, 4, 0),
        Err(backend_semantic::ir::ExtensionPoolFault::FreePredicateSubject {
            predicate: 0,
            raw: 4,
            limit: 4,
        })
    );
}
