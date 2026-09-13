//! Input and output geometry validation.

use super::{
    error::{CanonicalDataError, DataCountLane, DataOutputLane, count},
    types::{DataFacts, DataScratch, lane_index},
};
use crate::{ListSpan, ProductId, ProductRef, SemanticProductConstructor};

pub(super) fn validate_facts(
    facts: DataFacts<'_, '_>,
    atom_count: u32,
    product_count: u32,
    list_count: u32,
) -> Result<(), CanonicalDataError> {
    let constructor_count = count(DataCountLane::Constructors, facts.constructors.len())?;
    if constructor_count != product_count {
        return Err(CanonicalDataError::ConstructorCount {
            product_count,
            constructor_count,
        });
    }
    for (ordinal, product) in facts.products.iter().copied().enumerate() {
        let ordinal =
            ProductId::new(
                u32::try_from(ordinal).map_err(|source| CanonicalDataError::Count {
                    lane: DataCountLane::Products,
                    actual: ordinal,
                    source,
                })?,
            );
        if product.head.raw >= atom_count {
            return Err(CanonicalDataError::ProductHead {
                product: ordinal,
                target: product.head,
                atom_count,
            });
        }
        if product.children.raw >= list_count {
            return Err(CanonicalDataError::ProductList {
                product: ordinal,
                target: product.children,
                list_count,
            });
        }
        let Some(span) = facts.lists.get(lane_index(product.children.raw)).copied() else {
            return Err(CanonicalDataError::ProductList {
                product: ordinal,
                target: product.children,
                list_count,
            });
        };
        facts.constructors[lane_index(ordinal.raw)]
            .validate(span.length)
            .map_err(|fault| CanonicalDataError::ProductConstructor {
                product: ordinal,
                fault,
            })?;
        let start =
            usize::try_from(span.start).map_err(|source| CanonicalDataError::NativeExtent {
                list: product.children,
                span,
                pool_length: facts.children.len(),
                source,
            })?;
        let length =
            usize::try_from(span.length).map_err(|source| CanonicalDataError::NativeExtent {
                list: product.children,
                span,
                pool_length: facts.children.len(),
                source,
            })?;
        let Some(end) = start.checked_add(length) else {
            return Err(CanonicalDataError::ListExtent {
                list: product.children,
                span,
                pool_length: facts.children.len(),
            });
        };
        if end > facts.children.len() {
            return Err(CanonicalDataError::ListExtent {
                list: product.children,
                span,
                pool_length: facts.children.len(),
            });
        }
        validate_child_roles(
            ordinal,
            product.children,
            facts.constructors[lane_index(ordinal.raw)],
            &facts.children[start..end],
            span,
            facts.children.len(),
        )?;
        for (child_ordinal, child) in facts.children[start..end].iter().copied().enumerate() {
            let ProductRef::Local(target) = child.target else {
                continue;
            };
            if target.raw >= product_count {
                return Err(CanonicalDataError::ProductChild {
                    product: ordinal,
                    list: product.children,
                    child_ordinal,
                    target,
                    product_count,
                });
            }
        }
    }
    Ok(())
}

