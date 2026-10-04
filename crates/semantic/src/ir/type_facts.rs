//! The type-fact section is a deterministic, borrowed wire plane.
//!
//! Optional text uses a one-byte presence flag followed by a little-endian
//! length and bytes.  Nominal targets use tag 0 for none, tag 1 plus a local
//! ordinal, and tag 2 plus a typed fragment identity and ordinal.  All local
//! type coordinates point strictly backward: this is the DAG proof, so no
//! cycle detector is needed. A nominal target may additionally equal its own
//! row ordinal — a declaration naming its own declared type, the terminal
//! case every recursive nominal type closes on. No other forward coordinate
//! is expressible.

use crate::ir_vocabulary::{
    DeclarationFamilyId, DeclarationIdentity, EntityId, ExternalEntityRef, ListSpan, NominalRef,
    SemanticTypeChild, SemanticTypeFault, SemanticTypeRecord, SemanticTypeTag, StableRef,
    TypeChildTarget, TypeId, VariantFingerprint,
};
use backend_version::{ContentId, ContentIdDecodeError, HASH_BYTES, IrFragmentDomain};
use thiserror::Error;

const PRESENCE_NONE: u8 = 0;
const PRESENCE_SOME: u8 = 1;
const NOMINAL_NONE: u8 = 0;
const NOMINAL_LOCAL: u8 = 1;
const NOMINAL_EXTERNAL: u8 = 2;
/// A declaration-identified external nominal: fragment identity plus exact
/// composite declaration identity. Introduced at fragment schema 7; schema-6
/// content never contains it because it is a new tag value, not a renumber.
const NOMINAL_STABLE: u8 = 3;
/// Compact width of one declaration family or variant cell.
const COMPACT_DECLARATION_BYTES: usize = 16;
const CHILD_LOCAL: u8 = 0;
const CHILD_EXTERNAL: u8 = 1;
const CHILD_TEXT: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// One declared or computed type record paired with the entity that owns it.
pub struct TypeFactInput<'bytes> {
    /// Entity-table coordinate owning this semantic type record.
    pub owner: EntityId,
    /// Tag, payload, nominal reference, and child span retained for the record.
    pub record: SemanticTypeRecord<'bytes>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Borrowed inputs for both type-fact segments and their shared child table.
pub struct TypeFactLane<'bytes> {
    /// Schema-1-compatible declared records, in their assigned type order.
    pub inputs: &'bytes [TypeFactInput<'bytes>],
    /// Schema-2 computed rows, encoded after all declared rows.
    pub computed: &'bytes [TypeFactInput<'bytes>],
    /// Child values addressed by every record's `children` span.
    pub children: &'bytes [SemanticTypeChild<'bytes>],
}

/// Exact geometry of the two dense type-fact segments.
///
/// The declared and computed segments deliberately share one type-coordinate
/// space.  Extension pools, rich discovery, and durable reopen must therefore
/// use their sum — never the unrelated entity count — when admitting a type
/// reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeFactCounts {
    /// Number of declared records at the beginning of the type-coordinate space.
    pub declared: u32,
    /// Number of schema-2 computed records following the declared prefix.
    pub computed: u32,
}

impl TypeFactCounts {
    /// Creates segment counts; use [`Self::total`] to reject width overflow.
    pub const fn new(declared: u32, computed: u32) -> Self {
        Self { declared, computed }
    }

