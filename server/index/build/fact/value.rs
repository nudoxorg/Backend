//! Defines fact value behavior for `server-index-build`, whose purpose is to project reopened compiler IR into immutable exact and lexical segments.
//! This module owns the fact value invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::mem::{align_of, size_of};

use compiler_ir::TypeId;
use compiler_ir::{
    EntityKind, EntityKindCodeError, LinkKind, PrimitiveType, TypeNode, TypeNodeFault, TypeTag,
};
use thiserror::Error;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// Exact value bytes required to carry one typed entity fact through the generic exact plane.
pub const ENTITY_VALUE_BYTES: usize = size_of::<ExactEntityValueWire>();

const ENTITY_VALUE_SCHEMA: u8 = 2;
const KIND_MASK: u16 = 0x000f;
const TYPE_AUTHORITY_SHIFT: u32 = 4;
const TYPE_AUTHORITY_MASK: u16 = 0x0003;
const TYPE_CLASS_SHIFT: u32 = 6;
const TYPE_CLASS_MASK: u16 = 0x003f;
const LINK_LOW_SHIFT: u32 = 12;
const LINK_LOW_MASK: u16 = 0x000f;
const LINK_HIGH_RESERVED: u8 = 0x80;
const PRIMITIVE_TYPE_AUTHORITY: u16 = 0;
const REFERENCE_TYPE_AUTHORITY: u16 = 1;
const SEMANTIC_ABSENT_TYPE_AUTHORITY: u16 = 2;
const SEMANTIC_TYPE_AUTHORITY: u16 = 3;
const LINK_KIND_COUNT: u32 = 11;
const LINK_KIND_MASK: u16 = (1_u16 << LINK_KIND_COUNT) - 1;

/// A complete semantic type fact whose class belongs to the same image as its coordinate.
///
/// Keeping the pair inseparable prevents discovery code from reporting a type class while losing
/// the route required for structural traversal or canonical rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticTypeFact {
    /// Coordinate into the complete semantic image that owns the indexed entity.
    pub coordinate: TypeId,
    /// Coordinate-free top-level class of the referenced type expression.
    pub class: TypeTag,
}

/// Closed set of semantic relation kinds emitted by one declaration.
///
/// This is an eleven-bit value rather than a collection: the compiler vocabulary is closed,
/// membership is constant-time, and exact rows never allocate or retain duplicate edges.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct LinkKinds(u16);

impl LinkKinds {
    /// Every closed relation kind in canonical wire order.
    pub const ALL: [LinkKind; LINK_KIND_COUNT as usize] = [
        LinkKind::Calls,
        LinkKind::MethodCall,
        LinkKind::TypeReference,
        LinkKind::Reads,
        LinkKind::Writes,
        LinkKind::Imports,
        LinkKind::Implements,
        LinkKind::Overrides,
        LinkKind::Reexports,
        LinkKind::Inherits,
        LinkKind::Documents,
    ];

    /// The empty relation set.
    pub const NONE: Self = Self(0);

    /// Returns whether the declaration emits at least one relation of `kind`.
    #[must_use]
    pub const fn contains(self, kind: LinkKind) -> bool {
        self.0 & link_kind_bit(kind) != 0
    }

    pub(crate) const fn insert(self, kind: LinkKind) -> Self {
        Self(self.0 | link_kind_bit(kind))
    }

    const fn from_raw(raw: u16) -> Option<Self> {
        if raw & !LINK_KIND_MASK == 0 {
            Some(Self(raw))
        } else {
            None
        }
    }

    const fn raw(self) -> u16 {
        self.0
    }
}

const fn link_kind_bit(kind: LinkKind) -> u16 {
    1_u16
        << match kind {
            LinkKind::Calls => 0,
            LinkKind::MethodCall => 1,
            LinkKind::TypeReference => 2,
            LinkKind::Reads => 3,
            LinkKind::Writes => 4,
            LinkKind::Imports => 5,
            LinkKind::Implements => 6,
            LinkKind::Overrides => 7,
            LinkKind::Reexports => 8,
            LinkKind::Inherits => 9,
            LinkKind::Documents => 10,
        }
}

