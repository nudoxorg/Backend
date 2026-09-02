//! Defines fact value behavior for `server-index-build`, whose purpose is to project reopened compiler IR into immutable exact and lexical segments.
//! This module owns the fact value invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::mem::{align_of, size_of};

use compiler_ir::{EntityKind, PrimitiveType, TypeNode, TypeNodeFault};
use compiler_ir_vocabulary::TypeId;
use thiserror::Error;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// Exact value bytes required to carry one typed entity fact through the generic exact plane.
pub const ENTITY_VALUE_BYTES: usize = size_of::<ExactEntityValueWire>();

const ENTITY_VALUE_SCHEMA: u8 = 1;
const PRIMITIVE_TYPE_TAG: u8 = 0;
const REFERENCE_TYPE_TAG: u8 = 1;

/// A checked fixed-width exact entity value with no redundant semantic cache.
///
/// Construction validates every closed discriminant before retaining the canonical eight-byte
/// representation. [`ExactEntityValue::semantic_view`] recomputes a typed view when a consumer
/// needs semantic fields, so the builder stores only the bytes that existing exact rows use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct ExactEntityValue([u8; ENTITY_VALUE_BYTES]);

/// Immutable typed facts decoded from one canonical exact entity value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactEntityValueView {
    /// Closed compiler declaration kind.
    pub kind: EntityKind,
    /// Closed compiler type fact carried by this exact value.
    pub semantic_type: TypeNode,
}

