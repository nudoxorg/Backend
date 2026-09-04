//! Admission, wire encoding, and trusted reopen validation of the pooled
//! reference lanes shared by a fragment's language-extension section.
//!
//! The extension section's typed facts reference five pooled lanes by dense
//! coordinate: type-parameter elements, type-parameter ranges, atom lists,
//! type lists, and entity lists.
//! This plane carries those lanes and proves every reference stays inside
//! the carrying fragment's atom, type-fact, and entity lanes.

use core::ops::Deref;

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

/// One exact dense run in the pooled type-parameter element lane.
///
/// `TypeParameterListId` names this table in schema 4 and later.  The range
/// is deliberately distinct from an element offset: `(0, 0)` is a real empty
/// list and cannot alias a later nonempty list that happens to begin at zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionTypeParameterRange {
    /// First pooled type-parameter element.
    pub start: u32,
    /// Number of elements in this list.
    pub length: u32,
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
    /// Exact type-parameter list ranges.  Fresh schema-4 fragments address
    /// this dense table through `TypeParameterListId`.
    pub type_parameter_lists: &'bytes [ExtensionTypeParameterRange],
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
    #[error("type parameter {ordinal} {field:?} references type fact {raw} outside {limit}")]
    TypeReference {
        ordinal: u32,
        field: TypeParameterField,
        raw: u32,
        limit: u32,
    },
    #[error(
        "type-parameter list {list} range start {start} length {length} exceeds {element_count} pooled elements"
    )]
    TypeParameterRange {
        list: u32,
        start: u32,
        length: u32,
        element_count: u32,
    },
    #[error("type-parameter list {list} is outside {count} exact ranges")]
    TypeParameterList {
        list: u32,
        count: u32,
    },
    #[error("type-parameter element {ordinal} is outside {count} pooled elements")]
    TypeParameterElement { ordinal: u32, count: u32 },
    #[error("type-parameter list member {position} is outside its length {length}")]
    TypeParameterPosition { position: u32, length: u32 },
    #[error("legacy type-parameter start {start} is outside {element_count} pooled elements")]
    LegacyTypeParameterStart {
        start: u32,
        element_count: u32,
    },
    #[error("{lane:?} list {list} element {position} references {raw} outside {limit}")]
    Reference {
        lane: ExtensionPoolListLane,
        list: u32,
        position: u32,
        raw: u32,
        limit: u32,
    },
    #[error("pooled-lane payload is truncated before {needed} bytes")]
    Truncated { needed: usize },
    #[error("pooled-lane payload carries trailing bytes after its declared lanes")]
    TrailingBytes,
    #[error("pooled-lane payload carries an unknown presence tag {actual}")]
    Presence { actual: u8 },
    #[error("pooled-lane cursor arithmetic overflowed at byte {at}")]
    StructuralOverflow { at: usize },
}

/// Closed optional type-reference field in one type-parameter element.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeParameterField {
    Constraint,
    Default,
}

/// Exact reference bound interpretation exposed by a reopened pool section.
///
/// Schema 1--3 stored type-parameter *element starts* in extension facts.
/// Those starts do not encode a list length, particularly for empty lists, so
/// legacy fragments are intentionally not upgraded to an exact list view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeParameterListBounds {
    /// Schema 4+ dense range-table IDs.
    ExactRanges { count: u32 },
    /// Schema 1--3 start-only coordinates into the element lane.
    LegacyStarts { element_count: u32 },
}