pub(crate) const fn type_tag_code(tag: TypeTag) -> u8 {
    match tag {
        TypeTag::Builtin => 0,
        TypeTag::Literal => 1,
        TypeTag::Nominal => 2,
        TypeTag::External => 3,
        TypeTag::Parameter => 4,
        TypeTag::Applied => 5,
        TypeTag::Tuple => 6,
        TypeTag::Object => 7,
        TypeTag::Function => 8,
        TypeTag::Reference => 9,
        TypeTag::Pointer => 10,
        TypeTag::Slice => 11,
        TypeTag::Array => 12,
        TypeTag::Optional => 13,
        TypeTag::Union => 14,
        TypeTag::Intersection => 15,
        TypeTag::KeyOf => 16,
        TypeTag::TypeOf => 17,
        TypeTag::IndexedAccess => 18,
        TypeTag::Conditional => 19,
        TypeTag::Mapped => 20,
        TypeTag::Infer => 21,
        TypeTag::TemplateLiteral => 22,
        TypeTag::Import => 23,
        TypeTag::Awaited => 24,
        TypeTag::This => 25,
        TypeTag::Unknown => 26,
        TypeTag::ImplTrait => 27,
        TypeTag::DynTrait => 28,
        TypeTag::Wildcard => 29,
        TypeTag::Annotated => 30,
        TypeTag::Inferred => 31,
        TypeTag::QualifiedPath => 32,
        TypeTag::Map => 33,
        TypeTag::Channel => 34,
        TypeTag::CxxReference => 35,
        TypeTag::CPointer => 36,
        TypeTag::CxxMemberPointer => 37,
        TypeTag::CQualified => 38,
        TypeTag::CBlockPointer => 39,
        TypeTag::NativeCharacter => 40,
    }
}

const fn type_tag_from_code(code: u8) -> Option<TypeTag> {
    match code {
        0 => Some(TypeTag::Builtin),
        1 => Some(TypeTag::Literal),
        2 => Some(TypeTag::Nominal),
        3 => Some(TypeTag::External),
        4 => Some(TypeTag::Parameter),
        5 => Some(TypeTag::Applied),
        6 => Some(TypeTag::Tuple),
        7 => Some(TypeTag::Object),
        8 => Some(TypeTag::Function),
        9 => Some(TypeTag::Reference),
        10 => Some(TypeTag::Pointer),
        11 => Some(TypeTag::Slice),
        12 => Some(TypeTag::Array),
        13 => Some(TypeTag::Optional),
        14 => Some(TypeTag::Union),
        15 => Some(TypeTag::Intersection),
        16 => Some(TypeTag::KeyOf),
        17 => Some(TypeTag::TypeOf),
        18 => Some(TypeTag::IndexedAccess),
        19 => Some(TypeTag::Conditional),
        20 => Some(TypeTag::Mapped),
        21 => Some(TypeTag::Infer),
        22 => Some(TypeTag::TemplateLiteral),
        23 => Some(TypeTag::Import),
        24 => Some(TypeTag::Awaited),
        25 => Some(TypeTag::This),
        26 => Some(TypeTag::Unknown),
        27 => Some(TypeTag::ImplTrait),
        28 => Some(TypeTag::DynTrait),
        29 => Some(TypeTag::Wildcard),
        30 => Some(TypeTag::Annotated),
        31 => Some(TypeTag::Inferred),
        32 => Some(TypeTag::QualifiedPath),
        33 => Some(TypeTag::Map),
        34 => Some(TypeTag::Channel),
        35 => Some(TypeTag::CxxReference),
        36 => Some(TypeTag::CPointer),
        37 => Some(TypeTag::CxxMemberPointer),
        38 => Some(TypeTag::CQualified),
        39 => Some(TypeTag::CBlockPointer),
        40 => Some(TypeTag::NativeCharacter),
        _ => None,
    }
}

