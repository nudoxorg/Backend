//! Exact entity value encode and decode.

use backend_semantic::ir::{EntityKind, EntityKindCodeError, PrimitiveType, TypeId, TypeNode};

use super::{
    ENTITY_VALUE_BYTES, ENTITY_VALUE_SCHEMA, ExactEntityValue, ExactEntityValueError,
    ExactEntityValueView, ExactEntityValueWire, IndexedType, KIND_MASK, LINK_HIGH_RESERVED,
    LINK_LOW_MASK, LINK_LOW_SHIFT, LinkKinds, PRIMITIVE_TYPE_AUTHORITY, REFERENCE_TYPE_AUTHORITY,
    SEMANTIC_ABSENT_TYPE_AUTHORITY, SEMANTIC_TYPE_AUTHORITY, SemanticTypeFact, TYPE_AUTHORITY_MASK,
    TYPE_AUTHORITY_SHIFT, TYPE_CLASS_MASK, TYPE_CLASS_SHIFT, type_tag_code, type_tag_from_code,
};

impl ExactEntityValue {
    pub(crate) fn from_facts(
        kind: EntityKind,
        semantic_type: IndexedType,
        links: LinkKinds,
    ) -> Self {
        let (type_authority, type_class, type_operand) = match semantic_type {
            IndexedType::Compact(TypeNode::Primitive(primitive)) => {
                (PRIMITIVE_TYPE_AUTHORITY, 0, u32::from(primitive))
            }
            IndexedType::Compact(TypeNode::Reference(target)) => {
                (REFERENCE_TYPE_AUTHORITY, 0, target.raw)
            }
            IndexedType::Semantic(None) => (SEMANTIC_ABSENT_TYPE_AUTHORITY, 0, 0),
            IndexedType::Semantic(Some(fact)) => (
                SEMANTIC_TYPE_AUTHORITY,
                type_tag_code(fact.class) + 1,
                fact.coordinate.raw,
            ),
        };
        let link_bits = links.raw();
        let metadata = u16::from(kind)
            | (type_authority << TYPE_AUTHORITY_SHIFT)
            | (u16::from(type_class) << TYPE_CLASS_SHIFT)
            | ((link_bits & LINK_LOW_MASK) << LINK_LOW_SHIFT);
        let wire = ExactEntityValueWire {
            schema: ENTITY_VALUE_SCHEMA,
            metadata: metadata.to_le_bytes(),
            link_kinds_high: (link_bits >> 4) as u8,
            type_operand: type_operand.to_le_bytes(),
        };
        Self(zerocopy::transmute!(wire))
    }

    /// Reconstructs typed semantic facts without allocation or a persistent decoded cache.
    ///
    /// # Errors
    ///
    /// Returns every exact malformed-width, schema, kind, type-tag, or primitive cause.
    pub fn semantic_view(self) -> Result<ExactEntityValueView, ExactEntityValueError> {
        decode_exact_entity_value(self.0)
    }
}

impl TryFrom<&[u8]> for ExactEntityValue {
    type Error = ExactEntityValueError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let raw: [u8; ENTITY_VALUE_BYTES] =
            bytes
                .try_into()
                .map_err(|source| ExactEntityValueError::Width {
                    actual: bytes.len(),
                    source,
                })?;
        decode_exact_entity_value(raw)?;
        Ok(Self(raw))
    }
}

fn decode_exact_entity_value(
    raw: [u8; ENTITY_VALUE_BYTES],
) -> Result<ExactEntityValueView, ExactEntityValueError> {
    let wire: ExactEntityValueWire = zerocopy::transmute!(raw);
    if wire.schema != ENTITY_VALUE_SCHEMA {
        return Err(ExactEntityValueError::Schema {
            actual: wire.schema,
        });
    }
    let metadata = u16::from_le_bytes(wire.metadata);
    let kind_raw = metadata & KIND_MASK;
    let kind = EntityKind::try_from(kind_raw)
        .map_err(|EntityKindCodeError { actual }| ExactEntityValueError::Kind { actual })?;
    let type_authority = (metadata >> TYPE_AUTHORITY_SHIFT) & TYPE_AUTHORITY_MASK;
    let type_class = ((metadata >> TYPE_CLASS_SHIFT) & TYPE_CLASS_MASK) as u8;
    let links_raw = ((metadata >> LINK_LOW_SHIFT) & LINK_LOW_MASK)
        | (u16::from(wire.link_kinds_high & !LINK_HIGH_RESERVED) << 4);
    if wire.link_kinds_high & LINK_HIGH_RESERVED != 0 {
        return Err(ExactEntityValueError::LinkKinds {
            observed: links_raw | (u16::from(LINK_HIGH_RESERVED) << 4),
        });
    }
    let links = LinkKinds::from_raw(links_raw).ok_or(ExactEntityValueError::LinkKinds {
        observed: links_raw,
    })?;
    let operand = u32::from_le_bytes(wire.type_operand);
    let semantic_type = match type_authority {
        PRIMITIVE_TYPE_AUTHORITY if type_class == 0 => IndexedType::Compact(TypeNode::Primitive(
            PrimitiveType::try_from(operand).map_err(|source| {
                ExactEntityValueError::Primitive {
                    actual: operand,
                    source,
                }
            })?,
        )),
        REFERENCE_TYPE_AUTHORITY if type_class == 0 => {
            IndexedType::Compact(TypeNode::Reference(TypeId::new(operand)))
        }
        PRIMITIVE_TYPE_AUTHORITY | REFERENCE_TYPE_AUTHORITY => {
            return Err(ExactEntityValueError::CompactTypeClass {
                observed: type_class,
            });
        }
        SEMANTIC_ABSENT_TYPE_AUTHORITY if operand != 0 => {
            return Err(ExactEntityValueError::AbsentTypeOperand { observed: operand });
        }
        SEMANTIC_ABSENT_TYPE_AUTHORITY if type_class != 0 => {
            return Err(ExactEntityValueError::AbsentTypeClass {
                observed: type_class,
            });
        }
        SEMANTIC_ABSENT_TYPE_AUTHORITY => IndexedType::Semantic(None),
        SEMANTIC_TYPE_AUTHORITY => IndexedType::Semantic(Some(SemanticTypeFact {
            coordinate: TypeId::new(operand),
            class: type_tag_from_code(type_class.checked_sub(1).ok_or(
                ExactEntityValueError::SemanticTypeClass {
                    observed: type_class,
                },
            )?)
            .ok_or(ExactEntityValueError::SemanticTypeClass {
                observed: type_class,
            })?,
        })),
        _ => {
            return Err(ExactEntityValueError::TypeAuthority {
                actual: type_authority,
            });
        }
    };
    Ok(ExactEntityValueView {
        kind,
        semantic_type,
        links,
    })
}