impl<'bytes> ExtensionPoolsLane<'bytes> {
    /// Admits the schema-4 pooled lanes against the carrying fragment's lane
    /// counts. Legacy schemas are reopen-only and cannot be encoded through
    /// this current-write input.
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
                (TypeParameterField::Constraint, parameter.constraint),
                (TypeParameterField::Default, parameter.default),
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
        let element_count = u32::try_from(self.type_parameters.len()).unwrap_or(u32::MAX);
        for (list, range) in self.type_parameter_lists.iter().enumerate() {
            let list = u32::try_from(list).unwrap_or(u32::MAX);
            let end = range.start.checked_add(range.length).ok_or(
                ExtensionPoolFault::TypeParameterRange {
                    list,
                    start: range.start,
                    length: range.length,
                    element_count,
                },
            )?;
            if end > element_count {
                return Err(ExtensionPoolFault::TypeParameterRange {
                    list,
                    start: range.start,
                    length: range.length,
                    element_count,
                });
            }
        }
        for (lane, lists, limit) in [
            (ExtensionPoolListLane::Atoms, self.atom_lists, atom_count),
            (ExtensionPoolListLane::Types, self.type_lists, type_count),
            (ExtensionPoolListLane::Entities, self.entity_lists, entity_count),
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

    /// The exact schema-4 serialized payload length for these pooled lanes.
    #[must_use]
    pub fn payload_len(&self) -> usize {
        let mut length = 4;
        for parameter in self.type_parameters {
            length += 5 + parameter.name.len();
            length += optional_len(parameter.constraint);
            length += optional_len(parameter.default);
        }
        length += 4 + 8 * self.type_parameter_lists.len();
        for lane in [self.atom_lists, self.type_lists, self.entity_lists] {
            length += 4;
            for list in lane {
                length += 4 + 4 * list.elements.len();
            }
        }
        length
    }

    /// Serializes the schema-4 pooled lanes in canonical order. The caller-provided
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
        cursor = write_u32(
            payload,
            cursor,
            u32::try_from(self.type_parameter_lists.len()).unwrap_or(u32::MAX),
        );
        for range in self.type_parameter_lists {
            cursor = write_u32(payload, cursor, range.start);
            cursor = write_u32(payload, cursor, range.length);
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
        let start = position.checked_mul(4)?;
        let end = start.checked_add(4)?;
        let raw = self.words.get(start..end)?;
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

/// One exact borrowed type-parameter list reopened from a schema-4 range
/// table.  Its `get` method proves the returned member stays in the captured
/// range; callers never infer an end from a global element cursor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedTypeParameterList<'payload> {
    bytes: &'payload [u8],
    elements_start: usize,
    element_count: u32,
    range: ExtensionTypeParameterRange,
}

impl<'payload> DecodedTypeParameterList<'payload> {
    /// Decodes one member of this exact list.
    pub fn get(
        &self,
        position: u32,
    ) -> Result<DecodedTypeParameter<'payload>, ExtensionPoolFault> {
        if position >= self.range.length {
            return Err(ExtensionPoolFault::TypeParameterPosition {
                position,
                length: self.range.length,
            });
        }
        let ordinal = self
            .range
            .start
            .checked_add(position)
            .ok_or(ExtensionPoolFault::StructuralOverflow {
                at: self.elements_start,
            })?;
        decode_type_parameter(
            self.bytes,
            self.elements_start,
            self.element_count,
            ordinal,
        )
    }

    /// Opens one sequential cursor over this exact range.  The cursor seeks
    /// the global first element once, then advances each variable-width row
    /// exactly once; full-list discovery is linear and allocation-free.
    pub fn cursor(&self) -> Result<DecodedTypeParameterCursor<'payload>, ExtensionPoolFault> {
        let mut cursor = self.elements_start;
        for _ in 0..self.range.start {
            cursor = skip_type_parameter(self.bytes, cursor)?;
        }
        Ok(DecodedTypeParameterCursor {
            bytes: self.bytes,
            cursor,
            remaining: self.range.length,
        })
    }
}

impl Deref for DecodedTypeParameterList<'_> {
    type Target = ExtensionTypeParameterRange;

    fn deref(&self) -> &Self::Target {
        &self.range
    }
}

/// Fallible sequential view of one validated exact type-parameter range.
#[derive(Clone, Copy, Debug)]
pub struct DecodedTypeParameterCursor<'payload> {
    bytes: &'payload [u8],
    cursor: usize,
    remaining: u32,
}

impl<'payload> Iterator for DecodedTypeParameterCursor<'payload> {
    type Item = Result<DecodedTypeParameter<'payload>, ExtensionPoolFault>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let decoded = decode_type_parameter_at(self.bytes, self.cursor);
        match decoded {
            Ok((parameter, next)) => {
                self.cursor = next;
                self.remaining -= 1;
                Some(Ok(parameter))
            }
            Err(fault) => {
                self.remaining = 0;
                Some(Err(fault))
            }
        }
    }
}

