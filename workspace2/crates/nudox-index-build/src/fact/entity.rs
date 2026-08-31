use core::ops::Deref;

use super::{ExactEntityKey, ExactEntityValue};
use nudox_id::{ContentId, IndexExactSegmentDomain};
use nudox_ir_format::{EntityKind, TypeNode};
use nudox_ir_vocab::EntityId;

/// A nonforgeable semantic entity fact selected from one reopened compiler fragment.
///
/// The builder canonicalizes declaration order, but preserves [`TypeNode::Reference`]
/// coordinates. Those coordinates are compiler IR semantics: the compact fragment validator,
/// content identity, and publication binding all retain the type-node lane. A compiler that makes
/// type-table ordering incidental must first emit a structural type identity in its IR contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntityFact<'bytes> {
    view: EntityFactView<'bytes>,
}

/// Public immutable facts of one canonical entity index record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntityFactView<'bytes> {
    /// Canonical declaration ordinal used as the lexical document identity.
    pub entity: EntityId,
    /// Typed canonical exact-plane key; unlike a name, this is unique for every entity.
    pub exact_key: ExactEntityKey,
    /// Checked, fixed-width exact value carrying this entity's kind and type facts.
    pub exact_value: ExactEntityValue,
    /// Validated entity-name atom bytes borrowed from the reopened fragment.
    pub name: &'bytes [u8],
}

impl<'bytes> Deref for EntityFact<'bytes> {
    type Target = EntityFactView<'bytes>;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl<'bytes> EntityFact<'bytes> {
    pub(crate) fn new(
        fragment_namespace: ContentId<IndexExactSegmentDomain>,
        entity: EntityId,
        name: &'bytes [u8],
        kind: EntityKind,
        semantic_type: TypeNode,
    ) -> Self {
        Self {
            view: EntityFactView {
                entity,
                exact_key: ExactEntityKey::new(fragment_namespace, entity),
                exact_value: ExactEntityValue::from_facts(kind, semantic_type),
                name,
            },
        }
    }
}

/// Opaque caller scratch for one sortable declaration before it becomes a semantic fact.
///
/// This is intentionally not a proof and has no public constructor. Supply it only through
/// `MaybeUninit` slots in [`crate::IndexBuildScratch`].
#[derive(Clone, Copy)]
pub struct EntityProjection<'bytes> {
    pub(crate) name: &'bytes [u8],
    pub(crate) kind: EntityKind,
    pub(crate) semantic_type: TypeNode,
}

#[cfg(test)]
mod tests {
    use super::EntityFact;
    use crate::fact::{EXACT_ENTITY_KEY_BYTES, ExactEntityKey, ExactEntityValue};
    use core::mem::size_of;
    use thiserror::Error;

    #[cfg(target_pointer_width = "64")]
    #[derive(Debug, Error)]
    enum LayoutTestError {
        #[error("exact entity key size changed: observed {observed}")]
        ExactKey { observed: usize },
        #[error("exact entity value size changed: observed {observed}")]
        ExactValue { observed: usize },
        #[error("entity fact size changed: observed {observed}")]
        EntityFact { observed: usize },
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn public_entity_facts_remain_compact_on_64_bit_targets() -> Result<(), LayoutTestError> {
        if size_of::<ExactEntityKey>() != EXACT_ENTITY_KEY_BYTES {
            return Err(LayoutTestError::ExactKey {
                observed: size_of::<ExactEntityKey>(),
            });
        }
        if size_of::<ExactEntityValue>() != 8 {
            return Err(LayoutTestError::ExactValue {
                observed: size_of::<ExactEntityValue>(),
            });
        }
        if size_of::<EntityFact<'_>>() != 72 {
            return Err(LayoutTestError::EntityFact {
                observed: size_of::<EntityFact<'_>>(),
            });
        }
        Ok(())
    }
}