/// Closed type authority retained by one exact entity row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexedType {
    /// Legacy compact type-node authority.
    Compact(TypeNode),
    /// Full semantic-image coordinate and class, including captured absence.
    Semantic(Option<SemanticTypeFact>),
}

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
    pub semantic_type: IndexedType,
    /// Closed set of outbound graph relation kinds committed by this declaration.
    pub links: LinkKinds,
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
    /// A compact type authority carried a semantic type class.
    #[error("compact exact entity type carried semantic class cell {observed}")]
    CompactTypeClass {
        /// Complete rejected semantic class cell.
        observed: u8,
    },
    /// A present semantic type used an unknown type-class cell.
    #[error("semantic exact entity type class cell {observed} is unknown")]
    SemanticTypeClass {
        /// Complete rejected semantic class cell.
        observed: u8,
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
    /// Captured absent semantic type carried a nonzero coordinate payload.
    #[error("absent semantic type carried nonzero operand {observed}")]
    AbsentTypeOperand {
        /// Complete rejected coordinate payload.
        observed: u32,
    },
    /// Captured absent semantic type carried a nonzero class cell.
    #[error("absent semantic type carried class cell {observed}")]
    AbsentTypeClass {
        /// Complete rejected semantic class cell.
        observed: u8,
    },
    /// The relation bitfield used a reserved bit.
    #[error("exact entity relation kinds {observed:#06x} set a reserved bit")]
    LinkKinds {
        /// Complete rejected relation bitfield.
        observed: u16,
    },
}

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
        _ => unreachable!("two-bit semantic type authority is exhaustive"),
    };
    Ok(ExactEntityValueView {
        kind,
        semantic_type,
        links,
    })
}

/// Declarative, padding-free wire record carried by existing core exact rows.
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
#[repr(C)]
struct ExactEntityValueWire {
    schema: u8,
    metadata: [u8; size_of::<u16>()],
    link_kinds_high: u8,
    type_operand: [u8; size_of::<u32>()],
}

const _: () = assert!(size_of::<ExactEntityValueWire>() == 8);
const _: () = assert!(align_of::<ExactEntityValueWire>() == 1);

