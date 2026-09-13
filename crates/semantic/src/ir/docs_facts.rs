//! Admission, wire encoding, and trusted reopen validation of the
//! documentation plane: ordered doc fragments bound to entity rows.
//!
//! Like the occurrence plane this is a fact lane, not a canonicalized graph:
//! records are admitted once (validated with exact faults), written in lane
//! order, and revalidated identically on reopen. Lane order is wire order,
//! so one admitted fact set always writes identical section bytes.

use crate::ir_vocabulary::EntityId;
use thiserror::Error;

/// Wire tag of a text doc fragment.
pub(crate) const DOC_TEXT_TAG: u8 = 0;
/// Wire tag of a code doc fragment.
pub(crate) const DOC_CODE_TAG: u8 = 1;
/// Wire tag of a doc link fragment.
pub(crate) const DOC_LINK_TAG: u8 = 2;
/// Wire tag of a soft line break.
pub(crate) const DOC_SOFT_BREAK_TAG: u8 = 3;
/// Wire tag of a hard line break.
pub(crate) const DOC_HARD_BREAK_TAG: u8 = 4;
/// Wire tag of a same-fragment doc link target.
pub(crate) const LINK_LOCAL_TAG: u8 = 0;
/// Wire tag of a foreign doc link target.
pub(crate) const LINK_FOREIGN_TAG: u8 = 1;
/// Presence sentinel shared by the optional link cells.
const PRESENCE_NONE: u8 = 0;
const PRESENCE_SOME: u8 = 1;

/// Target of one documentation link.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocLinkTarget<'bytes> {
    /// A declaration inside the same fragment.
    Local(EntityId),
    /// A foreign symbol spelled by ecosystem and `/`-separated path.
    Foreign {
        /// Ecosystem registry name (`cargo`, `npm`, `pypi`, `go`, `nuget`,
        /// `maven`, ...).
        ecosystem: &'bytes [u8],
        /// Canonical cross-package path of the target.
        path: &'bytes [u8],
    },
}

/// One borrowed documentation fragment accepted from a frontend without an
/// intermediate string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocFragmentInput<'bytes> {
    /// A prose run.
    Text(&'bytes [u8]),
    /// An inline code run.
    Code(&'bytes [u8]),
    /// A named link.
    Link {
        /// Link label text.
        label: &'bytes [u8],
        /// Link target.
        target: DocLinkTarget<'bytes>,
    },
    /// A soft line break inside one paragraph.
    SoftBreak,
    /// A hard line break.
    HardBreak,
}

/// One admitted documentation fact bound to its owning entity ordinal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocFactInput<'bytes> {
    /// Owning entity row.
    pub owner: EntityId,
    /// The documentation fragment.
    pub fragment: DocFragmentInput<'bytes>,
}

/// The admitted documentation lane: ordered facts whose lane order is wire
/// order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentationLane<'bytes> {
    /// Ordered documentation facts.
    pub inputs: &'bytes [DocFactInput<'bytes>],
}

/// Exact documentation-plane rejection retaining the offending ordinal and
/// every observed operand.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum DocFactFault {
    #[error("doc fact {ordinal} names entity {owner:?} outside {entity_count}")]
    Owner {
        ordinal: u32,
        owner: EntityId,
        entity_count: u32,
    },
    #[error("doc fact {ordinal} carries an unknown fragment tag {actual}")]
    FragmentTag { ordinal: u32, actual: u8 },
    #[error("doc fact {ordinal} carries an unknown link target tag {actual}")]
    LinkTag { ordinal: u32, actual: u8 },
    #[error("doc fact {ordinal} carries empty {field} bytes")]
    EmptyCell { ordinal: u32, field: &'static str },
    #[error("doc fact {ordinal} names local link target {target} outside {entity_count}")]
    LinkTarget {
        ordinal: u32,
        target: u32,
        entity_count: u32,
    },
    #[error("doc fact {ordinal} is truncated before {needed} bytes")]
    Truncated { ordinal: u32, needed: usize },
    #[error("doc section declares {declared} records but carries trailing bytes")]
    TrailingBytes { declared: u32 },
    #[error("doc fact {ordinal} carries an unknown presence tag {actual}")]
    Presence { ordinal: u32, actual: u8 },
}

