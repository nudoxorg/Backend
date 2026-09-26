//! Admission, wire encoding, and trusted reopen validation of the
//! occurrence plane beside the semantic-data products.
//!
//! The occurrence plane is a fact lane, not a canonicalized graph: records
//! are admitted once (validated with exact faults), written in lane order,
//! and revalidated identically on reopen. Lane order is wire order, so one
//! admitted fact set always writes identical section bytes.

use crate::ir_vocabulary::{
    Confidence, EntityId, ForeignOrigin, Occurrence, OccurrenceTarget, ReferenceKind,
};
use backend_version::ContentIdDecodeError;
use thiserror::Error;

mod reopen;

pub use reopen::OccurrenceCursor;
pub(crate) use reopen::validate_occurrence_payload;

/// Wire tag of a stable occurrence target.
pub(crate) const STABLE_TARGET_TAG: u8 = 0;
/// Wire tag of a foreign occurrence target.
pub(crate) const FOREIGN_TARGET_TAG: u8 = 1;
/// Wire tag of a same-fragment occurrence target.
pub(crate) const LOCAL_TARGET_TAG: u8 = 2;

/// Byte width of one serialized 32-byte identity cell.
const IDENTITY_BYTES: usize = 32;
/// Byte width of one compact declaration family or variant cell.
const COMPACT_DECLARATION_BYTES: usize = 16;

/// One admitted occurrence fact bound to its owning entity ordinal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OccurrenceInput<'bytes> {
    /// Owning entity row; relative spans are measured from its span start.
    pub owner: EntityId,
    /// The reference fact itself.
    pub occurrence: Occurrence<'bytes>,
}

/// The admitted occurrence lane: ordered facts whose lane order is wire
/// order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OccurrenceLane<'bytes> {
    /// Ordered occurrence facts.
    pub inputs: &'bytes [OccurrenceInput<'bytes>],
}

/// Exact occurrence-plane rejection retaining the offending ordinal and
/// every observed operand.
#[derive(Debug, Error)]
pub enum OccurrenceFault {
    #[error(
        "occurrence {ordinal} names entity {owner:?} outside the fragment entity lane of {entity_count}"
    )]
    Owner {
        ordinal: u32,
        owner: EntityId,
        entity_count: u32,
    },
    #[error(
        "occurrence {ordinal} names local target {target} outside the fragment entity lane of {entity_count}"
    )]
    LocalTarget {
        ordinal: u32,
        target: u32,
        entity_count: u32,
    },
    #[error("occurrence {ordinal} carries an unknown target tag {actual}")]
    TargetTag { ordinal: u32, actual: u8 },
    #[error("occurrence {ordinal} carries an unknown foreign origin tag {actual}")]
    OriginTag { ordinal: u32, actual: u8 },
    #[error("occurrence {ordinal} carries an unknown reference kind {actual}")]
    ReferenceKind { ordinal: u32, actual: u8 },
    #[error("occurrence {ordinal} carries an unknown confidence {actual}")]
    Confidence { ordinal: u32, actual: u8 },
    #[error("occurrence {ordinal} carries an inverted relative span {start}..{end}")]
    Span { ordinal: u32, start: u32, end: u32 },
    #[error("occurrence {ordinal} carries an unknown foreign kind cell {actual}")]
    KindCell { ordinal: u32, actual: u16 },
    #[error("occurrence {ordinal} foreign key has an empty path")]
    EmptyPath { ordinal: u32 },
    #[error("occurrence record {ordinal} payload ended before {needed} bytes")]
    Truncated { ordinal: u32, needed: usize },
    #[error("occurrence section declares {declared} records but carries trailing bytes")]
    TrailingBytes { declared: u32 },
    #[error(
        "occurrence {ordinal} uses a schema-{schema} family-only stable endpoint that cannot represent an exact declaration variant"
    )]
    LegacyStableTarget { ordinal: u32, schema: u16 },
    #[error("a serialized identity cell in the occurrence section is invalid")]
    Authority(#[from] ContentIdDecodeError),
}

