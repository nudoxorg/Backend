//! Exercises the semantic-data section grammar through byte-exact mutations.
//! Every structural class of the embedded canonical graph rejects with its
//! exact typed fault and operands before a borrowed view exists.
use compiler_ir::{
    AtomInput, CanonicalDataError, DataFacts, DataOutput, DataResourceBudget, DataScratch,
    EntityKind, EntityRecord, FragmentError, FragmentView, PrepareError, PreparedFragment,
    PrimitiveType, SemanticDataFault, SourceIdentity, TypeNode, WriteError,
    canonicalize_data_with_budget,
};
use compiler_ir::{
    AtomId, ListSpan, ProductChildRole, ProductChildren, ProductConstructorFault,
    ProductConstructorTag, ProductId, ProductListId, ProductRef, SemanticAtom, SemanticProduct,
    SemanticProductChild, SemanticProductConstructor,
};
use compiler_vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage};
use core::mem::size_of;
use heart_identity::{ContentId, Domain, IrFragmentDomain, SourceFactDomain, ToolchainDomain};
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
    + (size_of::<u32>() + heart_identity::HASH_BYTES)
    + (size_of::<u8>() * 4 + heart_identity::HASH_BYTES * 2);

/// Payload-relative lane offsets: header(20) + atom stream(4+7).
const ATOM_LENGTH: usize = 20;
const PRODUCT_HEAD: usize = 31;
const CONSTRUCTOR_COUNT: usize = 8;
const CONSTRUCTOR_P1: usize = 47;
const LIST_START: usize = 51;
const LIST_LENGTH: usize = 55;
const CHILD_ROLE: usize = 59;
const CHILD_TAG: usize = 60;
const CHILD_TARGET: usize = 61;
const CHILD_RESERVED: usize = 65;
const CHILD_COUNT: usize = 16;
const SEMANTIC_LENGTH: usize = 20 + (4 + 7) + 8 + 12 + 8 + 38;
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
        semantic_type: compiler_ir::TypeId::new(0),
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
        &[7, 0, 1, 0, 97, 0, 0, 0]
    );
    // Header: one atom, one product, one constructor, one list, one child.
    assert_eq!(
        &fragment.bytes[SEMANTIC_RECORD..SEMANTIC_RECORD + 20],
        &[1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0]
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
        &fragment.bytes[SEMANTIC_RECORD + CHILD_RESERVED..SEMANTIC_END],
        &[0; 32]
    );
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
            bytes[SEMANTIC_COUNT] = 19;
            bytes[SEMANTIC_BYTE_LENGTH] = 19;
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
            required: 20,
            actual: 19,
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
            let mut actual = [0; heart_identity::HASH_BYTES];
            actual[0] = 1;
            SemanticDataFault::LocalReserved { child: 0, actual }
        }
        Mutation::ExternalAuthorityWrongDomain => {
            let mut raw = [0; heart_identity::HASH_BYTES];
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