/// Schema-aware reopened interpretation of one extension fact's
/// `TypeParameterListId`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReopenedTypeParameterList<'payload> {
    /// Schema-4 exact `(start, length)` membership.
    Exact(DecodedTypeParameterList<'payload>),
    /// Legacy schema records name only an element start.  No list membership
    /// is implied or fabricated.
    LegacyStartOnly { start: u32 },
}

/// A once-validated reopened pooled-lane section.
#[derive(Clone, Copy, Debug)]
pub struct ReopenedExtensionPools<'payload> {
    bytes: &'payload [u8],
    type_parameters: (usize, u32),
    type_parameter_lists: TypeParameterListLayout,
    atom_lists: (usize, u32),
    type_lists: (usize, u32),
    entity_lists: (usize, u32),
}

#[derive(Clone, Copy, Debug)]
enum TypeParameterListLayout {
    Exact { start: usize, count: u32 },
    LegacyStarts,
}

impl<'payload> ReopenedExtensionPools<'payload> {
    /// Number of pooled type parameters.
    #[must_use]
    pub fn type_parameter_count(&self) -> u32 {
        self.type_parameters.1
    }

    /// The schema-aware meaning of `TypeParameterListId` references in the
    /// paired language-extension section.
    #[must_use]
    pub const fn type_parameter_list_bounds(&self) -> TypeParameterListBounds {
        match self.type_parameter_lists {
            TypeParameterListLayout::Exact { count, .. } => {
                TypeParameterListBounds::ExactRanges { count }
            }
            TypeParameterListLayout::LegacyStarts => TypeParameterListBounds::LegacyStarts {
                element_count: self.type_parameters.1,
            },
        }
    }

    /// Number of exact type-parameter lists when this schema carries range
    /// membership. Legacy start-only pools deliberately report `None`.
    #[must_use]
    pub const fn type_parameter_list_count(&self) -> Option<u32> {
        match self.type_parameter_lists {
            TypeParameterListLayout::Exact { count, .. } => Some(count),
            TypeParameterListLayout::LegacyStarts => None,
        }
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
        decode_type_parameter(
            self.bytes,
            self.type_parameters.0,
            self.type_parameters.1,
            ordinal,
        )
    }

    /// Reopens one `TypeParameterListId` under the schema's explicit
    /// membership contract. See [`ReopenedTypeParameterList`] for legacy
    /// behavior.
    pub fn type_parameter_list(
        &self,
        list: crate::TypeParameterListId,
    ) -> Result<ReopenedTypeParameterList<'payload>, ExtensionPoolFault> {
        match self.type_parameter_lists {
            TypeParameterListLayout::Exact { start, count } => {
                if list.raw >= count {
                    return Err(ExtensionPoolFault::TypeParameterList {
                        list: list.raw,
                        count,
                    });
                }
                let offset = usize::try_from(list.raw)
                    .ok()
                    .and_then(|ordinal| ordinal.checked_mul(8))
                    .and_then(|width| start.checked_add(width))
                    .ok_or(ExtensionPoolFault::StructuralOverflow { at: start })?;
                let range = ExtensionTypeParameterRange {
                    start: read_u32(self.bytes, offset)?,
                    length: read_u32(
                        self.bytes,
                        offset.checked_add(4).ok_or(ExtensionPoolFault::StructuralOverflow {
                            at: offset,
                        })?,
                    )?,
                };
                Ok(ReopenedTypeParameterList::Exact(DecodedTypeParameterList {
                    bytes: self.bytes,
                    elements_start: self.type_parameters.0,
                    element_count: self.type_parameters.1,
                    range,
                }))
            }
            TypeParameterListLayout::LegacyStarts => {
                if list.raw > self.type_parameters.1 {
                    return Err(ExtensionPoolFault::LegacyTypeParameterStart {
                        start: list.raw,
                        element_count: self.type_parameters.1,
                    });
                }
                Ok(ReopenedTypeParameterList::LegacyStartOnly { start: list.raw })
            }
        }
    }

    /// Decodes one pooled reference list from its closed homogeneous lane.
    pub fn reference_list(
        &self,
        lane: ExtensionPoolListLane,
        ordinal: u32,
    ) -> Result<DecodedRefList<'payload>, ExtensionPoolFault> {
        let (start, count) = match lane {
            ExtensionPoolListLane::Atoms => self.atom_lists,
            ExtensionPoolListLane::Types => self.type_lists,
            ExtensionPoolListLane::Entities => self.entity_lists,
        };
        if ordinal >= count {
            return Err(ExtensionPoolFault::Reference {
                lane,
                list: ordinal,
                position: 0,
                raw: ordinal,
                limit: count,
            });
        }
        let mut cursor = start;
        for _ in 0..ordinal {
            cursor = skip_ref_list(self.bytes, cursor)?;
        }
        let len = usize::try_from(read_u32(self.bytes, cursor)?)
            .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
        let words_start = cursor
            .checked_add(4)
            .ok_or(ExtensionPoolFault::StructuralOverflow { at: cursor })?;
        let words_len = len
            .checked_mul(4)
            .ok_or(ExtensionPoolFault::StructuralOverflow { at: cursor })?;
        let words_end = words_start
            .checked_add(words_len)
            .ok_or(ExtensionPoolFault::StructuralOverflow { at: cursor })?;
        let words = self
            .bytes
            .get(words_start..words_end)
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
        self.reference_list(lane, ordinal)
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
    schema: u16,
    payload: &[u8],
    atom_count: u32,
    type_count: u32,
    entity_count: u32,
) -> Result<ReopenedExtensionPools<'_>, ExtensionPoolFault> {
    validate_extension_pool_payload(schema, payload, atom_count, type_count, entity_count)?;
    reopen_validated_extension_pools(schema, payload)
}

