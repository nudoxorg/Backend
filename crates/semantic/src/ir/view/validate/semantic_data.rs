//! Admission of one embedded canonical semantic-data section.
use crate::ir::view::{FragmentError, SemanticDataFault};
use crate::ir::wire::{
    SEMANTIC_CHILD_BYTES, SEMANTIC_CONSTRUCTOR_BYTES, SEMANTIC_EXTERNAL_TAG, SEMANTIC_LIST_BYTES,
    SEMANTIC_LOCAL_TAG, SEMANTIC_PRODUCT_BYTES, SemanticDataLayout, read_u32,
    semantic_data_header_bytes,
};
use crate::ir::{ProductChildRole, SemanticProductConstructor};
use backend_version::{ContentId, HASH_BYTES, IrFragmentDomain};

/// Validates one embedded canonical semantic-data section completely before
/// any borrowed view exists.
///
/// The walk covers the exact wire grammar written by `write_semantic_data`:
/// five dense lane counts, one length-prefixed canonical atom stream, the
/// product and constructor lanes, the pooled-list table, and the fixed-width
/// role-bearing child lane. Every rejection retains the rejected operands.
pub(super) fn validate_semantic_data(
    section: &[u8],
    schema: u16,
    entity_count: u32,
) -> Result<SemanticDataLayout, FragmentError> {
    let header_bytes = semantic_data_header_bytes(schema);
    if section.len() < header_bytes {
        return Err(FragmentError::SemanticData {
            fault: SemanticDataFault::Header {
                required: header_bytes,
                actual: section.len(),
            },
        });
    }

    let atom_count = read_u32(section, 0);
    let product_count = read_u32(section, size_of::<u32>());
    let constructor_count = read_u32(section, size_of::<u32>() * 2);
    let list_count = read_u32(section, size_of::<u32>() * 3);
    let child_count = read_u32(section, size_of::<u32>() * 4);
    let entity_root_count = if schema >= 3 {
        read_u32(section, size_of::<u32>() * 5)
    } else {
        0
    };
    if constructor_count != product_count {
        return Err(FragmentError::SemanticData {
            fault: SemanticDataFault::ConstructorCount {
                product_count,
                constructor_count,
            },
        });
    }

    let mut cursor = header_bytes;
    for ordinal in 0..atom_count {
        let Some(length_bytes) = take(section, &mut cursor, size_of::<u32>()) else {
            return Err(semantic_trailing(section, cursor));
        };
        let length = read_u32(length_bytes, 0);
        let length_index = semantic_index(length, section, cursor, |length, available| {
            SemanticDataFault::AtomLength {
                ordinal,
                length,
                available,
            }
        })?;
        cursor += length_index;
    }

    let products_offset = cursor;
    let constructors_offset = advance_offset(
        products_offset,
        product_count,
        SEMANTIC_PRODUCT_BYTES,
        section,
    )?;
    let lists_offset = advance_offset(
        constructors_offset,
        constructor_count,
        SEMANTIC_CONSTRUCTOR_BYTES,
        section,
    )?;
    let children_offset = advance_offset(lists_offset, list_count, SEMANTIC_LIST_BYTES, section)?;
    let children_end = advance_offset(children_offset, child_count, SEMANTIC_CHILD_BYTES, section)?;
    let declared_end = if schema >= 3 {
        if entity_root_count != entity_count {
            return Err(FragmentError::SemanticData {
                fault: SemanticDataFault::EntityRootCount {
                    expected: entity_count,
                    actual: entity_root_count,
                },
            });
        }
        advance_offset(children_end, entity_root_count, size_of::<u32>(), section)?
    } else {
        children_end
    };
    if declared_end != section.len() {
        return Err(FragmentError::SemanticData {
            fault: SemanticDataFault::Trailing {
                actual: section.len().saturating_sub(declared_end),
            },
        });
    }

    let product_record =
        |product: u32| record_at(section, products_offset, product, SEMANTIC_PRODUCT_BYTES);
    for list in 0..list_count {
        let Some(record) = record_at(section, lists_offset, list, SEMANTIC_LIST_BYTES) else {
            return Err(semantic_trailing(section, lists_offset));
        };
        let start = read_u32(record, 0);
        let length = read_u32(record, size_of::<u32>());
        let Some(end) = start.checked_add(length) else {
            return Err(FragmentError::SemanticData {
                fault: SemanticDataFault::ListExtent {
                    list,
                    start,
                    length,
                    child_count,
                },
            });
        };
        if end > child_count {
            return Err(FragmentError::SemanticData {
                fault: SemanticDataFault::ListExtent {
                    list,
                    start,
                    length,
                    child_count,
                },
            });
        }
    }
    for product in 0..product_count {
        let Some(record) = product_record(product) else {
            return Err(semantic_trailing(section, products_offset));
        };
        let head = read_u32(record, 0);
        if head >= atom_count {
            return Err(FragmentError::SemanticData {
                fault: SemanticDataFault::ProductHead {
                    product,
                    target: head,
                    atom_count,
                },
            });
        }
        let list = read_u32(record, size_of::<u32>());
        if list >= list_count {
            return Err(FragmentError::SemanticData {
                fault: SemanticDataFault::ProductList {
                    product,
                    target: list,
                    list_count,
                },
            });
        }
        let Some(constructor_record) = record_at(
            section,
            constructors_offset,
            product,
            SEMANTIC_CONSTRUCTOR_BYTES,
        ) else {
            return Err(semantic_trailing(section, constructors_offset));
        };
        let tag = read_u32(constructor_record, 0);
        let payload0 = read_u32(constructor_record, size_of::<u32>());
        let payload1 = read_u32(constructor_record, size_of::<u32>() * 2);
        let Some(list_record) = record_at(section, lists_offset, list, SEMANTIC_LIST_BYTES) else {
            return Err(semantic_trailing(section, lists_offset));
        };
        let span_start = read_u32(list_record, 0);
        let span_length = read_u32(list_record, size_of::<u32>());
        let constructor =
            SemanticProductConstructor::try_from_parts(tag, payload0, payload1, span_length)
                .map_err(|fault| FragmentError::SemanticData {
                    fault: SemanticDataFault::Constructor { product, fault },
                })?;
        for position in 0..span_length {
            let child = span_start.saturating_add(position);
            let Some(child_record) =
                record_at(section, children_offset, child, SEMANTIC_CHILD_BYTES)
            else {
                return Err(semantic_trailing(section, children_offset));
            };
            validate_child_role(child_record, child, &constructor, position)?;
        }
    }

    for child in 0..child_count {
        let Some(record) = record_at(section, children_offset, child, SEMANTIC_CHILD_BYTES) else {
            return Err(semantic_trailing(section, children_offset));
        };
        validate_child_target(record, child, product_count)?;
    }
    if schema >= 3 {
        for entity in 0..entity_root_count {
            let root_at = children_end + usize::try_from(entity).unwrap_or(usize::MAX) * 4;
            let root = read_u32(section, root_at);
            if root >= product_count {
                return Err(FragmentError::SemanticData {
                    fault: SemanticDataFault::EntityRoot {
                        entity,
                        target: root,
                        product_count,
                    },
                });
            }
        }
    }
    Ok(SemanticDataLayout {
        atom_count,
        product_count,
        constructor_count,
        list_count,
        child_count,
        atom_bytes_start: header_bytes,
        products_start: products_offset,
        constructors_start: constructors_offset,
        lists_start: lists_offset,
        children_start: children_offset,
        entity_roots_start: (schema >= 3).then_some(children_end),
        entity_root_count,
    })
}