impl<'bytes> DocumentationLane<'bytes> {
    /// Admits the lane against an entity lane of `entity_count` rows: every
    /// owner ordinal must name a declared entity, text/code/label cells must
    /// be non-empty, and local link targets must name declared entities.
    pub fn admit(&self, entity_count: u32) -> Result<(), DocFactFault> {
        for (position, input) in self.inputs.iter().enumerate() {
            let ordinal = u32::try_from(position).unwrap_or(u32::MAX);
            if input.owner.raw >= entity_count {
                return Err(DocFactFault::Owner {
                    ordinal,
                    owner: input.owner,
                    entity_count,
                });
            }
            match input.fragment {
                DocFragmentInput::Text(bytes) | DocFragmentInput::Code(bytes) => {
                    if bytes.is_empty() {
                        return Err(DocFactFault::EmptyCell {
                            ordinal,
                            field: "text",
                        });
                    }
                }
                DocFragmentInput::Link { label, target } => {
                    if label.is_empty() {
                        return Err(DocFactFault::EmptyCell {
                            ordinal,
                            field: "label",
                        });
                    }
                    match target {
                        DocLinkTarget::Local(target) => {
                            if target.raw >= entity_count {
                                return Err(DocFactFault::LinkTarget {
                                    ordinal,
                                    target: target.raw,
                                    entity_count,
                                });
                            }
                        }
                        DocLinkTarget::Foreign { ecosystem, path } => {
                            if ecosystem.is_empty() {
                                return Err(DocFactFault::EmptyCell {
                                    ordinal,
                                    field: "ecosystem",
                                });
                            }
                            if path.is_empty() {
                                return Err(DocFactFault::EmptyCell {
                                    ordinal,
                                    field: "path",
                                });
                            }
                        }
                    }
                }
                DocFragmentInput::SoftBreak | DocFragmentInput::HardBreak => {}
            }
        }
        Ok(())
    }

    /// The exact serialized payload length for this lane.
    #[must_use]
    pub fn payload_len(&self) -> usize {
        let mut length = 4;
        for input in self.inputs {
            length += 4 + 1;
            match input.fragment {
                DocFragmentInput::Text(bytes) | DocFragmentInput::Code(bytes) => {
                    length += cell_len(bytes);
                }
                DocFragmentInput::Link { label, target } => {
                    length += cell_len(label);
                    match target {
                        DocLinkTarget::Local(_) => length += 1 + 4,
                        DocLinkTarget::Foreign { ecosystem, path } => {
                            length += 1 + cell_len(ecosystem) + cell_len(path);
                        }
                    }
                }
                DocFragmentInput::SoftBreak | DocFragmentInput::HardBreak => {}
            }
        }
        length
    }

