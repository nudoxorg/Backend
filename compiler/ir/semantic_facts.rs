//! Admission, wire encoding, and trusted reopen validation of the
//! occurrence plane beside the semantic-data products.
//!
//! The occurrence plane is a fact lane, not a canonicalized graph: records
//! are admitted once (validated with exact faults), written in lane order,
//! and revalidated identically on reopen. Lane order is wire order, so one
//! admitted fact set always writes identical section bytes.

use compiler_ir_vocabulary::{
    Confidence, EntityId, ForeignKey, ForeignOrigin, Occurrence, OccurrenceTarget, ReferenceKind,
    StableRef,
};
use heart_identity::{ContentId, ContentIdDecodeError, IrFragmentDomain, SourceFactDomain};
use thiserror::Error;

/// Wire tag of a stable occurrence target.
pub(crate) const STABLE_TARGET_TAG: u8 = 0;
/// Wire tag of a foreign occurrence target.
pub(crate) const FOREIGN_TARGET_TAG: u8 = 1;

/// Byte width of one serialized 32-byte identity cell.
const IDENTITY_BYTES: usize = 32;

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
                OccurrenceTarget::Stable(_) => length += IDENTITY_BYTES * 2,
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
                    payload[cursor..cursor + IDENTITY_BYTES]
                        .copy_from_slice(stable.entity.as_ref());
                    cursor += IDENTITY_BYTES;
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

struct PayloadReader<'payload> {
    payload: &'payload [u8],
    cursor: usize,
    ordinal: u32,
}

impl<'payload> PayloadReader<'payload> {
    fn take(&mut self, count: usize) -> Result<&'payload [u8], OccurrenceFault> {
        let end = self
            .cursor
            .checked_add(count)
            .ok_or(OccurrenceFault::Truncated {
                ordinal: self.ordinal,
                needed: usize::MAX,
            })?;
        let cell = self
            .payload
            .get(self.cursor..end)
            .ok_or(OccurrenceFault::Truncated {
                ordinal: self.ordinal,
                needed: end,
            })?;
        self.cursor = end;
        Ok(cell)
    }

    fn read_u8(&mut self) -> Result<u8, OccurrenceFault> {
        Ok(self.take(1)?[0])
    }

    fn read_u32(&mut self) -> Result<u32, OccurrenceFault> {
        let mut raw = [0_u8; 4];
        raw.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(raw))
    }

    fn read_u16(&mut self) -> Result<u16, OccurrenceFault> {
        let mut raw = [0_u8; 2];
        raw.copy_from_slice(self.take(2)?);
        Ok(u16::from_le_bytes(raw))
    }

    fn read_cell(&mut self) -> Result<&'payload [u8], OccurrenceFault> {
        let length = self.read_u32()? as usize;
        self.take(length)
    }
}

