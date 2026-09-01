//! Red falsifiers for allocation-free semantic-data canonicalization.
//! Every case pins exact typed rejections, transactional admission, the
//! coinductive quotient, and borrowed output views.
#![allow(
    clippy::indexing_slicing,
    reason = "fixtures slice fixed arrays to emulate short caller lanes and index snapshots only after exact-equality assertions; out-of-range access in a test is a loud failure, not a silent invariant breach"
)]
use core::mem::{align_of, size_of};

use compiler_ir::{
    CanonicalDataError, DataCanonicalization, DataCountLane, DataFacts, DataOutput, DataOutputLane,
    DataResource, DataResourceBudget, DataScratch, DataScratchLane, canonicalize_data_with_budget,
};
use compiler_ir::{
    AtomId, ExternalCoordinate, ExternalFragmentId, ListSpan, ProductChildRole, ProductChildren,
    ProductConstructorFault, ProductConstructorTag, ProductId, ProductListId, ProductRef,
    SemanticAtom, SemanticProduct, SemanticProductChild, SemanticProductConstructor,
};
use thiserror::Error;

#[derive(Debug, Error)]
enum FixtureFailure {
    #[error("canonical external child must retain its authority")]
    ExternalAuthorityLost,
    #[error(transparent)]
    Canonical(#[from] CanonicalDataError),
}

const TEST_BUDGET: DataResourceBudget = DataResourceBudget {
    max_refinement_rounds: 64,
    max_sort_comparisons: 1_000_000,
    max_hash_evaluations: 1_000_000,
    max_intern_probes: 1_000_000,
    max_work: 10_000_000,
};

fn child(target: ProductRef) -> SemanticProductChild {
    SemanticProductChild {
        target,
        role: ProductChildRole::ProductMember,
    }
}

fn external_product(ordinal: u32) -> SemanticProductChild {
    external_product_with_fragment(b"remote-fragment", ordinal)
}

fn external_product_with_fragment(
    fragment_bytes: &'static [u8],
    ordinal: u32,
) -> SemanticProductChild {
    let fragment = ExternalFragmentId::from_canonical_bytes(fragment_bytes);
    child(ProductRef::External(ExternalCoordinate::bind(
        fragment, ordinal,
    )))
}

fn canonicalize_one_constructor(
    constructors: &[SemanticProductConstructor],
    children: &[SemanticProductChild],
    output_constructors: &mut [SemanticProductConstructor],
) -> Result<(), CanonicalDataError> {
    let atoms = [SemanticAtom { bytes: b"shape" }];
    let products = [SemanticProduct {
        head: AtomId::new(0),
        children: ProductListId::new(0),
    }];
    let child_count =
        u32::try_from(children.len()).map_err(|source| CanonicalDataError::Count {
            lane: DataCountLane::Children,
            actual: children.len(),
            source,
        })?;
    let lists = [ListSpan::<ProductChildren>::new(0, child_count)];
    let mut atom_order = [AtomId::new(0)];
    let mut atom_map = [0];
    let mut product_order = [ProductId::new(0)];
    let mut product_map = [0];
    let mut colors = [0];
    let mut next_colors = [0];
    let mut hashes = [0];
    let mut next_hashes = [0];
    let mut representatives = [ProductId::new(0)];
    let mut intern_slots = [0; 2];
    let mut scratch = scratch(
        &mut atom_order,
        &mut atom_map,
        &mut product_order,
        &mut product_map,
        &mut colors,
        &mut next_colors,
        &mut hashes,
        &mut next_hashes,
        &mut representatives,
        &mut intern_slots,
    );
    let mut output_atoms = [SemanticAtom { bytes: b"sentinel" }];
    let mut output_products = [SemanticProduct {
        head: AtomId::new(9),
        children: ProductListId::new(9),
    }];
    let mut output_lists = [ListSpan::<ProductChildren>::new(9, 9)];
    let mut output_children = [child(ProductRef::Local(ProductId::new(9))); 2];
    let mut output = DataOutput {
        atoms: &mut output_atoms,
        products: &mut output_products,
        constructors: output_constructors,
        lists: &mut output_lists,
        children: &mut output_children,
    };
    canonicalize_data_with_budget(
        DataFacts {
            atoms: &atoms,
            products: &products,
            constructors,
            lists: &lists,
            children,
        },
        &mut scratch,
        &mut output,
        TEST_BUDGET,
    )
    .map(|_| ())
}

#[allow(
    clippy::too_many_arguments,
    reason = "The test fixture names each independent caller-scratch lane explicitly."
)]
fn scratch<'scratch>(
    atom_order: &'scratch mut [AtomId],
    atom_to_canonical: &'scratch mut [u32],
    product_order: &'scratch mut [ProductId],
    product_to_canonical: &'scratch mut [u32],
    colors: &'scratch mut [u32],
    next_colors: &'scratch mut [u32],
    hashes: &'scratch mut [u64],
    next_hashes: &'scratch mut [u64],
    product_representatives: &'scratch mut [ProductId],
    intern_slots: &'scratch mut [u64],
) -> DataScratch<'scratch> {
    DataScratch {
        atom_order,
        atom_to_canonical,
        product_order,
        product_to_canonical,
        colors,
        next_colors,
        hashes,
        next_hashes,
        product_representatives,
        intern_slots,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LaneSnapshot<'bytes> {
    atom_order: [AtomId; 2],
    atom_map: [u32; 2],
    product_order: [ProductId; 2],
    product_map: [u32; 2],
    colors: [u32; 2],
    next_colors: [u32; 2],
    hashes: [u64; 2],
    next_hashes: [u64; 2],
    representatives: [ProductId; 2],
    intern_slots: [u64; 2],
    output_atoms: [SemanticAtom<'bytes>; 2],
    output_products: [SemanticProduct; 2],
    output_constructors: [SemanticProductConstructor; 2],
    output_lists: [ListSpan<ProductChildren>; 2],
    output_children: [SemanticProductChild; 2],
}

fn run_budget_failure<'facts, 'bytes>(
    facts: DataFacts<'facts, 'bytes>,
    budget: DataResourceBudget,
    intern_capacity: usize,
) -> (Option<CanonicalDataError>, LaneSnapshot<'bytes>) {
    let mut atom_order = [AtomId::new(90); 2];
    let mut atom_map = [90_u32; 2];
    let mut product_order = [ProductId::new(91); 2];
    let mut product_map = [91_u32; 2];
    let mut colors = [92_u32; 2];
    let mut next_colors = [93_u32; 2];
    let mut hashes = [94_u64; 2];
    let mut next_hashes = [95_u64; 2];
    let mut representatives = [ProductId::new(96); 2];
    let mut intern_storage = [97_u64; 2];
    let intern_slots = &mut intern_storage[..intern_capacity];
    let mut scratch = scratch(
        &mut atom_order,
        &mut atom_map,
        &mut product_order,
        &mut product_map,
        &mut colors,
        &mut next_colors,
        &mut hashes,
        &mut next_hashes,
        &mut representatives,
        intern_slots,
    );
    let mut output_atoms = [SemanticAtom { bytes: b"sentinel" }; 2];
    let mut output_products = [SemanticProduct {
        head: AtomId::new(80),
        children: ProductListId::new(80),
    }; 2];
    let mut output_constructors = [SemanticProductConstructor::array(80); 2];
    let mut output_lists = [ListSpan::<ProductChildren>::new(80, 80); 2];
    let mut output_children = [child(ProductRef::Local(ProductId::new(98))); 2];
    let error = canonicalize_data_with_budget(
        facts,
        &mut scratch,
        &mut DataOutput {
            atoms: &mut output_atoms,
            products: &mut output_products,
            constructors: &mut output_constructors,
            lists: &mut output_lists,
            children: &mut output_children,
        },
        budget,
    )
    .err();
    (
        error,
        LaneSnapshot {
            atom_order,
            atom_map,
            product_order,
            product_map,
            colors,
            next_colors,
            hashes,
            next_hashes,
            representatives,
            intern_slots: intern_storage,
            output_atoms,
            output_products,
            output_constructors,
            output_lists,
            output_children,
        },
    )
}

fn pristine_snapshot() -> LaneSnapshot<'static> {
    LaneSnapshot {
        atom_order: [AtomId::new(90); 2],
        atom_map: [90; 2],
        product_order: [ProductId::new(91); 2],
        product_map: [91; 2],
        colors: [92; 2],
        next_colors: [93; 2],
        hashes: [94; 2],
        next_hashes: [95; 2],
        representatives: [ProductId::new(96); 2],
        intern_slots: [97; 2],
        output_atoms: [SemanticAtom { bytes: b"sentinel" }; 2],
        output_products: [SemanticProduct {
            head: AtomId::new(80),
            children: ProductListId::new(80),
        }; 2],
        output_constructors: [SemanticProductConstructor::array(80); 2],
        output_lists: [ListSpan::<ProductChildren>::new(80, 80); 2],
        output_children: [child(ProductRef::Local(ProductId::new(98))); 2],
    }
}