    /// Serializes the complete section payload in lane order. The
    /// caller-provided payload must measure exactly
    /// [`DocumentationLane::payload_len`]; admission proved every cell.
    pub fn write_payload(&self, payload: &mut [u8]) {
        let count = u32::try_from(self.inputs.len()).unwrap_or(u32::MAX);
        payload[..4].copy_from_slice(&count.to_le_bytes());
        let mut cursor = 4;
        for input in self.inputs {
            payload[cursor..cursor + 4].copy_from_slice(&input.owner.raw.to_le_bytes());
            cursor += 4;
            match input.fragment {
                DocFragmentInput::Text(bytes) => {
                    payload[cursor] = DOC_TEXT_TAG;
                    cursor += 1;
                    cursor = write_cell(payload, cursor, bytes);
                }
                DocFragmentInput::Code(bytes) => {
                    payload[cursor] = DOC_CODE_TAG;
                    cursor += 1;
                    cursor = write_cell(payload, cursor, bytes);
                }
                DocFragmentInput::Link { label, target } => {
                    payload[cursor] = DOC_LINK_TAG;
                    cursor += 1;
                    cursor = write_cell(payload, cursor, label);
                    match target {
                        DocLinkTarget::Local(target) => {
                            payload[cursor] = LINK_LOCAL_TAG;
                            cursor += 1;
                            payload[cursor..cursor + 4].copy_from_slice(&target.raw.to_le_bytes());
                            cursor += 4;
                        }
                        DocLinkTarget::Foreign { ecosystem, path } => {
                            payload[cursor] = LINK_FOREIGN_TAG;
                            cursor += 1;
                            cursor = write_cell(payload, cursor, ecosystem);
                            cursor = write_cell(payload, cursor, path);
                        }
                    }
                }
                DocFragmentInput::SoftBreak => {
                    payload[cursor] = DOC_SOFT_BREAK_TAG;
                    cursor += 1;
                }
                DocFragmentInput::HardBreak => {
                    payload[cursor] = DOC_HARD_BREAK_TAG;
                    cursor += 1;
                }
            }
        }
    }
}

fn cell_len(bytes: &[u8]) -> usize {
    1 + 4 + bytes.len()
}

fn write_cell(payload: &mut [u8], cursor: usize, bytes: &[u8]) -> usize {
    payload[cursor] = PRESENCE_SOME;
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    payload[cursor + 1..cursor + 5].copy_from_slice(&len.to_le_bytes());
    payload[cursor + 5..cursor + 5 + bytes.len()].copy_from_slice(bytes);
    cursor + 5 + bytes.len()
}

/// One decoded doc fact read back from a reopened fragment, lending envelope
/// bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedDocFact<'fragment> {
    /// Owning entity ordinal.
    pub owner: EntityId,
    /// The decoded documentation fragment lending envelope bytes.
    pub fragment: DocFragmentInput<'fragment>,
}

/// Validates one documentation section payload against an entity lane of
/// `entity_count` rows.
pub(crate) fn validate_doc_payload(payload: &[u8], entity_count: u32) -> Result<(), DocFactFault> {
    let mut reader = DocReader {
        payload,
        cursor: 0,
        ordinal: 0,
    };
    let declared = reader.u32().map_err(|_| DocFactFault::Truncated {
        ordinal: 0,
        needed: 4,
    })?;
    for ordinal in 0..declared {
        reader.ordinal = ordinal;
        let owner = reader.u32()?;
        if owner >= entity_count {
            return Err(DocFactFault::Owner {
                ordinal,
                owner: EntityId::new(owner),
                entity_count,
            });
        }
        match reader.u8()? {
            DOC_TEXT_TAG | DOC_CODE_TAG => {
                let bytes = reader.cell()?;
                if bytes.is_empty() {
                    return Err(DocFactFault::EmptyCell {
                        ordinal,
                        field: "text",
                    });
                }
            }
            DOC_LINK_TAG => {
                let label = reader.cell()?;
                if label.is_empty() {
                    return Err(DocFactFault::EmptyCell {
                        ordinal,
                        field: "label",
                    });
                }
                match reader.u8()? {
                    LINK_LOCAL_TAG => {
                        let target = reader.u32()?;
                        if target >= entity_count {
                            return Err(DocFactFault::LinkTarget {
                                ordinal,
                                target,
                                entity_count,
                            });
                        }
                    }
                    LINK_FOREIGN_TAG => {
                        let ecosystem = reader.cell()?;
                        if ecosystem.is_empty() {
                            return Err(DocFactFault::EmptyCell {
                                ordinal,
                                field: "ecosystem",
                            });
                        }
                        let path = reader.cell()?;
                        if path.is_empty() {
                            return Err(DocFactFault::EmptyCell {
                                ordinal,
                                field: "path",
                            });
                        }
                    }
                    actual => {
                        return Err(DocFactFault::LinkTag { ordinal, actual });
                    }
                }
            }
            DOC_SOFT_BREAK_TAG | DOC_HARD_BREAK_TAG => {}
            actual => {
                return Err(DocFactFault::FragmentTag { ordinal, actual });
            }
        }
    }
    if reader.cursor != payload.len() {
        return Err(DocFactFault::TrailingBytes { declared });
    }
    Ok(())
}