/// Maps an admission-grade occurrence fault onto its Copy view mirror,
/// preserving every exact operand the mirror can carry.
pub(crate) fn occurrence_view_fault(fault: OccurrenceFault) -> crate::view::OccurrenceFault {
    use crate::view::OccurrenceFault as View;
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

/// Validates one occurrence section payload against an entity lane of
/// `entity_count` rows. Bytes are checked exactly where the admission
/// contract demands: identity authority cells, closed tags, ordered spans,
/// and non-empty foreign paths.
pub(crate) fn validate_occurrence_payload(
    payload: &[u8],
    entity_count: u32,
) -> Result<(), OccurrenceFault> {
    let mut reader = PayloadReader {
        payload,
        cursor: 0,
        ordinal: 0,
    };
    let declared = reader.read_u32().map_err(|_| OccurrenceFault::Truncated {
        ordinal: 0,
        needed: 4,
    })?;
    for ordinal in 0..declared {
        reader.ordinal = ordinal;
        let owner = reader.read_u32()?;
        if owner >= entity_count {
            return Err(OccurrenceFault::Owner {
                ordinal,
                owner: EntityId::new(owner),
                entity_count,
            });
        }
        let target_tag = reader.read_u8()?;
        match target_tag {
            STABLE_TARGET_TAG => {
                let fragment = reader.take(IDENTITY_BYTES)?;
                ContentId::<IrFragmentDomain>::try_from(fragment)?;
                let entity = reader.take(IDENTITY_BYTES)?;
                ContentId::<SourceFactDomain>::try_from(entity)?;
            }
            FOREIGN_TARGET_TAG => {
                let origin_tag = reader.read_u8()?;
                let kind_cell = reader.read_u16()?;
                if kind_cell > u16::from(compiler_ir_vocabulary::EntityKind::Parameter) + 1 {
                    return Err(OccurrenceFault::KindCell {
                        ordinal,
                        actual: kind_cell,
                    });
                }
                reader.read_cell()?;
                reader.read_cell()?;
                match origin_tag {
                    ForeignOrigin::PACKAGE_TAG => {
                        reader.read_cell()?;
                        reader.read_cell()?;
                    }
                    ForeignOrigin::NAMESPACE_TAG => {
                        reader.read_cell()?;
                        reader.read_cell()?;
                    }
                    ForeignOrigin::UNIVERSE_TAG => {
                        reader.read_cell()?;
                    }
                    _ => {
                        return Err(OccurrenceFault::OriginTag {
                            ordinal,
                            actual: origin_tag,
                        });
                    }
                }
            }
            actual => return Err(OccurrenceFault::TargetTag { ordinal, actual }),
        }
        let kind = reader.read_u8()?;
        ReferenceKind::try_from(kind).map_err(|_| OccurrenceFault::ReferenceKind {
            ordinal,
            actual: kind,
        })?;
        let confidence = reader.read_u8()?;
        Confidence::try_from(confidence).map_err(|_| OccurrenceFault::Confidence {
            ordinal,
            actual: confidence,
        })?;
        let start = reader.read_u32()?;
        let end = reader.read_u32()?;
        if start > end {
            return Err(OccurrenceFault::Span {
                ordinal,
                start,
                end,
            });
        }
    }
    if reader.cursor != payload.len() {
        return Err(OccurrenceFault::TrailingBytes { declared });
    }
    Ok(())
}

/// Lazily decodes one validated occurrence section, lending envelope bytes.
pub struct OccurrenceCursor<'fragment> {
    payload: &'fragment [u8],
    cursor: usize,
    remaining: u32,
}

impl<'fragment> OccurrenceCursor<'fragment> {
    pub(crate) const fn new(payload: &'fragment [u8]) -> Self {
        Self {
            payload,
            cursor: 4,
            remaining: if payload.len() >= 4 {
                u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]])
            } else {
                0
            },
        }
    }

    /// Decodes the next occurrence, or `None` after the declared count.
    #[expect(
        clippy::should_implement_trait,
        reason = "the cursor API mirrors Iterator::next without claiming the Iterator surface; fragments lend one cursor per section"
    )]
    pub fn next(&mut self) -> Option<Result<DecodedOccurrence<'fragment>, OccurrenceFault>> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let mut reader = PayloadReader {
            payload: self.payload,
            cursor: self.cursor,
            ordinal: 0,
        };
        let decoded = decode_one(&mut reader);
        self.cursor = reader.cursor;
        Some(decoded)
    }
}