fn run_short_lane_failure(
    short_scratch: Option<DataScratchLane>,
    short_output: Option<DataOutputLane>,
) -> (Option<CanonicalDataError>, LaneSnapshot<'static>) {
    let atoms = [
        SemanticAtom { bytes: b"first" },
        SemanticAtom { bytes: b"second" },
    ];
    let products = [
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(0),
        },
        SemanticProduct {
            head: AtomId::new(1),
            children: ProductListId::new(1),
        },
    ];
    let constructors = [SemanticProductConstructor::PRODUCT; 2];
    let lists = [ListSpan::<ProductChildren>::new(0, 1), ListSpan::new(1, 0)];
    let children = [child(ProductRef::Local(ProductId::new(1)))];

    let mut atom_order = [AtomId::new(90); 2];
    let atom_order_lane = if short_scratch == Some(DataScratchLane::AtomOrder) {
        &mut atom_order[..1]
    } else {
        &mut atom_order[..]
    };
    let mut atom_map = [90_u32; 2];
    let atom_map_lane = if short_scratch == Some(DataScratchLane::AtomMap) {
        &mut atom_map[..1]
    } else {
        &mut atom_map[..]
    };
    let mut product_order = [ProductId::new(91); 2];
    let product_order_lane = if short_scratch == Some(DataScratchLane::ProductOrder) {
        &mut product_order[..1]
    } else {
        &mut product_order[..]
    };
    let mut product_map = [91_u32; 2];
    let product_map_lane = if short_scratch == Some(DataScratchLane::ProductMap) {
        &mut product_map[..1]
    } else {
        &mut product_map[..]
    };
    let mut colors = [92_u32; 2];
    let colors_lane = if short_scratch == Some(DataScratchLane::Colors) {
        &mut colors[..1]
    } else {
        &mut colors[..]
    };
    let mut next_colors = [93_u32; 2];
    let next_colors_lane = if short_scratch == Some(DataScratchLane::NextColors) {
        &mut next_colors[..1]
    } else {
        &mut next_colors[..]
    };
    let mut hashes = [94_u64; 2];
    let hashes_lane = if short_scratch == Some(DataScratchLane::Hashes) {
        &mut hashes[..1]
    } else {
        &mut hashes[..]
    };
    let mut next_hashes = [95_u64; 2];
    let next_hashes_lane = if short_scratch == Some(DataScratchLane::NextHashes) {
        &mut next_hashes[..1]
    } else {
        &mut next_hashes[..]
    };
    let mut representatives = [ProductId::new(96); 2];
    let representatives_lane = if short_scratch == Some(DataScratchLane::Representatives) {
        &mut representatives[..1]
    } else {
        &mut representatives[..]
    };
    let mut intern_storage = [97_u64; 2];
    let intern_slots = if short_scratch == Some(DataScratchLane::InternSlots) {
        &mut intern_storage[..0]
    } else {
        &mut intern_storage[..]
    };
    let mut scratch = scratch(
        atom_order_lane,
        atom_map_lane,
        product_order_lane,
        product_map_lane,
        colors_lane,
        next_colors_lane,
        hashes_lane,
        next_hashes_lane,
        representatives_lane,
        intern_slots,
    );

    let mut output_atoms = [SemanticAtom { bytes: b"sentinel" }; 2];
    let output_atoms_lane = if short_output == Some(DataOutputLane::Atoms) {
        &mut output_atoms[..0]
    } else {
        &mut output_atoms[..]
    };
    let mut output_products = [SemanticProduct {
        head: AtomId::new(80),
        children: ProductListId::new(80),
    }; 2];
    let output_products_lane = if short_output == Some(DataOutputLane::Products) {
        &mut output_products[..0]
    } else {
        &mut output_products[..]
    };
    let mut output_constructors = [SemanticProductConstructor::array(80); 2];
    let output_constructors_lane = if short_output == Some(DataOutputLane::Constructors) {
        &mut output_constructors[..0]
    } else {
        &mut output_constructors[..]
    };
    let mut output_lists = [ListSpan::<ProductChildren>::new(80, 80); 2];
    let output_lists_lane = if short_output == Some(DataOutputLane::Lists) {
        &mut output_lists[..0]
    } else {
        &mut output_lists[..]
    };
    let mut output_children = [child(ProductRef::Local(ProductId::new(98))); 2];
    let output_children_lane = if short_output == Some(DataOutputLane::Children) {
        &mut output_children[..0]
    } else {
        &mut output_children[..]
    };
    let error = canonicalize_data_with_budget(
        DataFacts {
            atoms: &atoms,
            products: &products,
            constructors: &constructors,
            lists: &lists,
            children: &children,
        },
        &mut scratch,
        &mut DataOutput {
            atoms: output_atoms_lane,
            products: output_products_lane,
            constructors: output_constructors_lane,
            lists: output_lists_lane,
            children: output_children_lane,
        },
        TEST_BUDGET,
    )
    .err();

    (
        error,
        LaneSnapshot {
            atom_order,
            atom_map,
            product_order,
            product_map,
            colors,
            next_colors,
            hashes,
            next_hashes,
            representatives,
            intern_slots: intern_storage,
            output_atoms,
            output_products,
            output_constructors,
            output_lists,
            output_children,
        },
    )
}

#[test]
fn every_short_scratch_and_output_lane_is_preflighted_as_a_table() {
    let scratch_lanes = [
        DataScratchLane::AtomOrder,
        DataScratchLane::AtomMap,
        DataScratchLane::ProductOrder,
        DataScratchLane::ProductMap,
        DataScratchLane::Colors,
        DataScratchLane::NextColors,
        DataScratchLane::Hashes,
        DataScratchLane::NextHashes,
        DataScratchLane::Representatives,
        DataScratchLane::InternSlots,
    ];
    for lane in scratch_lanes {
        let (error, snapshot) = run_short_lane_failure(Some(lane), None);
        let required = if lane == DataScratchLane::InternSlots {
            1
        } else {
            2
        };
        let actual = if lane == DataScratchLane::InternSlots {
            0
        } else {
            1
        };
        assert_eq!(
            error,
            Some(CanonicalDataError::Scratch {
                lane,
                required,
                actual,
            })
        );
        assert_eq!(snapshot, pristine_snapshot());
    }

    let output_lanes = [
        DataOutputLane::Atoms,
        DataOutputLane::Products,
        DataOutputLane::Constructors,
        DataOutputLane::Lists,
        DataOutputLane::Children,
    ];
    for lane in output_lanes {
        let (error, snapshot) = run_short_lane_failure(None, Some(lane));
        let required = if lane == DataOutputLane::Children {
            1
        } else {
            2
        };
        assert_eq!(
            error,
            Some(CanonicalDataError::OutputTooSmall {
                lane,
                required,
                available: 0,
            })
        );
        assert_eq!(snapshot, pristine_snapshot());
    }
}

