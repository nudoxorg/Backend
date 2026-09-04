//! Admission, wire encoding, and trusted reopen validation of the pooled
//! reference lanes shared by a fragment's language-extension section.
//!
//! The extension section's typed facts reference four pooled lanes by dense
//! coordinate: type parameters, atom lists, type lists, and entity lists.
//! This plane carries those lanes and proves every reference stays inside
//! the carrying fragment's atom, type-fact, and entity lanes.

use thiserror::Error;

const PRESENCE_NONE: u8 = 0;
const PRESENCE_SOME: u8 = 1;

/// One borrowed type parameter: a name plus optional constraint and default
/// type references into the fragment's type-fact lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionTypeParameter<'bytes> {
    /// Parameter name spelling.
    pub name: &'bytes [u8],
    /// Constraint type-fact ordinal, when the source wrote one.
    pub constraint: Option<u32>,
    /// Default type-fact ordinal, when the source wrote one.
    pub default: Option<u32>,
}

/// One borrowed pooled reference list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionRefList<'bytes> {
    /// Ordered references into the atom, type-fact, or entity lane.
    pub elements: &'bytes [u32],
}

/// The admitted pooled lanes of one fragment's extension section.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionPoolsLane<'bytes> {
    /// Pooled type parameters.
    pub type_parameters: &'bytes [ExtensionTypeParameter<'bytes>],
    /// Pooled atom-reference lists.
    pub atom_lists: &'bytes [ExtensionRefList<'bytes>],
    /// Pooled type-reference lists.
    pub type_lists: &'bytes [ExtensionRefList<'bytes>],
    /// Pooled entity-reference lists.
    pub entity_lists: &'bytes [ExtensionRefList<'bytes>],
}

/// Exact pooled-lane rejection retaining the offending ordinal and operand.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ExtensionPoolFault {
    #[error("type parameter {ordinal} has an empty name")]
    EmptyName { ordinal: u32 },
    #[error("type parameter {ordinal} {field} references type fact {raw} outside {limit}")]
    TypeReference {
        ordinal: u32,
        field: &'static str,
        raw: u32,
        limit: u32,
    },
    #[error("{lane} list {list} element {position} references {raw} outside {limit}")]
    Reference {
        lane: &'static str,
        list: u32,
        position: u32,
        raw: u32,
        limit: u32,
    },
    #[error("pooled-lane payload is truncated before {needed} bytes")]
    Truncated { needed: usize },
    #[error("pooled-lane payload carries trailing bytes after four lanes")]
    TrailingBytes,
    #[error("pooled-lane payload carries an unknown presence tag {actual}")]
    Presence { actual: u8 },
}