fn decode_one<'payload>(
    reader: &mut PayloadReader<'payload>,
) -> Result<DecodedOccurrence<'payload>, OccurrenceFault> {
    let owner = reader.read_u32()?;
    let target_tag = reader.read_u8()?;
    let target = match target_tag {
        STABLE_TARGET_TAG => {
            let fragment = ContentId::<IrFragmentDomain>::try_from(reader.take(IDENTITY_BYTES)?)?;
            let entity = ContentId::<SourceFactDomain>::try_from(reader.take(IDENTITY_BYTES)?)?;
            OccurrenceTarget::Stable(StableRef { fragment, entity })
        }
        FOREIGN_TARGET_TAG => {
            let origin_tag = reader.read_u8()?;
            let kind_cell = reader.read_u16()?;
            let kind = if kind_cell == 0 {
                None
            } else {
                Some(
                    compiler_ir_vocabulary::EntityKind::try_from(kind_cell - 1).map_err(|_| {
                        OccurrenceFault::KindCell {
                            ordinal: reader.ordinal,
                            actual: kind_cell,
                        }
                    })?,
                )
            };
            let path = reader.read_cell()?;
            let display = reader.read_cell()?;
            let origin = match origin_tag {
                ForeignOrigin::PACKAGE_TAG => {
                    let ecosystem = reader.read_cell()?;
                    let name = reader.read_cell()?;
                    let ecosystem = core::str::from_utf8(ecosystem).map_err(|_| {
                        OccurrenceFault::OriginTag {
                            ordinal: reader.ordinal,
                            actual: origin_tag,
                        }
                    })?;
                    let name =
                        core::str::from_utf8(name).map_err(|_| OccurrenceFault::OriginTag {
                            ordinal: reader.ordinal,
                            actual: origin_tag,
                        })?;
                    ForeignOrigin::Package(
                        compiler_ir_vocabulary::PackageLineage::new(ecosystem, name).map_err(
                            |_| OccurrenceFault::OriginTag {
                                ordinal: reader.ordinal,
                                actual: origin_tag,
                            },
                        )?,
                    )
                }
                ForeignOrigin::NAMESPACE_TAG => {
                    let ecosystem = reader.read_cell()?;
                    let namespace = reader.read_cell()?;
                    let ecosystem = core::str::from_utf8(ecosystem).map_err(|_| {
                        OccurrenceFault::OriginTag {
                            ordinal: reader.ordinal,
                            actual: origin_tag,
                        }
                    })?;
                    let namespace = core::str::from_utf8(namespace).map_err(|_| {
                        OccurrenceFault::OriginTag {
                            ordinal: reader.ordinal,
                            actual: origin_tag,
                        }
                    })?;
                    ForeignOrigin::Namespace {
                        ecosystem,
                        namespace,
                    }
                }
                ForeignOrigin::UNIVERSE_TAG => {
                    let ecosystem = reader.read_cell()?;
                    let ecosystem = core::str::from_utf8(ecosystem).map_err(|_| {
                        OccurrenceFault::OriginTag {
                            ordinal: reader.ordinal,
                            actual: origin_tag,
                        }
                    })?;
                    ForeignOrigin::Universe { ecosystem }
                }
                actual => {
                    return Err(OccurrenceFault::OriginTag {
                        ordinal: reader.ordinal,
                        actual,
                    });
                }
            };
            OccurrenceTarget::Foreign(ForeignKey {
                origin,
                path: core::str::from_utf8(path).map_err(|_| OccurrenceFault::TargetTag {
                    ordinal: reader.ordinal,
                    actual: FOREIGN_TARGET_TAG,
                })?,
                display: core::str::from_utf8(display).map_err(|_| OccurrenceFault::TargetTag {
                    ordinal: reader.ordinal,
                    actual: FOREIGN_TARGET_TAG,
                })?,
                kind,
            })
        }
        actual => {
            return Err(OccurrenceFault::TargetTag {
                ordinal: reader.ordinal,
                actual,
            });
        }
    };
    let kind = reader.read_u8()?;
    let kind = ReferenceKind::try_from(kind).map_err(|_| OccurrenceFault::ReferenceKind {
        ordinal: reader.ordinal,
        actual: kind,
    })?;
    let confidence = reader.read_u8()?;
    let confidence = Confidence::try_from(confidence).map_err(|_| OccurrenceFault::Confidence {
        ordinal: reader.ordinal,
        actual: confidence,
    })?;
    let start = reader.read_u32()?;
    let end = reader.read_u32()?;
    let span = RelSpan::new_trusted(start, end);
    Ok(DecodedOccurrence {
        owner: EntityId::new(owner),
        occurrence: Occurrence {
            target,
            kind,
            confidence,
            span,
        },
    })
}

use compiler_ir_vocabulary::RelSpan;