impl<'bytes> OccurrenceLane<'bytes> {
    /// Admits the lane against an entity lane of `entity_count` rows:
    /// every owner ordinal must name a declared entity, every reference
    /// kind and confidence must decode, every relative span must stay
    /// ordered, and every foreign key path must be non-empty.
    pub fn admit(&self, entity_count: u32) -> Result<(), OccurrenceFault> {
        for (position, input) in self.inputs.iter().enumerate() {
            let ordinal = u32::try_from(position).unwrap_or(u32::MAX);
            if input.owner.raw >= entity_count {
                return Err(OccurrenceFault::Owner {
                    ordinal,
                    owner: input.owner,
                    entity_count,
                });
            }
            match input.occurrence.target {
                OccurrenceTarget::Stable(_) => {}
                OccurrenceTarget::Local(target) => {
                    if target.raw >= entity_count {
                        return Err(OccurrenceFault::LocalTarget {
                            ordinal,
                            target: target.raw,
                            entity_count,
                        });
                    }
                }
                OccurrenceTarget::Foreign(foreign) => {
                    if foreign.path.is_empty() {
                        return Err(OccurrenceFault::EmptyPath { ordinal });
                    }
                }
            }
            if ReferenceKind::try_from(u8::from(input.occurrence.kind)).is_err() {
                return Err(OccurrenceFault::ReferenceKind {
                    ordinal,
                    actual: u8::from(input.occurrence.kind),
                });
            }
            if Confidence::try_from(u8::from(input.occurrence.confidence)).is_err() {
                return Err(OccurrenceFault::Confidence {
                    ordinal,
                    actual: u8::from(input.occurrence.confidence),
                });
            }
            if input.occurrence.span.start > input.occurrence.span.end {
                return Err(OccurrenceFault::Span {
                    ordinal,
                    start: input.occurrence.span.start,
                    end: input.occurrence.span.end,
                });
            }
        }
        Ok(())
    }

    /// The exact serialized payload length for this lane.
    #[must_use]
    pub fn payload_len(&self) -> usize {
        let mut length = 4;
        for input in self.inputs {
            length += 4 + 1 + 1 + 1 + 4 + 4;
            match input.occurrence.target {
                OccurrenceTarget::Stable(_) => {
                    length += IDENTITY_BYTES + (COMPACT_DECLARATION_BYTES * 2)
                }
                OccurrenceTarget::Local(_) => length += 4,
                OccurrenceTarget::Foreign(foreign) => {
                    length += 1 + 2;
                    length += cell_len(foreign.path.as_bytes());
                    length += cell_len(foreign.display.as_bytes());
                    length += match foreign.origin {
                        ForeignOrigin::Package(lineage) => {
                            cell_len(lineage.ecosystem.as_bytes())
                                + cell_len(lineage.name.as_bytes())
                        }
                        ForeignOrigin::Namespace {
                            ecosystem,
                            namespace,
                        } => cell_len(ecosystem.as_bytes()) + cell_len(namespace.as_bytes()),
                        ForeignOrigin::Universe { ecosystem } => cell_len(ecosystem.as_bytes()),
                    };
                }
            }
        }
        length
    }

    /// Serializes the complete section payload (header plus records) in
    /// lane order. The caller-provided payload must measure exactly
    /// [`OccurrenceLane::payload_len`]; admission proved every cell.
    pub fn write_payload(&self, payload: &mut [u8]) {
        let count = u32::try_from(self.inputs.len()).unwrap_or(u32::MAX);
        payload[..4].copy_from_slice(&count.to_le_bytes());
        let mut cursor = 4;
        for input in self.inputs {
            payload[cursor..cursor + 4].copy_from_slice(&input.owner.raw.to_le_bytes());
            cursor += 4;
            match input.occurrence.target {
                OccurrenceTarget::Stable(stable) => {
                    payload[cursor] = STABLE_TARGET_TAG;
                    cursor += 1;
                    payload[cursor..cursor + IDENTITY_BYTES]
                        .copy_from_slice(stable.fragment.as_ref());
                    cursor += IDENTITY_BYTES;
                    payload[cursor..cursor + COMPACT_DECLARATION_BYTES]
                        .copy_from_slice(stable.declaration.family.as_bytes());
                    cursor += COMPACT_DECLARATION_BYTES;
                    payload[cursor..cursor + COMPACT_DECLARATION_BYTES]
                        .copy_from_slice(stable.declaration.variant.as_bytes());
                    cursor += COMPACT_DECLARATION_BYTES;
                }
                OccurrenceTarget::Foreign(foreign) => {
                    payload[cursor] = FOREIGN_TARGET_TAG;
                    cursor += 1;
                    payload[cursor] = foreign.origin.tag();
                    cursor += 1;
                    let kind_cell = match foreign.kind {
                        None => 0_u16,
                        Some(kind) => u16::from(kind) + 1,
                    };
                    payload[cursor..cursor + 2].copy_from_slice(&kind_cell.to_le_bytes());
                    cursor += 2;
                    cursor = write_payload_cell(payload, cursor, foreign.path.as_bytes());
                    cursor = write_payload_cell(payload, cursor, foreign.display.as_bytes());
                    match foreign.origin {
                        ForeignOrigin::Package(lineage) => {
                            cursor =
                                write_payload_cell(payload, cursor, lineage.ecosystem.as_bytes());
                            cursor = write_payload_cell(payload, cursor, lineage.name.as_bytes());
                        }
                        ForeignOrigin::Namespace {
                            ecosystem,
                            namespace,
                        } => {
                            cursor = write_payload_cell(payload, cursor, ecosystem.as_bytes());
                            cursor = write_payload_cell(payload, cursor, namespace.as_bytes());
                        }
                        ForeignOrigin::Universe { ecosystem } => {
                            cursor = write_payload_cell(payload, cursor, ecosystem.as_bytes());
                        }
                    }
                }
                OccurrenceTarget::Local(target) => {
                    payload[cursor] = LOCAL_TARGET_TAG;
                    cursor += 1;
                    payload[cursor..cursor + 4].copy_from_slice(&target.raw.to_le_bytes());
                    cursor += 4;
                }
            }
            payload[cursor] = u8::from(input.occurrence.kind);
            cursor += 1;
            payload[cursor] = u8::from(input.occurrence.confidence);
            cursor += 1;
            payload[cursor..cursor + 4].copy_from_slice(&input.occurrence.span.start.to_le_bytes());
            cursor += 4;
            payload[cursor..cursor + 4].copy_from_slice(&input.occurrence.span.end.to_le_bytes());
            cursor += 4;
        }
    }
}