impl<'bytes> ExtensionPoolsLane<'bytes> {
    /// Admits the pooled lanes against the carrying fragment's lane counts.
    pub fn admit(
        &self,
        atom_count: u32,
        type_count: u32,
        entity_count: u32,
    ) -> Result<(), ExtensionPoolFault> {
        for (ordinal, parameter) in self.type_parameters.iter().enumerate() {
            let ordinal = u32::try_from(ordinal).unwrap_or(u32::MAX);
            if parameter.name.is_empty() {
                return Err(ExtensionPoolFault::EmptyName { ordinal });
            }
            for (field, raw) in [
                ("constraint", parameter.constraint),
                ("default", parameter.default),
            ] {
                if raw.is_some_and(|raw| raw >= type_count) {
                    return Err(ExtensionPoolFault::TypeReference {
                        ordinal,
                        field,
                        raw: raw.unwrap_or_default(),
                        limit: type_count,
                    });
                }
            }
        }
        for (lane, lists, limit) in [
            ("atom_lists", self.atom_lists, atom_count),
            ("type_lists", self.type_lists, type_count),
            ("entity_lists", self.entity_lists, entity_count),
        ] {
            for (list, refs) in lists.iter().enumerate() {
                let list = u32::try_from(list).unwrap_or(u32::MAX);
                for (position, raw) in refs.elements.iter().enumerate() {
                    if *raw >= limit {
                        return Err(ExtensionPoolFault::Reference {
                            lane,
                            list,
                            position: u32::try_from(position).unwrap_or(u32::MAX),
                            raw: *raw,
                            limit,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// The exact serialized payload length for these pooled lanes.
    #[must_use]
    pub fn payload_len(&self) -> usize {
        let mut length = 4;
        for parameter in self.type_parameters {
            length += 5 + parameter.name.len();
            length += optional_len(parameter.constraint);
            length += optional_len(parameter.default);
        }
        for lane in [self.atom_lists, self.type_lists, self.entity_lists] {
            length += 4;
            for list in lane {
                length += 4 + 4 * list.elements.len();
            }
        }
        length
    }

    /// Serializes the pooled lanes in canonical order. The caller-provided
    /// payload must measure exactly [`ExtensionPoolsLane::payload_len`];
    /// admission proved every cell.
    pub fn write_payload(&self, payload: &mut [u8]) {
        let mut cursor = write_u32(
            payload,
            0,
            u32::try_from(self.type_parameters.len()).unwrap_or(u32::MAX),
        );
        for parameter in self.type_parameters {
            cursor = write_cell(payload, cursor, parameter.name);
            cursor = write_optional(payload, cursor, parameter.constraint);
            cursor = write_optional(payload, cursor, parameter.default);
        }
        for lane in [self.atom_lists, self.type_lists, self.entity_lists] {
            cursor = write_u32(
                payload,
                cursor,
                u32::try_from(lane.len()).unwrap_or(u32::MAX),
            );
            for list in lane {
                cursor = write_u32(
                    payload,
                    cursor,
                    u32::try_from(list.elements.len()).unwrap_or(u32::MAX),
                );
                for raw in list.elements {
                    cursor = write_u32(payload, cursor, *raw);
                }
            }
        }
    }
}

/// One decoded pooled reference list, read back in bounded pieces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedRefList<'payload> {
    /// The list bytes, a dense run of little-endian `u32` references.
    pub words: &'payload [u8],
}

/// Closed identity of one homogeneous extension reference-list lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionPoolListLane {
    Atoms,
    Types,
    Entities,
}

impl ExtensionPoolListLane {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::Atoms => "atom_lists",
            Self::Types => "type_lists",
            Self::Entities => "entity_lists",
        }
    }
}

impl DecodedRefList<'_> {
    /// Number of references in the list.
    #[must_use]
    pub fn len(&self) -> usize {
        self.words.len() / 4
    }

    /// True when the list carries no references.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Decodes one reference by position.
    #[must_use]
    pub fn get(&self, position: usize) -> Option<u32> {
        let raw = self.words.get(4 * position..4 * position + 4)?;
        Some(u32::from_le_bytes(raw.try_into().ok()?))
    }

    /// Iterates every reference in list order.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.words
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
    }
}

/// One decoded type parameter, lending the section bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedTypeParameter<'payload> {
    /// Parameter name spelling.
    pub name: &'payload [u8],
    /// Constraint type-fact ordinal, when present.
    pub constraint: Option<u32>,
    /// Default type-fact ordinal, when present.
    pub default: Option<u32>,
}

/// A once-validated reopened pooled-lane section.
#[derive(Clone, Copy, Debug)]
pub struct ReopenedExtensionPools<'payload> {
    bytes: &'payload [u8],
    type_parameters: (usize, u32),
    atom_lists: (usize, u32),
    type_lists: (usize, u32),
    entity_lists: (usize, u32),
}

impl<'payload> ReopenedExtensionPools<'payload> {
    /// Number of pooled type parameters.
    #[must_use]
    pub fn type_parameter_count(&self) -> u32 {
        self.type_parameters.1
    }

    /// Number of pooled atom-reference lists.
    #[must_use]
    pub fn atom_list_count(&self) -> u32 {
        self.atom_lists.1
    }

    /// Number of pooled type-reference lists.
    #[must_use]
    pub fn type_list_count(&self) -> u32 {
        self.type_lists.1
    }

    /// Number of pooled entity-reference lists.
    #[must_use]
    pub fn entity_list_count(&self) -> u32 {
        self.entity_lists.1
    }