/// Decodes an extension-pool layout whose bytes and cross-lane references
/// were already proved by fragment validation. This is crate-private so a
/// caller cannot bypass the public checked reopen boundary.
pub(crate) fn reopen_validated_extension_pools(
    schema: u16,
    payload: &[u8],
) -> Result<ReopenedExtensionPools<'_>, ExtensionPoolFault> {
    let type_count = read_u32(payload, 0)?;
    let mut cursor = 4;
    let type_parameters = (cursor, type_count);
    for _ in 0..type_count {
        cursor = skip_type_parameter(payload, cursor)?;
    }
    let type_parameter_lists = if schema >= 4 {
        let count = read_u32(payload, cursor)?;
        cursor = advance(cursor, 4)?;
        let start = cursor;
        let bytes = usize::try_from(count)
            .ok()
            .and_then(|count| count.checked_mul(8))
            .ok_or(ExtensionPoolFault::StructuralOverflow { at: cursor })?;
        cursor = advance(cursor, bytes)?;
        TypeParameterListLayout::Exact { start, count }
    } else {
        TypeParameterListLayout::LegacyStarts
    };
    let atom_count = read_u32(payload, cursor)?;
    cursor = advance(cursor, 4)?;
    let atom_lists = (cursor, atom_count);
    for _ in 0..atom_count {
        cursor = skip_ref_list(payload, cursor)?;
    }
    let type_count = read_u32(payload, cursor)?;
    cursor = advance(cursor, 4)?;
    let type_lists = (cursor, type_count);
    for _ in 0..type_count {
        cursor = skip_ref_list(payload, cursor)?;
    }
    let entity_count = read_u32(payload, cursor)?;
    cursor = advance(cursor, 4)?;
    let entity_lists = (cursor, entity_count);
    Ok(ReopenedExtensionPools {
        bytes: payload,
        type_parameters,
        type_parameter_lists,
        atom_lists,
        type_lists,
        entity_lists,
    })
}