#[test]
fn recursive_products_are_hash_consistent_and_pool_views_lend_output() -> Result<(), FixtureFailure>
{
    let atoms = [
        SemanticAtom { bytes: b"pair" },
        SemanticAtom { bytes: b"leaf" },
    ];
    let children = [
        child(ProductRef::Local(ProductId::new(1))),
        external_product(7),
    ];
    let lists = [ListSpan::<ProductChildren>::new(0, 2), ListSpan::new(2, 0)];
    let products = [
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(0),
        },
        SemanticProduct {
            head: AtomId::new(1),
            children: ProductListId::new(1),
        },
    ];
    let constructors = [SemanticProductConstructor::PRODUCT; 2];
    let facts = DataFacts {
        atoms: &atoms,
        products: &products,
        constructors: &constructors,
        lists: &lists,
        children: &children,
    };
    let mut atom_order = [AtomId::new(0); 2];
    let mut atom_map = [0; 2];
    let mut product_order = [ProductId::new(0); 2];
    let mut product_map = [0; 2];
    let mut colors = [0; 2];
    let mut next_colors = [0; 2];
    let mut hashes = [0; 2];
    let mut next_hashes = [0; 2];
    let mut representatives = [ProductId::new(0); 2];
    let mut intern_slots = [0; 4];
    let mut scratch = scratch(
        &mut atom_order,
        &mut atom_map,
        &mut product_order,
        &mut product_map,
        &mut colors,
        &mut next_colors,
        &mut hashes,
        &mut next_hashes,
        &mut representatives,
        &mut intern_slots,
    );
    let mut output_atoms = [SemanticAtom { bytes: &[] }; 2];
    let mut output_products = [SemanticProduct {
        head: AtomId::new(99),
        children: ProductListId::new(99),
    }; 2];
    let mut output_constructors = [SemanticProductConstructor::array(99); 2];
    let mut output_lists = [ListSpan::<ProductChildren>::new(99, 99); 2];
    let mut output_children = [child(ProductRef::Local(ProductId::new(99))); 2];
    let output_children_ptr = output_children.as_ptr();
    let mut output = DataOutput {
        atoms: &mut output_atoms,
        products: &mut output_products,
        constructors: &mut output_constructors,
        lists: &mut output_lists,
        children: &mut output_children,
    };
    let graph = canonicalize_data_with_budget(
        facts,
        &mut scratch,
        &mut output,
        DataResourceBudget {
            max_refinement_rounds: 4,
            max_sort_comparisons: 128,
            max_hash_evaluations: 32,
            max_intern_probes: 32,
            max_work: 512,
        },
    )?;

    assert_eq!(
        graph.atoms(),
        &[
            SemanticAtom { bytes: b"leaf" },
            SemanticAtom { bytes: b"pair" }
        ]
    );
    assert_eq!(graph.metrics().canonical_product_count, 2);
    assert_eq!(graph.metrics().canonical_list_count, 2);
    assert_eq!(graph.metrics().canonical_child_count, 2);
    assert!(graph.metrics().refinement_rounds <= 4);
    assert!(graph.metrics().hash_evaluations > 0);
    assert!(graph.metrics().intern_probes >= u64::from(graph.metrics().canonical_product_count));
    let list = graph.list(ProductListId::new(1))?;
    assert_eq!(list.as_slice().as_ptr(), output_children_ptr);
    assert_eq!(list.as_slice().len(), 2);
    assert_eq!(graph.canonical_atom(AtomId::new(0)), Ok(AtomId::new(1)));
    assert_eq!(
        graph.canonical_product(ProductId::new(0)),
        Ok(ProductId::new(1))
    );
    assert_eq!(
        graph.canonical_product(ProductId::new(8)).err(),
        Some(CanonicalDataError::CanonicalProduct {
            ordinal: ProductId::new(8),
            count: 2
        })
    );
    Ok(())
}

#[test]
fn recursive_self_cycle_converges_with_bounded_scratch() -> Result<(), FixtureFailure> {
    let atoms = [SemanticAtom { bytes: b"cycle" }];
    let products = [SemanticProduct {
        head: AtomId::new(0),
        children: ProductListId::new(0),
    }];
    let lists = [ListSpan::<ProductChildren>::new(0, 1)];
    let children = [child(ProductRef::Local(ProductId::new(0)))];
    let (snapshot, metrics) = canonical_fixture_with_metrics(&atoms, &products, &lists, &children)?;
    assert_eq!(metrics.canonical_product_count, 1);
    assert_eq!(snapshot.product_count, 1);
    assert_eq!(snapshot.child_count, 1);
    assert_eq!(
        snapshot.children[0],
        child(ProductRef::Local(ProductId::new(0)))
    );
    Ok(())
}

#[test]
fn coinductive_cycles_quotient_same_heads_but_preserve_head_labels() -> Result<(), FixtureFailure> {
    let one_atom = [SemanticAtom { bytes: b"cycle" }];
    let one_product = [SemanticProduct {
        head: AtomId::new(0),
        children: ProductListId::new(0),
    }];
    let one_list = [ListSpan::<ProductChildren>::new(0, 1)];
    let one_child = [child(ProductRef::Local(ProductId::new(0)))];
    let one = canonical_fixture(&one_atom, &one_product, &one_list, &one_child)?;

    let same_head_products = [
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(0),
        },
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(1),
        },
    ];
    let same_head_lists = [ListSpan::<ProductChildren>::new(0, 1), ListSpan::new(1, 1)];
    let same_head_children = [
        child(ProductRef::Local(ProductId::new(1))),
        child(ProductRef::Local(ProductId::new(0))),
    ];
    let same_head = canonical_fixture(
        &one_atom,
        &same_head_products,
        &same_head_lists,
        &same_head_children,
    )?;
    assert_eq!(one, same_head);

    let distinct_atoms = [
        SemanticAtom { bytes: b"left" },
        SemanticAtom { bytes: b"right" },
    ];
    let distinct_head_products = [
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(0),
        },
        SemanticProduct {
            head: AtomId::new(1),
            children: ProductListId::new(1),
        },
    ];
    let distinct_heads = canonical_fixture(
        &distinct_atoms,
        &distinct_head_products,
        &same_head_lists,
        &same_head_children,
    )?;
    assert_eq!(distinct_heads.product_count, 2);
    assert_ne!(one, distinct_heads);
    Ok(())
}

#[test]
fn permuted_products_and_atoms_have_identical_canonical_facts() -> Result<(), FixtureFailure> {
    let first_atoms = [
        SemanticAtom { bytes: b"pair" },
        SemanticAtom { bytes: b"leaf" },
    ];
    let first_children = [
        child(ProductRef::Local(ProductId::new(1))),
        external_product(7),
    ];
    let first_lists = [ListSpan::<ProductChildren>::new(0, 2), ListSpan::new(2, 0)];
    let first_products = [
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(0),
        },
        SemanticProduct {
            head: AtomId::new(1),
            children: ProductListId::new(1),
        },
    ];
    let permuted_atoms = [
        SemanticAtom { bytes: b"leaf" },
        SemanticAtom { bytes: b"pair" },
    ];
    let permuted_children = [
        child(ProductRef::Local(ProductId::new(0))),
        external_product(7),
    ];
    let permuted_lists = [ListSpan::<ProductChildren>::new(0, 0), ListSpan::new(0, 2)];
    let permuted_products = [
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(0),
        },
        SemanticProduct {
            head: AtomId::new(1),
            children: ProductListId::new(1),
        },
    ];

    let first = canonical_fixture(&first_atoms, &first_products, &first_lists, &first_children)?;
    let second = canonical_fixture(
        &permuted_atoms,
        &permuted_products,
        &permuted_lists,
        &permuted_children,
    )?;
    assert_eq!(first, second);
    Ok(())
}