    /// Decodes one pooled type parameter.
    pub fn type_parameter(
        &self,
        ordinal: u32,
    ) -> Result<DecodedTypeParameter<'payload>, ExtensionPoolFault> {
        if ordinal >= self.type_parameters.1 {
            return Err(ExtensionPoolFault::Truncated { needed: usize::MAX });
        }
        let mut cursor = self.type_parameters.0;
        for _ in 0..ordinal {
            cursor = skip_type_parameter(self.bytes, cursor)?;
        }
        let name_len = usize::try_from(read_u32(self.bytes, cursor + 1)?)
            .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
        let name = self
            .bytes
            .get(cursor + 5..cursor + 5 + name_len)
            .ok_or(ExtensionPoolFault::Truncated { needed: cursor })?;
        let mut at = cursor + 5 + name_len;
        let constraint = read_optional(self.bytes, &mut at)?;
        let default = read_optional(self.bytes, &mut at)?;
        Ok(DecodedTypeParameter {
            name,
            constraint,
            default,
        })
    }

    /// Decodes one pooled reference list of the named lane.
    pub fn reference_list(
        &self,
        lane: &'static str,
        ordinal: u32,
    ) -> Result<DecodedRefList<'payload>, ExtensionPoolFault> {
        let (start, count) = match lane {
            "atom_lists" => self.atom_lists,
            "type_lists" => self.type_lists,
            "entity_lists" => self.entity_lists,
            _ => return Err(ExtensionPoolFault::Truncated { needed: usize::MAX }),
        };
        if ordinal >= count {
            return Err(ExtensionPoolFault::Truncated { needed: usize::MAX });
        }
        let mut cursor = start;
        for _ in 0..ordinal {
            cursor = skip_ref_list(self.bytes, cursor)?;
        }
        let len = usize::try_from(read_u32(self.bytes, cursor)?)
            .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
        let words = self
            .bytes
            .get(cursor + 4..cursor + 4 + 4 * len)
            .ok_or(ExtensionPoolFault::Truncated { needed: cursor })?;
        Ok(DecodedRefList { words })
    }

    /// Reads one homogeneous pooled list without admitting a stringly lane
    /// selection at a render or discovery call site.
    pub fn list(
        &self,
        lane: ExtensionPoolListLane,
        ordinal: u32,
    ) -> Result<DecodedRefList<'payload>, ExtensionPoolFault> {
        self.reference_list(lane.wire_name(), ordinal)
    }

    pub fn atom_list(&self, ordinal: u32) -> Result<DecodedRefList<'payload>, ExtensionPoolFault> {
        self.list(ExtensionPoolListLane::Atoms, ordinal)
    }

    pub fn type_list(&self, ordinal: u32) -> Result<DecodedRefList<'payload>, ExtensionPoolFault> {
        self.list(ExtensionPoolListLane::Types, ordinal)
    }

    pub fn entity_list(
        &self,
        ordinal: u32,
    ) -> Result<DecodedRefList<'payload>, ExtensionPoolFault> {
        self.list(ExtensionPoolListLane::Entities, ordinal)
    }
}

/// Reopens the shared extension pools after proving every byte and reference
/// against the carrying fragment's already-validated common lanes.
pub fn reopen_extension_pools(
    payload: &[u8],
    atom_count: u32,
    type_count: u32,
    entity_count: u32,
) -> Result<ReopenedExtensionPools<'_>, ExtensionPoolFault> {
    validate_extension_pool_payload(payload, atom_count, type_count, entity_count)?;
    let type_count = read_u32(payload, 0)?;
    let mut cursor = 4;
    let type_parameters = (cursor, type_count);
    for _ in 0..type_count {
        cursor = skip_type_parameter(payload, cursor)?;
    }
    let atom_count = read_u32(payload, cursor)?;
    cursor += 4;
    let atom_lists = (cursor, atom_count);
    for _ in 0..atom_count {
        cursor = skip_ref_list(payload, cursor)?;
    }
    let type_count = read_u32(payload, cursor)?;
    cursor += 4;
    let type_lists = (cursor, type_count);
    for _ in 0..type_count {
        cursor = skip_ref_list(payload, cursor)?;
    }
    let entity_count = read_u32(payload, cursor)?;
    cursor += 4;
    let entity_lists = (cursor, entity_count);
    Ok(ReopenedExtensionPools {
        bytes: payload,
        type_parameters,
        atom_lists,
        type_lists,
        entity_lists,
    })
}