#[cfg(test)]
mod tests {
    use super::{
        ENTITY_VALUE_BYTES, ENTITY_VALUE_SCHEMA, ExactEntityValue, ExactEntityValueError,
        ExactEntityValueWire, IndexedType, KIND_MASK, LINK_HIGH_RESERVED, LINK_LOW_SHIFT,
        LinkKinds, PRIMITIVE_TYPE_AUTHORITY, SEMANTIC_ABSENT_TYPE_AUTHORITY,
        SEMANTIC_TYPE_AUTHORITY, SemanticTypeFact, TYPE_AUTHORITY_SHIFT, TYPE_CLASS_SHIFT,
    };
    use compiler_ir::{EntityKind, LinkKind, PrimitiveType, TypeId, TypeNode, TypeTag};
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
        #[error("unknown semantic type class was accepted")]
        TypeClass,
        #[error("unknown exact entity primitive was accepted")]
        Primitive,
        #[error("semantic type and relation facts did not round trip exactly")]
        SemanticFacts,
        #[error("an absent semantic type accepted a nonzero coordinate")]
        AbsentOperand,
        #[error("a reserved relation bit was accepted")]
        LinkKinds,
    }

    #[test]
    fn exact_entity_value_roundtrips_without_a_decoded_cache()
    -> Result<(), ExactEntityValueTestError> {
        let value = ExactEntityValue::from_facts(
            EntityKind::Record,
            IndexedType::Compact(TypeNode::Primitive(PrimitiveType::String)),
            LinkKinds::NONE,
        );
        let decoded = ExactEntityValue::try_from(value.as_ref())
            .map_err(|cause| ExactEntityValueTestError::Decode { cause })?;
        let view = decoded
            .semantic_view()
            .map_err(|cause| ExactEntityValueTestError::Decode { cause })?;
        if decoded != value
            || view.kind != EntityKind::Record
            || view.semantic_type
                != IndexedType::Compact(TypeNode::Primitive(PrimitiveType::String))
            || view.links != LinkKinds::NONE
        {
            return Err(ExactEntityValueTestError::RoundTrip);
        }
        Ok(())
    }

    #[test]
    fn exact_entity_value_retains_semantic_coordinate_class_and_relations()
    -> Result<(), ExactEntityValueTestError> {
        for expected in [
            IndexedType::Semantic(None),
            IndexedType::Semantic(Some(SemanticTypeFact {
                coordinate: TypeId::new(4096),
                class: TypeTag::Conditional,
            })),
        ] {
            let links = LinkKinds::NONE
                .insert(LinkKind::Calls)
                .insert(LinkKind::Documents)
                .insert(LinkKind::Inherits);
            let value = ExactEntityValue::from_facts(EntityKind::Function, expected, links);
            let observed = value
                .semantic_view()
                .map_err(|cause| ExactEntityValueTestError::Decode { cause })?;
            if observed.semantic_type != expected
                || observed.links != links
                || !observed.links.contains(LinkKind::Calls)
                || !observed.links.contains(LinkKind::Documents)
                || !observed.links.contains(LinkKind::Inherits)
                || observed.links.contains(LinkKind::Reads)
            {
                return Err(ExactEntityValueTestError::SemanticFacts);
            }
        }
        let malformed = invalid_wire(
            ENTITY_VALUE_SCHEMA,
            metadata(EntityKind::Function, SEMANTIC_ABSENT_TYPE_AUTHORITY, 0, 0),
            0,
            1,
        );
        if !matches!(
            ExactEntityValue::try_from(malformed.as_bytes()),
            Err(ExactEntityValueError::AbsentTypeOperand { observed: 1 })
        ) {
            return Err(ExactEntityValueTestError::AbsentOperand);
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
                metadata(EntityKind::Function, PRIMITIVE_TYPE_AUTHORITY, 0, 0),
                0,
                0,
            )
            .as_bytes(),
            Rejection::Schema,
        )?;
        rejects(
            invalid_wire(ENTITY_VALUE_SCHEMA, KIND_MASK, 0, 0).as_bytes(),
            Rejection::Kind,
        )?;
        rejects(
            invalid_wire(
                ENTITY_VALUE_SCHEMA,
                metadata(EntityKind::Function, SEMANTIC_TYPE_AUTHORITY, 63, 0),
                0,
                0,
            )
            .as_bytes(),
            Rejection::TypeClass,
        )?;
        rejects(
            invalid_wire(
                ENTITY_VALUE_SCHEMA,
                metadata(EntityKind::Function, PRIMITIVE_TYPE_AUTHORITY, 0, 0),
                0,
                u32::MAX,
            )
            .as_bytes(),
            Rejection::Primitive,
        )?;
        rejects(
            invalid_wire(
                ENTITY_VALUE_SCHEMA,
                metadata(EntityKind::Function, PRIMITIVE_TYPE_AUTHORITY, 0, 0),
                LINK_HIGH_RESERVED,
                0,
            )
            .as_bytes(),
            Rejection::LinkKinds,
        )
    }

    const fn invalid_wire(
        schema: u8,
        metadata: u16,
        link_kinds_high: u8,
        type_operand: u32,
    ) -> ExactEntityValueWire {
        ExactEntityValueWire {
            schema,
            metadata: metadata.to_le_bytes(),
            link_kinds_high,
            type_operand: type_operand.to_le_bytes(),
        }
    }

    fn metadata(kind: EntityKind, authority: u16, class: u8, link_low: u16) -> u16 {
        u16::from(kind)
            | (authority << TYPE_AUTHORITY_SHIFT)
            | ((class as u16) << TYPE_CLASS_SHIFT)
            | (link_low << LINK_LOW_SHIFT)
    }

    #[derive(Clone, Copy)]
    enum Rejection {
        Width,
        Schema,
        Kind,
        TypeClass,
        Primitive,
        LinkKinds,
    }

    fn rejects(bytes: &[u8], expected: Rejection) -> Result<(), ExactEntityValueTestError> {
        match (expected, ExactEntityValue::try_from(bytes)) {
            (Rejection::Width, Err(ExactEntityValueError::Width { .. }))
            | (Rejection::Schema, Err(ExactEntityValueError::Schema { .. }))
            | (Rejection::Kind, Err(ExactEntityValueError::Kind { .. }))
            | (Rejection::TypeClass, Err(ExactEntityValueError::SemanticTypeClass { .. }))
            | (Rejection::Primitive, Err(ExactEntityValueError::Primitive { .. }))
            | (Rejection::LinkKinds, Err(ExactEntityValueError::LinkKinds { .. })) => Ok(()),
            (Rejection::Width, _) => Err(ExactEntityValueTestError::Width),
            (Rejection::Schema, _) => Err(ExactEntityValueTestError::Schema),
            (Rejection::Kind, _) => Err(ExactEntityValueTestError::Kind),
            (Rejection::TypeClass, _) => Err(ExactEntityValueTestError::TypeClass),
            (Rejection::Primitive, _) => Err(ExactEntityValueTestError::Primitive),
            (Rejection::LinkKinds, _) => Err(ExactEntityValueTestError::LinkKinds),
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