    /// Exact width of the common local type-coordinate space.
    pub fn total(self) -> Result<u32, TypeFactFault> {
        self.declared
            .checked_add(self.computed)
            .ok_or(TypeFactFault::CountOverflow {
                declared: self.declared,
                computed: self.computed,
            })
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
/// Admission, decoding, or coordinate error in the type-fact plane.
pub enum TypeFactFault {
    /// The declared and computed counts cannot share one `u32` coordinate space.
    #[error("type-fact segments {declared}+{computed} overflow the type-coordinate width")]
    CountOverflow {
        /// Declared segment count.
        declared: u32,
        /// Computed segment count.
        computed: u32,
    },
    /// A type record names an entity outside the image's entity table.
    #[error("type fact {ordinal} names entity {owner:?} outside {entity_count}")]
    Owner {
        /// Coordinate of the offending type record in the combined type plane.
        ordinal: u32,
        /// Entity-table coordinate encoded as the owner.
        owner: EntityId,
        /// Number of entity rows available for this image.
        entity_count: u32,
    },
    /// The record's tag-specific payload or child layout violates the semantic type grammar.
    #[error("type fact {ordinal} is invalid: {fault:?}")]
    Record {
        /// Coordinate of the offending type record.
        ordinal: u32,
        /// Exact grammar validation failure for that record.
        fault: SemanticTypeFault,
    },
    /// A record's child span does not fit in the shared child pool.
    #[error("type fact {ordinal} child span {start}+{length} exceeds {child_count}")]
    ChildSpan {
        /// Coordinate of the record containing the invalid span.
        ordinal: u32,
        /// Child-pool start offset.
        start: u32,
        /// Number of child rows requested by the span.
        length: u32,
        /// Number of child rows in the shared pool.
        child_count: u32,
    },
    /// A schema-1 local child type target is not strictly before its source row.
    #[error("type fact {ordinal} child {position} target {target} is not strictly backward")]
    ForwardReference {
        /// Source type-record coordinate.
        ordinal: u32,
        /// Child position within the record's span.
        position: u32,
        /// Referenced local type-record coordinate.
        target: u32,
    },
    /// A declared record may refer only to the declared prefix in schema 2.
    #[error(
        "type fact {ordinal} child {position} targets computed row {target} from declared segment"
    )]
    ComputedTargetFromDeclared {
        /// Declared source-record coordinate.
        ordinal: u32,
        /// Child position within the source record.
        position: u32,
        /// Computed target that the declared row is not allowed to reference.
        target: u32,
    },
    /// A computed child target must precede the computed record that refers to it.
    #[error("computed type fact {ordinal} child {position} targets later computed row {target}")]
    ComputedForwardReference {
        /// Computed source-record coordinate in the combined type space.
        ordinal: u32,
        /// Child position within the source record.
        position: u32,
        /// Later computed target that would violate the acyclic ordering.
        target: u32,
    },
    /// A local child target is outside both declared and computed records.
    #[error(
        "type fact {ordinal} child {position} target {target} is outside {record_count} records"
    )]
    ChildTargetOutOfRange {
        /// Source type-record coordinate.
        ordinal: u32,
        /// Child position within the source record.
        position: u32,
        /// Referenced type coordinate.
        target: u32,
        /// Total number of records in the schema's type-coordinate space.
        record_count: u32,
    },
    /// A schema-1 nominal reference points forward to a non-self row.
    #[error("type fact {ordinal} nominal target {target} points forward at another row")]
    NominalForward {
        /// Source type-record coordinate.
        ordinal: u32,
        /// Forward local nominal target.
        target: u32,
    },
    /// A local nominal declaration coordinate does not fit the allowed domain.
    #[error("type fact {ordinal} nominal target {target} is outside the type lane")]
    NominalOutOfRange {
        /// Source type-record coordinate.
        ordinal: u32,
        /// Referenced entity coordinate.
        target: u32,
    },
    /// A stable or external child identity is malformed.
    #[error("type fact {ordinal} child {position} external identity is invalid")]
    Authority {
        /// Source type-record coordinate.
        ordinal: u32,
        /// Child position containing the malformed external identity.
        position: u32,
        /// Content-ID decoding failure from the typed identity.
        #[source]
        source: ContentIdDecodeError,
    },
    /// The payload ends before the declared record or child fields are complete.
    #[error("type fact {ordinal} is truncated before {needed} bytes")]
    Truncated {
        /// Type-record coordinate being decoded.
        ordinal: u32,
        /// Minimum payload length in bytes required to reach the missing field.
        needed: usize,
    },
    /// Bytes remain after the number of records and children encoded by the header.
    #[error("type-fact section declares {declared} records but has trailing bytes")]
    TrailingBytes {
        /// Combined number of records advertised by the segment header.
        declared: u32,
    },
    /// A discriminant in a record does not belong to its closed wire registry.
    #[error("type fact {ordinal} has unknown {field} tag {actual}")]
    Tag {
        /// Type-record coordinate containing the unknown discriminant.
        ordinal: u32,
        /// Wire field whose discriminant failed decoding.
        field: &'static str,
        /// Unrecognized discriminant byte.
        actual: u8,
    },
    /// Child text bytes are not valid UTF-8 for the text-child case.
    #[error("type fact {ordinal} child {position} has invalid text")]
    Text {
        /// Source type-record coordinate.
        ordinal: u32,
        /// Child position whose text payload failed validation.
        position: u32,
    },
}