/// Lazily decodes one validated documentation section, lending envelope bytes.
pub struct DocFactCursor<'fragment> {
    payload: &'fragment [u8],
    cursor: usize,
    remaining: u32,
}

impl<'fragment> DocFactCursor<'fragment> {
    /// Creates the cursor over a validated section payload.
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
}

impl<'fragment> Iterator for DocFactCursor<'fragment> {
    type Item = Result<DecodedDocFact<'fragment>, DocFactFault>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let mut reader = DocReader {
            payload: self.payload,
            cursor: self.cursor,
            ordinal: 0,
        };
        let decoded = decode_fact(&mut reader);
        self.cursor = reader.cursor;
        Some(decoded)
    }
}

fn decode_fact<'fragment>(
    reader: &mut DocReader<'fragment>,
) -> Result<DecodedDocFact<'fragment>, DocFactFault> {
    let owner = reader.u32()?;
    let fragment = match reader.u8()? {
        DOC_TEXT_TAG => DocFragmentInput::Text(reader.cell()?),
        DOC_CODE_TAG => DocFragmentInput::Code(reader.cell()?),
        DOC_LINK_TAG => {
            let label = reader.cell()?;
            let target = match reader.u8()? {
                LINK_LOCAL_TAG => DocLinkTarget::Local(EntityId::new(reader.u32()?)),
                LINK_FOREIGN_TAG => DocLinkTarget::Foreign {
                    ecosystem: reader.cell()?,
                    path: reader.cell()?,
                },
                actual => {
                    return Err(DocFactFault::LinkTag {
                        ordinal: reader.ordinal,
                        actual,
                    });
                }
            };
            DocFragmentInput::Link { label, target }
        }
        DOC_SOFT_BREAK_TAG => DocFragmentInput::SoftBreak,
        DOC_HARD_BREAK_TAG => DocFragmentInput::HardBreak,
        actual => {
            return Err(DocFactFault::FragmentTag {
                ordinal: reader.ordinal,
                actual,
            });
        }
    };
    Ok(DecodedDocFact {
        owner: EntityId::new(owner),
        fragment,
    })
}

struct DocReader<'payload> {
    payload: &'payload [u8],
    cursor: usize,
    ordinal: u32,
}

impl<'payload> DocReader<'payload> {
    fn take(&mut self, count: usize) -> Result<&'payload [u8], DocFactFault> {
        let end = self
            .cursor
            .checked_add(count)
            .ok_or(DocFactFault::Truncated {
                ordinal: self.ordinal,
                needed: usize::MAX,
            })?;
        let cell = self
            .payload
            .get(self.cursor..end)
            .ok_or(DocFactFault::Truncated {
                ordinal: self.ordinal,
                needed: end,
            })?;
        self.cursor = end;
        Ok(cell)
    }

    fn u8(&mut self) -> Result<u8, DocFactFault> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, DocFactFault> {
        let mut raw = [0_u8; 4];
        raw.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(raw))
    }

    fn cell(&mut self) -> Result<&'payload [u8], DocFactFault> {
        match self.u8()? {
            PRESENCE_NONE => Ok(&[]),
            PRESENCE_SOME => {
                let length = usize::try_from(self.u32()?).unwrap_or(usize::MAX);
                self.take(length)
            }
            actual => Err(DocFactFault::Presence {
                ordinal: self.ordinal,
                actual,
            }),
        }
    }
}