/// One decoded occurrence read back from a reopened fragment. Borrowed
/// text cells lend the fragment envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedOccurrence<'fragment> {
    /// Owning entity ordinal.
    pub owner: EntityId,
    /// The decoded reference fact lending envelope bytes.
    pub occurrence: Occurrence<'fragment>,
}

fn cell_len(bytes: &[u8]) -> usize {
    4 + bytes.len()
}

fn write_payload_cell(payload: &mut [u8], cursor: usize, bytes: &[u8]) -> usize {
    let Ok(len) = u32::try_from(bytes.len()) else {
        return cursor;
    };
    payload[cursor..cursor + 4].copy_from_slice(&len.to_le_bytes());
    payload[cursor + 4..cursor + 4 + bytes.len()].copy_from_slice(bytes);
    cursor + 4 + bytes.len()
}

/// Maps an admission-grade occurrence fault onto its Copy view mirror,
/// preserving every exact operand the mirror can carry.
pub(crate) fn occurrence_view_fault(fault: OccurrenceFault) -> crate::ir::view::OccurrenceFault {
    use crate::ir::view::OccurrenceFault as View;
    match fault {
        OccurrenceFault::Owner {
            ordinal,
            owner,
            entity_count,
        } => View::Owner {
            ordinal,
            owner: owner.raw,
            entity_count,
        },
        OccurrenceFault::TargetTag { ordinal, actual } => View::TargetTag { ordinal, actual },
        OccurrenceFault::LocalTarget {
            ordinal,
            target,
            entity_count,
        } => View::LocalTarget {
            ordinal,
            target,
            entity_count,
        },
        OccurrenceFault::OriginTag { ordinal, actual } => View::OriginTag { ordinal, actual },
        OccurrenceFault::ReferenceKind { ordinal, actual } => {
            View::ReferenceKind { ordinal, actual }
        }
        OccurrenceFault::Confidence { ordinal, actual } => View::Confidence { ordinal, actual },
        OccurrenceFault::Span {
            ordinal,
            start,
            end,
        } => View::Span {
            ordinal,
            start,
            end,
        },
        OccurrenceFault::KindCell { ordinal, actual } => View::KindCell { ordinal, actual },
        OccurrenceFault::EmptyPath { ordinal } => View::EmptyPath { ordinal },
        OccurrenceFault::Truncated { ordinal, needed } => View::Truncated { ordinal, needed },
        OccurrenceFault::TrailingBytes { declared } => View::TrailingBytes { declared },
        OccurrenceFault::LegacyStableTarget { ordinal, schema } => {
            View::LegacyStableTarget { ordinal, schema }
        }
        OccurrenceFault::Authority(source) => match source {
            ContentIdDecodeError::Width { actual, .. } => {
                View::AuthorityWidth { ordinal: 0, actual }
            }
            ContentIdDecodeError::Domain {
                expected,
                observed,
                raw,
            } => View::AuthorityDomain {
                ordinal: 0,
                expected,
                observed,
                raw,
            },
        },
    }
}