impl AsRef<[u8]> for ExactEntityValue {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Exact rejection while decoding a persisted exact entity value.
#[derive(Debug, Error)]
pub enum ExactEntityValueError {
    /// The borrowed exact value did not have its fixed canonical width.
    #[error("exact entity value has {actual} bytes, expected {ENTITY_VALUE_BYTES}")]
    Width {
        /// Complete observed value width.
        actual: usize,
        /// Structural exact-array conversion failure.
        #[source]
        source: core::array::TryFromSliceError,
    },
    /// The exact entity value used an unknown wire schema.
    #[error("exact entity value schema {actual} is unknown")]
    Schema {
        /// Complete observed schema byte.
        actual: u8,
    },
    /// The exact entity value used an unknown compiler declaration kind.
    #[error("exact entity value kind {actual} is unknown")]
    Kind {
        /// Complete observed declaration-kind code.
        actual: u16,
    },
    /// The exact entity value used an unknown type-node tag.
    #[error("exact entity value type tag {actual} is unknown")]
    TypeTag {
        /// Complete observed type-node tag.
        actual: u8,
    },
    /// The exact entity value used an unknown primitive type code.
    #[error("exact entity value primitive type {actual} is unknown")]
    Primitive {
        /// Complete observed primitive-type code.
        actual: u32,
        /// Original closed type-node rejection.
        #[source]
        source: TypeNodeFault,
    },
}

impl ExactEntityValue {
    pub(crate) fn from_facts(kind: EntityKind, semantic_type: TypeNode) -> Self {
        let (type_tag, type_operand) = match semantic_type {
            TypeNode::Primitive(primitive) => (PRIMITIVE_TYPE_TAG, u32::from(primitive)),
            TypeNode::Reference(target) => (REFERENCE_TYPE_TAG, target.raw),
        };
        let wire = ExactEntityValueWire {
            schema: ENTITY_VALUE_SCHEMA,
            kind: u16::from(kind).to_le_bytes(),
            type_tag,
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
    let kind_raw = u16::from_le_bytes(wire.kind);
    let kind =
        EntityKind::try_from(kind_raw).map_err(|actual| ExactEntityValueError::Kind { actual })?;
    let operand = u32::from_le_bytes(wire.type_operand);
    let semantic_type = match wire.type_tag {
        PRIMITIVE_TYPE_TAG => {
            TypeNode::Primitive(PrimitiveType::try_from(operand).map_err(|source| {
                ExactEntityValueError::Primitive {
                    actual: operand,
                    source,
                }
            })?)
        }
        REFERENCE_TYPE_TAG => TypeNode::Reference(TypeId::new(operand)),
        actual => return Err(ExactEntityValueError::TypeTag { actual }),
    };
    Ok(ExactEntityValueView {
        kind,
        semantic_type,
    })
}

/// Declarative, padding-free wire record carried by existing core exact rows.
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
#[repr(C)]
struct ExactEntityValueWire {
    schema: u8,
    kind: [u8; size_of::<u16>()],
    type_tag: u8,
    type_operand: [u8; size_of::<u32>()],
}

const _: () = assert!(size_of::<ExactEntityValueWire>() == 8);
const _: () = assert!(align_of::<ExactEntityValueWire>() == 1);

#[cfg(test)]
mod tests {
    use super::{
        ENTITY_VALUE_BYTES, ENTITY_VALUE_SCHEMA, ExactEntityValue, ExactEntityValueError,
        ExactEntityValueWire, PRIMITIVE_TYPE_TAG, REFERENCE_TYPE_TAG,
    };
    use compiler_ir::{EntityKind, PrimitiveType, TypeNode};
    use core::mem::size_of;
    use thiserror::Error;
    use zerocopy::IntoBytes;

    #[derive(Debug, Error)]
    enum ExactEntityValueTestError {
        #[error("valid exact entity value rejected during typed decode")]
        Decode {
            #[source]
            cause: ExactEntityValueError,
        },
        #[error("valid exact entity value changed during typed round trip")]
        RoundTrip,
        #[error("short exact entity value was accepted")]
        Width,
        #[error("unknown exact entity schema was accepted")]
        Schema,
        #[error("unknown exact entity kind was accepted")]
        Kind,
        #[error("unknown exact entity type tag was accepted")]
        TypeTag,
        #[error("unknown exact entity primitive was accepted")]
        Primitive,
    }

    #[test]
    fn exact_entity_value_roundtrips_without_a_decoded_cache()
    -> Result<(), ExactEntityValueTestError> {
        let value = ExactEntityValue::from_facts(
            EntityKind::Record,
            TypeNode::Primitive(PrimitiveType::String),
        );
        let decoded = ExactEntityValue::try_from(value.as_ref())
            .map_err(|cause| ExactEntityValueTestError::Decode { cause })?;
        let view = decoded
            .semantic_view()
            .map_err(|cause| ExactEntityValueTestError::Decode { cause })?;
        if decoded != value
            || view.kind != EntityKind::Record
            || view.semantic_type != TypeNode::Primitive(PrimitiveType::String)
        {
            return Err(ExactEntityValueTestError::RoundTrip);
        }
        Ok(())
    }

    #[test]
    fn exact_entity_value_rejects_every_malformed_field() -> Result<(), ExactEntityValueTestError> {
        let short = [0_u8; ENTITY_VALUE_BYTES - 1];
        rejects(short.as_slice(), Rejection::Width)?;
        rejects(
            invalid_wire(
                ENTITY_VALUE_SCHEMA.wrapping_add(1),
                0,
                PRIMITIVE_TYPE_TAG,
                0,
            )
            .as_bytes(),
            Rejection::Schema,
        )?;
        rejects(
            invalid_wire(ENTITY_VALUE_SCHEMA, u16::MAX, PRIMITIVE_TYPE_TAG, 0).as_bytes(),
            Rejection::Kind,
        )?;
        rejects(
            invalid_wire(
                ENTITY_VALUE_SCHEMA,
                0,
                REFERENCE_TYPE_TAG.wrapping_add(1),
                0,
            )
            .as_bytes(),
            Rejection::TypeTag,
        )?;
        rejects(
            invalid_wire(ENTITY_VALUE_SCHEMA, 0, PRIMITIVE_TYPE_TAG, u32::MAX).as_bytes(),
            Rejection::Primitive,
        )
    }

    const fn invalid_wire(
        schema: u8,
        kind: u16,
        type_tag: u8,
        type_operand: u32,
    ) -> ExactEntityValueWire {
        ExactEntityValueWire {
            schema,
            kind: kind.to_le_bytes(),
            type_tag,
            type_operand: type_operand.to_le_bytes(),
        }
    }

    #[derive(Clone, Copy)]
    enum Rejection {
        Width,
        Schema,
        Kind,
        TypeTag,
        Primitive,
    }

    fn rejects(bytes: &[u8], expected: Rejection) -> Result<(), ExactEntityValueTestError> {
        match (expected, ExactEntityValue::try_from(bytes)) {
            (Rejection::Width, Err(ExactEntityValueError::Width { .. }))
            | (Rejection::Schema, Err(ExactEntityValueError::Schema { .. }))
            | (Rejection::Kind, Err(ExactEntityValueError::Kind { .. }))
            | (Rejection::TypeTag, Err(ExactEntityValueError::TypeTag { .. }))
            | (Rejection::Primitive, Err(ExactEntityValueError::Primitive { .. })) => Ok(()),
            (Rejection::Width, _) => Err(ExactEntityValueTestError::Width),
            (Rejection::Schema, _) => Err(ExactEntityValueTestError::Schema),
            (Rejection::Kind, _) => Err(ExactEntityValueTestError::Kind),
            (Rejection::TypeTag, _) => Err(ExactEntityValueTestError::TypeTag),
            (Rejection::Primitive, _) => Err(ExactEntityValueTestError::Primitive),
        }
    }

    #[cfg(target_pointer_width = "64")]
    #[derive(Debug, Error)]
    enum LayoutTestError {
        #[error("exact entity value size changed: observed {observed}")]
        ExactValue { observed: usize },
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn exact_entity_value_remains_the_eight_byte_wire() -> Result<(), LayoutTestError> {
        if size_of::<ExactEntityValue>() != ENTITY_VALUE_BYTES {
            return Err(LayoutTestError::ExactValue {
                observed: size_of::<ExactEntityValue>(),
            });
        }
        Ok(())
    }
}