/// Validates one pooled-lane payload against the carrying fragment's lane
/// counts, proving every reference and type-parameter cell.
pub(crate) fn validate_extension_pool_payload(
    payload: &[u8],
    atom_count: u32,
    type_count: u32,
    entity_count: u32,
) -> Result<(), ExtensionPoolFault> {
    let mut cursor = 4;
    let type_parameter_count = read_u32(payload, 0)?;
    for ordinal in 0..type_parameter_count {
        let name_len = usize::try_from(read_u32(payload, cursor + 1)?)
            .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
        let name = payload
            .get(cursor + 5..cursor + 5 + name_len)
            .ok_or(ExtensionPoolFault::Truncated { needed: cursor })?;
        if name.is_empty() {
            return Err(ExtensionPoolFault::EmptyName { ordinal });
        }
        let mut at = cursor + 5 + name_len;
        for field in ["constraint", "default"] {
            let Some(raw) = read_optional(payload, &mut at)? else {
                continue;
            };
            if raw >= type_count {
                return Err(ExtensionPoolFault::TypeReference {
                    ordinal,
                    field,
                    raw,
                    limit: type_count,
                });
            }
        }
        cursor = at;
    }
    for (lane, limit) in [
        ("atom_lists", atom_count),
        ("type_lists", type_count),
        ("entity_lists", entity_count),
    ] {
        let count = read_u32(payload, cursor)
            .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
        cursor += 4;
        for list in 0..count {
            let len = usize::try_from(read_u32(payload, cursor)?)
                .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
            for position in 0..len {
                let raw = read_u32(payload, cursor + 4 + 4 * position)?;
                if raw >= limit {
                    return Err(ExtensionPoolFault::Reference {
                        lane,
                        list,
                        position: u32::try_from(position).unwrap_or(u32::MAX),
                        raw,
                        limit,
                    });
                }
            }
            cursor += 4 + 4 * len;
        }
    }
    if cursor != payload.len() {
        return Err(ExtensionPoolFault::TrailingBytes);
    }
    Ok(())
}

fn read_u32(bytes: &[u8], at: usize) -> Result<u32, ExtensionPoolFault> {
    let end = at
        .checked_add(4)
        .ok_or(ExtensionPoolFault::Truncated { needed: at })?;
    let word: [u8; 4] = bytes
        .get(at..end)
        .ok_or(ExtensionPoolFault::Truncated { needed: at })?
        .try_into()
        .map_err(|_| ExtensionPoolFault::Truncated { needed: at })?;
    Ok(u32::from_le_bytes(word))
}

fn skip_type_parameter(bytes: &[u8], cursor: usize) -> Result<usize, ExtensionPoolFault> {
    if bytes.get(cursor).copied() != Some(PRESENCE_SOME) {
        return Err(ExtensionPoolFault::Presence {
            actual: bytes.get(cursor).copied().unwrap_or(PRESENCE_NONE),
        });
    }
    let name_len = usize::try_from(read_u32(bytes, cursor + 1)?)
        .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
    let mut next = cursor
        .checked_add(5)
        .and_then(|at| at.checked_add(name_len))
        .ok_or(ExtensionPoolFault::Truncated { needed: cursor })?;
    // Optional operands are variable-width: `None` occupies its tag only,
    // while `Some` carries four more bytes. Reuse the same parser used by the
    // public decoder rather than embedding a false fixed stride here.
    let _ = read_optional(bytes, &mut next)?;
    let _ = read_optional(bytes, &mut next)?;
    Ok(next)
}

fn skip_ref_list(bytes: &[u8], cursor: usize) -> Result<usize, ExtensionPoolFault> {
    let len = usize::try_from(read_u32(bytes, cursor)?)
        .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
    Ok(cursor + 4 + 4 * len)
}

fn read_optional(bytes: &[u8], at: &mut usize) -> Result<Option<u32>, ExtensionPoolFault> {
    match bytes.get(*at).copied() {
        Some(PRESENCE_NONE) => {
            *at += 1;
            Ok(None)
        }
        Some(PRESENCE_SOME) => {
            let raw = read_u32(bytes, *at + 1)?;
            *at += 5;
            Ok(Some(raw))
        }
        actual => Err(ExtensionPoolFault::Presence {
            actual: actual.unwrap_or(PRESENCE_NONE),
        }),
    }
}

fn write_u32(payload: &mut [u8], at: usize, value: u32) -> usize {
    payload[at..at + 4].copy_from_slice(&value.to_le_bytes());
    at + 4
}

/// Exact serialized width of one optional type-reference cell.
const fn optional_len(value: Option<u32>) -> usize {
    match value {
        None => 1,
        Some(_) => 5,
    }
}

fn write_cell(payload: &mut [u8], cursor: usize, bytes: &[u8]) -> usize {
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    payload[cursor] = PRESENCE_SOME;
    payload[cursor + 1..cursor + 5].copy_from_slice(&len.to_le_bytes());
    payload[cursor + 5..cursor + 5 + bytes.len()].copy_from_slice(bytes);
    cursor + 5 + bytes.len()
}

fn write_optional(payload: &mut [u8], cursor: usize, value: Option<u32>) -> usize {
    match value {
        None => {
            payload[cursor] = PRESENCE_NONE;
            cursor + 1
        }
        Some(raw) => {
            payload[cursor] = PRESENCE_SOME;
            payload[cursor + 1..cursor + 5].copy_from_slice(&raw.to_le_bytes());
            cursor + 5
        }
    }
}