/// Proves one product-reachable child carries the closed role required by its
/// constructor position. Tag, target, and authority classes are proven for
/// every declared child by [`validate_child_target`].
fn validate_child_role(
    record: &[u8],
    child: u32,
    constructor: &SemanticProductConstructor,
    position: u32,
) -> Result<(), FragmentError> {
    let expected = constructor.expected_role(position);
    let actual =
        ProductChildRole::try_from(record[0]).map_err(|error| FragmentError::SemanticData {
            fault: SemanticDataFault::ChildRoleCode {
                child,
                actual: error.actual,
            },
        })?;
    if actual != expected {
        return Err(FragmentError::SemanticData {
            fault: SemanticDataFault::ChildRole {
                child,
                expected,
                actual,
            },
        });
    }
    Ok(())
}

fn validate_child_target(
    record: &[u8],
    child: u32,
    product_count: u32,
) -> Result<(), FragmentError> {
    let tag = record[size_of::<u8>()];
    let target = read_u32(record, size_of::<u8>() * 2);
    let mut authority = [0; HASH_BYTES];
    authority.copy_from_slice(&record[size_of::<u8>() * 2 + size_of::<u32>()..]);
    match tag {
        SEMANTIC_LOCAL_TAG => {
            if target >= product_count {
                return Err(FragmentError::SemanticData {
                    fault: SemanticDataFault::LocalChild {
                        child,
                        target,
                        product_count,
                    },
                });
            }
            if authority != [0; HASH_BYTES] {
                return Err(FragmentError::SemanticData {
                    fault: SemanticDataFault::LocalReserved {
                        child,
                        actual: authority,
                    },
                });
            }
        }
        SEMANTIC_EXTERNAL_TAG => {
            if let Err(error) = ContentId::<IrFragmentDomain>::try_from(authority) {
                let (expected, observed) = match error {
                    backend_version::ContentIdDecodeError::Domain {
                        expected, observed, ..
                    } => (u8::from(expected), observed),
                    backend_version::ContentIdDecodeError::Width { .. } => (0, 0),
                };
                return Err(FragmentError::SemanticData {
                    fault: SemanticDataFault::ExternalAuthority {
                        child,
                        expected,
                        observed,
                        raw: authority,
                    },
                });
            }
        }
        actual => {
            return Err(FragmentError::SemanticData {
                fault: SemanticDataFault::ChildTag { child, actual },
            });
        }
    }
    Ok(())
}