impl<'bytes> TypeFactLane<'bytes> {
    /// Measures the complete local type-coordinate space emitted by this
    /// lane.  This is the only count extension lists may use.
    pub fn counts(&self) -> Result<TypeFactCounts, TypeFactFault> {
        let declared =
            u32::try_from(self.inputs.len()).map_err(|_| TypeFactFault::CountOverflow {
                declared: u32::MAX,
                computed: u32::MAX,
            })?;
        let computed =
            u32::try_from(self.computed.len()).map_err(|_| TypeFactFault::CountOverflow {
                declared,
                computed: u32::MAX,
            })?;
        let counts = TypeFactCounts::new(declared, computed);
        let _ = counts.total()?;
        Ok(counts)
    }

    /// Validates this lane using the schema-1 rule set: all local child edges
    /// must point to an earlier declared row.
    pub fn admit(
        &self,
        entity_count: u32,
        children: &[SemanticTypeChild<'bytes>],
    ) -> Result<(), TypeFactFault> {
        self.admit_schema(entity_count, children, 1)
    }

    /// Validates owners, spans, tags, and reference ordering for `schema`.
    /// Schema 2 adds computed rows after the declared prefix and permits a
    /// declared nominal to name its owning entity.
    pub fn admit_schema(
        &self,
        entity_count: u32,
        children: &[SemanticTypeChild<'bytes>],
        schema: u16,
    ) -> Result<(), TypeFactFault> {
        let counts = self.counts()?;
        let count = counts.declared;
        let total = if schema >= 2 { counts.total()? } else { count };
        // Schema-2 declared and computed rows share a type-coordinate space.
        // Validate all of it: checking only the declared prefix left every
        // computed record as unchecked durable payload.
        for (ordinal, input) in (0..total).zip(self.inputs.iter().chain(self.computed)) {
            let computed = schema >= 2 && ordinal >= count;
            if input.owner.raw >= entity_count {
                return Err(TypeFactFault::Owner {
                    ordinal,
                    owner: input.owner,
                    entity_count,
                });
            }
            let start = input.record.children.start;
            let length = input.record.children.length;
            let end = start.checked_add(length).ok_or(TypeFactFault::ChildSpan {
                ordinal,
                start,
                length,
                child_count: u32::try_from(children.len()).unwrap_or(u32::MAX),
            })?;
            if end > u32::try_from(children.len()).unwrap_or(u32::MAX) {
                return Err(TypeFactFault::ChildSpan {
                    ordinal,
                    start,
                    length,
                    child_count: u32::try_from(children.len()).unwrap_or(u32::MAX),
                });
            }
            input
                .record
                .validate(length)
                .map_err(|fault| TypeFactFault::Record { ordinal, fault })?;
            for position in 0..length {
                let child = &children[usize::try_from(start + position).unwrap_or(usize::MAX)];
                input
                    .record
                    .validate_child_in_row(position, length, child)
                    .map_err(|fault| TypeFactFault::Record { ordinal, fault })?;
                if let TypeChildTarget::Type(crate::ir_vocabulary::TypeRef::Local(target)) =
                    child.target
                {
                    if target.raw >= total {
                        return Err(TypeFactFault::ChildTargetOutOfRange {
                            ordinal,
                            position,
                            target: target.raw,
                            record_count: total,
                        });
                    }
                    if !computed && schema >= 2 && target.raw >= count {
                        return Err(TypeFactFault::ComputedTargetFromDeclared {
                            ordinal,
                            position,
                            target: target.raw,
                        });
                    }
                    if computed && schema >= 2 && target.raw >= count && target.raw >= ordinal {
                        return Err(TypeFactFault::ComputedForwardReference {
                            ordinal,
                            position,
                            target: target.raw,
                        });
                    }
                    if schema == 1 && target.raw >= ordinal {
                        return Err(TypeFactFault::ForwardReference {
                            ordinal,
                            position,
                            target: target.raw,
                        });
                    }
                }
            }
            if let Some(NominalRef::Local(target)) = input.record.nominal {
                let nominal_count = if schema >= 2 { entity_count } else { count };
                if target.raw >= nominal_count {
                    return Err(TypeFactFault::NominalOutOfRange {
                        ordinal,
                        target: target.raw,
                    });
                }
                // The diagonal self-nominal is the terminal recursive case; a
                // nominal at another row must still point strictly backward.
                if schema == 1 && target.raw > ordinal {
                    return Err(TypeFactFault::NominalForward {
                        ordinal,
                        target: target.raw,
                    });
                }
            }
        }
        Ok(())
    }

    #[must_use]
    /// Returns the exact encoded byte length for this lane's current contents.
    pub fn payload_len(&self) -> usize {
        let mut size = 8;
        for input in self.inputs.iter().chain(self.computed) {
            size += 4
                + 1
                + 4
                + 4
                + cell_len(input.record.text)
                + cell_len(input.record.text2)
                + nominal_len(input.record.nominal)
                + 8;
        }
        size += 4;
        for child in self.children {
            size += match child.target {
                TypeChildTarget::Type(crate::ir_vocabulary::TypeRef::Local(_)) => 1 + 4,
                TypeChildTarget::Type(crate::ir_vocabulary::TypeRef::External(_)) => {
                    1 + HASH_BYTES + 4
                }
                TypeChildTarget::Text => 1,
            };
            size += cell_len(child.name);
            size += 1;
        }
        size
    }

    /// Writes the canonical payload into a caller-provided buffer of at least [`Self::payload_len`] bytes.
    pub fn write_payload(&self, output: &mut [u8]) {
        let mut at = 0;
        put_u32(
            output,
            &mut at,
            u32::try_from(self.inputs.len()).unwrap_or(u32::MAX),
        );
        put_u32(
            output,
            &mut at,
            u32::try_from(self.computed.len()).unwrap_or(u32::MAX),
        );
        for input in self.inputs.iter().chain(self.computed) {
            put_u32(output, &mut at, input.owner.raw);
            output[at] = u8::from(input.record.tag);
            at += 1;
            put_u32(output, &mut at, input.record.payload0);
            put_u32(output, &mut at, input.record.payload1);
            put_cell(output, &mut at, input.record.text);
            put_cell(output, &mut at, input.record.text2);
            put_nominal(output, &mut at, input.record.nominal);
            put_u32(output, &mut at, input.record.children.start);
            put_u32(output, &mut at, input.record.children.length);
        }
        put_u32(
            output,
            &mut at,
            u32::try_from(self.children.len()).unwrap_or(u32::MAX),
        );
        for child in self.children {
            match child.target {
                TypeChildTarget::Type(crate::ir_vocabulary::TypeRef::Local(target)) => {
                    output[at] = CHILD_LOCAL;
                    at += 1;
                    put_u32(output, &mut at, target.raw);
                }
                TypeChildTarget::Type(crate::ir_vocabulary::TypeRef::External(target)) => {
                    output[at] = CHILD_EXTERNAL;
                    at += 1;
                    output[at..at + HASH_BYTES].copy_from_slice(target.fragment.as_ref());
                    at += HASH_BYTES;
                    put_u32(output, &mut at, target.ordinal);
                }
                TypeChildTarget::Text => {
                    output[at] = CHILD_TEXT;
                    at += 1;
                }
            }
            put_cell(output, &mut at, child.name);
            output[at] = child.flags;
            at += 1;
        }
    }
}

fn cell_len(cell: Option<&[u8]>) -> usize {
    1 + cell.map_or(0, |bytes| 4 + bytes.len())
}
fn nominal_len(nominal: Option<NominalRef>) -> usize {
    match nominal {
        None => 1,
        Some(NominalRef::Local(_)) => 5,
        Some(NominalRef::External(_)) => 1 + HASH_BYTES + 4,
        Some(NominalRef::Stable(_)) => 1 + HASH_BYTES + COMPACT_DECLARATION_BYTES * 2,
    }
}
fn put_u32(output: &mut [u8], at: &mut usize, value: u32) {
    output[*at..*at + 4].copy_from_slice(&value.to_le_bytes());
    *at += 4;
}
fn put_cell(output: &mut [u8], at: &mut usize, cell: Option<&[u8]>) {
    output[*at] = if cell.is_some() {
        PRESENCE_SOME
    } else {
        PRESENCE_NONE
    };
    *at += 1;
    if let Some(bytes) = cell {
        put_u32(output, at, u32::try_from(bytes.len()).unwrap_or(u32::MAX));
        output[*at..*at + bytes.len()].copy_from_slice(bytes);
        *at += bytes.len();
    }
}
fn put_nominal(output: &mut [u8], at: &mut usize, nominal: Option<NominalRef>) {
    match nominal {
        None => output[*at] = NOMINAL_NONE,
        Some(NominalRef::Local(target)) => {
            output[*at] = NOMINAL_LOCAL;
            *at += 1;
            put_u32(output, at, target.raw);
            return;
        }
        Some(NominalRef::External(target)) => {
            output[*at] = NOMINAL_EXTERNAL;
            *at += 1;
            output[*at..*at + HASH_BYTES].copy_from_slice(target.fragment.as_ref());
            *at += HASH_BYTES;
            put_u32(output, at, target.ordinal);
            return;
        }
        Some(NominalRef::Stable(stable)) => {
            output[*at] = NOMINAL_STABLE;
            *at += 1;
            output[*at..*at + HASH_BYTES].copy_from_slice(stable.fragment.as_ref());
            *at += HASH_BYTES;
            output[*at..*at + COMPACT_DECLARATION_BYTES]
                .copy_from_slice(stable.declaration.family.as_bytes());
            *at += COMPACT_DECLARATION_BYTES;
            output[*at..*at + COMPACT_DECLARATION_BYTES]
                .copy_from_slice(stable.declaration.variant.as_bytes());
            *at += COMPACT_DECLARATION_BYTES;
            return;
        }
    }
    *at += 1;
}

/// Reads one declaration-identified external nominal: the owning fragment's
/// typed identity followed by the exact composite declaration identity.
fn read_stable_ref(reader: &mut Reader<'_>) -> Result<StableRef, TypeFactFault> {
    let fragment = reader.identity()?;
    let mut family = [0_u8; COMPACT_DECLARATION_BYTES];
    family.copy_from_slice(reader.take(COMPACT_DECLARATION_BYTES)?);
    let mut variant = [0_u8; COMPACT_DECLARATION_BYTES];
    variant.copy_from_slice(reader.take(COMPACT_DECLARATION_BYTES)?);
    Ok(StableRef {
        fragment,
        declaration: DeclarationIdentity {
            family: DeclarationFamilyId::from_raw(family),
            variant: VariantFingerprint::from_raw(variant),
        },
    })
}

pub(crate) fn validate_payload(
    payload: &[u8],
    entity_count: u32,
    schema: u16,
) -> Result<TypeFactCounts, TypeFactFault> {
    let mut reader = Reader {
        bytes: payload,
        at: 0,
        ordinal: 0,
    };
    let declared_count = reader.u32()?;
    let computed_count = if schema >= 2 { reader.u32()? } else { 0 };
    let counts = TypeFactCounts::new(declared_count, computed_count);
    let count = counts.total()?;
    for ordinal in 0..count {
        reader.ordinal = ordinal;
        let owner = reader.u32()?;
        if owner >= entity_count {
            return Err(TypeFactFault::Owner {
                ordinal,
                owner: EntityId::new(owner),
                entity_count,
            });
        }
        let tag = reader.u8()?;
        let _tag = SemanticTypeTag::try_from(tag).map_err(|error| TypeFactFault::Tag {
            ordinal,
            field: "record",
            actual: error.actual,
        })?;
        let _payload0 = reader.u32()?;
        let _payload1 = reader.u32()?;
        let _text = read_cell(&mut reader)?;
        let _text2 = read_cell(&mut reader)?;
        let nominal_tag = reader.u8()?;
        let _nominal = match nominal_tag {
            NOMINAL_NONE => None,
            NOMINAL_LOCAL => {
                let target = reader.u32()?;
                check_nominal(ordinal, target, entity_count, declared_count, schema)?;
                Some(NominalRef::Local(EntityId::new(target)))
            }
            NOMINAL_EXTERNAL => Some(NominalRef::External(ExternalEntityRef::bind(
                reader.identity()?,
                reader.u32()?,
            ))),
            NOMINAL_STABLE => Some(NominalRef::Stable(read_stable_ref(&mut reader)?)),
            actual => {
                return Err(TypeFactFault::Tag {
                    ordinal,
                    field: "nominal",
                    actual,
                });
            }
        };
        reader.u32()?;
        reader.u32()?;
    }
    let child_section = reader.at;
    let child_count = reader.u32()?;
    for _position in 0..child_count {
        let target = reader.u8()?;
        match target {
            CHILD_LOCAL => {
                reader.u32()?;
            }
            CHILD_EXTERNAL => {
                reader.identity()?;
                reader.u32()?;
            }
            CHILD_TEXT => {}
            actual => {
                return Err(TypeFactFault::Tag {
                    ordinal: reader.ordinal,
                    field: "child",
                    actual,
                });
            }
        }
        reader.cell()?;
        reader.u8()?;
    }
    let mut records = Reader {
        bytes: payload,
        at: if schema >= 2 { 8 } else { 4 },
        ordinal: 0,
    };
    // Child cells are variable-width, so the section is walked by one
    // forward cursor. Records emit their child spans in ascending order, so
    // each cell is skipped once overall; a span that starts before the
    // cursor rewinds it to the section start. Re-scanning from the start for
    // every child made validation quadratic in the child count.
    let first_child = child_section + 4;
    let mut cursor = Reader {
        bytes: payload,
        at: first_child,
        ordinal: 0,
    };
    let mut cursor_index = 0_u32;
    for ordinal in 0..count {
        records.ordinal = ordinal;
        let owner = EntityId::new(records.u32()?);
        let record = decode_record(&mut records, owner)?.record;
        let end = record
            .children
            .start
            .checked_add(record.children.length)
            .ok_or(TypeFactFault::ChildSpan {
                ordinal,
                start: record.children.start,
                length: record.children.length,
                child_count,
            })?;
        if end > child_count {
            return Err(TypeFactFault::ChildSpan {
                ordinal,
                start: record.children.start,
                length: record.children.length,
                child_count,
            });
        }
        record
            .validate(record.children.length)
            .map_err(|fault| TypeFactFault::Record { ordinal, fault })?;
        for position in 0..record.children.length {
            let child_index = record.children.start + position;
            if child_index < cursor_index {
                cursor.at = first_child;
                cursor_index = 0;
            }
            cursor.ordinal = ordinal;
            while cursor_index < child_index {
                skip_child(&mut cursor)?;
                cursor_index += 1;
            }
            let mut child_reader = Reader {
                bytes: payload,
                at: cursor.at,
                ordinal,
            };
            let child = decode_child(&mut child_reader, ordinal, position)?;
            record
                .validate_child_in_row(position, record.children.length, &child)
                .map_err(|fault| TypeFactFault::Record { ordinal, fault })?;
            if let TypeChildTarget::Type(crate::ir_vocabulary::TypeRef::Local(target)) =
                child.target
            {
                let computed = schema >= 2 && ordinal >= declared_count;
                if target.raw >= count {
                    return Err(TypeFactFault::ChildTargetOutOfRange {
                        ordinal,
                        position,
                        target: target.raw,
                        record_count: count,
                    });
                }
                if !computed && schema >= 2 && target.raw >= declared_count {
                    return Err(TypeFactFault::ComputedTargetFromDeclared {
                        ordinal,
                        position,
                        target: target.raw,
                    });
                }
                if computed && schema >= 2 && target.raw >= declared_count && target.raw >= ordinal
                {
                    return Err(TypeFactFault::ComputedForwardReference {
                        ordinal,
                        position,
                        target: target.raw,
                    });
                }
                if schema == 1 && target.raw >= ordinal {
                    return Err(TypeFactFault::ForwardReference {
                        ordinal,
                        position,
                        target: target.raw,
                    });
                }
            }
        }
    }
    if reader.at != payload.len() {
        return Err(TypeFactFault::TrailingBytes { declared: count });
    }
    Ok(counts)
}

fn check_nominal(
    ordinal: u32,
    target: u32,
    entity_count: u32,
    record_count: u32,
    schema: u16,
) -> Result<(), TypeFactFault> {
    let limit = if schema >= 2 {
        entity_count
    } else {
        record_count
    };
    if target >= limit {
        return Err(TypeFactFault::NominalOutOfRange { ordinal, target });
    }
    // The diagonal self-nominal is the terminal recursive case; a nominal at
    // another row must still point strictly backward.
    if schema == 1 && target > ordinal {
        return Err(TypeFactFault::NominalForward { ordinal, target });
    }
    Ok(())
}

fn skip_child(reader: &mut Reader<'_>) -> Result<(), TypeFactFault> {
    match reader.u8()? {
        CHILD_LOCAL => {
            reader.u32()?;
        }
        CHILD_EXTERNAL => {
            reader.identity()?;
            reader.u32()?;
        }
        CHILD_TEXT => {}
        actual => {
            return Err(TypeFactFault::Tag {
                ordinal: reader.ordinal,
                field: "child",
                actual,
            });
        }
    }
    reader.cell()?;
    reader.u8()?;
    Ok(())
}

fn decode_child<'bytes>(
    reader: &mut Reader<'bytes>,
    ordinal: u32,
    _position: u32,
) -> Result<SemanticTypeChild<'bytes>, TypeFactFault> {
    let target = match reader.u8()? {
        CHILD_LOCAL => TypeChildTarget::Type(crate::ir_vocabulary::TypeRef::Local(TypeId::new(
            reader.u32()?,
        ))),
        CHILD_EXTERNAL => TypeChildTarget::Type(crate::ir_vocabulary::TypeRef::External(
            crate::ir_vocabulary::ExternalTypeRef::bind(reader.identity()?, reader.u32()?),
        )),
        CHILD_TEXT => TypeChildTarget::Text,
        actual => {
            return Err(TypeFactFault::Tag {
                ordinal,
                field: "child",
                actual,
            });
        }
    };
    let name = read_cell(reader)?;
    let flags = reader.u8()?;
    Ok(SemanticTypeChild {
        target,
        name,
        flags,
    })
}