#[test]
fn equal_external_ordinals_keep_distinct_fragment_authorities_across_permutation()
-> Result<(), FixtureFailure> {
    let atoms = [
        SemanticAtom { bytes: b"pair-a" },
        SemanticAtom { bytes: b"pair-b" },
    ];
    let products = [
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(0),
        },
        SemanticProduct {
            head: AtomId::new(1),
            children: ProductListId::new(1),
        },
    ];
    let lists = [ListSpan::<ProductChildren>::new(0, 1), ListSpan::new(1, 1)];
    let children = [
        external_product_with_fragment(b"fragment-a", 7),
        external_product_with_fragment(b"fragment-b", 7),
    ];
    let first = canonical_fixture(&atoms, &products, &lists, &children)?;

    let permuted_atoms = [
        SemanticAtom { bytes: b"pair-b" },
        SemanticAtom { bytes: b"pair-a" },
    ];
    let permuted_products = [
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(0),
        },
        SemanticProduct {
            head: AtomId::new(1),
            children: ProductListId::new(1),
        },
    ];
    let permuted_lists = [ListSpan::<ProductChildren>::new(0, 1), ListSpan::new(1, 1)];
    let permuted_children = [
        external_product_with_fragment(b"fragment-b", 7),
        external_product_with_fragment(b"fragment-a", 7),
    ];
    let second = canonical_fixture(
        &permuted_atoms,
        &permuted_products,
        &permuted_lists,
        &permuted_children,
    )?;

    assert_eq!(first, second);
    assert_eq!(first.child_count, 2);
    let ProductRef::External(first_authority) = first.children[0].target else {
        return Err(FixtureFailure::ExternalAuthorityLost);
    };
    let ProductRef::External(second_authority) = first.children[1].target else {
        return Err(FixtureFailure::ExternalAuthorityLost);
    };
    assert_eq!(first_authority.ordinal, 7);
    assert_eq!(second_authority.ordinal, 7);
    assert_ne!(first_authority.fragment, second_authority.fragment);
    Ok(())
}

#[test]
fn duplicate_products_are_deduplicated_and_input_changes_survive() -> Result<(), FixtureFailure> {
    let atoms = [
        SemanticAtom { bytes: b"leaf" },
        SemanticAtom { bytes: b"other" },
    ];
    let lists = [ListSpan::<ProductChildren>::new(0, 0), ListSpan::new(0, 0)];
    let children: [SemanticProductChild; 0] = [];
    let products = [
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(0),
        },
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(1),
        },
    ];
    let (canonical, metrics) =
        canonical_fixture_with_metrics(&atoms, &products, &lists, &children)?;
    assert_eq!(metrics.canonical_product_count, 1);
    assert_eq!(canonical.products[0].head, AtomId::new(0));

    let changed_atoms = [
        SemanticAtom { bytes: b"changed" },
        SemanticAtom { bytes: b"other" },
    ];
    let (changed, _) =
        canonical_fixture_with_metrics(&changed_atoms, &products, &lists, &children)?;
    assert_ne!(canonical, changed);

    let external_products = [
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(0),
        },
        SemanticProduct {
            head: AtomId::new(1),
            children: ProductListId::new(1),
        },
    ];
    let external_lists = [ListSpan::<ProductChildren>::new(0, 2), ListSpan::new(2, 0)];
    let base_external = canonical_fixture(
        &atoms,
        &external_products,
        &external_lists,
        &[
            child(ProductRef::Local(ProductId::new(1))),
            external_product(7),
        ],
    )?;
    let changed_external = canonical_fixture(
        &atoms,
        &external_products,
        &external_lists,
        &[
            child(ProductRef::Local(ProductId::new(1))),
            external_product(8),
        ],
    )?;
    assert_ne!(base_external, changed_external);
    Ok(())
}

#[test]
fn invalid_owner_coordinate_and_budget_fail_before_output_mutation() -> Result<(), FixtureFailure> {
    let atoms = [SemanticAtom { bytes: b"leaf" }];
    let lists = [ListSpan::<ProductChildren>::new(0, 0)];
    let children: [SemanticProductChild; 0] = [];
    let products = [SemanticProduct {
        head: AtomId::new(9),
        children: ProductListId::new(0),
    }];
    let before_atoms = [SemanticAtom { bytes: b"sentinel" }];
    let before_products = [SemanticProduct {
        head: AtomId::new(8),
        children: ProductListId::new(8),
    }];
    let before_lists = [ListSpan::<ProductChildren>::new(8, 8)];
    let before_children: [SemanticProductChild; 0] = [];
    let (result, after_atoms, after_products, after_lists, after_children) = run_with_output(
        &atoms,
        &products,
        &lists,
        &children,
        before_atoms,
        before_products,
        before_lists,
        before_children,
        TEST_BUDGET,
        4,
    );
    let error = result.expect_err("invalid head must reject");
    assert_eq!(
        error,
        CanonicalDataError::ProductHead {
            product: ProductId::new(0),
            target: AtomId::new(9),
            atom_count: 1,
        }
    );
    assert_eq!(after_atoms, [SemanticAtom { bytes: b"sentinel" }]);
    assert_eq!(
        after_products,
        [SemanticProduct {
            head: AtomId::new(8),
            children: ProductListId::new(8),
        }]
    );
    assert_eq!(after_lists, [ListSpan::<ProductChildren>::new(8, 8)]);
    assert_eq!(after_children, []);

    let products = [SemanticProduct {
        head: AtomId::new(0),
        children: ProductListId::new(0),
    }];
    let (result, after_atoms, after_products, after_lists, after_children) = run_with_output(
        &atoms,
        &products,
        &lists,
        &children,
        [SemanticAtom { bytes: b"sentinel" }],
        [SemanticProduct {
            head: AtomId::new(8),
            children: ProductListId::new(8),
        }],
        [ListSpan::<ProductChildren>::new(8, 8)],
        [],
        DataResourceBudget {
            max_refinement_rounds: 0,
            ..TEST_BUDGET
        },
        4,
    );
    let error = result.expect_err("zero refinement budget must reject");
    assert_eq!(
        error,
        CanonicalDataError::BudgetAdmission {
            resource: DataResource::RefinementRounds,
            required: 1,
            limit: 0,
        }
    );
    assert_eq!(after_atoms, [SemanticAtom { bytes: b"sentinel" }]);
    assert_eq!(
        after_products,
        [SemanticProduct {
            head: AtomId::new(8),
            children: ProductListId::new(8),
        }]
    );
    assert_eq!(after_lists, [ListSpan::<ProductChildren>::new(8, 8)]);
    assert_eq!(after_children, []);
    Ok(())
}

#[test]
fn deduplicated_atom_output_uses_canonical_extent() -> Result<(), CanonicalDataError> {
    let atoms = [
        SemanticAtom { bytes: b"same" },
        SemanticAtom { bytes: b"same" },
    ];
    let products: [SemanticProduct; 0] = [];
    let lists: [ListSpan<ProductChildren>; 0] = [];
    let children: [SemanticProductChild; 0] = [];
    let (result, output_atoms, output_products, output_lists, output_children) = run_with_output(
        &atoms,
        &products,
        &lists,
        &children,
        [SemanticAtom { bytes: b"sentinel" }],
        [],
        [],
        [],
        TEST_BUDGET,
        0,
    );
    assert_eq!(result, Ok(()));
    assert_eq!(output_atoms, [SemanticAtom { bytes: b"same" }]);
    assert_eq!(output_products, []);
    assert_eq!(output_lists, []);
    assert_eq!(output_children, []);
    Ok(())
}

