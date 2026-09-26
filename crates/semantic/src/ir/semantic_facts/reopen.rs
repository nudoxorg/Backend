//! Reopens and validates occurrence-section payloads.

use super::{
    COMPACT_DECLARATION_BYTES, DecodedOccurrence, FOREIGN_TARGET_TAG, IDENTITY_BYTES,
    LOCAL_TARGET_TAG, OccurrenceFault, STABLE_TARGET_TAG,
};
use crate::ir_vocabulary::{
    Confidence, DeclarationFamilyId, DeclarationIdentity, EntityId, ForeignKey, ForeignOrigin,
    Occurrence, OccurrenceTarget, ReferenceKind, StableRef, VariantFingerprint,
};
use backend_version::{ContentId, IrFragmentDomain};

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

/// Validates one occurrence section payload against an entity lane of
/// `entity_count` rows. Bytes are checked exactly where the admission
/// contract demands: identity authority cells, closed tags, ordered spans,
/// and non-empty foreign paths.
pub(crate) fn validate_occurrence_payload(
    payload: &[u8],
    entity_count: u32,
    schema: u16,
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
                if schema < 6 {
                    return Err(OccurrenceFault::LegacyStableTarget { ordinal, schema });
                }
                let fragment = reader.take(IDENTITY_BYTES)?;
                ContentId::<IrFragmentDomain>::try_from(fragment)?;
                reader.take(COMPACT_DECLARATION_BYTES)?;
                reader.take(COMPACT_DECLARATION_BYTES)?;
            }
            FOREIGN_TARGET_TAG => {
                let origin_tag = reader.read_u8()?;
                let kind_cell = reader.read_u16()?;
                if kind_cell > u16::from(crate::ir_vocabulary::EntityKind::Namespace) + 1 {
                    return Err(OccurrenceFault::KindCell {
                        ordinal,
                        actual: kind_cell,
                    });
                }
                let path = reader.read_cell()?;
                if path.is_empty() {
                    return Err(OccurrenceFault::EmptyPath { ordinal });
                }
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
            LOCAL_TARGET_TAG => {
                let target = reader.read_u32()?;
                if target >= entity_count {
                    return Err(OccurrenceFault::LocalTarget {
                        ordinal,
                        target,
                        entity_count,
                    });
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
    schema: u16,
    cursor: usize,
    ordinal: u32,
    remaining: u32,
}

impl<'fragment> OccurrenceCursor<'fragment> {
    pub(crate) const fn new(payload: &'fragment [u8], schema: u16) -> Self {
        Self {
            payload,
            schema,
            cursor: 4,
            ordinal: 0,
            remaining: if payload.len() >= 4 {
                u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]])
            } else {
                0
            },
        }
    }
}

impl<'fragment> Iterator for OccurrenceCursor<'fragment> {
    type Item = Result<DecodedOccurrence<'fragment>, OccurrenceFault>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let ordinal = self.ordinal;
        self.ordinal = self.ordinal.saturating_add(1);
        let mut reader = PayloadReader {
            payload: self.payload,
            cursor: self.cursor,
            ordinal,
        };
        let decoded = decode_one(&mut reader, self.schema);
        self.cursor = reader.cursor;
        Some(decoded)
    }
}

fn decode_one<'payload>(
    reader: &mut PayloadReader<'payload>,
    schema: u16,
) -> Result<DecodedOccurrence<'payload>, OccurrenceFault> {
    let owner = reader.read_u32()?;
    let target_tag = reader.read_u8()?;
    let target = match target_tag {
        STABLE_TARGET_TAG => {
            if schema < 6 {
                return Err(OccurrenceFault::LegacyStableTarget {
                    ordinal: reader.ordinal,
                    schema,
                });
            }
            let fragment = ContentId::<IrFragmentDomain>::try_from(reader.take(IDENTITY_BYTES)?)?;
            let family = raw_compact_declaration_id(reader)?;
            let variant = raw_variant_fingerprint(reader)?;
            OccurrenceTarget::Stable(StableRef {
                fragment,
                declaration: DeclarationIdentity { family, variant },
            })
        }
        LOCAL_TARGET_TAG => OccurrenceTarget::Local(EntityId::new(reader.read_u32()?)),
        FOREIGN_TARGET_TAG => {
            let origin_tag = reader.read_u8()?;
            let kind_cell = reader.read_u16()?;
            let kind = if kind_cell == 0 {
                None
            } else {
                Some(
                    crate::ir_vocabulary::EntityKind::try_from(kind_cell - 1).map_err(|_| {
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
                        crate::ir_vocabulary::PackageLineage::new(ecosystem, name).map_err(
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

fn raw_compact_declaration_id(
    reader: &mut PayloadReader<'_>,
) -> Result<DeclarationFamilyId, OccurrenceFault> {
    let mut bytes = [0_u8; COMPACT_DECLARATION_BYTES];
    bytes.copy_from_slice(reader.take(COMPACT_DECLARATION_BYTES)?);
    Ok(DeclarationFamilyId::from_raw(bytes))
}

fn raw_variant_fingerprint(
    reader: &mut PayloadReader<'_>,
) -> Result<VariantFingerprint, OccurrenceFault> {
    let mut bytes = [0_u8; COMPACT_DECLARATION_BYTES];
    bytes.copy_from_slice(reader.take(COMPACT_DECLARATION_BYTES)?);
    Ok(VariantFingerprint::from_raw(bytes))
}

use crate::ir_vocabulary::RelSpan;