/// Validates one pooled-lane payload against the carrying fragment's lane
/// counts, proving every reference and type-parameter cell.
pub(crate) fn validate_extension_pool_payload(
    schema: u16,
    payload: &[u8],
    atom_count: u32,
    type_count: u32,
    entity_count: u32,
) -> Result<(), ExtensionPoolFault> {
    let mut cursor = 4;
    let type_parameter_count = read_u32(payload, 0)?;
    for ordinal in 0..type_parameter_count {
        let name_len_offset = advance(cursor, 1)?;
        let name_len = usize::try_from(read_u32(payload, name_len_offset)?)
            .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
        let name_start = advance(cursor, 5)?;
        let name_end = advance(name_start, name_len)?;
        let name = payload
            .get(name_start..name_end)
            .ok_or(ExtensionPoolFault::Truncated { needed: cursor })?;
        if name.is_empty() {
            return Err(ExtensionPoolFault::EmptyName { ordinal });
        }
        let mut at = name_end;
        for field in [TypeParameterField::Constraint, TypeParameterField::Default] {
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
    if schema >= 4 {
        let range_count = read_u32(payload, cursor)?;
        cursor = advance(cursor, 4)?;
        for list in 0..range_count {
            let start = read_u32(payload, cursor)?;
            let length = read_u32(payload, advance(cursor, 4)?)?;
            let end = start.checked_add(length).ok_or(
                ExtensionPoolFault::TypeParameterRange {
                    list,
                    start,
                    length,
                    element_count: type_parameter_count,
                },
            )?;
            if end > type_parameter_count {
                return Err(ExtensionPoolFault::TypeParameterRange {
                    list,
                    start,
                    length,
                    element_count: type_parameter_count,
                });
            }
            cursor = advance(cursor, 8)?;
        }
    }
    for (lane, limit) in [
        (ExtensionPoolListLane::Atoms, atom_count),
        (ExtensionPoolListLane::Types, type_count),
        (ExtensionPoolListLane::Entities, entity_count),
    ] {
        let count = read_u32(payload, cursor)
            .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
        cursor = advance(cursor, 4)?;
        for list in 0..count {
            let len = usize::try_from(read_u32(payload, cursor)?)
                .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
            let words_start = advance(cursor, 4)?;
            for position in 0..len {
                let word_offset = position
                    .checked_mul(4)
                    .and_then(|width| words_start.checked_add(width))
                    .ok_or(ExtensionPoolFault::StructuralOverflow { at: cursor })?;
                let raw = read_u32(payload, word_offset)?;
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
            let words = len
                .checked_mul(4)
                .ok_or(ExtensionPoolFault::StructuralOverflow { at: cursor })?;
            cursor = advance(advance(cursor, 4)?, words)?;
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
    let name_len = usize::try_from(read_u32(bytes, advance(cursor, 1)?)?)
        .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
    let mut next = advance(advance(cursor, 5)?, name_len)?;
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
    let words = len
        .checked_mul(4)
        .ok_or(ExtensionPoolFault::StructuralOverflow { at: cursor })?;
    advance(advance(cursor, 4)?, words)
}

fn read_optional(bytes: &[u8], at: &mut usize) -> Result<Option<u32>, ExtensionPoolFault> {
    match bytes.get(*at).copied() {
        Some(PRESENCE_NONE) => {
            *at = advance(*at, 1)?;
            Ok(None)
        }
        Some(PRESENCE_SOME) => {
            let raw = read_u32(bytes, advance(*at, 1)?)?;
            *at = advance(*at, 5)?;
            Ok(Some(raw))
        }
        actual => Err(ExtensionPoolFault::Presence {
            actual: actual.unwrap_or(PRESENCE_NONE),
        }),
    }
}

fn advance(at: usize, width: usize) -> Result<usize, ExtensionPoolFault> {
    at.checked_add(width)
        .ok_or(ExtensionPoolFault::StructuralOverflow { at })
}

fn decode_type_parameter<'payload>(
    bytes: &'payload [u8],
    start: usize,
    count: u32,
    ordinal: u32,
) -> Result<DecodedTypeParameter<'payload>, ExtensionPoolFault> {
    if ordinal >= count {
        return Err(ExtensionPoolFault::TypeParameterElement {
            ordinal,
            count,
        });
    }
    let mut cursor = start;
    for _ in 0..ordinal {
        cursor = skip_type_parameter(bytes, cursor)?;
    }
    decode_type_parameter_at(bytes, cursor).map(|(parameter, _)| parameter)
}

fn decode_type_parameter_at<'payload>(
    bytes: &'payload [u8],
    cursor: usize,
) -> Result<(DecodedTypeParameter<'payload>, usize), ExtensionPoolFault> {
    let name_len = usize::try_from(read_u32(bytes, advance(cursor, 1)?)?)
        .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
    let name_start = advance(cursor, 5)?;
    let name_end = advance(name_start, name_len)?;
    let name = bytes
        .get(name_start..name_end)
        .ok_or(ExtensionPoolFault::Truncated { needed: cursor })?;
    let mut at = name_end;
    let constraint = read_optional(bytes, &mut at)?;
    let default = read_optional(bytes, &mut at)?;
    Ok((
        DecodedTypeParameter {
            name,
            constraint,
            default,
        },
        at,
    ))
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