#[test]
fn oversized_intern_scratch_prefix_is_not_measured_or_mutated() -> Result<(), CanonicalDataError> {
    let mut small_slots = [0_u64; 1];
    let small = one_product_metrics(&mut small_slots)?;

    let suffix_before = [0xfeed_face_cafe_beef_u64; 7];
    let mut large_slots = [0_u64; 8];
    large_slots[1..].copy_from_slice(&suffix_before);
    let large = one_product_metrics(&mut large_slots)?;

    assert_eq!(small, large);
    assert_eq!(&large_slots[1..], &suffix_before);
    Ok(())
}

#[test]
fn resource_and_atom_capacity_failures_are_non_mutating() {
    let atoms = [SemanticAtom { bytes: b"one" }];
    let products = [SemanticProduct {
        head: AtomId::new(0),
        children: ProductListId::new(0),
    }];
    let lists = [ListSpan::<ProductChildren>::new(0, 0)];
    let children: [SemanticProductChild; 0] = [];
    let sentinel_atom = SemanticAtom { bytes: b"sentinel" };
    let sentinel_product = SemanticProduct {
        head: AtomId::new(8),
        children: ProductListId::new(8),
    };
    let sentinel_list = ListSpan::<ProductChildren>::new(8, 8);

    let (result, after_atoms, after_products, after_lists, after_children) = run_with_output(
        &atoms,
        &products,
        &lists,
        &children,
        [sentinel_atom],
        [sentinel_product],
        [sentinel_list],
        [],
        DataResourceBudget {
            max_hash_evaluations: 0,
            ..TEST_BUDGET
        },
        1,
    );
    assert_eq!(
        result,
        Err(CanonicalDataError::BudgetAdmission {
            resource: DataResource::HashEvaluations,
            required: 2,
            limit: 0,
        })
    );
    assert_eq!(after_atoms, [sentinel_atom]);
    assert_eq!(after_products, [sentinel_product]);
    assert_eq!(after_lists, [sentinel_list]);
    assert_eq!(after_children, []);

    let (result, after_atoms, after_products, after_lists, after_children) = run_with_output(
        &atoms,
        &products,
        &lists,
        &children,
        [sentinel_atom],
        [sentinel_product],
        [sentinel_list],
        [],
        DataResourceBudget {
            max_work: 0,
            ..TEST_BUDGET
        },
        1,
    );
    assert_eq!(
        result,
        Err(CanonicalDataError::BudgetAdmission {
            resource: DataResource::Work,
            required: 11,
            limit: 0,
        })
    );
    assert_eq!(after_atoms, [sentinel_atom]);
    assert_eq!(after_products, [sentinel_product]);
    assert_eq!(after_lists, [sentinel_list]);
    assert_eq!(after_children, []);

    let two_atoms = [
        SemanticAtom { bytes: b"one" },
        SemanticAtom { bytes: b"two" },
    ];
    let two_products = [
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(0),
        },
        SemanticProduct {
            head: AtomId::new(1),
            children: ProductListId::new(1),
        },
    ];
    let two_lists = [ListSpan::<ProductChildren>::new(0, 0), ListSpan::new(0, 0)];
    let (result, after_atoms, after_products, after_lists, after_children) = run_with_output(
        &two_atoms,
        &two_products,
        &two_lists,
        &children,
        [sentinel_atom; 2],
        [sentinel_product; 2],
        [sentinel_list; 2],
        [],
        DataResourceBudget {
            max_sort_comparisons: 0,
            ..TEST_BUDGET
        },
        2,
    );
    assert_eq!(
        result,
        Err(CanonicalDataError::BudgetAdmission {
            resource: DataResource::SortComparisons,
            required: 20,
            limit: 0,
        })
    );
    assert_eq!(after_atoms, [sentinel_atom; 2]);
    assert_eq!(after_products, [sentinel_product; 2]);
    assert_eq!(after_lists, [sentinel_list; 2]);
    assert_eq!(after_children, []);

    let (result, after_atoms, after_products, after_lists, after_children) = run_with_output(
        &atoms,
        &products,
        &lists,
        &children,
        [],
        [sentinel_product],
        [sentinel_list],
        [],
        TEST_BUDGET,
        1,
    );
    assert_eq!(
        result,
        Err(CanonicalDataError::OutputTooSmall {
            lane: DataOutputLane::Atoms,
            required: 1,
            available: 0,
        })
    );
    assert_eq!(after_atoms, []);
    assert_eq!(after_products, [sentinel_product]);
    assert_eq!(after_lists, [sentinel_list]);
    assert_eq!(after_children, []);
}

#[test]
fn capacity_and_intern_failures_are_non_mutating() {
    let atoms = [
        SemanticAtom { bytes: b"first" },
        SemanticAtom { bytes: b"second" },
    ];
    let lists = [ListSpan::<ProductChildren>::new(0, 0), ListSpan::new(0, 0)];
    let children: [SemanticProductChild; 0] = [];
    let products = [
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(0),
        },
        SemanticProduct {
            head: AtomId::new(1),
            children: ProductListId::new(1),
        },
    ];
    let sentinel_atom = SemanticAtom { bytes: b"sentinel" };
    let sentinel_product = SemanticProduct {
        head: AtomId::new(8),
        children: ProductListId::new(8),
    };
    let sentinel_list = ListSpan::<ProductChildren>::new(8, 8);

    let (result, after_atoms, after_products, after_lists, after_children) = run_with_output(
        &atoms,
        &products,
        &lists,
        &children,
        [sentinel_atom; 2],
        [sentinel_product; 1],
        [sentinel_list; 2],
        [],
        TEST_BUDGET,
        4,
    );
    assert_eq!(
        result,
        Err(CanonicalDataError::OutputTooSmall {
            lane: DataOutputLane::Products,
            required: 2,
            available: 1,
        })
    );
    assert_eq!(after_atoms, [sentinel_atom; 2]);
    assert_eq!(after_products, [sentinel_product; 1]);
    assert_eq!(after_lists, [sentinel_list; 2]);
    assert_eq!(after_children, []);

    let (result, after_atoms, after_products, after_lists, after_children) = run_with_output(
        &atoms,
        &products,
        &lists,
        &children,
        [sentinel_atom; 2],
        [sentinel_product; 2],
        [sentinel_list; 1],
        [],
        TEST_BUDGET,
        4,
    );
    assert_eq!(
        result,
        Err(CanonicalDataError::OutputTooSmall {
            lane: DataOutputLane::Lists,
            required: 2,
            available: 1,
        })
    );
    assert_eq!(after_atoms, [sentinel_atom; 2]);
    assert_eq!(after_products, [sentinel_product; 2]);
    assert_eq!(after_lists, [sentinel_list; 1]);
    assert_eq!(after_children, []);

    let child_atoms = [SemanticAtom { bytes: b"cycle" }];
    let child_products = [SemanticProduct {
        head: AtomId::new(0),
        children: ProductListId::new(0),
    }];
    let child_lists = [ListSpan::<ProductChildren>::new(0, 1)];
    let child_pool = [child(ProductRef::Local(ProductId::new(0)))];
    let (result, after_atoms, after_products, after_lists, after_children) = run_with_output(
        &child_atoms,
        &child_products,
        &child_lists,
        &child_pool,
        [sentinel_atom; 1],
        [sentinel_product; 1],
        [sentinel_list; 1],
        [],
        TEST_BUDGET,
        4,
    );
    assert_eq!(
        result,
        Err(CanonicalDataError::OutputTooSmall {
            lane: DataOutputLane::Children,
            required: 1,
            available: 0,
        })
    );
    assert_eq!(after_atoms, [sentinel_atom; 1]);
    assert_eq!(after_products, [sentinel_product; 1]);
    assert_eq!(after_lists, [sentinel_list; 1]);
    assert_eq!(after_children, []);

    let (result, after_atoms, after_products, after_lists, after_children) = run_with_output(
        &atoms,
        &products,
        &lists,
        &children,
        [sentinel_atom; 2],
        [sentinel_product; 2],
        [sentinel_list; 2],
        [],
        TEST_BUDGET,
        1,
    );
    assert!(matches!(
        result,
        Err(CanonicalDataError::InternTableFull {
            capacity: 1,
            product: _,
        })
    ));
    assert_eq!(after_atoms, [sentinel_atom; 2]);
    assert_eq!(after_products, [sentinel_product; 2]);
    assert_eq!(after_lists, [sentinel_list; 2]);
    assert_eq!(after_children, []);
}

