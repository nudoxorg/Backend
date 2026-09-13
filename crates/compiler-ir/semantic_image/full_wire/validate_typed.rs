//! Typed-pool structural validation for a full semantic image.

use super::super::{
    fault::{FullSemanticImageFault, FullSemanticImageField},
    typed::role_from_wire,
    wire::{
        FullDirectoryKind, FullImageLayout, TYPED_EDGE_ROW_BYTES, TYPED_NODE_ROW_BYTES, get_u32,
    },
};
use super::TypedLayout;

pub(super) fn validate_typed(
    bytes: &[u8],
    layout: FullImageLayout,
) -> Result<TypedLayout, FullSemanticImageFault> {
    let nodes = layout.entry(FullDirectoryKind::TypedNodes);
    let edges = layout.entry(FullDirectoryKind::TypedEdges);
    let mut starts = [0_u32; 8];
    let mut counts = [0_u32; 8];
    let mut expected_domain = 0_u8;
    let mut expected_coordinate = 0_u32;
    let mut expected_edge = 0_u32;
    for row in 0..nodes.count {
        let offset = nodes.offset
            + usize::try_from(row).map_err(|_| FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::TypedNodes,
            })? * TYPED_NODE_ROW_BYTES;
        let domain = bytes[offset];
        if domain > 7 {
            return Err(FullSemanticImageFault::TypedDomain { node: row, domain });
        }
        for value in &bytes[offset + 1..offset + 4] {
            if *value != 0 {
                return Err(FullSemanticImageFault::Reserved {
                    field: FullSemanticImageField::TypedNodes,
                    row,
                    observed: *value,
                });
            }
        }
        while expected_domain < domain {
            expected_domain = expected_domain
                .checked_add(1)
                .ok_or(FullSemanticImageFault::TypedDomain { node: row, domain })?;
            expected_coordinate = 0;
            starts[usize::from(expected_domain)] = row;
        }
        let coordinate = get_u32(bytes, offset + 4, FullSemanticImageField::TypedNodes)?;
        if domain != expected_domain || coordinate != expected_coordinate {
            return Err(FullSemanticImageFault::CanonicalOrder {
                field: FullSemanticImageField::TypedNodes,
                previous: row
                    .checked_sub(1)
                    .ok_or(FullSemanticImageFault::TypedDomain { node: row, domain })?,
                row,
            });
        }
        let edge_start = get_u32(bytes, offset + 8, FullSemanticImageField::TypedNodes)?;
        let edge_count = get_u32(bytes, offset + 12, FullSemanticImageField::TypedNodes)?;
        if edge_start != expected_edge
            || match edge_start.checked_add(edge_count) {
                Some(end) => end > edges.count,
                None => true,
            }
        {
            return Err(FullSemanticImageFault::Reference {
                field: FullSemanticImageField::TypedEdges,
                row,
                expected: edges.count,
                observed: edge_start,
            });
        }
        expected_edge =
            edge_start
                .checked_add(edge_count)
                .ok_or(FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::TypedEdges,
                })?;
        counts[usize::from(domain)] = counts[usize::from(domain)].checked_add(1).ok_or(
            FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::TypedNodes,
            },
        )?;
        expected_coordinate =
            expected_coordinate
                .checked_add(1)
                .ok_or(FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::TypedNodes,
                })?;
    }
    if expected_edge != edges.count {
        return Err(FullSemanticImageFault::Reference {
            field: FullSemanticImageField::TypedEdges,
            row: nodes.count,
            expected: edges.count,
            observed: expected_edge,
        });
    }
    let typed = TypedLayout { starts, counts };
    for node in 0..nodes.count {
        validate_typed_edges(bytes, layout, typed, node)?;
    }
    Ok(typed)
}

fn validate_typed_edges(
    bytes: &[u8],
    layout: FullImageLayout,
    typed: TypedLayout,
    node: u32,
) -> Result<(), FullSemanticImageFault> {
    let nodes = layout.entry(FullDirectoryKind::TypedNodes);
    let node_offset = nodes.offset
        + usize::try_from(node).map_err(|_| FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::TypedNodes,
        })? * TYPED_NODE_ROW_BYTES;
    let start = get_u32(bytes, node_offset + 8, FullSemanticImageField::TypedNodes)?;
    let count = get_u32(bytes, node_offset + 12, FullSemanticImageField::TypedNodes)?;
    let edges = layout.entry(FullDirectoryKind::TypedEdges);
    for ordinal in 0..count {
        let edge = start
            .checked_add(ordinal)
            .ok_or(FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::TypedEdges,
            })?;
        let offset = edges.offset
            + usize::try_from(edge).map_err(|_| FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::TypedEdges,
            })? * TYPED_EDGE_ROW_BYTES;
        let role = bytes[offset];
        let role_index = get_u32(bytes, offset + 4, FullSemanticImageField::TypedEdges)?;
        if role_from_wire(role, role_index).is_none() {
            return Err(FullSemanticImageFault::TypedRole { node, edge, role });
        }
        let target_tag = bytes[offset + 1];
        let domain = bytes[offset + 2];
        if bytes[offset + 3] != 0
            || bytes[offset + 16] != 0
            || bytes[offset + 17] != 0
            || bytes[offset + 18] != 0
            || bytes[offset + 19] != 0
        {
            return Err(FullSemanticImageFault::Reserved {
                field: FullSemanticImageField::TypedEdges,
                row: edge,
                observed: 1,
            });
        }
        let low = get_u32(bytes, offset + 8, FullSemanticImageField::TypedEdges)?;
        let high = get_u32(bytes, offset + 12, FullSemanticImageField::TypedEdges)?;
        match target_tag {
            0 => {
                let count = typed
                    .count(domain)
                    .ok_or(FullSemanticImageFault::TypedDomain { node, domain })?;
                if low >= count || high != 0 {
                    return Err(FullSemanticImageFault::Reference {
                        field: FullSemanticImageField::TypedEdges,
                        row: edge,
                        expected: count,
                        observed: low,
                    });
                }
            }
            1 => terminal_edge(
                domain,
                high,
                low,
                layout.entry(FullDirectoryKind::Atoms).count,
                edge,
            )?,
            2 => terminal_edge(
                domain,
                high,
                low,
                layout.entry(FullDirectoryKind::Entities).count,
                edge,
            )?,
            3 => terminal_edge(
                domain,
                high,
                low,
                layout.entry(FullDirectoryKind::Externals).count,
                edge,
            )?,
            4 if domain == 0 && high == 0 => {}
            4 => {
                return Err(FullSemanticImageFault::TypedTarget {
                    node,
                    edge,
                    tag: target_tag,
                });
            }
            _ => {
                return Err(FullSemanticImageFault::TypedTarget {
                    node,
                    edge,
                    tag: target_tag,
                });
            }
        }
    }
    Ok(())
}

fn terminal_edge(
    domain: u8,
    high: u32,
    low: u32,
    count: u32,
    edge: u32,
) -> Result<(), FullSemanticImageFault> {
    if domain != 0 || high != 0 || low >= count {
        return Err(FullSemanticImageFault::Reference {
            field: FullSemanticImageField::TypedEdges,
            row: edge,
            expected: count,
            observed: low,
        });
    }
    Ok(())
}
