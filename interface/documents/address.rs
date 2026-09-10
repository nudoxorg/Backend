//! Defines address behavior for `interface-documents`, whose purpose is to project semantic images into one presentation-neutral document model every surface renders.
//! This module owns the address invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Walking an entity's parent chain into the one readable address that names it.

use compiler_ir::{EntityId, SemanticReader};
use interface_identity::{
    ContentKey, ExactAddress, MAX_PATH_SEGMENTS, PackageCoordinate, PathSegment, SymbolPath,
};

use crate::{Name, ProjectionError};

/// Deepest parent chain the walker follows before reporting an address it cannot spell.
///
/// One more than the path budget, so a chain exactly at the budget still resolves and a chain that
/// exceeds it is reported rather than silently truncated into a wrong address.
pub const MAX_ADDRESS_WALK: usize = MAX_PATH_SEGMENTS + 1;

/// One entity's root-to-leaf path together with the ancestors it passed through.
///
/// The ancestors come back in root-to-parent order so a projector can spell breadcrumbs without
/// walking the chain a second time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AddressWalk {
    /// The address of the walked entity.
    pub address: ExactAddress,
    /// Ancestors from the package root down to the immediate parent.
    pub ancestors: Box<[EntityId]>,
}

/// Walks one entity's parent chain into its address.
///
/// # Errors
///
/// Reports the exact entity whose row, name atom, or identity was absent, or the entity whose
/// chain is deeper than [`MAX_ADDRESS_WALK`] — never a partially spelled address.
pub(crate) fn walk_address<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    package: &PackageCoordinate,
    entity: EntityId,
) -> Result<AddressWalk, ProjectionError> {
    let row = reader
        .entity(entity)
        .ok_or(ProjectionError::MissingEntity { entity })?;
    let key = ContentKey::new(row.version.identity());
    let mut segments = Vec::new();
    let mut ancestors = Vec::new();
    let mut cursor = Some(entity);
    while let Some(current) = cursor {
        if segments.len() > MAX_ADDRESS_WALK {
            return Err(ProjectionError::AddressDepth { entity });
        }
        let step = reader
            .entity(current)
            .ok_or(ProjectionError::MissingEntity { entity: current })?;
        let name = reader
            .atom(step.name)
            .ok_or(ProjectionError::MissingPool {
                entity: current,
                pool: crate::MissingPool::Name,
            })?;
        let text = Name::displayable(name);
        let segment = PathSegment::new(text.as_str(), Some(step.kind))
            .ok_or(ProjectionError::AddressDepth { entity: current })?;
        segments.push(segment);
        if current != entity {
            ancestors.push(current);
        }
        cursor = step.parent.filter(|parent| *parent != current);
    }
    segments.reverse();
    ancestors.reverse();
    let path = SymbolPath::new(segments).map_err(|_| ProjectionError::AddressDepth { entity })?;
    Ok(AddressWalk {
        address: ExactAddress::mint(package.clone(), path, key),
        ancestors: ancestors.into_boxed_slice(),
    })
}