pub(super) fn validate_output_layout<'output, 'facts, 'bytes>(
    facts: DataFacts<'facts, 'bytes>,
    scratch: &DataScratch<'output>,
    canonical_atom_count: u32,
    canonical_product_count: u32,
    canonical_list_count: u32,
    canonical_child_count: u32,
) -> Result<(), CanonicalDataError> {
    let mut observed_atoms = 0_u32;
    let mut previous_atom: Option<&[u8]> = None;
    for source in scratch.atom_order[..facts.atoms.len()].iter().copied() {
        let bytes = facts.atoms[lane_index(source.raw)].bytes;
        if previous_atom != Some(bytes) {
            observed_atoms = observed_atoms.checked_add(1).ok_or(
                CanonicalDataError::CanonicalCountOverflow {
                    lane: DataCountLane::Atoms,
                },
            )?;
        }
        previous_atom = Some(bytes);
    }
    if observed_atoms != canonical_atom_count {
        return Err(CanonicalDataError::CanonicalCountMismatch {
            lane: DataCountLane::Atoms,
            expected: canonical_atom_count,
            actual: observed_atoms,
        });
    }
    if canonical_list_count != canonical_product_count {
        return Err(CanonicalDataError::CanonicalCountMismatch {
            lane: DataCountLane::Lists,
            expected: canonical_product_count,
            actual: canonical_list_count,
        });
    }

    let product_count = usize::try_from(canonical_product_count).map_err(|source| {
        CanonicalDataError::NativeCount {
            lane: DataCountLane::Products,
            actual: canonical_product_count,
            source,
        }
    })?;
    let mut child_count = 0_usize;
    for source in scratch.product_representatives[..product_count]
        .iter()
        .copied()
    {
        let product = facts.products[lane_index(source.raw)];
        let span = facts.lists[lane_index(product.children.raw)];
        let start =
            usize::try_from(span.start).map_err(|source| CanonicalDataError::NativeExtent {
                list: product.children,
                span,
                pool_length: facts.children.len(),
                source,
            })?;
        let length =
            usize::try_from(span.length).map_err(|source| CanonicalDataError::NativeExtent {
                list: product.children,
                span,
                pool_length: facts.children.len(),
                source,
            })?;
        let Some(end) = start.checked_add(length) else {
            return Err(CanonicalDataError::ListExtent {
                list: product.children,
                span,
                pool_length: facts.children.len(),
            });
        };
        let Some(children) = facts.children.get(start..end) else {
            return Err(CanonicalDataError::ListExtent {
                list: product.children,
                span,
                pool_length: facts.children.len(),
            });
        };
        child_count =
            child_count
                .checked_add(children.len())
                .ok_or(CanonicalDataError::OutputLength {
                    lane: DataOutputLane::Children,
                    actual: usize::MAX,
                })?;
        for (child_ordinal, child) in children.iter().copied().enumerate() {
            let ProductRef::Local(target) = child.target else {
                continue;
            };
            if lane_index(target.raw) >= facts.products.len()
                || scratch
                    .product_to_canonical
                    .get(lane_index(target.raw))
                    .is_none()
            {
                return Err(CanonicalDataError::ProductChild {
                    product: source,
                    list: product.children,
                    child_ordinal,
                    target,
                    product_count: count(DataCountLane::Products, facts.products.len())?,
                });
            }
        }
    }
    let expected_children = usize::try_from(canonical_child_count).map_err(|source| {
        CanonicalDataError::NativeCount {
            lane: DataCountLane::Children,
            actual: canonical_child_count,
            source,
        }
    })?;
    if child_count != expected_children {
        let actual = u32::try_from(child_count).map_err(|source| CanonicalDataError::Count {
            lane: DataCountLane::Children,
            actual: child_count,
            source,
        })?;
        return Err(CanonicalDataError::CanonicalCountMismatch {
            lane: DataCountLane::Children,
            expected: canonical_child_count,
            actual,
        });
    }
    Ok(())
}

fn validate_child_roles(
    product: ProductId,
    list: crate::ProductListId,
    constructor: SemanticProductConstructor,
    children: &[crate::SemanticProductChild],
    span: ListSpan<crate::ProductChildren>,
    pool_length: usize,
) -> Result<(), CanonicalDataError> {
    for (child_ordinal, child) in children.iter().copied().enumerate() {
        let position =
            u32::try_from(child_ordinal).map_err(|source| CanonicalDataError::NativeExtent {
                list,
                span,
                pool_length,
                source,
            })?;
        let expected = constructor.expected_role(position);
        if child.role != expected {
            return Err(CanonicalDataError::ProductChildRole {
                product,
                list,
                child_ordinal,
                expected,
                actual: child.role,
            });
        }
    }
    Ok(())
}