#[test]
fn every_budget_and_intern_admission_failure_preserves_all_caller_lanes() {
    let atoms = [
        SemanticAtom { bytes: b"first" },
        SemanticAtom { bytes: b"second" },
    ];
    let products = [
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(0),
        },
        SemanticProduct {
            head: AtomId::new(1),
            children: ProductListId::new(1),
        },
    ];
    let constructors = [SemanticProductConstructor::PRODUCT; 2];
    let lists = [ListSpan::<ProductChildren>::new(0, 1), ListSpan::new(1, 0)];
    let children = [child(ProductRef::Local(ProductId::new(1)))];
    let expected =
        |resource: DataResource, required: u64, limit: u64| CanonicalDataError::BudgetAdmission {
            resource,
            required,
            limit,
        };

    let cases = [
        (
            DataResourceBudget {
                max_refinement_rounds: 0,
                ..TEST_BUDGET
            },
            expected(DataResource::RefinementRounds, 2, 0),
        ),
        (
            DataResourceBudget {
                max_refinement_rounds: 1,
                ..TEST_BUDGET
            },
            expected(DataResource::RefinementRounds, 2, 1),
        ),
        (
            DataResourceBudget {
                max_sort_comparisons: 0,
                ..TEST_BUDGET
            },
            expected(DataResource::SortComparisons, 20, 0),
        ),
        (
            DataResourceBudget {
                max_hash_evaluations: 0,
                ..TEST_BUDGET
            },
            expected(DataResource::HashEvaluations, 8, 0),
        ),
        (
            DataResourceBudget {
                max_intern_probes: 0,
                ..TEST_BUDGET
            },
            expected(DataResource::InternProbes, 4, 0),
        ),
        (
            DataResourceBudget {
                max_work: 0,
                ..TEST_BUDGET
            },
            expected(DataResource::Work, 65, 0),
        ),
    ];

    for (budget, expected_error) in cases {
        let (result, snapshot) = run_budget_failure(
            DataFacts {
                atoms: &atoms,
                products: &products,
                constructors: &constructors,
                lists: &lists,
                children: &children,
            },
            budget,
            2,
        );
        assert!(
            matches!(result, Some(error) if error == expected_error),
            "result={result:?}, expected={expected_error:?}"
        );
        assert_eq!(snapshot, pristine_snapshot());
    }

    let (result, snapshot) = run_budget_failure(
        DataFacts {
            atoms: &atoms,
            products: &products,
            constructors: &constructors,
            lists: &lists,
            children: &children,
        },
        TEST_BUDGET,
        1,
    );
    assert!(matches!(
        result,
        Some(CanonicalDataError::InternTableFull { product, capacity: 1 }) if product.raw == 0
    ));
    assert_eq!(snapshot, pristine_snapshot());
}

#[test]
fn malformed_list_and_local_edge_fail_with_exact_coordinates() {
    let atoms = [SemanticAtom { bytes: b"leaf" }];
    let sentinel_atom = [SemanticAtom { bytes: b"sentinel" }];
    let sentinel_product = [SemanticProduct {
        head: AtomId::new(8),
        children: ProductListId::new(8),
    }];
    let sentinel_list = [ListSpan::<ProductChildren>::new(8, 8)];

    let products = [SemanticProduct {
        head: AtomId::new(0),
        children: ProductListId::new(1),
    }];
    let lists = [ListSpan::<ProductChildren>::new(0, 0)];
    let children: [SemanticProductChild; 0] = [];
    let (result, after_atoms, after_products, after_lists, after_children) = run_with_output(
        &atoms,
        &products,
        &lists,
        &children,
        sentinel_atom,
        sentinel_product,
        sentinel_list,
        [],
        TEST_BUDGET,
        4,
    );
    assert_eq!(
        result,
        Err(CanonicalDataError::ProductList {
            product: ProductId::new(0),
            target: ProductListId::new(1),
            list_count: 1,
        })
    );
    assert_eq!(after_atoms, sentinel_atom);
    assert_eq!(after_products, sentinel_product);
    assert_eq!(after_lists, sentinel_list);
    assert_eq!(after_children, []);

    let products = [SemanticProduct {
        head: AtomId::new(0),
        children: ProductListId::new(0),
    }];
    let lists = [ListSpan::<ProductChildren>::new(1, 1)];
    let (result, after_atoms, after_products, after_lists, after_children) = run_with_output(
        &atoms,
        &products,
        &lists,
        &children,
        sentinel_atom,
        sentinel_product,
        sentinel_list,
        [],
        TEST_BUDGET,
        4,
    );
    assert_eq!(
        result,
        Err(CanonicalDataError::ListExtent {
            list: ProductListId::new(0),
            span: ListSpan::new(1, 1),
            pool_length: 0,
        })
    );
    assert_eq!(after_atoms, sentinel_atom);
    assert_eq!(after_products, sentinel_product);
    assert_eq!(after_lists, sentinel_list);
    assert_eq!(after_children, []);

    let lists = [ListSpan::<ProductChildren>::new(0, 1)];
    let children = [child(ProductRef::Local(ProductId::new(1)))];
    let (result, after_atoms, after_products, after_lists, after_children) = run_with_output(
        &atoms,
        &products,
        &lists,
        &children,
        sentinel_atom,
        sentinel_product,
        sentinel_list,
        [],
        TEST_BUDGET,
        4,
    );
    assert_eq!(
        result,
        Err(CanonicalDataError::ProductChild {
            product: ProductId::new(0),
            list: ProductListId::new(0),
            child_ordinal: 0,
            target: ProductId::new(1),
            product_count: 1,
        })
    );
    assert_eq!(after_atoms, sentinel_atom);
    assert_eq!(after_products, sentinel_product);
    assert_eq!(after_lists, sentinel_list);
    assert_eq!(after_children, []);
}