fn record_at(section: &[u8], base: usize, ordinal: u32, width: usize) -> Option<&[u8]> {
    let index = usize::try_from(ordinal).ok()?;
    let start = base.checked_add(index.checked_mul(width)?)?;
    section.get(start..start.checked_add(width)?)
}

fn take<'section>(
    section: &'section [u8],
    cursor: &mut usize,
    width: usize,
) -> Option<&'section [u8]> {
    let end = cursor.checked_add(width)?;
    let record = section.get(*cursor..end)?;
    *cursor = end;
    Some(record)
}

fn advance_offset(
    base: usize,
    count: u32,
    width: usize,
    section: &[u8],
) -> Result<usize, FragmentError> {
    let index = usize::try_from(count).map_err(|_| FragmentError::SemanticData {
        fault: SemanticDataFault::Trailing {
            actual: section.len(),
        },
    })?;
    let advanced = index
        .checked_mul(width)
        .and_then(|length| base.checked_add(length))
        .ok_or(FragmentError::SemanticData {
            fault: SemanticDataFault::Trailing {
                actual: section.len(),
            },
        })?;
    if advanced > section.len() {
        return Err(FragmentError::SemanticData {
            fault: SemanticDataFault::Trailing {
                actual: section.len().saturating_sub(base),
            },
        });
    }
    Ok(advanced)
}

fn semantic_index(
    length: u32,
    section: &[u8],
    cursor: usize,
    fault: impl Fn(u32, usize) -> SemanticDataFault,
) -> Result<usize, FragmentError> {
    let available = section.len().saturating_sub(cursor);
    let index = usize::try_from(length).map_err(|_| FragmentError::SemanticData {
        fault: fault(length, available),
    })?;
    if index > available {
        return Err(FragmentError::SemanticData {
            fault: fault(length, available),
        });
    }
    Ok(index)
}

fn semantic_trailing(section: &[u8], cursor: usize) -> FragmentError {
    FragmentError::SemanticData {
        fault: SemanticDataFault::Trailing {
            actual: section.len().saturating_sub(cursor),
        },
    }
}
