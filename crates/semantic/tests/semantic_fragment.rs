//! Red falsifier binding the canonical semantic-data graph into fragment
//! bytes. A fragment that omits constructors, roles, products, lists, or
//! child authorities must either reject or change its canonical bytes.
use core::num::TryFromIntError;

use backend_semantic::ir::{
    AtomId, ListSpan, ProductChildRole, ProductChildren, ProductId, ProductListId, ProductRef,
    SemanticAtom, SemanticProduct, SemanticProductChild, SemanticProductConstructor,
};
use backend_semantic::ir::{
    AtomInput, CanonicalDataError, DataFacts, DataOutput, DataScratch, EntityKind, EntityRecord,
    FragmentError, FragmentView, PrepareError, PreparedFragment, PrimitiveType, SourceIdentity,
    TypeNode, WriteError, canonicalize_data_with_budget,
};
use backend_semantic::vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
use thiserror::Error;

#[derive(Debug, Error)]
enum TestFailure {
    #[error("test source length {actual} does not fit the compact source identity width")]
    SourceLength {
        actual: usize,
        #[source]
        source: TryFromIntError,
    },
    #[error(transparent)]
    Canonical(#[from] CanonicalDataError),
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Write(#[from] WriteError),
    #[error(transparent)]
    Validate(#[from] FragmentError),
}

const SOURCE_BYTES: &[u8] = b"semantic-fragment-source";
const TEST_MAX_REFINEMENT_ROUNDS: u32 = 4;
const TEST_MAX_SEMANTIC_WORK: u64 = 128;
const TEST_SEMANTIC_BUDGET: backend_semantic::ir::DataResourceBudget = backend_semantic::ir::DataResourceBudget {
    max_refinement_rounds: TEST_MAX_REFINEMENT_ROUNDS,
    max_sort_comparisons: TEST_MAX_SEMANTIC_WORK,
    max_hash_evaluations: TEST_MAX_SEMANTIC_WORK,
    max_intern_probes: TEST_MAX_SEMANTIC_WORK,
    max_work: TEST_MAX_SEMANTIC_WORK,
};

fn source_identity() -> Result<SourceIdentity, TestFailure> {
    let byte_len =
        u32::try_from(SOURCE_BYTES.len()).map_err(|source| TestFailure::SourceLength {
            actual: SOURCE_BYTES.len(),
            source,
        })?;
    Ok(SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(SOURCE_BYTES),
        byte_len,
    })
}

fn recipe_fact() -> Result<CompileRecipeFact, TestFailure> {
    Ok(CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        source_identity()?.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"semantic-fragment-toolchain"),
    ))
}

fn write_fragment(
    constructor: SemanticProductConstructor,
    child_role: ProductChildRole,
) -> Result<Vec<u8>, TestFailure> {
    let semantic_atoms = [SemanticAtom { bytes: b"nominal" }];
    let semantic_products = [SemanticProduct {
        head: AtomId::new(0),
        children: ProductListId::new(0),
    }];
    let semantic_lists = [ListSpan::<ProductChildren>::new(0, 1)];
    let semantic_children = [SemanticProductChild {
        target: ProductRef::Local(ProductId::new(0)),
        role: child_role,
    }];
    let mut atom_order = [AtomId::new(0)];
    let mut atom_to_canonical = [0_u32];
    let mut product_order = [ProductId::new(0)];
    let mut product_to_canonical = [0_u32];
    let mut colors = [0_u32];
    let mut next_colors = [0_u32];
    let mut hashes = [0_u64];
    let mut next_hashes = [0_u64];
    let mut product_representatives = [ProductId::new(0)];
    let mut intern_slots = [0_u64; 1];
    let mut data_scratch = DataScratch {
        atom_order: &mut atom_order,
        atom_to_canonical: &mut atom_to_canonical,
        product_order: &mut product_order,
        product_to_canonical: &mut product_to_canonical,
        colors: &mut colors,
        next_colors: &mut next_colors,
        hashes: &mut hashes,
        next_hashes: &mut next_hashes,
        product_representatives: &mut product_representatives,
        intern_slots: &mut intern_slots,
    };
    let mut data_atoms = [SemanticAtom { bytes: b"" }];
    let mut data_products = [SemanticProduct {
        head: AtomId::new(0),
        children: ProductListId::new(0),
    }];
    let mut data_constructors = [SemanticProductConstructor::PRODUCT];
    let mut data_lists = [ListSpan::<ProductChildren>::new(0, 0)];
    let mut data_children = [SemanticProductChild {
        target: ProductRef::Local(ProductId::new(0)),
        role: ProductChildRole::ProductMember,
    }];
    let mut data_output = DataOutput {
        atoms: &mut data_atoms,
        products: &mut data_products,
        constructors: &mut data_constructors,
        lists: &mut data_lists,
        children: &mut data_children,
    };
    let canonical = canonicalize_data_with_budget(
        DataFacts {
            atoms: &semantic_atoms,
            products: &semantic_products,
            constructors: &[constructor],
            lists: &semantic_lists,
            children: &semantic_children,
        },
        &mut data_scratch,
        &mut data_output,
        TEST_SEMANTIC_BUDGET,
    )?;
    let entities = [EntityRecord {
        semantic_type: backend_semantic::ir::TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Function,
    }];
    let types = [TypeNode::Primitive(PrimitiveType::I32)];
    let atoms = [AtomInput { bytes: b"decl" }];
    let prepared = PreparedFragment::prepare_with_data(
        source_identity()?,
        recipe_fact()?,
        &entities,
        &types,
        &atoms,
        &canonical,
    )?;
    let required = prepared.required_capacity();
    let mut output = vec![0; required];
    let bytes = prepared.write_into(&mut output)?;
    let view = FragmentView::validate(bytes)?;
    assert_eq!(view.as_ref().len(), required);
    Ok(output)
}

#[test]
fn semantic_product_shape_and_role_change_canonical_fragment_bytes() -> Result<(), TestFailure> {
    let product_output = write_fragment(
        SemanticProductConstructor::PRODUCT,
        ProductChildRole::ProductMember,
    )?;
    let tuple_output = write_fragment(
        SemanticProductConstructor::TUPLE,
        ProductChildRole::TupleElement,
    )?;
    assert_ne!(
        product_output, tuple_output,
        "the fragment must commit the complete canonical product structure, not only entity/type shells"
    );
    Ok(())
}