#[test]
fn representation_is_compact_and_scratch_is_explicit() {
    assert_eq!(size_of::<SemanticAtom<'static>>(), size_of::<&[u8]>());
    assert_eq!(align_of::<SemanticAtom<'static>>(), align_of::<&[u8]>());
    assert_eq!(size_of::<SemanticProduct>(), size_of::<u32>() * 2);
    assert_eq!(
        size_of::<SemanticProductConstructor>(),
        size_of::<u32>() * 3
    );
    assert_eq!(size_of::<ProductListId>(), size_of::<u32>());
    assert_eq!(size_of::<DataScratchLane>(), 1);
    let _ = DataOutputLane::Atoms;
    let _ = DataResource::Work;
}

#[test]
fn constructor_alignment_and_payload_faults_keep_exact_operands() {
    let mut output = [SemanticProductConstructor::array(99)];
    assert_eq!(
        canonicalize_one_constructor(&[], &[], &mut output),
        Err(CanonicalDataError::ConstructorCount {
            product_count: 1,
            constructor_count: 0,
        })
    );
    assert_eq!(output, [SemanticProductConstructor::array(99)]);

    assert_eq!(
        canonicalize_one_constructor(
            &[SemanticProductConstructor::function(1, 1)],
            &[child(ProductRef::Local(ProductId::new(0)))],
            &mut output,
        ),
        Err(CanonicalDataError::ProductConstructor {
            product: ProductId::new(0),
            fault: ProductConstructorFault::Arity {
                tag: ProductConstructorTag::Function,
                expected: 2,
                actual: 1,
            },
        })
    );
    assert_eq!(output, [SemanticProductConstructor::array(99)]);

    let malformed_generic = SemanticProductConstructor {
        tag: ProductConstructorTag::Generic,
        payload0: 0,
        payload1: 7,
    };
    assert_eq!(
        canonicalize_one_constructor(&[malformed_generic], &[], &mut output),
        Err(CanonicalDataError::ProductConstructor {
            product: ProductId::new(0),
            fault: ProductConstructorFault::ReservedPayload {
                tag: ProductConstructorTag::Generic,
                payload0: 0,
                payload1: 7,
            },
        })
    );
    assert_eq!(output, [SemanticProductConstructor::array(99)]);

    assert_eq!(
        canonicalize_one_constructor(&[SemanticProductConstructor::PRODUCT], &[], &mut [],),
        Err(CanonicalDataError::OutputTooSmall {
            lane: DataOutputLane::Constructors,
            required: 1,
            available: 0,
        })
    );
}

#[test]
fn child_roles_are_retained_and_role_mutations_are_rejected_typed() -> Result<(), FixtureFailure> {
    let atoms = [SemanticAtom { bytes: b"function" }];
    let products = [SemanticProduct {
        head: AtomId::new(0),
        children: ProductListId::new(0),
    }];
    let constructors = [SemanticProductConstructor::function(1, 1)];
    let lists = [ListSpan::<ProductChildren>::new(0, 2)];
    let children = [
        SemanticProductChild {
            target: ProductRef::Local(ProductId::new(0)),
            role: ProductChildRole::FunctionParameter,
        },
        SemanticProductChild {
            target: ProductRef::Local(ProductId::new(0)),
            role: ProductChildRole::FunctionResult,
        },
    ];
    let snapshot =
        canonical_fixture_with_constructors(&atoms, &products, &constructors, &lists, &children)?;
    assert_eq!(snapshot.constructors[0], constructors[0]);
    assert_eq!(snapshot.children[..2], children);

    let malformed = [
        SemanticProductChild {
            target: ProductRef::Local(ProductId::new(0)),
            role: ProductChildRole::FunctionResult,
        },
        children[1],
    ];
    let mut output_constructors = [SemanticProductConstructor::PRODUCT];
    assert_eq!(
        canonicalize_one_constructor(&constructors, &malformed, &mut output_constructors),
        Err(CanonicalDataError::ProductChildRole {
            product: ProductId::new(0),
            list: ProductListId::new(0),
            child_ordinal: 0,
            expected: ProductChildRole::FunctionParameter,
            actual: ProductChildRole::FunctionResult,
        })
    );
    assert_eq!(output_constructors, [SemanticProductConstructor::PRODUCT]);
    Ok(())
}

#[test]
fn constructor_shape_and_array_length_participate_in_hash_consing() -> Result<(), FixtureFailure> {
    let atoms = [SemanticAtom { bytes: b"shape" }];
    let products = [
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(0),
        },
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(1),
        },
    ];
    let lists = [ListSpan::<ProductChildren>::new(0, 0); 2];
    let children: [SemanticProductChild; 0] = [];
    let distinct = canonical_fixture_with_constructor_metrics(
        &atoms,
        &products,
        &[
            SemanticProductConstructor::PRODUCT,
            SemanticProductConstructor::TUPLE,
        ],
        &lists,
        &children,
    )?;
    assert_eq!(distinct.1.canonical_product_count, 2);
    assert_ne!(distinct.0.constructors[0], distinct.0.constructors[1]);

    let one_product = [products[0]];
    let one_list = [lists[0]];
    let one_child = [SemanticProductChild {
        target: ProductRef::Local(ProductId::new(0)),
        role: ProductChildRole::ArrayElement,
    }];
    let length_two = canonical_fixture_with_constructors(
        &atoms,
        &one_product,
        &[SemanticProductConstructor::array(2)],
        &one_list.map(|_| ListSpan::new(0, 1)),
        &one_child,
    )?;
    let length_four = canonical_fixture_with_constructors(
        &atoms,
        &one_product,
        &[SemanticProductConstructor::array(4)],
        &one_list.map(|_| ListSpan::new(0, 1)),
        &one_child,
    )?;
    assert_ne!(length_two, length_four);
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Snapshot {
    atom_bytes: [[u8; 16]; 4],
    atom_lengths: [usize; 4],
    products: [SemanticProduct; 4],
    constructors: [SemanticProductConstructor; 4],
    lists: [ListSpan<ProductChildren>; 4],
    children: [SemanticProductChild; 8],
    atom_count: usize,
    product_count: usize,
    list_count: usize,
    child_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FixtureMetrics {
    canonical_product_count: u32,
}

fn canonical_fixture(
    atoms: &[SemanticAtom<'_>],
    products: &[SemanticProduct],
    lists: &[ListSpan<ProductChildren>],
    children: &[SemanticProductChild],
) -> Result<Snapshot, FixtureFailure> {
    canonical_fixture_with_metrics(atoms, products, lists, children).map(|(snapshot, _)| snapshot)
}

fn canonical_fixture_with_metrics(
    atoms: &[SemanticAtom<'_>],
    products: &[SemanticProduct],
    lists: &[ListSpan<ProductChildren>],
    children: &[SemanticProductChild],
) -> Result<(Snapshot, FixtureMetrics), FixtureFailure> {
    let constructor_storage = [SemanticProductConstructor::PRODUCT; 4];
    let constructors = &constructor_storage[..products.len()];
    canonical_fixture_with_constructor_metrics(atoms, products, constructors, lists, children)
}

fn canonical_fixture_with_constructors(
    atoms: &[SemanticAtom<'_>],
    products: &[SemanticProduct],
    constructors: &[SemanticProductConstructor],
    lists: &[ListSpan<ProductChildren>],
    children: &[SemanticProductChild],
) -> Result<Snapshot, FixtureFailure> {
    canonical_fixture_with_constructor_metrics(atoms, products, constructors, lists, children)
        .map(|(snapshot, _)| snapshot)
}

#[allow(
    clippy::too_many_arguments,
    reason = "The fixture mirrors the five source lanes and the constructor lane explicitly."
)]
fn canonical_fixture_with_constructor_metrics(
    atoms: &[SemanticAtom<'_>],
    products: &[SemanticProduct],
    constructors: &[SemanticProductConstructor],
    lists: &[ListSpan<ProductChildren>],
    children: &[SemanticProductChild],
) -> Result<(Snapshot, FixtureMetrics), FixtureFailure> {
    let mut atom_order = [AtomId::new(0); 4];
    let mut atom_map = [0; 4];
    let mut product_order = [ProductId::new(0); 4];
    let mut product_map = [0; 4];
    let mut colors = [0; 4];
    let mut next_colors = [0; 4];
    let mut hashes = [0; 4];
    let mut next_hashes = [0; 4];
    let mut representatives = [ProductId::new(0); 4];
    let mut intern_slots = [0; 8];
    let mut scratch = scratch(
        &mut atom_order,
        &mut atom_map,
        &mut product_order,
        &mut product_map,
        &mut colors,
        &mut next_colors,
        &mut hashes,
        &mut next_hashes,
        &mut representatives,
        &mut intern_slots,
    );
    let mut output_atoms = [SemanticAtom { bytes: &[] }; 4];
    let mut output_products = [SemanticProduct {
        head: AtomId::new(0),
        children: ProductListId::new(0),
    }; 4];
    let mut output_constructors = [SemanticProductConstructor::PRODUCT; 4];
    let mut output_lists = [ListSpan::<ProductChildren>::new(0, 0); 4];
    let mut output_children = [child(ProductRef::Local(ProductId::new(0))); 8];
    let mut output = DataOutput {
        atoms: &mut output_atoms,
        products: &mut output_products,
        constructors: &mut output_constructors,
        lists: &mut output_lists,
        children: &mut output_children,
    };
    let graph = canonicalize_data_with_budget(
        DataFacts {
            atoms,
            products,
            constructors,
            lists,
            children,
        },
        &mut scratch,
        &mut output,
        TEST_BUDGET,
    )?;
    let metrics = FixtureMetrics {
        canonical_product_count: graph.metrics().canonical_product_count,
    };
    let mut atom_bytes = [[0; 16]; 4];
    let mut atom_lengths = [0; 4];
    for (ordinal, atom) in graph.atoms().iter().enumerate() {
        atom_bytes[ordinal][..atom.bytes.len()].copy_from_slice(atom.bytes);
        atom_lengths[ordinal] = atom.bytes.len();
    }
    let mut canonical_products = [SemanticProduct {
        head: AtomId::new(0),
        children: ProductListId::new(0),
    }; 4];
    canonical_products[..graph.products().len()].copy_from_slice(graph.products());
    let mut canonical_constructors = [SemanticProductConstructor::PRODUCT; 4];
    canonical_constructors[..graph.constructors().len()].copy_from_slice(graph.constructors());
    let mut canonical_lists = [ListSpan::<ProductChildren>::new(0, 0); 4];
    let list_count = usize::try_from(graph.metrics().canonical_list_count).map_err(|source| {
        CanonicalDataError::NativeCount {
            lane: DataCountLane::Lists,
            actual: graph.metrics().canonical_list_count,
            source,
        }
    })?;
    for (ordinal, destination) in canonical_lists.iter_mut().enumerate().take(list_count) {
        let ordinal = u32::try_from(ordinal).map_err(|source| CanonicalDataError::Count {
            lane: DataCountLane::Lists,
            actual: ordinal,
            source,
        })?;
        *destination = graph
            .list_span(ProductListId::new(ordinal))
            .map_err(FixtureFailure::from)?;
    }
    let mut canonical_children = [child(ProductRef::Local(ProductId::new(0))); 8];
    let child_count = usize::try_from(graph.metrics().canonical_child_count).map_err(|source| {
        CanonicalDataError::NativeCount {
            lane: DataCountLane::Children,
            actual: graph.metrics().canonical_child_count,
            source,
        }
    })?;
    canonical_children[..child_count].copy_from_slice(&graph.children()[..child_count]);
    Ok((
        Snapshot {
            atom_bytes,
            atom_lengths,
            products: canonical_products,
            constructors: canonical_constructors,
            lists: canonical_lists,
            children: canonical_children,
            atom_count: graph.atoms().len(),
            product_count: graph.products().len(),
            list_count,
            child_count,
        },
        metrics,
    ))
}

fn one_product_metrics(slots: &mut [u64]) -> Result<DataCanonicalization, CanonicalDataError> {
    let atoms = [SemanticAtom { bytes: b"one" }];
    let products = [SemanticProduct {
        head: AtomId::new(0),
        children: ProductListId::new(0),
    }];
    let constructors = [SemanticProductConstructor::PRODUCT];
    let lists = [ListSpan::<ProductChildren>::new(0, 0)];
    let children: [SemanticProductChild; 0] = [];
    let mut atom_order = [AtomId::new(0)];
    let mut atom_map = [0];
    let mut product_order = [ProductId::new(0)];
    let mut product_map = [0];
    let mut colors = [0];
    let mut next_colors = [0];
    let mut hashes = [0];
    let mut next_hashes = [0];
    let mut representatives = [ProductId::new(0)];
    let mut scratch = scratch(
        &mut atom_order,
        &mut atom_map,
        &mut product_order,
        &mut product_map,
        &mut colors,
        &mut next_colors,
        &mut hashes,
        &mut next_hashes,
        &mut representatives,
        slots,
    );
    let mut output_atoms = [SemanticAtom { bytes: &[] }];
    let mut output_products = [SemanticProduct {
        head: AtomId::new(0),
        children: ProductListId::new(0),
    }];
    let mut output_constructors = [SemanticProductConstructor::PRODUCT];
    let mut output_lists = [ListSpan::<ProductChildren>::new(0, 0)];
    let mut output_children: [SemanticProductChild; 0] = [];
    let mut output = DataOutput {
        atoms: &mut output_atoms,
        products: &mut output_products,
        constructors: &mut output_constructors,
        lists: &mut output_lists,
        children: &mut output_children,
    };
    canonicalize_data_with_budget(
        DataFacts {
            atoms: &atoms,
            products: &products,
            constructors: &constructors,
            lists: &lists,
            children: &children,
        },
        &mut scratch,
        &mut output,
        TEST_BUDGET,
    )
    .map(|graph| graph.metrics())
}

type OutputRun<'bytes, const A: usize, const P: usize, const L: usize, const C: usize> = (
    Result<(), CanonicalDataError>,
    [SemanticAtom<'bytes>; A],
    [SemanticProduct; P],
    [ListSpan<ProductChildren>; L],
    [SemanticProductChild; C],
);

#[allow(
    clippy::too_many_arguments,
    reason = "The helper mirrors the four output lanes and independent admission inputs."
)]
fn run_with_output<'bytes, const A: usize, const P: usize, const L: usize, const C: usize>(
    atoms: &[SemanticAtom<'bytes>],
    products: &[SemanticProduct],
    lists: &[ListSpan<ProductChildren>],
    children: &[SemanticProductChild],
    output_atoms: [SemanticAtom<'bytes>; A],
    output_products: [SemanticProduct; P],
    output_lists: [ListSpan<ProductChildren>; L],
    output_children: [SemanticProductChild; C],
    budget: DataResourceBudget,
    intern_capacity: usize,
) -> OutputRun<'bytes, A, P, L, C> {
    let constructor_storage = [SemanticProductConstructor::PRODUCT; 4];
    let constructors = &constructor_storage[..products.len()];
    let mut atom_order = [AtomId::new(0); 4];
    let mut atom_map = [0; 4];
    let mut product_order = [ProductId::new(0); 4];
    let mut product_map = [0; 4];
    let mut colors = [0; 4];
    let mut next_colors = [0; 4];
    let mut hashes = [0; 4];
    let mut next_hashes = [0; 4];
    let mut representatives = [ProductId::new(0); 4];
    let mut intern_storage = [0; 4];
    let intern_slots = &mut intern_storage[..intern_capacity];
    let mut scratch = scratch(
        &mut atom_order,
        &mut atom_map,
        &mut product_order,
        &mut product_map,
        &mut colors,
        &mut next_colors,
        &mut hashes,
        &mut next_hashes,
        &mut representatives,
        intern_slots,
    );
    let mut output_atoms = output_atoms;
    let mut output_products = output_products;
    let mut output_constructors = [SemanticProductConstructor::PRODUCT; 4];
    let mut output_lists = output_lists;
    let mut output_children = output_children;
    let mut output = DataOutput {
        atoms: &mut output_atoms,
        products: &mut output_products,
        constructors: &mut output_constructors,
        lists: &mut output_lists,
        children: &mut output_children,
    };
    let result = match canonicalize_data_with_budget(
        DataFacts {
            atoms,
            products,
            constructors,
            lists,
            children,
        },
        &mut scratch,
        &mut output,
        budget,
    ) {
        Ok(_) => Ok(()),
        Err(error) => Err(error),
    };
    (
        result,
        output_atoms,
        output_products,
        output_lists,
        output_children,
    )
}