struct Reader<'bytes> {
    bytes: &'bytes [u8],
    at: usize,
    ordinal: u32,
}
impl<'bytes> Reader<'bytes> {
    fn take(&mut self, count: usize) -> Result<&'bytes [u8], TypeFactFault> {
        let end = self.at.checked_add(count).ok_or(TypeFactFault::Truncated {
            ordinal: self.ordinal,
            needed: usize::MAX,
        })?;
        let value = self
            .bytes
            .get(self.at..end)
            .ok_or(TypeFactFault::Truncated {
                ordinal: self.ordinal,
                needed: end,
            })?;
        self.at = end;
        Ok(value)
    }
    fn u8(&mut self) -> Result<u8, TypeFactFault> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, TypeFactFault> {
        let mut raw = [0; 4];
        raw.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(raw))
    }
    fn cell(&mut self) -> Result<(), TypeFactFault> {
        let present = self.u8()?;
        match present {
            PRESENCE_NONE => Ok(()),
            PRESENCE_SOME => {
                let length = usize::try_from(self.u32()?).unwrap_or(usize::MAX);
                self.take(length).map(|_| ())
            }
            actual => Err(TypeFactFault::Tag {
                ordinal: self.ordinal,
                field: "cell",
                actual,
            }),
        }
    }
    fn identity(&mut self) -> Result<ContentId<IrFragmentDomain>, TypeFactFault> {
        let mut raw = [0; HASH_BYTES];
        raw.copy_from_slice(self.take(HASH_BYTES)?);
        ContentId::try_from(raw).map_err(|source| TypeFactFault::Authority {
            ordinal: self.ordinal,
            position: 0,
            source,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Identifies whether a decoded row came from the declared or computed segment.
pub enum TypeFactSegment {
    /// Row belongs to the declared prefix used by older schema readers.
    Declared,
    /// Row belongs to the schema-2 computed suffix.
    Computed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// One decoded type record with the entity owner and segment that assigned its coordinate.
pub struct DecodedTypeFact<'fragment> {
    /// Entity-table coordinate that owns this record.
    pub owner: EntityId,
    /// Decoded semantic type value borrowing any retained text from the payload.
    pub record: SemanticTypeRecord<'fragment>,
    /// Declared prefix or computed suffix containing this record.
    pub segment: TypeFactSegment,
}

/// One decoded ordered child from the durable type-fact child pool.
///
/// Its ordinal is the common type-child coordinate referenced by each
/// `SemanticTypeRecord::children` span; it is not an entity or image row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedTypeFactChild<'fragment> {
    /// Shared child-pool coordinate referenced by record spans.
    pub ordinal: u32,
    /// Decoded child value borrowing text bytes from the payload when present.
    pub child: SemanticTypeChild<'fragment>,
}

/// Lazy decoder over a validated type-fact payload.
pub struct TypeFactCursor<'fragment> {
    payload: &'fragment [u8],
    at: usize,
    remaining: u32,
    declared_remaining: u32,
    schema: u16,
    ordinal: u32,
}
impl<'fragment> TypeFactCursor<'fragment> {
    pub(crate) fn new(payload: &'fragment [u8], schema: u16) -> Self {
        let remaining = payload
            .get(..4)
            .map(|raw| u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
            .unwrap_or(0);
        let computed = if schema >= 2 {
            payload
                .get(4..8)
                .map(|raw| u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
                .unwrap_or(0)
        } else {
            0
        };
        Self {
            payload,
            at: if schema >= 2 { 8 } else { 4 },
            remaining: remaining.saturating_add(computed),
            declared_remaining: remaining,
            schema,
            ordinal: 0,
        }
    }

    /// Opens the exact common child pool referenced by decoded type-record
    /// list spans.  The fragment validator has already proved its offset,
    /// cardinality, tags, and targets; this only establishes a borrowed lazy
    /// cursor and never reconstructs source syntax.
    pub fn children(&self) -> Result<TypeFactChildCursor<'fragment>, TypeFactFault> {
        let declared = self
            .payload
            .get(..4)
            .map(|raw| u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
            .unwrap_or(0);
        let computed = if self.schema >= 2 {
            self.payload
                .get(4..8)
                .map(|raw| u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
                .unwrap_or(0)
        } else {
            0
        };
        let count = TypeFactCounts::new(declared, computed).total()?;
        let mut reader = Reader {
            bytes: self.payload,
            at: if self.schema >= 2 { 8 } else { 4 },
            ordinal: 0,
        };
        for ordinal in 0..count {
            reader.ordinal = ordinal;
            let owner = EntityId::new(reader.u32()?);
            let _record = decode_record(&mut reader, owner)?;
        }
        let child_count = reader.u32()?;
        Ok(TypeFactChildCursor {
            payload: self.payload,
            at: reader.at,
            remaining: child_count,
            ordinal: 0,
        })
    }
}

/// Lazy borrowed traversal of the validated common type-child pool.
pub struct TypeFactChildCursor<'fragment> {
    payload: &'fragment [u8],
    at: usize,
    remaining: u32,
    ordinal: u32,
}

impl<'fragment> Iterator for TypeFactChildCursor<'fragment> {
    type Item = Result<DecodedTypeFactChild<'fragment>, TypeFactFault>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let ordinal = self.ordinal;
        self.ordinal += 1;
        let mut reader = Reader {
            bytes: self.payload,
            at: self.at,
            ordinal,
        };
        let child = decode_child(&mut reader, ordinal, 0);
        self.at = reader.at;
        Some(child.map(|child| DecodedTypeFactChild { ordinal, child }))
    }
}

impl<'fragment> Iterator for TypeFactCursor<'fragment> {
    type Item = Result<DecodedTypeFact<'fragment>, TypeFactFault>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let segment = if self.schema >= 2 && self.ordinal >= self.declared_remaining {
            TypeFactSegment::Computed
        } else {
            TypeFactSegment::Declared
        };
        let mut reader = Reader {
            bytes: self.payload,
            at: self.at,
            ordinal: self.ordinal,
        };
        self.ordinal += 1;
        let owner = match reader.u32() {
            Ok(value) => EntityId::new(value),
            Err(error) => return Some(Err(error)),
        };
        let result = decode_record(&mut reader, owner);
        self.at = reader.at;
        Some(result.map(|mut decoded| {
            decoded.segment = segment;
            decoded
        }))
    }
}
fn decode_record<'fragment>(
    reader: &mut Reader<'fragment>,
    owner: EntityId,
) -> Result<DecodedTypeFact<'fragment>, TypeFactFault> {
    let tag = SemanticTypeTag::try_from(reader.u8()?).map_err(|error| TypeFactFault::Tag {
        ordinal: reader.ordinal,
        field: "record",
        actual: error.actual,
    })?;
    let payload0 = reader.u32()?;
    let payload1 = reader.u32()?;
    let text = read_cell(reader)?;
    let text2 = read_cell(reader)?;
    let nominal = match reader.u8()? {
        NOMINAL_NONE => None,
        NOMINAL_LOCAL => Some(NominalRef::Local(EntityId::new(reader.u32()?))),
        NOMINAL_EXTERNAL => Some(NominalRef::External(ExternalEntityRef::bind(
            reader.identity()?,
            reader.u32()?,
        ))),
        NOMINAL_STABLE => Some(NominalRef::Stable(read_stable_ref(reader)?)),
        actual => {
            return Err(TypeFactFault::Tag {
                ordinal: reader.ordinal,
                field: "nominal",
                actual,
            });
        }
    };
    let start = reader.u32()?;
    let length = reader.u32()?;
    Ok(DecodedTypeFact {
        owner,
        segment: TypeFactSegment::Declared,
        record: SemanticTypeRecord {
            tag,
            payload0,
            payload1,
            text,
            text2,
            nominal,
            children: ListSpan::new(start, length),
        },
    })
}
fn read_cell<'bytes>(reader: &mut Reader<'bytes>) -> Result<Option<&'bytes [u8]>, TypeFactFault> {
    let present = reader.u8()?;
    match present {
        PRESENCE_NONE => Ok(None),
        PRESENCE_SOME => {
            let length = usize::try_from(reader.u32()?).unwrap_or(usize::MAX);
            Ok(Some(reader.take(length)?))
        }
        actual => Err(TypeFactFault::Tag {
            ordinal: reader.ordinal,
            field: "cell",
            actual,
        }),
    }
}
